//! Calls in the client: guild voice channels and calls in private conversations. Joining asks
//! the server where to dial, then a call task on the tokio runtime owns the media: a mesh
//! (signals out through `/voice/signal`, in through `VOICE_SIGNAL`) or an SFU room. Remote
//! voices go into the audio engine, links and failures come back here. The roster itself stays
//! what `VOICE_STATE_UPDATE` says.
//!
//! A conversation call adds privacy (`docs/calls.md`): our posture goes to the others in hellos
//! sealed with the group's call signal key, the mesh pins itself to TURN when the call's
//! strictest posture asks for it, and an SFU room is end-to-end encrypted with the group's media
//! key, rekeyed on every epoch.

use std::collections::HashSet;
use std::sync::Arc;

use rhein_voice::livekit::webrtc::media_stream_track::MediaStreamTrack;
use rhein_voice::livekit::webrtc::peer_connection_factory::PeerConnectionFactory;
use rhein_voice::livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt as _;
use rhein_proto::{VoiceClaim, VoiceServer};
use rhein_voice::audio::{AudioConfig, AudioEngine};
use rhein_voice::e2ee;
use rhein_voice::mesh::{self, Mesh};
use rhein_voice::posture::{self, Hello, Posture};
use rhein_voice::screen::ScreenShare;
use rhein_voice::sfu::{self, SfuCall};
use rhein_voice::video::{VideoFeed, VideoOut};
use rhein_voice::{CallEvent, Slot};
use serde_json::Value;
use tokio::sync::mpsc;

use super::*;

/// How a call's media travels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallMode {
    Mesh,
    Sfu { region: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallStatus {
    /// Asking the server, opening devices, reaching the SFU.
    Connecting,
    Connected,
    Failed(String),
}

/// The call this client is in (one at a time).
pub struct ActiveCall {
    pub server: String,
    /// The guild of a voice channel, or the conversation itself.
    pub room_id: String,
    /// The voice channel, or for a conversation call the conversation id again.
    pub channel_id: String,
    /// A call in a private conversation (on the home server, with posture and E2EE).
    pub conversation: bool,
    pub session_id: Option<String>,
    pub mode: Option<CallMode>,
    pub status: CallStatus,
    /// Participants whose media reaches us.
    pub linked: HashSet<String>,
    pub muted: bool,
    pub deafened: bool,
    /// Our own screen share, while it runs.
    pub sharing_screen: bool,
    /// A conversation call's effective posture, once hellos have been weighed.
    pub posture: Option<Posture>,
    /// The MLS epoch a conversation call is keyed with.
    epoch: Option<u64>,
    /// Whose screens we asked to see (an SFU forwards them only then).
    pub watching: HashSet<String>,
    engine: Option<Arc<AudioEngine>>,
    commands: Option<mpsc::UnboundedSender<Command>>,
    speaking: HashSet<String>,
    screen: Option<ScreenShare>,
    videos: HashMap<(String, Slot), VideoFeed>,
    /// The picture drawn for each video, and which frame it is.
    images: HashMap<(String, Slot), (u64, Arc<gpui::RenderImage>)>,
    _task: Option<Task<()>>,
}

impl ActiveCall {
    pub fn engine(&self) -> Option<&Arc<AudioEngine>> {
        self.engine.as_ref()
    }
}

enum Command {
    Signal { from: String, payload: Value },
    /// A conversation's call secrets for a (new) epoch: the hello key and the media key.
    Keys { hello: Option<(Vec<u8>, u64)>, media: Option<(Vec<u8>, u64)> },
    Roster(Vec<String>),
    Server(VoiceServer),
    Watch { user: String, on: bool },
    Publish { slot: Slot, video: VideoOut },
    Unpublish(Slot),
    Leave,
}

enum Update {
    Connected(CallMode),
    /// A conversation call's effective posture changed.
    Posture(Posture),
    Link { user: String, up: bool },
    Video { user: String, slot: Slot, feed: Option<VideoFeed> },
    Failed(String),
}

pub use crate::prefs::VoicePrefs;

trait AudioConfigOf {
    fn audio_config(&self) -> AudioConfig;
}

impl AudioConfigOf for VoicePrefs {
    fn audio_config(&self) -> AudioConfig {
        let null = || Some("alsa:null".to_owned());
        AudioConfig {
            input: if self.null_devices { null() } else { self.input.clone() },
            output: if self.null_devices { null() } else { self.output.clone() },
            echo_cancellation: !self.raw_input,
            noise_suppression: !self.raw_input,
            auto_gain: !self.raw_input,
        }
    }
}

impl Client {
    pub fn call(&self) -> Option<&ActiveCall> {
        self.call.as_ref()
    }

    pub fn in_call(&self, channel_id: &str) -> bool {
        self.call.as_ref().is_some_and(|c| c.channel_id == channel_id)
    }

    /// Whether someone in the call is audible right now (our own user included).
    pub fn is_speaking(&self, user_id: &str) -> bool {
        self.call.as_ref().is_some_and(|c| c.speaking.contains(user_id))
    }

    /// Joins a guild voice channel, leaving any other call first.
    pub fn join_voice(&mut self, channel_id: String, cx: &mut Context<Self>) {
        if self.in_call(&channel_id) {
            return;
        }
        self.leave_voice(cx);
        let (Some(api), Some(guild)) = (self.api_for_channel(&channel_id), self.guild_of_channel(&channel_id)) else { return };
        let (server, guild_id) = (guild.server.clone(), guild.guild.id.clone());
        let (muted, deafened) = (self.prefs.voice.muted, self.prefs.voice.deafened);
        let (http, channel) = (self.http.clone(), channel_id.clone());
        let route = Route::Channel { api: api.clone(), channel: channel_id.clone() };
        let dial = async move {
            let published = api.voice_regions().await.unwrap_or(Value::Null);
            let region = rhein_voice::region::choose(&http, &published, None).await;
            let joined = api.voice_join(&channel, muted, deafened, false, region.as_deref()).await?;
            let here = api.voice_channel(&channel).await.ok();
            Ok((joined, None, here))
        };
        self.begin(server, guild_id, channel_id, false, route, dial, cx);
    }

    /// Joins (or starts) the call in a private conversation.
    pub fn join_dm_call(&mut self, conversation: String, cx: &mut Context<Self>) {
        if self.in_call(&conversation) {
            return;
        }
        self.leave_voice(cx);
        let Some(home) = &self.home else { return };
        let (api, server) = (home.api.clone(), home.doc.server_name.clone());
        let (muted, deafened) = (self.prefs.voice.muted, self.prefs.voice.deafened);
        let own = Posture::parse(&self.prefs.voice.posture);
        let engine = self.dm_engine.clone();
        let (id, route) = (conversation.clone(), Route::Conversation { api: api.clone(), id: conversation.clone() });
        let dial = async move {
            let keys = match &engine {
                Some(engine) => {
                    let mut engine = engine.lock().await;
                    (engine.call_secret(&id, posture::SIGNAL_LABEL), engine.call_secret(&id, e2ee::MEDIA_EXPORTER_LABEL))
                }
                None => (None, None),
            };
            // Whether our home has a relay is only known from its answer; until then a relay
            // posture asks for an SFU, the strict way round.
            let joined = api.conversation_voice_join(&id, muted, deafened, posture::wants_sfu(own, false), false).await?;
            let here = api.conversation_voice(&id).await.ok();
            Ok((joined, Some(Privacy::new(id, own, keys.0, keys.1)), here))
        };
        self.begin(server, conversation.clone(), conversation, true, route, dial, cx);
    }

    /// Starts a call: dials (off the UI thread), opens the devices and hands the media to a call
    /// task.
    #[allow(clippy::too_many_arguments)]
    fn begin(
        &mut self,
        server: String,
        room_id: String,
        channel_id: String,
        conversation: bool,
        route: Route,
        // The join answer, a conversation's privacy state, and who is already there.
        dial: impl std::future::Future<Output = Result<(rhein_proto::VoiceJoin, Option<Privacy>, Option<Value>)>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(me) = self.home.as_ref().map(|h| h.user_id.clone()) else { return };
        let prefs = self.voice_prefs();
        let (io, http) = (self.io.clone(), self.http.clone());
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (updates, mut update_rx) = mpsc::unbounded_channel();

        let task = cx.spawn(async move |this, cx| {
            let started = join(io.spawn(async move {
                let (joined, privacy, here) = dial.await?;
                let engine = tokio::task::spawn_blocking(move || AudioEngine::start(prefs.audio_config())).await??;
                Ok((joined, privacy, Arc::new(engine), here))
            }))
            .await;
            let Ok(Some((joined, privacy, engine, _))) = this.update(cx, |this, cx| {
                if let Ok((_, _, _, Some(here))) = &started {
                    let states: Vec<rhein_proto::VoiceState> = here["voice_states"].as_array().into_iter().flatten().filter_map(|v| serde_json::from_value(v.clone()).ok()).collect();
                    let room = this.call.as_ref()?.room_id.clone();
                    let roster = this.voice.entry(room).or_default();
                    for state in states.into_iter().filter(|s| s.channel_id.is_some()) {
                        roster.insert(state.user_id.clone(), state);
                    }
                }
                let call = this.call.as_mut()?;
                match started {
                    Ok(ready) => {
                        call.session_id = ready.0.voice_state.session_id.clone();
                        call.engine = Some(ready.2.clone());
                        call.epoch = ready.1.as_ref().and_then(|p| p.hello.as_ref().map(|(_, e)| *e));
                        this.apply_call_audio();
                        this.claim_voice(cx);
                        cx.notify();
                        Some(ready)
                    }
                    Err(err) => {
                        log::warn!("joining a call: {err:#}");
                        call.status = CallStatus::Failed(friendly(&err));
                        cx.notify();
                        None
                    }
                }
            }) else {
                return;
            };
            for error in engine.errors() {
                log::warn!("call audio: {error}");
            }
            let roster = this.read_with(cx, |this, _| this.call_roster()).unwrap_or_default();
            io.spawn(drive(me, route, http, joined.voice_server, engine, roster, privacy, command_rx, updates));

            while let Some(update) = update_rx.recv().await {
                let alive = this.update(cx, |this, cx| {
                    let Some(call) = this.call.as_mut() else { return false };
                    match update {
                        Update::Connected(mode) => {
                            call.mode = Some(mode);
                            call.status = CallStatus::Connected;
                        }
                        Update::Posture(posture) => call.posture = Some(posture),
                        Update::Link { user, up } => {
                            if up {
                                call.linked.insert(user);
                            } else {
                                call.linked.remove(&user);
                            }
                        }
                        Update::Video { user, slot, feed } => match feed {
                            Some(feed) => {
                                call.videos.insert((user, slot), feed);
                            }
                            None => {
                                call.videos.remove(&(user.clone(), slot));
                                if let Some((_, image)) = call.images.remove(&(user, slot)) {
                                    cx.drop_image(image, None);
                                }
                            }
                        },
                        Update::Failed(err) => call.status = CallStatus::Failed(err),
                    }
                    cx.notify();
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        });
        self.call = Some(ActiveCall {
            server,
            room_id,
            channel_id,
            conversation,
            session_id: None,
            mode: None,
            posture: None,
            epoch: None,
            status: CallStatus::Connecting,
            linked: HashSet::new(),
            muted: self.prefs.voice.muted,
            deafened: self.prefs.voice.deafened,
            sharing_screen: false,
            watching: HashSet::new(),
            engine: None,
            commands: Some(commands),
            speaking: HashSet::new(),
            screen: None,
            videos: HashMap::new(),
            images: HashMap::new(),
            _task: Some(task),
        });
        self.watch_speaking(cx);
        cx.notify();
    }

    /// The route (endpoints) of the call we are in.
    fn route(&self) -> Option<Route> {
        let call = self.call.as_ref()?;
        Some(if call.conversation {
            Route::Conversation { api: self.home.as_ref()?.api.clone(), id: call.room_id.clone() }
        } else {
            Route::Channel { api: self.api_for_guild(&call.room_id)?, channel: call.channel_id.clone() }
        })
    }

    /// Hangs up.
    pub fn leave_voice(&mut self, cx: &mut Context<Self>) {
        let Some(mut call) = self.call.take() else { return };
        if let Some(commands) = &call.commands {
            let _ = commands.send(Command::Leave);
        }
        for (_, (_, image)) in call.images.drain() {
            cx.drop_image(image, None);
        }
        if let Some(route) = self.route_of(&call) {
            self.io.spawn(async move { route.leave().await });
        }
        // The engine goes with the last handle (the call task drops its own on Leave).
        drop(call);
        self.claim_voice(cx);
        cx.notify();
    }

    pub fn set_voice_muted(&mut self, muted: bool, cx: &mut Context<Self>) {
        self.prefs.voice.muted = muted;
        // Unmuting while deafened undeafens too, as in the web client.
        if !muted {
            self.prefs.voice.deafened = false;
        }
        self.save_prefs();
        self.push_voice_state(cx);
    }

    pub fn set_voice_deafened(&mut self, deafened: bool, cx: &mut Context<Self>) {
        self.prefs.voice.deafened = deafened;
        self.save_prefs();
        self.push_voice_state(cx);
    }

    pub fn voice_muted(&self) -> bool {
        self.prefs.voice.muted
    }

    pub fn voice_deafened(&self) -> bool {
        self.prefs.voice.deafened
    }

    /// The saved voice settings, with this run's device override.
    pub fn voice_prefs(&self) -> VoicePrefs {
        VoicePrefs { null_devices: self.null_audio, ..self.prefs.voice.clone() }
    }

    /// Changes the voice settings and hands them to a running call.
    pub fn update_voice_prefs(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut VoicePrefs)) {
        let before = self.voice_prefs();
        change(&mut self.prefs.voice);
        self.save_prefs();
        let after = self.voice_prefs();
        if (before.input != after.input || before.output != after.output || before.raw_input != after.raw_input)
            && let Some(engine) = self.call.as_ref().and_then(|c| c.engine.clone())
        {
            let config = after.audio_config();
            engine.set_devices(config.input, config.output);
        }
        self.apply_call_audio();
        cx.notify();
    }

    /// Push-to-talk key held or released.
    pub fn set_talking(&self, held: bool) {
        if let Some(engine) = self.call.as_ref().and_then(|c| c.engine.as_ref()) {
            engine.set_talking(held);
        }
    }

    /// A local volume for one participant, 0..200.
    pub fn set_user_volume(&mut self, user_id: &str, percent: u32) {
        self.prefs.voice.volumes.insert(user_id.to_owned(), percent);
        self.save_prefs();
        if let Some(engine) = self.call.as_ref().and_then(|c| c.engine.as_ref()) {
            engine.set_volume(user_id, percent);
        }
    }

    pub fn user_volume(&self, user_id: &str) -> u32 {
        self.prefs.voice.volumes.get(user_id).copied().unwrap_or(100)
    }

    /// The microphone's level, 0..1, while in a call.
    pub fn input_level(&self) -> Option<f32> {
        self.call.as_ref().and_then(|c| c.engine.as_ref()).map(|e| e.input_level())
    }

    fn push_voice_state(&mut self, cx: &mut Context<Self>) {
        let (muted, deafened) = (self.prefs.voice.muted, self.prefs.voice.deafened);
        if let Some(call) = self.call.as_mut() {
            call.muted = muted;
            call.deafened = deafened;
        }
        if let Some(route) = self.route() {
            self.io.spawn(async move { route.flags(muted, deafened).await });
        }
        self.apply_call_audio();
        cx.notify();
    }

    /// Hands mute, deafen and the settings to the running engine.
    fn apply_call_audio(&self) {
        let Some(engine) = self.call.as_ref().and_then(|c| c.engine.as_ref()) else { return };
        let voice = &self.prefs.voice;
        engine.set_muted(voice.muted);
        engine.set_deafened(voice.deafened);
        engine.set_push_to_talk(voice.push_to_talk);
        engine.set_gate(voice.gate);
        engine.set_input_gain(voice.input_gain.unwrap_or(100));
        for (user, volume) in &voice.volumes {
            engine.set_volume(user, *volume);
        }
    }

    /// Names the call we are in to the guild server's gateway, so a restart keeps our seat.
    fn claim_voice(&self, _cx: &mut Context<Self>) {
        for server in &self.servers {
            let claims = self
                .call
                .as_ref()
                .filter(|c| c.server == server.name)
                .and_then(|c| Some(VoiceClaim { room_id: c.room_id.clone(), session_id: c.session_id.clone()? }))
                .into_iter()
                .collect();
            if let Some(gateway) = &server.gateway {
                gateway.set_voice_claims(claims);
            }
        }
    }

    /// Who the server says is in our call's channel.
    fn call_roster(&self) -> Vec<String> {
        let Some(call) = &self.call else { return Vec::new() };
        self.voice_members(&call.room_id, &call.channel_id).into_iter().map(|s| s.user_id).collect()
    }

    /// Called for every voice state change of a guild or conversation: the mesh follows the
    /// roster, and a call the server moved or ended follows the server.
    pub(super) fn on_voice_roster(&mut self, state: &rhein_proto::VoiceState, cx: &mut Context<Self>) {
        let Some(call) = &self.call else { return };
        let room = state.guild_id.as_deref().or(state.conversation_id.as_deref());
        if room != Some(call.room_id.as_str()) {
            return;
        }
        let me = self.home.as_ref().map(|h| h.user_id.as_str());
        if Some(state.user_id.as_str()) == me {
            match state.channel_id.as_deref() {
                // Disconnected by the server (kicked, swept): the call is over.
                None => {
                    self.call = None;
                    self.claim_voice(cx);
                    cx.notify();
                    return;
                }
                // Moved by a moderator: dial the new channel ourselves.
                Some(channel) if channel != call.channel_id && !call.conversation => {
                    let channel = channel.to_owned();
                    self.call = None;
                    self.join_voice(channel, cx);
                    return;
                }
                _ => {}
            }
        }
        let roster = self.call_roster();
        if let Some(commands) = self.call.as_ref().and_then(|c| c.commands.as_ref()) {
            let _ = commands.send(Command::Roster(roster));
        }
    }

    pub(super) fn on_voice_signal(&mut self, channel_id: &str, from: String, payload: Value) {
        if let Some(call) = self.call.as_ref().filter(|c| c.channel_id == channel_id)
            && let Some(commands) = &call.commands
        {
            let _ = commands.send(Command::Signal { from, payload });
        }
    }

    pub(super) fn on_voice_server(&mut self, data: Value) {
        let Some(call) = &self.call else { return };
        if data["channel_id"].as_str().is_some_and(|c| c != call.channel_id) {
            return;
        }
        if let (Ok(server), Some(commands)) = (serde_json::from_value::<VoiceServer>(data), &call.commands) {
            let _ = commands.send(Command::Server(server));
        }
    }

    /// Starts (or stops) watching someone's screen share. The server records it and has the SFU
    /// forward it; a mesh sends screens to everyone anyway.
    pub fn watch_screen(&mut self, user_id: &str, on: bool, cx: &mut Context<Self>) {
        let Some(call) = self.call.as_mut() else { return };
        if on {
            call.watching.insert(user_id.to_owned());
        } else {
            call.watching.remove(user_id);
        }
        if let Some(commands) = &call.commands {
            let _ = commands.send(Command::Watch { user: user_id.to_owned(), on });
        }
        let user = user_id.to_owned();
        if let Some(route) = self.route() {
            self.io.spawn(async move { route.watch(&user, on).await });
        }
        cx.notify();
    }

    /// The latest picture of someone's camera or screen in the call.
    pub fn video(&self, user_id: &str, slot: Slot) -> Option<Arc<gpui::RenderImage>> {
        self.call.as_ref()?.images.get(&(user_id.to_owned(), slot)).map(|(_, image)| image.clone())
    }

    /// Asks the desktop for a screen or window and shares it into the call.
    pub fn start_screen_share(&mut self, cx: &mut Context<Self>) {
        let Some(call) = self.call.as_ref() else { return };
        if call.sharing_screen {
            return;
        }
        let io = self.io.clone();
        cx.spawn(async move |this, cx| {
            let started = join(io.spawn(async move { tokio::task::spawn_blocking(|| ScreenShare::start(30, true)).await? })).await;
            this.update(cx, |this, cx| {
                let Some(call) = this.call.as_mut() else { return };
                match started {
                    Ok(share) => {
                        if let Some(commands) = &call.commands {
                            let _ = commands.send(Command::Publish { slot: Slot::Screen, video: share.video().clone() });
                        }
                        call.screen = Some(share);
                        call.sharing_screen = true;
                        if let Some(route) = this.route() {
                            this.io.spawn(async move { route.stream(true).await });
                        }
                    }
                    Err(err) => log::info!("screen share not started: {err:#}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn stop_screen_share(&mut self, cx: &mut Context<Self>) {
        let Some(call) = self.call.as_mut() else { return };
        if !call.sharing_screen {
            return;
        }
        call.sharing_screen = false;
        call.screen = None;
        if let Some(commands) = &call.commands {
            let _ = commands.send(Command::Unpublish(Slot::Screen));
        }
        if let Some(route) = self.route() {
            self.io.spawn(async move { route.stream(false).await });
        }
        cx.notify();
    }

    /// Polls the engine for who is audible and the videos for new pictures, and redraws when
    /// something changed.
    fn watch_speaking(&mut self, cx: &mut Context<Self>) {
        self.speaking_task = Some(cx.spawn(async move |this, cx| {
            let mut tick = 0u32;
            loop {
                cx.background_executor().timer(Duration::from_millis(33)).await;
                tick += 1;
                let Ok(alive) = this.update(cx, |this, cx| {
                    let me = this.home.as_ref().map(|h| h.user_id.clone()).unwrap_or_default();
                    let Some(call) = this.call.as_mut() else { return false };
                    let mut changed = false;
                    let mut rekey = None;
                    if tick.is_multiple_of(3) {
                        let now: HashSet<String> = call
                            .engine
                            .as_ref()
                            .map(|e| e.speaking().into_iter().map(|k| if k.is_empty() { me.clone() } else { k }).collect())
                            .unwrap_or_default();
                        if now != call.speaking {
                            call.speaking = now;
                            changed = true;
                        }
                    }
                    let mut stale = Vec::new();
                    for (key, feed) in &call.videos {
                        let Some((seq, picture)) = feed.latest() else { continue };
                        if call.images.get(key).is_some_and(|(have, _)| *have == seq) {
                            continue;
                        }
                        if let Some(image) = render_image(&picture) {
                            if let Some((_, old)) = call.images.insert(key.clone(), (seq, image)) {
                                stale.push(old);
                            }
                            changed = true;
                        }
                    }
                    // A conversation call follows the group's epoch: new keys for hellos and media.
                    if tick.is_multiple_of(30) && call.conversation {
                        rekey = Some((call.room_id.clone(), call.epoch));
                    }
                    // The desktop's own "stop sharing" ends a share without us.
                    let ended = call.screen.as_ref().is_some_and(|s| s.ended());
                    for old in stale {
                        cx.drop_image(old, None);
                    }
                    if ended {
                        this.stop_screen_share(cx);
                    }
                    if let Some((conversation, epoch)) = rekey {
                        this.follow_epoch(&conversation, epoch);
                    }
                    if changed {
                        cx.notify();
                    }
                    true
                }) else {
                    return;
                };
                if !alive {
                    return;
                }
            }
        }));
    }
}

impl Client {
    /// Hands a conversation call the keys of the group's current epoch when it moved on.
    fn follow_epoch(&mut self, conversation: &str, epoch: Option<u64>) {
        let Some(engine) = &self.dm_engine else { return };
        // The engine may be busy syncing; the next tick tries again.
        let Ok(mut engine) = engine.try_lock() else { return };
        let hello = engine.call_secret(conversation, posture::SIGNAL_LABEL);
        let now = hello.as_ref().map(|(_, e)| *e);
        if now.is_none() || now == epoch {
            return;
        }
        let media = engine.call_secret(conversation, e2ee::MEDIA_EXPORTER_LABEL);
        drop(engine);
        if let Some(call) = self.call.as_mut() {
            call.epoch = now;
            if let Some(commands) = &call.commands {
                let _ = commands.send(Command::Keys { hello, media });
            }
        }
    }

    fn route_of(&self, call: &ActiveCall) -> Option<Route> {
        Some(if call.conversation {
            Route::Conversation { api: self.home.as_ref()?.api.clone(), id: call.room_id.clone() }
        } else {
            Route::Channel { api: self.api_for_guild(&call.room_id)?, channel: call.channel_id.clone() }
        })
    }
}

/// Where a call's requests go: a guild channel's routes, or a conversation's on the home server.
#[derive(Clone)]
enum Route {
    Channel { api: Api, channel: String },
    Conversation { api: Api, id: String },
}

impl Route {
    async fn signal(&self, target: &str, payload: &Value) -> Result<Value, rhein_net::ApiError> {
        match self {
            Route::Channel { api, channel } => api.voice_signal(channel, target, payload).await,
            Route::Conversation { api, id } => api.conversation_voice_signal(id, target, payload).await,
        }
    }

    async fn leave(&self) {
        let _ = match self {
            Route::Channel { api, channel } => api.voice_leave(channel).await,
            Route::Conversation { api, id } => api.conversation_voice_leave(id).await,
        };
    }

    async fn flags(&self, muted: bool, deafened: bool) {
        let result = match self {
            Route::Channel { api, channel } => api.voice_state(channel, Some(muted), Some(deafened), None).await.map(|_| Value::Null),
            Route::Conversation { api, id } => api.conversation_voice_state(id, &serde_json::json!({ "self_mute": muted, "self_deaf": deafened })).await,
        };
        if let Err(err) = result {
            log::warn!("voice state: {err}");
        }
    }

    async fn stream(&self, on: bool) {
        let _ = match self {
            Route::Channel { api, channel } => api.voice_stream(channel, on, on.then_some("detail native@30")).await,
            Route::Conversation { api, id } => api.conversation_voice_state(id, &serde_json::json!({ "streaming": on })).await,
        };
    }

    /// Guild calls record watching with the server; a conversation's has no watcher rows.
    async fn watch(&self, user: &str, on: bool) {
        if let Route::Channel { api, channel } = self
            && let Err(err) = api.voice_watch(channel, user, on, None).await
        {
            log::warn!("watch {user}: {err}");
        }
    }
}

/// A conversation call's privacy state, inside the call task.
struct Privacy {
    conversation: String,
    own: Posture,
    /// The hello key and its epoch.
    hello: Option<(Vec<u8>, u64)>,
    /// The media key and its epoch.
    media: Option<(Vec<u8>, u64)>,
    hellos: HashMap<String, Hello>,
    /// Who has this epoch's hello from us.
    greeted: HashSet<String>,
    counter: u64,
}

impl Privacy {
    fn new(conversation: String, own: Posture, hello: Option<(Vec<u8>, u64)>, media: Option<(Vec<u8>, u64)>) -> Privacy {
        Privacy { conversation, own, hello, media, hellos: HashMap::new(), greeted: HashSet::new(), counter: 0 }
    }

    fn epoch(&self) -> Option<u64> {
        self.hello.as_ref().map(|(_, e)| *e)
    }

    fn effective(&self, me: &str, roster: &[String]) -> Posture {
        posture::effective(self.own, me, roster, &self.hellos, self.epoch())
    }

    /// Sealed hellos for everyone in the roster who has not had this epoch's from us.
    fn greetings(&mut self, me: &str, roster: &[String]) -> Vec<(String, Value)> {
        let Some((key, epoch)) = self.hello.clone() else { return Vec::new() };
        let mut out = Vec::new();
        for user in roster {
            if user == me || !self.greeted.insert(user.clone()) {
                continue;
            }
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
            self.counter = (self.counter + 1).max(now);
            let hello = posture::seal_hello(&key, &self.conversation, me, self.own, self.counter, epoch);
            out.push((user.clone(), serde_json::json!({ "hello": hello })));
        }
        out
    }

    /// Takes in a hello; whether it changed anything.
    fn hear(&mut self, from: &str, sealed: &Value) -> bool {
        let Some((key, epoch)) = &self.hello else { return false };
        let opened = posture::open_hello(key, &self.conversation, from, Some(*epoch), sealed);
        posture::note_hello(&mut self.hellos, from, opened)
    }

    /// New keys: every hello in hand is from the last epoch now, and everyone is owed ours.
    fn rekey(&mut self, hello: Option<(Vec<u8>, u64)>, media: Option<(Vec<u8>, u64)>) {
        self.hello = hello;
        self.media = media;
        self.greeted.clear();
    }
}

fn render_image(picture: &rhein_voice::video::Picture) -> Option<Arc<gpui::RenderImage>> {
    let buffer = image::RgbaImage::from_raw(picture.width, picture.height, picture.bgra.clone())?;
    Some(Arc::new(gpui::RenderImage::new(smallvec::smallvec![image::Frame::new(buffer)])))
}

/// The media side of one call, on the tokio runtime. Reconnects when the server points the call
/// somewhere else (`VOICE_SERVER_UPDATE`).
#[allow(clippy::too_many_arguments)]
async fn drive(
    me: String,
    route: Route,
    http: reqwest::Client,
    mut server: VoiceServer,
    engine: Arc<AudioEngine>,
    mut roster: Vec<String>,
    mut privacy: Option<Privacy>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    updates: mpsc::UnboundedSender<Update>,
) {
    // Signals leave one at a time and in order: an answer overtaking its offer is a failed call.
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel::<(String, Value)>();
    tokio::spawn(async move {
        while let Some((target, payload)) = signal_rx.recv().await {
            if let Err(err) = route.signal(&target, &payload).await {
                log::debug!("call signal to {target}: {err}");
            }
        }
    });
    // Hellos to whoever has not had one this epoch, and the posture that follows.
    let greet = |privacy: &mut Option<Privacy>, roster: &[String]| -> Option<Posture> {
        let privacy = privacy.as_mut()?;
        for (target, payload) in privacy.greetings(&me, roster) {
            let _ = signal_tx.send((target, payload));
        }
        Some(privacy.effective(&me, roster))
    };
    loop {
        let next = match server {
            VoiceServer::Mesh { ice_servers, relay, .. } => {
                let factory = PeerConnectionFactory::default();
                let microphone: MediaStreamTrack = factory.create_audio_track("microphone", engine.microphone()).into();
                // Our own strict posture pins every connection to TURN from the first offer.
                let pinned = |privacy: &Option<Privacy>, effective: Option<Posture>| {
                    privacy.as_ref().is_some_and(|p| posture::relay_only(p.own, effective.unwrap_or(p.own), relay))
                };
                if privacy.as_ref().is_some_and(|p| p.own >= Posture::Relay) && !relay {
                    let _ = updates.send(Update::Failed("Your privacy setting needs a relay, and your server has none: set a custom SFU for this conversation, or lower the setting.".into()));
                }
                let effective = greet(&mut privacy, &roster);
                let (mut call, mut events) = Mesh::new(&me, factory, mesh::ice_servers(&ice_servers), pinned(&privacy, effective));
                call.set_source(Slot::Mic, Some(microphone));
                call.sync(roster.iter().map(String::as_str));
                let _ = updates.send(Update::Connected(CallMode::Mesh));
                if let Some(effective) = effective {
                    let _ = updates.send(Update::Posture(effective));
                }
                loop {
                    tokio::select! {
                        command = commands.recv() => match command {
                            Some(Command::Signal { from, payload }) => {
                                if let Some(sealed) = payload.get("hello") {
                                    // A hello rides the same relay as an offer, and is not one.
                                    if privacy.as_mut().is_some_and(|p| p.hear(&from, sealed)) {
                                        let effective = greet(&mut privacy, &roster);
                                        call.set_relay(pinned(&privacy, effective));
                                        if let Some(effective) = effective {
                                            let _ = updates.send(Update::Posture(effective));
                                        }
                                    }
                                } else {
                                    call.accept(&from, payload);
                                }
                            }
                            Some(Command::Roster(users)) => {
                                call.sync(users.iter().map(String::as_str));
                                roster = users;
                                let effective = greet(&mut privacy, &roster);
                                call.set_relay(pinned(&privacy, effective));
                                if let Some(effective) = effective {
                                    let _ = updates.send(Update::Posture(effective));
                                }
                            }
                            Some(Command::Keys { hello, media }) => {
                                if let Some(p) = privacy.as_mut() {
                                    p.rekey(hello, media);
                                }
                                let effective = greet(&mut privacy, &roster);
                                call.set_relay(pinned(&privacy, effective));
                            }
                            Some(Command::Server(new)) => break Some(new),
                            Some(Command::Watch { .. }) => {}
                            Some(Command::Publish { slot, video }) => {
                                let track: MediaStreamTrack = call.factory().create_video_track(if slot == Slot::Screen { "screen" } else { "camera" }, video.source()).into();
                                call.set_source(slot, Some(track));
                            }
                            Some(Command::Unpublish(slot)) => call.set_source(slot, None),
                            Some(Command::Leave) | None => break None,
                        },
                        Some(event) = events.recv() => match event {
                            CallEvent::Signal { target, payload } => { let _ = signal_tx.send((target, payload)); }
                            event => media(&engine, &updates, event),
                        },
                    }
                }
            }
            VoiceServer::Sfu { endpoint, token, region, custom, e2ee: required, .. } => {
                // A conversation's SFU never gets plaintext: no media key, no call.
                let keys = match &privacy {
                    Some(p) => match &p.media {
                        Some((key, epoch)) => Ok(Some(e2ee::key_provider(key, *epoch))),
                        None => Err(anyhow!("This device has no keys for this conversation yet, so it cannot join an encrypted call.")),
                    },
                    None if required.is_some() => Err(anyhow!("This call must be end-to-end encrypted and there are no keys for it.")),
                    None => Ok(None),
                };
                let joined = match keys {
                    Err(err) => Err(err),
                    Ok(_) if custom && endpoint.is_empty() => Err(anyhow!("This conversation uses its own SFU, whose address this device does not know yet.")),
                    Ok(provider) => match sfu::session(&http, &endpoint, &token).await {
                        Ok(session) => SfuCall::connect(session, engine.microphone(), provider.clone()).await.map(|c| (c, provider)),
                        Err(err) => Err(err),
                    },
                };
                match joined {
                    Ok(((mut call, mut events), provider)) => {
                        let _ = updates.send(Update::Connected(CallMode::Sfu { region }));
                        if let Some(effective) = greet(&mut privacy, &roster) {
                            let _ = updates.send(Update::Posture(effective));
                        }
                        let next = loop {
                            tokio::select! {
                                command = commands.recv() => match command {
                                    Some(Command::Watch { user, on }) => call.watch(&user, on),
                                    Some(Command::Publish { slot, video }) => {
                                        if let Err(err) = call.publish_video(slot, video.source()).await {
                                            let _ = updates.send(Update::Failed(format!("{err:#}")));
                                        }
                                    }
                                    Some(Command::Unpublish(slot)) => call.unpublish(slot).await,
                                    Some(Command::Keys { hello, media }) => {
                                        // An epoch change rekeys; the ring keeps the last keys for a
                                        // peer a commit behind.
                                        if let (Some(provider), Some((key, epoch))) = (&provider, &media) {
                                            e2ee::set_epoch_key(provider, key, *epoch);
                                        }
                                        if let Some(p) = privacy.as_mut() {
                                            p.rekey(hello, media);
                                        }
                                        greet(&mut privacy, &roster);
                                    }
                                    Some(Command::Server(new)) => break Some(new),
                                    Some(Command::Signal { from, payload }) => {
                                        if let Some(sealed) = payload.get("hello")
                                            && privacy.as_mut().is_some_and(|p| p.hear(&from, sealed))
                                            && let Some(effective) = greet(&mut privacy, &roster)
                                        {
                                            let _ = updates.send(Update::Posture(effective));
                                        }
                                    }
                                    Some(Command::Roster(users)) => {
                                        roster = users;
                                        greet(&mut privacy, &roster);
                                    }
                                    Some(Command::Leave) | None => break None,
                                },
                                Some(event) = events.recv() => media(&engine, &updates, event),
                            }
                        };
                        call.close().await;
                        next
                    }
                    Err(err) => {
                        let _ = updates.send(Update::Failed(format!("{err:#}")));
                        // Wait for the server to point us elsewhere, or for the hang-up.
                        loop {
                            match commands.recv().await {
                                Some(Command::Server(new)) => break Some(new),
                                Some(Command::Leave) | None => break None,
                                Some(Command::Roster(users)) => roster = users,
                                Some(Command::Keys { hello, media }) => {
                                    if let Some(p) = privacy.as_mut() {
                                        p.rekey(hello, media);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        };
        match next {
            Some(new) => server = new,
            None => return,
        }
    }
}

/// What a call's media events mean for the engine and the UI.
fn media(engine: &AudioEngine, updates: &mpsc::UnboundedSender<Update>, event: CallEvent) {
    match event {
        CallEvent::Track { from, slot: Slot::Mic, track: MediaStreamTrack::Audio(track) } => engine.add_voice(&from, track),
        CallEvent::Track { from, slot: Slot::ScreenAudio, track: MediaStreamTrack::Audio(track) } => engine.add_voice(&format!("{from}#screen"), track),
        CallEvent::TrackEnded { from, slot: Slot::Mic } => engine.remove_voice(&from),
        CallEvent::TrackEnded { from, slot: Slot::ScreenAudio } => engine.remove_voice(&format!("{from}#screen")),
        CallEvent::Track { from, slot: slot @ (Slot::Camera | Slot::Screen), track: MediaStreamTrack::Video(track) } => {
            let feed = VideoFeed::start(track, if slot == Slot::Screen { 1920 } else { 640 }, 30);
            let _ = updates.send(Update::Video { user: from, slot, feed: Some(feed) });
        }
        CallEvent::TrackEnded { from, slot: slot @ (Slot::Camera | Slot::Screen) } => {
            let _ = updates.send(Update::Video { user: from, slot, feed: None });
        }
        CallEvent::Link { user, up } => {
            let _ = updates.send(Update::Link { user, up });
        }
        CallEvent::Error(err) => log::warn!("call: {err}"),
        _ => {}
    }
}
