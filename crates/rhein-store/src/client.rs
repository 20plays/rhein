//! `Client`: the one entity that holds everything the UI shows — the home session, every connected
//! guild server, their guilds, channel histories and member lists — and owns all network work.
//!
//! Views read fields and call methods; every method that talks to a server hands its future to
//! [`Io`] and applies the answer in `this.update`, ending with `cx.notify()`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use gpui::{Context, EventEmitter, SharedString, Task, WeakEntity};
use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_net::gateway::{self, GatewayConfig};
use rhein_net::{Api, GatewayEvent, GatewayHandle, GatewayStatus, auth, discover};
use rhein_proto::{
    Attachment, Channel, Discovery, Event, Guild, Member, Message, Ready, SendMessage,
    USER_INTENTS, localpart,
};

use crate::io::{Io, join};
use crate::persist::{self, Saved};

const PAGE: u32 = 50;
const TYPING_SHOWN_FOR: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    /// Looking for a saved session at startup.
    Restoring,
    SignedOut,
    SigningIn,
    SignedIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStatus {
    Connecting,
    Ready,
    Reconnecting,
    Renewing,
    Incompatible,
    Failed,
}

impl ServerStatus {
    pub fn label(self) -> &'static str {
        match self {
            ServerStatus::Connecting => "Connecting…",
            ServerStatus::Ready => "Connected",
            ServerStatus::Reconnecting => "Connection lost, retrying…",
            ServerStatus::Renewing => "Renewing session…",
            ServerStatus::Incompatible => "Incompatible protocol version",
            ServerStatus::Failed => "Unreachable",
        }
    }
}

pub struct Home {
    pub input: String,
    pub doc: Discovery,
    pub api: Api,
    pub user_id: String,
    pub device_id: String,
    pub display_name: Option<String>,
    device_seed: String,
    expires_at: Option<i64>,
}

pub struct Server {
    /// The logical server name (the audience of its assertions).
    pub name: String,
    /// What it was added as, for persistence.
    pub input: String,
    pub doc: Option<Discovery>,
    pub api: Option<Api>,
    pub status: ServerStatus,
    pub error: Option<String>,
    gateway: Option<GatewayHandle>,
    connect: Option<Task<()>>,
}

pub struct GuildEntry {
    pub server: String,
    pub guild: Guild,
}

impl GuildEntry {
    /// Channels in display order: uncategorised first, then each category followed by its
    /// children, everything by `position`.
    pub fn ordered_channels(&self) -> Vec<&Channel> {
        let mut chans: Vec<&Channel> = self.guild.channels.iter().collect();
        chans.sort_by_key(|c| (c.position, c.id.clone()));
        let mut out = Vec::with_capacity(chans.len());
        for c in chans.iter().filter(|c| c.parent_id.is_none() && !c.is_category()) {
            out.push(*c);
        }
        for cat in chans.iter().filter(|c| c.is_category()) {
            out.push(*cat);
            for c in chans.iter().filter(|c| c.parent_id.as_deref() == Some(cat.id.as_str())) {
                out.push(*c);
            }
        }
        // Children whose category is not visible to us.
        for c in chans.iter() {
            if !out.iter().any(|o| o.id == c.id) {
                out.push(*c);
            }
        }
        out
    }
}

#[derive(Default)]
pub struct ChannelLog {
    pub messages: Vec<Message>,
    pub loaded: bool,
    pub loading: bool,
    pub reached_start: bool,
    pub error: Option<String>,
    task: Option<Task<()>>,
}

/// How much of a channel this user has not read yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadMark {
    pub unread: u32,
    pub mentions: u32,
    pub muted: bool,
}

impl ReadMark {
    pub fn is_unread(&self) -> bool {
        self.unread > 0 && !self.muted
    }
}

/// What changed in a channel's message list, so the view can keep its scroll position.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    MessagesReset { channel_id: String },
    MessagesPrepended { channel_id: String, count: usize },
    MessageAppended { channel_id: String },
    MessageChanged { channel_id: String, index: usize },
    MessageRemoved { channel_id: String, index: usize },
    Notice(SharedString),
}

pub struct Client {
    io: Io,
    http: reqwest::Client,
    pub phase: Phase,
    pub error: Option<SharedString>,
    pub notice: Option<SharedString>,
    pub home: Option<Home>,
    pub servers: Vec<Server>,
    pub guilds: Vec<GuildEntry>,
    pub selected_guild: Option<String>,
    /// The home view (direct messages) is showing instead of a guild.
    pub showing_home: bool,
    read: HashMap<String, ReadMark>,
    /// guild id → user id → the voice channel they are in.
    voice: HashMap<String, HashMap<String, String>>,
    selected_channel: HashMap<String, String>,
    pub logs: HashMap<String, ChannelLog>,
    pub members: HashMap<String, Vec<Member>>,
    typing: HashMap<String, Vec<(String, Instant)>>,
    signin: Option<Task<()>>,
}

impl EventEmitter<ClientEvent> for Client {}

impl Client {
    pub fn new(io: Io, cx: &mut Context<Self>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("Rhein/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("http client");
        let mut this = Self {
            io,
            http,
            phase: Phase::Restoring,
            error: None,
            notice: None,
            home: None,
            servers: Vec::new(),
            guilds: Vec::new(),
            selected_guild: None,
            selected_channel: HashMap::new(),
            logs: HashMap::new(),
            members: HashMap::new(),
            typing: HashMap::new(),
            showing_home: false,
            read: HashMap::new(),
            voice: HashMap::new(),
            signin: None,
        };
        this.restore(cx);
        this
    }

    // ---------------------------------------------------------------- sign-in

    fn restore(&mut self, cx: &mut Context<Self>) {
        let Some(saved) = persist::load() else {
            log::info!("no saved session at {}", persist::path().display());
            self.phase = Phase::SignedOut;
            return;
        };
        log::info!("restoring session for {} on {}", saved.user_id, saved.home);
        if saved.expires_at.is_some_and(|at| at <= now_ms()) {
            persist::clear();
            self.phase = Phase::SignedOut;
            self.error = Some("Your session has expired. Please sign in again.".into());
            return;
        }
        let io = self.io.clone();
        let http = self.http.clone();
        let input = saved.home.clone();
        let token = saved.access_token.clone();
        self.signin = Some(cx.spawn(async move |this, cx| {
            let result = join(io.spawn(async move {
                let doc = discover(&http, &input).await?;
                let api = Api::new(http, &doc.api, Some(token));
                let me = api.whoami().await.context("whoami")?;
                Ok((doc, api, me))
            }))
            .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((doc, api, me)) => {
                        let display_name = me.profile.and_then(|p| p.display_name);
                        let home = Home {
                            input: saved.home.clone(),
                            doc,
                            api,
                            user_id: saved.user_id.clone(),
                            device_id: saved.device_id.clone(),
                            display_name,
                            device_seed: saved.device_seed.clone(),
                            expires_at: saved.expires_at,
                        };
                        this.signed_in(home, saved.servers.clone(), cx);
                    }
                    Err(err) => {
                        log::warn!("restoring session failed: {err:#}");
                        let unauthorized = err
                            .downcast_ref::<rhein_net::ApiError>()
                            .and_then(|e| e.status())
                            .is_some_and(|s| s == 401);
                        if unauthorized {
                            persist::clear();
                            this.error = Some("Your session was revoked. Please sign in again.".into());
                        } else {
                            this.error = Some(format!("Can't reach your home server: {err:#}").into());
                        }
                        this.phase = Phase::SignedOut;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Signs in (or registers) with OPAQUE. `server` is a domain such as `chat.example.com` or
    /// `localhost:8448`; `user` is a localpart or a full `@user:server`.
    pub fn sign_in(&mut self, server: String, user: String, password: String, register: bool, cx: &mut Context<Self>) {
        if self.phase == Phase::SigningIn {
            return;
        }
        self.phase = Phase::SigningIn;
        self.error = None;
        cx.notify();
        let io = self.io.clone();
        let http = self.http.clone();
        let device_name = device_name();
        self.signin = Some(cx.spawn(async move |this, cx| {
            let input = server.trim().to_owned();
            let result = join(io.spawn(async move {
                let doc = discover(&http, &input).await?;
                let api = Api::new(http, &doc.api, None);
                let device = DeviceKey::generate();
                let ksf = SerenityKsf::memory_constrained();
                let user = user.trim();
                let signed = if register {
                    auth::register(&api, localpart(user), &password, &device, &device_name, &ksf).await?
                } else {
                    auth::login(&api, user, &password, &device, &device_name, &ksf).await?
                };
                let me = api.whoami().await.ok();
                Ok((input, doc, api, signed, device, me))
            }))
            .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok((input, doc, api, signed, device, me)) => {
                        let home = Home {
                            input,
                            doc,
                            api,
                            user_id: signed.session.user_id.clone(),
                            device_id: signed.session.device_id.clone(),
                            display_name: me.and_then(|m| m.profile).and_then(|p| p.display_name),
                            device_seed: rhein_auth::b64u(&device.seed()),
                            expires_at: signed.session.expires_at,
                        };
                        this.signed_in(home, Vec::new(), cx);
                    }
                    Err(err) => {
                        this.phase = Phase::SignedOut;
                        this.error = Some(friendly(&err).into());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn signed_in(&mut self, home: Home, extra: Vec<String>, cx: &mut Context<Self>) {
        let home_name = home.doc.server_name.clone();
        let home_input = home.input.clone();
        self.home = Some(home);
        self.phase = Phase::SignedIn;
        self.error = None;
        self.persist();
        // The home server may host guilds too; it gets a guild session like any other server.
        self.add_server_named(home_input, Some(home_name), cx);
        for input in extra {
            self.add_server_named(input, None, cx);
        }
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        log::info!("signing out");
        persist::clear();
        self.servers.clear();
        self.guilds.clear();
        self.logs.clear();
        self.members.clear();
        self.typing.clear();
        self.read.clear();
        self.voice.clear();
        self.showing_home = false;
        self.selected_guild = None;
        self.selected_channel.clear();
        self.home = None;
        self.phase = Phase::SignedOut;
        cx.notify();
    }

    fn persist(&self) {
        let Some(home) = &self.home else { return };
        let saved = Saved {
            home: home.input.clone(),
            user_id: home.user_id.clone(),
            device_id: home.device_id.clone(),
            access_token: String::new(),
            expires_at: home.expires_at,
            device_seed: home.device_seed.clone(),
            servers: self
                .servers
                .iter()
                .filter(|s| s.name != home.doc.server_name)
                .map(|s| s.input.clone())
                .collect(),
        };
        let io = self.io.clone();
        let api = home.api.clone();
        // The token lives inside `Api`; read it on the runtime and write the file there too.
        io.spawn(async move {
            let mut saved = saved;
            saved.access_token = api.token().await.unwrap_or_default();
            if let Err(err) = persist::store(&saved) {
                log::error!("saving session: {err:#}");
            }
        });
    }

    pub fn user_id(&self) -> Option<&str> {
        self.home.as_ref().map(|h| h.user_id.as_str())
    }

    // ---------------------------------------------------------------- servers

    /// Connects to another guild server under the signed-in account.
    pub fn add_server(&mut self, input: String, cx: &mut Context<Self>) {
        let input = input.trim().to_owned();
        if input.is_empty() || self.servers.iter().any(|s| s.input == input || s.name == input) {
            return;
        }
        self.add_server_named(input, None, cx);
        self.persist();
    }

    pub fn remove_server(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.home.as_ref().is_some_and(|h| h.doc.server_name == name) {
            return;
        }
        self.servers.retain(|s| s.name != name);
        self.guilds.retain(|g| g.server != name);
        if self.selected_guild.as_ref().is_some_and(|g| self.guild(g).is_none()) {
            self.selected_guild = None;
        }
        self.persist();
        cx.notify();
    }

    fn add_server_named(&mut self, input: String, name: Option<String>, cx: &mut Context<Self>) {
        let placeholder = name.clone().unwrap_or_else(|| input.clone());
        self.servers.push(Server {
            name: placeholder.clone(),
            input: input.clone(),
            doc: None,
            api: None,
            status: ServerStatus::Connecting,
            error: None,
            gateway: None,
            connect: None,
        });
        self.connect_server(placeholder, cx);
        cx.notify();
    }

    fn server_mut(&mut self, name: &str) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.name == name)
    }

    pub fn server(&self, name: &str) -> Option<&Server> {
        self.servers.iter().find(|s| s.name == name)
    }

    /// Discovery, assertion → guild session, then the gateway.
    fn connect_server(&mut self, key: String, cx: &mut Context<Self>) {
        if self.home.is_none() {
            return;
        }
        let Some(server) = self.server_mut(&key) else { return };
        server.status = ServerStatus::Connecting;
        server.error = None;
        server.gateway = None;
        let input = server.input.clone();
        let io = self.io.clone();
        let http = self.http.clone();
        let home_api = self.home.as_ref().map(|h| h.api.clone()).unwrap();
        let task_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let key = task_key;
            let result = join(io.spawn(async move {
                let doc = discover(&http, &input).await?;
                let api = Api::new(http, &doc.api, None);
                let session = auth::guild_session(&home_api, &api, &doc.server_name).await?;
                let url = session
                    .gateway
                    .clone()
                    .or_else(|| doc.gateway.clone())
                    .ok_or_else(|| anyhow!("{} publishes no gateway", doc.server_name))?;
                let (handle, events) = gateway::connect(GatewayConfig {
                    url,
                    token: session.session_token,
                    intents: USER_INTENTS,
                    status: "online".into(),
                });
                Ok((doc, api, handle, events))
            }))
            .await;
            let Ok(Some(mut events)) = this.update(cx, |this, cx| {
                let server = this.server_mut(&key)?;
                match result {
                    Ok((doc, api, handle, events)) => {
                        let real = doc.server_name.clone();
                        server.name = real.clone();
                        server.doc = Some(doc);
                        server.api = Some(api);
                        server.gateway = Some(handle);
                        cx.notify();
                        // Two inputs can name the same server; keep the one connected first.
                        let duplicate = this.servers.iter().filter(|s| s.name == real).count() > 1;
                        if duplicate {
                            if let Some(i) = this.servers.iter().rposition(|s| s.name == real) {
                                this.servers.remove(i);
                            }
                            return None;
                        }
                        Some(events)
                    }
                    Err(err) => {
                        log::warn!("connecting {key}: {err:#}");
                        server.status = ServerStatus::Failed;
                        server.error = Some(friendly(&err));
                        cx.notify();
                        None
                    }
                }
            }) else {
                return;
            };
            let name = this
                .read_with(cx, |this, _| {
                    this.servers
                        .iter()
                        .find(|s| s.input == key || s.name == key)
                        .map(|s| s.name.clone())
                })
                .ok()
                .flatten();
            let Some(name) = name else { return };
            // Pump gateway events into the entity until the connection is dropped.
            while let Some(event) = events.recv().await {
                if this.update(cx, |this, cx| this.on_gateway(&name, event, cx)).is_err() {
                    break;
                }
            }
        });
        if let Some(server) = self.server_mut(&key) {
            server.connect = Some(task);
        }
    }

    /// Mints a fresh guild session and hands its token to the live gateway task.
    fn renew_server(&mut self, name: &str, cx: &mut Context<Self>) {
        let (Some(home), Some(server)) = (&self.home, self.servers.iter().find(|s| s.name == name)) else {
            return;
        };
        let (Some(api), Some(doc)) = (server.api.clone(), server.doc.clone()) else { return };
        let home_api = home.api.clone();
        let io = self.io.clone();
        let name = name.to_owned();
        let task = cx.spawn(async move |this, cx| {
            let result = join(io.spawn(async move {
                let session = auth::guild_session(&home_api, &api, &doc.server_name).await?;
                Ok(session.session_token)
            }))
            .await;
            this.update(cx, |this, cx| {
                let Some(server) = this.server_mut(&name) else { return };
                match result {
                    Ok(token) => {
                        if let Some(gw) = &server.gateway {
                            gw.reconnect(token);
                        }
                    }
                    Err(err) => {
                        server.status = ServerStatus::Failed;
                        server.error = Some(friendly(&err));
                    }
                }
                cx.notify();
            })
            .ok();
        });
        task.detach();
    }

    /// Re-identifies so a guild created or joined since the last READY is streamed too; the
    /// gateway does not add guilds to a live session.
    fn refresh_server(&mut self, name: &str, cx: &mut Context<Self>) {
        self.renew_server(name, cx);
    }

    fn on_gateway(&mut self, server: &str, event: GatewayEvent, cx: &mut Context<Self>) {
        match event {
            GatewayEvent::Status(status) => {
                let renew = matches!(status, GatewayStatus::Rejected);
                if let Some(s) = self.server_mut(server) {
                    s.status = match status {
                        GatewayStatus::Connecting => ServerStatus::Connecting,
                        GatewayStatus::Ready => ServerStatus::Ready,
                        GatewayStatus::Backoff => ServerStatus::Reconnecting,
                        GatewayStatus::Rejected => ServerStatus::Renewing,
                        GatewayStatus::Incompatible => ServerStatus::Incompatible,
                    };
                }
                if renew {
                    self.renew_server(server, cx);
                }
            }
            GatewayEvent::Ready(ready) => self.on_ready(server, *ready, cx),
            GatewayEvent::Dispatch(event) => self.on_dispatch(server, *event, cx),
        }
        cx.notify();
    }

    fn on_ready(&mut self, server: &str, ready: Ready, cx: &mut Context<Self>) {
        log::info!("READY from {server}: {} guild(s)", ready.guilds.len());
        // READY replaces everything this server said before: there is no resume.
        let insert_at = self
            .guilds
            .iter()
            .position(|g| g.server == server)
            .unwrap_or(self.guilds.len());
        self.guilds.retain(|g| g.server != server);
        let incoming: Vec<GuildEntry> = ready
            .guilds
            .into_iter()
            .filter(|g| g.moved_to.is_none() || !g.channels.is_empty())
            .map(|guild| GuildEntry { server: server.to_owned(), guild })
            .collect();
        let at = insert_at.min(self.guilds.len());
        for entry in &incoming {
            let roster = entry
                .guild
                .voice_states
                .iter()
                .filter_map(|v| v.channel_id.clone().map(|c| (v.user_id.clone(), c)))
                .collect();
            self.voice.insert(entry.guild.id.clone(), roster);
        }
        let ids: Vec<String> = incoming.iter().map(|g| g.guild.id.clone()).collect();
        self.guilds.splice(at..at, incoming);
        for id in ids {
            self.load_read_state(id, cx);
        }

        if self.selected_guild.as_ref().is_none_or(|g| self.guild(g).is_none()) {
            self.selected_guild = self.guilds.first().map(|g| g.guild.id.clone());
        }
        // History may have moved on while we were away; reload what is on screen.
        let stale: Vec<String> = self
            .logs
            .keys()
            .filter(|c| self.server_of_channel(c).is_some_and(|s| s == server))
            .cloned()
            .collect();
        for channel in stale {
            self.logs.remove(&channel);
            cx.emit(ClientEvent::MessagesReset { channel_id: channel });
        }
        if let Some(guild) = self.selected_guild.clone()
            && self.guild(&guild).is_some_and(|g| g.server == server) {
                self.open_guild(guild, cx);
            }
    }

    fn on_dispatch(&mut self, _server: &str, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::MessageCreate(message) => {
                log::debug!("message {} in {}", message.id, message.channel_id);
                let channel_id = message.channel_id.clone();
                if let Some(typing) = self.typing.get_mut(&channel_id) {
                    typing.retain(|(u, _)| *u != message.author);
                }
                self.count_unread(&message, cx);
                let Some(log) = self.logs.get_mut(&channel_id) else { return };
                if !log.loaded {
                    return;
                }
                if log.messages.iter().any(|m| m.id == message.id) {
                    return;
                }
                // Our own send may already be here under its nonce.
                if let Some(nonce) = &message.nonce
                    && let Some(i) = log.messages.iter().position(|m| m.nonce.as_ref() == Some(nonce) && m.id.starts_with("pending:")) {
                        log.messages[i] = message;
                        cx.emit(ClientEvent::MessageChanged { channel_id, index: i });
                        return;
                    }
                log.messages.push(message);
                cx.emit(ClientEvent::MessageAppended { channel_id });
            }
            Event::MessageUpdate { id, channel_id, content, edited_at } => {
                if let Some(log) = self.logs.get_mut(&channel_id)
                    && let Some(i) = log.messages.iter().position(|m| m.id == id) {
                        log.messages[i].content = content;
                        log.messages[i].edited_at = edited_at;
                        cx.emit(ClientEvent::MessageChanged { channel_id, index: i });
                    }
            }
            Event::MessageDelete { id, channel_id } => {
                if let Some(log) = self.logs.get_mut(&channel_id)
                    && let Some(i) = log.messages.iter().position(|m| m.id == id) {
                        log.messages.remove(i);
                        cx.emit(ClientEvent::MessageRemoved { channel_id, index: i });
                    }
            }
            Event::ReactionAdd { message_id, channel_id, emoji, user_id } => {
                self.reaction(channel_id, message_id, emoji, user_id, true, cx);
            }
            Event::ReactionRemove { message_id, channel_id, emoji, user_id } => {
                self.reaction(channel_id, message_id, emoji, user_id, false, cx);
            }
            Event::TypingStart { channel_id, user_id } => {
                if Some(user_id.as_str()) == self.user_id() {
                    return;
                }
                let entry = self.typing.entry(channel_id).or_default();
                entry.retain(|(u, _)| *u != user_id);
                entry.push((user_id, Instant::now()));
                // Repaint once the indicator has expired.
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(TYPING_SHOWN_FOR).await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                });
                task.detach();
            }
            Event::ChannelCreate(channel) | Event::ChannelUpdate(channel) => {
                let Some(gid) = channel.guild_id.clone() else { return };
                if let Some(entry) = self.guild_mut(&gid) {
                    match entry.guild.channels.iter().position(|c| c.id == channel.id) {
                        Some(i) => entry.guild.channels[i] = channel,
                        None => entry.guild.channels.push(channel),
                    }
                }
            }
            Event::ChannelDelete { id, guild_id } => {
                if let Some(entry) = guild_id.as_deref().and_then(|g| self.guild_mut(g)) {
                    entry.guild.channels.retain(|c| c.id != id);
                }
            }
            Event::GuildUpdate(d) => {
                let id = d.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
                if let Some(entry) = self.guild_mut(&id)
                    && let Some(name) = d.get("name").and_then(|v| v.as_str()) {
                        entry.guild.name = name.to_owned();
                    }
            }
            Event::GuildDelete { id } => {
                self.guilds.retain(|g| g.guild.id != id);
                if self.selected_guild.as_deref() == Some(id.as_str()) {
                    self.selected_guild = self.guilds.first().map(|g| g.guild.id.clone());
                }
            }
            Event::MemberAdd(_) | Event::MemberUpdate(_) | Event::MemberRemove(_) => {
                // Cheap enough to refetch the visible roster.
                let gid = match &event {
                    Event::MemberAdd(d) | Event::MemberUpdate(d) | Event::MemberRemove(d) => {
                        d.get("guild_id").and_then(|v| v.as_str()).map(str::to_owned)
                    }
                    _ => None,
                };
                if let Some(gid) = gid
                    && self.members.contains_key(&gid) {
                        self.load_members(gid, cx);
                    }
            }
            Event::PresenceUpdate { user_id, status } => {
                for list in self.members.values_mut() {
                    for m in list.iter_mut().filter(|m| m.user_id == user_id) {
                        m.presence = Some(serde_json::json!({ "status": status }));
                    }
                }
            }
            Event::VoiceStateUpdate(state) => {
                let Some(guild) = state.guild_id.clone() else { return };
                let roster = self.voice.entry(guild).or_default();
                match state.channel_id {
                    Some(channel) => {
                        roster.insert(state.user_id, channel);
                    }
                    None => {
                        roster.remove(&state.user_id);
                    }
                }
            }
            Event::MemberListUpdate(_) | Event::Other { .. } => {}
        }
    }

    fn reaction(&mut self, channel_id: String, message_id: String, emoji: String, user_id: String, add: bool, cx: &mut Context<Self>) {
        let me = self.user_id().map(str::to_owned);
        let Some(log) = self.logs.get_mut(&channel_id) else { return };
        let Some(i) = log.messages.iter().position(|m| m.id == message_id) else { return };
        apply_reaction(&mut log.messages[i].reactions, &emoji, &user_id, add, me.as_deref());
        cx.emit(ClientEvent::MessageChanged { channel_id, index: i });
    }

    // ---------------------------------------------------------------- guilds & channels

    pub fn guild(&self, id: &str) -> Option<&GuildEntry> {
        self.guilds.iter().find(|g| g.guild.id == id)
    }

    fn guild_mut(&mut self, id: &str) -> Option<&mut GuildEntry> {
        self.guilds.iter_mut().find(|g| g.guild.id == id)
    }

    pub fn guild_of_channel(&self, channel_id: &str) -> Option<&GuildEntry> {
        self.guilds.iter().find(|g| g.guild.channels.iter().any(|c| c.id == channel_id))
    }

    pub fn channel(&self, channel_id: &str) -> Option<&Channel> {
        self.guild_of_channel(channel_id)
            .and_then(|g| g.guild.channels.iter().find(|c| c.id == channel_id))
    }

    fn server_of_channel(&self, channel_id: &str) -> Option<&str> {
        self.guild_of_channel(channel_id).map(|g| g.server.as_str())
    }

    fn api_for_guild(&self, guild_id: &str) -> Option<Api> {
        let server = &self.guild(guild_id)?.server;
        self.server(server)?.api.clone()
    }

    fn api_for_channel(&self, channel_id: &str) -> Option<Api> {
        let server = self.server_of_channel(channel_id)?;
        self.server(server)?.api.clone()
    }

    pub fn selected_channel(&self) -> Option<&str> {
        if self.showing_home {
            return None;
        }
        let guild = self.selected_guild.as_ref()?;
        self.selected_channel.get(guild).map(String::as_str)
    }

    pub fn show_home(&mut self, cx: &mut Context<Self>) {
        self.showing_home = true;
        cx.notify();
    }

    pub fn select_guild(&mut self, id: String, cx: &mut Context<Self>) {
        self.showing_home = false;
        self.selected_guild = Some(id.clone());
        self.open_guild(id, cx);
        cx.notify();
    }

    fn open_guild(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(entry) = self.guild(&id) else { return };
        let current = self.selected_channel.get(&id).cloned();
        let valid = current.filter(|c| entry.guild.channels.iter().any(|ch| &ch.id == c));
        let channel = valid.or_else(|| {
            entry.ordered_channels().into_iter().find(|c| c.is_textual()).map(|c| c.id.clone())
        });
        if let Some(channel) = channel {
            self.select_channel(channel, cx);
        }
        if !self.members.contains_key(&id) {
            self.load_members(id, cx);
        }
    }

    pub fn select_channel(&mut self, channel_id: String, cx: &mut Context<Self>) {
        let Some(guild) = self.guild_of_channel(&channel_id).map(|g| g.guild.id.clone()) else { return };
        self.showing_home = false;
        self.selected_guild = Some(guild.clone());
        self.selected_channel.insert(guild, channel_id.clone());
        let had_unread = self.read.get(&channel_id).is_some_and(|m| m.unread > 0 || m.mentions > 0);
        if let Some(mark) = self.read.get_mut(&channel_id) {
            mark.unread = 0;
            mark.mentions = 0;
        }
        let needs_load = self.logs.get(&channel_id).is_none_or(|l| !l.loaded && !l.loading);
        if needs_load {
            self.load_history(channel_id, false, cx);
        } else if had_unread {
            self.ack_latest(&channel_id, cx);
        }
        cx.notify();
    }

    /// Loads the newest page, or with `older` the page before the oldest message we hold.
    pub fn load_history(&mut self, channel_id: String, older: bool, cx: &mut Context<Self>) {
        let Some(api) = self.api_for_channel(&channel_id) else { return };
        let log = self.logs.entry(channel_id.clone()).or_default();
        if log.loading || (older && log.reached_start) {
            return;
        }
        let before = if older {
            log.messages.iter().find(|m| !m.id.starts_with("pending:")).map(|m| m.id.clone())
        } else {
            None
        };
        if older && before.is_none() {
            return;
        }
        log.loading = true;
        log.error = None;
        let io = self.io.clone();
        let chan = channel_id.clone();
        log.task = Some(cx.spawn(async move |this, cx| {
            let result = join(io.spawn(async move {
                Ok(api.messages(&chan, before.as_deref(), PAGE).await?)
            }))
            .await;
            this.update(cx, |this, cx| {
                let Some(log) = this.logs.get_mut(&channel_id) else { return };
                log.loading = false;
                match result {
                    Ok(page) => {
                        log::debug!("history {channel_id}: {} message(s)", page.len());
                        let full = page.len() as u32 >= PAGE;
                        if older {
                            let count = page.len();
                            log.messages.splice(0..0, page);
                            log.reached_start = !full;
                            cx.emit(ClientEvent::MessagesPrepended { channel_id: channel_id.clone(), count });
                        } else {
                            log.messages = page;
                            log.loaded = true;
                            log.reached_start = !full;
                            cx.emit(ClientEvent::MessagesReset { channel_id: channel_id.clone() });
                            this.ack_latest(&channel_id, cx);
                        }
                    }
                    Err(err) => log.error = Some(friendly(&err)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn ack_latest(&mut self, channel_id: &str, _cx: &mut Context<Self>) {
        let Some(api) = self.api_for_channel(channel_id) else { return };
        let Some(last) = self.logs.get(channel_id).and_then(|l| l.messages.last()) else { return };
        if last.id.starts_with("pending:") {
            return;
        }
        let (chan, id) = (channel_id.to_owned(), last.id.clone());
        self.io.spawn(async move {
            let _ = api.ack(&chan, &id).await;
        });
    }

    fn load_read_state(&mut self, guild_id: String, cx: &mut Context<Self>) {
        let Some(api) = self.api_for_guild(&guild_id) else { return };
        let io = self.io.clone();
        cx.spawn(async move |this, cx| {
            let result = join(io.spawn(async move { Ok(api.read_state(&guild_id).await?) })).await;
            this.update(cx, |this, cx| {
                let Ok(list) = result else { return };
                let open = this.selected_channel().map(str::to_owned);
                for ch in list.channels {
                    // What is on screen right now is read, whatever the server counted.
                    let on_screen = open.as_deref() == Some(ch.channel_id.as_str());
                    this.read.insert(
                        ch.channel_id,
                        ReadMark {
                            unread: if on_screen { 0 } else { ch.unread },
                            mentions: if on_screen { 0 } else { ch.mentions },
                            muted: ch.muted,
                        },
                    );
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A message arrived: count it as unread unless it is ours or on screen, and acknowledge it
    /// when it is on screen.
    fn count_unread(&mut self, message: &Message, cx: &mut Context<Self>) {
        let Some(me) = self.user_id().map(str::to_owned) else { return };
        if message.author == me {
            return;
        }
        if self.selected_channel() == Some(message.channel_id.as_str()) {
            if let Some(api) = self.api_for_channel(&message.channel_id) {
                let (chan, id) = (message.channel_id.clone(), message.id.clone());
                self.io.spawn(async move {
                    let _ = api.ack(&chan, &id).await;
                });
            }
            return;
        }
        let mention = mentions_user(&message.content, &me);
        let mark = self.read.entry(message.channel_id.clone()).or_default();
        mark.unread += 1;
        if mention {
            mark.mentions += 1;
        }
        cx.notify();
    }

    pub fn read_mark(&self, channel_id: &str) -> ReadMark {
        self.read.get(channel_id).copied().unwrap_or_default()
    }

    /// Whether anything in the guild is unread, and how many mentions wait there.
    pub fn guild_unread(&self, guild_id: &str) -> (bool, u32) {
        let Some(entry) = self.guild(guild_id) else { return (false, 0) };
        entry.guild.channels.iter().fold((false, 0), |(unread, mentions), c| {
            let mark = self.read_mark(&c.id);
            (unread || mark.is_unread(), mentions + mark.mentions)
        })
    }

    /// Mentions across every guild, for the window title and the home button.
    pub fn total_mentions(&self) -> u32 {
        self.read.values().map(|m| m.mentions).sum()
    }

    /// Who is in a voice channel right now, in join order as far as the server told us.
    pub fn voice_members(&self, guild_id: &str, channel_id: &str) -> Vec<String> {
        let mut users: Vec<String> = self
            .voice
            .get(guild_id)
            .map(|roster| roster.iter().filter(|(_, c)| *c == channel_id).map(|(u, _)| u.clone()).collect())
            .unwrap_or_default();
        users.sort();
        users
    }

    pub fn load_members(&mut self, guild_id: String, cx: &mut Context<Self>) {
        let Some(api) = self.api_for_guild(&guild_id) else { return };
        let io = self.io.clone();
        let gid = guild_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = join(io.spawn(async move { Ok(api.members(&gid, 0, 200).await?) })).await;
            this.update(cx, |this, cx| {
                if let Ok(list) = result {
                    this.members.insert(guild_id, list.members);
                    cx.notify();
                }
            })
            .ok();
        });
        task.detach();
    }

    /// Who to show a user as in a guild: nickname, display name, then the localpart.
    pub fn display_name(&self, guild_id: Option<&str>, user_id: &str) -> String {
        if let Some(m) = guild_id
            .and_then(|g| self.members.get(g))
            .and_then(|list| list.iter().find(|m| m.user_id == user_id))
        {
            return m.shown_name().to_owned();
        }
        if self.user_id() == Some(user_id)
            && let Some(name) = self.home.as_ref().and_then(|h| h.display_name.clone()) {
                return name;
            }
        localpart(user_id).to_owned()
    }

    pub fn typing_in(&self, channel_id: &str) -> Vec<String> {
        let now = Instant::now();
        self.typing
            .get(channel_id)
            .map(|list| {
                list.iter()
                    .filter(|(_, at)| now.duration_since(*at) < TYPING_SHOWN_FOR)
                    .map(|(u, _)| u.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    // ---------------------------------------------------------------- actions

    pub fn send_message(&mut self, channel_id: String, content: String, attachments: Vec<Attachment>, cx: &mut Context<Self>) {
        let content = content.trim().to_owned();
        if content.is_empty() && attachments.is_empty() {
            return;
        }
        let Some(api) = self.api_for_channel(&channel_id) else { return };
        let Some(me) = self.user_id().map(str::to_owned) else { return };
        let nonce = format!("rhein-{}-{}", now_ms(), next_nonce());
        let guild_id = self.guild_of_channel(&channel_id).map(|g| g.guild.id.clone());
        // Show it immediately; the echo or the response replaces it.
        if let Some(log) = self.logs.get_mut(&channel_id) {
            log.messages.push(Message {
                id: format!("pending:{nonce}"),
                channel_id: channel_id.clone(),
                guild_id,
                author: me,
                content: content.clone(),
                attachments: attachments.clone(),
                created_at: now_ms(),
                nonce: Some(nonce.clone()),
                ..Default::default()
            });
            cx.emit(ClientEvent::MessageAppended { channel_id: channel_id.clone() });
        }
        let io = self.io.clone();
        let chan = channel_id.clone();
        let body = SendMessage { content, attachments, reply_to: None, nonce: Some(nonce.clone()) };
        let task = cx.spawn(async move |this, cx| {
            let result = join(io.spawn(async move { Ok(api.send_message(&chan, &body).await?) })).await;
            this.update(cx, |this, cx| {
                let Some(log) = this.logs.get_mut(&channel_id) else { return };
                let pending = log.messages.iter().position(|m| m.id == format!("pending:{nonce}"));
                match (result, pending) {
                    (Ok(sent), Some(i)) => {
                        if log.messages.iter().any(|m| m.id == sent.id) {
                            log.messages.remove(i);
                            cx.emit(ClientEvent::MessageRemoved { channel_id: channel_id.clone(), index: i });
                        } else {
                            log.messages[i] = sent;
                            cx.emit(ClientEvent::MessageChanged { channel_id: channel_id.clone(), index: i });
                        }
                        this.ack_latest(&channel_id, cx);
                    }
                    (Err(err), Some(i)) => {
                        log.messages.remove(i);
                        cx.emit(ClientEvent::MessageRemoved { channel_id: channel_id.clone(), index: i });
                        this.error = Some(format!("Couldn't send message: {}", friendly(&err)).into());
                    }
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        });
        task.detach();
    }

    /// Uploads a file to the home server's media origin, then posts it.
    pub fn send_file(&mut self, channel_id: String, path: std::path::PathBuf, caption: String, cx: &mut Context<Self>) {
        let Some(home) = self.home.as_ref().map(|h| h.api.clone()) else { return };
        let io = self.io.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = join(io.spawn(async move {
                let bytes = tokio::task::spawn_blocking({
                    let path = path.clone();
                    move || std::fs::read(path)
                })
                .await??;
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_owned();
                let mime = guess_mime(&name);
                Ok(home.upload(&name, mime, bytes).await?)
            }))
            .await;
            this.update(cx, |this, cx| match result {
                Ok(attachment) => this.send_message(channel_id, caption, vec![attachment], cx),
                Err(err) => {
                    this.error = Some(format!("Upload failed: {}", friendly(&err)).into());
                    cx.notify();
                }
            })
            .ok();
        });
        task.detach();
    }

    pub fn edit_message(&mut self, channel_id: String, message_id: String, content: String, cx: &mut Context<Self>) {
        let Some(api) = self.api_for_channel(&channel_id) else { return };
        self.fire(cx, async move { api.edit_message(&channel_id, &message_id, &content).await.map(drop) });
    }

    pub fn delete_message(&mut self, channel_id: String, message_id: String, cx: &mut Context<Self>) {
        let Some(api) = self.api_for_channel(&channel_id) else { return };
        self.fire(cx, async move { api.delete_message(&channel_id, &message_id).await.map(drop) });
    }

    pub fn toggle_reaction(&mut self, channel_id: String, message_id: String, emoji: String, cx: &mut Context<Self>) {
        let Some(api) = self.api_for_channel(&channel_id) else { return };
        let mine = self
            .logs
            .get(&channel_id)
            .and_then(|l| l.messages.iter().find(|m| m.id == message_id))
            .and_then(|m| m.reactions.iter().find(|r| r.emoji == emoji))
            .is_some_and(|r| r.me);
        self.fire(cx, async move { api.react(&message_id, &emoji, !mine).await.map(drop) });
    }

    pub fn send_typing(&mut self, channel_id: &str) {
        let Some(server) = self.server_of_channel(channel_id) else { return };
        if let Some(gw) = self.server(server).and_then(|s| s.gateway.as_ref()) {
            gw.send(rhein_proto::typing(channel_id));
        }
    }

    pub fn create_guild(&mut self, server: String, name: String, cx: &mut Context<Self>) {
        let Some(api) = self.server(&server).and_then(|s| s.api.clone()) else { return };
        let io = self.io.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = join(io.spawn(async move { Ok(api.create_guild(name.trim()).await?) })).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(guild) => {
                        this.selected_guild = Some(guild.id);
                        this.refresh_server(&server, cx);
                    }
                    Err(err) => this.error = Some(format!("Couldn't create guild: {}", friendly(&err)).into()),
                }
                cx.notify();
            })
            .ok();
        });
        task.detach();
    }

    /// Accepts `CODE` (on `default_server`), `server/CODE` or an invite URL
    /// (`https://server/invite/CODE`). Connects to the server first if needed.
    pub fn join_invite(&mut self, text: String, default_server: Option<String>, cx: &mut Context<Self>) {
        let Some((server_input, code)) = parse_invite(&text, default_server.as_deref()) else {
            self.error = Some("That doesn't look like an invite".into());
            cx.notify();
            return;
        };
        let known = self
            .servers
            .iter()
            .find(|s| s.name == server_input || s.input == server_input)
            .map(|s| s.name.clone());
        let name = match known {
            Some(name) => name,
            None => {
                self.add_server(server_input.clone(), cx);
                server_input.clone()
            }
        };
        let task = cx.spawn(async move |this, cx| {
            // Wait (bounded) for the server's session if we just added it.
            let mut api = None;
            for _ in 0..100 {
                api = this
                    .read_with(cx, |this, _| {
                        this.servers
                            .iter()
                            .find(|s| s.name == name || s.input == name)
                            .and_then(|s| s.api.clone().map(|a| (s.name.clone(), a)))
                    })
                    .ok()
                    .flatten();
                if api.is_some() {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(100)).await;
            }
            let Some((server, api)) = api else {
                this.update(cx, |this, cx| {
                    this.error = Some(format!("Can't reach {name}").into());
                    cx.notify();
                })
                .ok();
                return;
            };
            let io = this.read_with(cx, |this, _| this.io.clone()).ok();
            let Some(io) = io else { return };
            let result = join(io.spawn(async move { Ok(api.join_invite(&code).await?) })).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(answer) => {
                        if answer.get("pending").and_then(|v| v.as_bool()) == Some(true) {
                            this.notice = Some("Join requested – a moderator has to approve it.".into());
                        } else if let Some(id) = answer.get("id").and_then(|v| v.as_str()) {
                            this.selected_guild = Some(id.to_owned());
                        }
                        this.refresh_server(&server, cx);
                    }
                    Err(err) => this.error = Some(format!("Couldn't join: {}", friendly(&err)).into()),
                }
                cx.notify();
            })
            .ok();
        });
        task.detach();
    }

    /// Creates an invite for the guild and puts a shareable form of it in `notice`.
    pub fn create_invite(&mut self, guild_id: String, cx: &mut Context<Self>) {
        let Some(api) = self.api_for_guild(&guild_id) else { return };
        let server = self.guild(&guild_id).map(|g| g.server.clone()).unwrap_or_default();
        let io = self.io.clone();
        let channel = self.selected_channel.get(&guild_id).cloned();
        let task = cx.spawn(async move |this, cx| {
            let result = join(io.spawn(async move { Ok(api.create_invite(&guild_id, channel.as_deref()).await?) })).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(invite) => {
                        let code = invite.get("code").and_then(|v| v.as_str()).unwrap_or_default();
                        let text = format!("{server}/{code}");
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.clone()));
                        this.notice = Some(format!("Invite copied: {text}").into());
                    }
                    Err(err) => this.error = Some(format!("Couldn't create invite: {}", friendly(&err)).into()),
                }
                cx.notify();
            })
            .ok();
        });
        task.detach();
    }

    pub fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.notice = None;
        cx.notify();
    }

    /// Runs a request whose result arrives over the gateway anyway; only failures are reported.
    fn fire<F>(&mut self, cx: &mut Context<Self>, fut: F)
    where
        F: Future<Output = Result<(), rhein_net::ApiError>> + Send + 'static,
    {
        let io = self.io.clone();
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let result = io.spawn(fut).await;
            let failed = match result {
                Ok(Ok(())) => None,
                Ok(Err(err)) => Some(err.to_string()),
                Err(err) => Some(err.to_string()),
            };
            if let Some(message) = failed {
                this.update(cx, |this, cx| {
                    this.error = Some(message.into());
                    cx.notify();
                })
                .ok();
            }
        });
        task.detach();
    }
}

fn apply_reaction(
    reactions: &mut Vec<rhein_proto::Reaction>,
    emoji: &str,
    user_id: &str,
    add: bool,
    me: Option<&str>,
) {
    let mine = me == Some(user_id);
    match reactions.iter().position(|r| r.emoji == emoji) {
        Some(i) => {
            let r = &mut reactions[i];
            if add {
                if !r.users.iter().any(|u| u == user_id) {
                    r.users.push(user_id.to_owned());
                    r.count += 1;
                }
                if mine {
                    r.me = true;
                }
            } else {
                if let Some(p) = r.users.iter().position(|u| u == user_id) {
                    r.users.remove(p);
                }
                r.count = (r.count - 1).max(0);
                if mine {
                    r.me = false;
                }
                if r.count == 0 {
                    reactions.remove(i);
                }
            }
        }
        None if add => reactions.push(rhein_proto::Reaction {
            emoji: emoji.to_owned(),
            count: 1,
            me: mine,
            users: vec![user_id.to_owned()],
        }),
        None => {}
    }
}

/// Mirrors the server's mention rule: a literal `@user:server`, or `@everyone` / `@here`.
fn mentions_user(content: &str, user_id: &str) -> bool {
    content.contains(user_id) || content.contains("@everyone") || content.contains("@here")
}

pub fn parse_invite(text: &str, default_server: Option<&str>) -> Option<(String, String)> {
    let text = text.trim().trim_end_matches('/');
    if text.is_empty() {
        return None;
    }
    let bare = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"))
        .unwrap_or(text);
    if let Some((server, code)) = bare.split_once("/invite/") {
        return Some((server.to_owned(), code.to_owned()));
    }
    if let Some((server, code)) = bare.rsplit_once('/') {
        return Some((server.to_owned(), code.to_owned()));
    }
    Some((default_server?.to_owned(), bare.to_owned()))
}

fn guess_mime(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" | "opus" => "audio/ogg",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" => "text/plain",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

fn device_name() -> String {
    let host = std::fs::read_to_string("/etc/hostname")
        .map(|h| h.trim().to_owned())
        .unwrap_or_default();
    if host.is_empty() { "Rhein".into() } else { format!("Rhein on {host}") }
}

fn next_nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// A one-line explanation for the UI, preferring the server's own message.
fn friendly(err: &anyhow::Error) -> String {
    if let Some(api) = err.downcast_ref::<rhein_net::ApiError>() {
        return match api.code() {
            Some("INVALID_CREDENTIALS") => "Wrong username or password".into(),
            Some("USERNAME_TAKEN") => "That username is taken".into(),
            Some("REGISTRATION_CLOSED") => "Registration is closed on this server".into(),
            Some("ACCOUNT_DEACTIVATED") => "This account is deactivated".into(),
            _ => api.to_string(),
        };
    }
    format!("{err:#}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invites() {
        assert_eq!(parse_invite("abc", Some("a.x")), Some(("a.x".into(), "abc".into())));
        assert_eq!(parse_invite("b.x/abc", None), Some(("b.x".into(), "abc".into())));
        assert_eq!(
            parse_invite("https://b.x/invite/abc", None),
            Some(("b.x".into(), "abc".into()))
        );
        assert_eq!(parse_invite("abc", None), None);
    }

    #[test]
    fn reactions() {
        let mut r = Vec::new();
        apply_reaction(&mut r, "👍", "@a:x", true, Some("@a:x"));
        apply_reaction(&mut r, "👍", "@b:x", true, Some("@a:x"));
        assert_eq!(r[0].count, 2);
        assert!(r[0].me);
        apply_reaction(&mut r, "👍", "@a:x", false, Some("@a:x"));
        assert_eq!(r[0].count, 1);
        assert!(!r[0].me);
        apply_reaction(&mut r, "👍", "@b:x", false, Some("@a:x"));
        assert!(r.is_empty());
    }
}
