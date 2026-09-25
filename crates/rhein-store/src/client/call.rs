//! Guild voice in the client. Joining asks the guild server where to dial, then a call task on
//! the tokio runtime owns the media: a mesh (signals out through `/voice/signal`, in through
//! `VOICE_SIGNAL`) or an SFU room. Remote voices go into the audio engine, links and failures
//! come back here. The roster itself stays what `VOICE_STATE_UPDATE` says.

use std::collections::HashSet;
use std::sync::Arc;

use rhein_voice::livekit::webrtc::media_stream_track::MediaStreamTrack;
use rhein_voice::livekit::webrtc::peer_connection_factory::PeerConnectionFactory;
use rhein_voice::livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt as _;
use rhein_proto::{VoiceClaim, VoiceServer};
use rhein_voice::audio::{AudioConfig, AudioEngine};
use rhein_voice::mesh::{self, Mesh};
use rhein_voice::sfu::{self, SfuCall};
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
    pub guild_id: String,
    pub channel_id: String,
    pub session_id: Option<String>,
    pub mode: Option<CallMode>,
    pub status: CallStatus,
    /// Participants whose media reaches us.
    pub linked: HashSet<String>,
    pub muted: bool,
    pub deafened: bool,
    engine: Option<Arc<AudioEngine>>,
    commands: Option<mpsc::UnboundedSender<Command>>,
    speaking: HashSet<String>,
    _task: Option<Task<()>>,
}

impl ActiveCall {
    pub fn engine(&self) -> Option<&Arc<AudioEngine>> {
        self.engine.as_ref()
    }
}

enum Command {
    Signal { from: String, payload: Value },
    Roster(Vec<String>),
    Server(VoiceServer),
    Watch { user: String, on: bool },
    Leave,
}

enum Update {
    Connected(CallMode),
    Link { user: String, up: bool },
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
        let Some(me) = self.home.as_ref().map(|h| h.user_id.clone()) else { return };
        let (muted, deafened) = (self.prefs.voice.muted, self.prefs.voice.deafened);
        let prefs = self.voice_prefs();
        let (io, http) = (self.io.clone(), self.http.clone());
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (updates, mut update_rx) = mpsc::unbounded_channel();
        let channel = channel_id.clone();

        let task = cx.spawn(async move |this, cx| {
            // Where to dial, and the devices, off the UI thread.
            let started = join(io.spawn(async move {
                let published = api.voice_regions().await.unwrap_or(Value::Null);
                let region = rhein_voice::region::choose(&http, &published, None).await;
                let joined = api.voice_join(&channel, muted, deafened, false, region.as_deref()).await?;
                let engine = tokio::task::spawn_blocking(move || AudioEngine::start(prefs.audio_config())).await??;
                Ok((api, http, joined, Arc::new(engine)))
            }))
            .await;
            let Ok(Some((api, http, joined, engine))) = this.update(cx, |this, cx| {
                let call = this.call.as_mut()?;
                match started {
                    Ok(ready) => {
                        call.session_id = ready.2.voice_state.session_id.clone();
                        call.engine = Some(ready.3.clone());
                        this.apply_call_audio();
                        this.claim_voice(cx);
                        cx.notify();
                        Some(ready)
                    }
                    Err(err) => {
                        log::warn!("joining voice: {err:#}");
                        call.status = CallStatus::Failed(friendly(&err));
                        cx.notify();
                        None
                    }
                }
            }) else {
                return;
            };
            for error in engine.errors() {
                log::warn!("voice audio: {error}");
            }
            let roster = this.read_with(cx, |this, _| this.call_roster()).unwrap_or_default();
            let channel = joined.voice_state.channel_id.clone().unwrap_or_default();
            io.spawn(drive(me, channel, api, http, joined.voice_server, engine, roster, command_rx, updates));

            while let Some(update) = update_rx.recv().await {
                let alive = this.update(cx, |this, cx| {
                    let Some(call) = this.call.as_mut() else { return false };
                    match update {
                        Update::Connected(mode) => {
                            call.mode = Some(mode);
                            call.status = CallStatus::Connected;
                        }
                        Update::Link { user, up } => {
                            if up {
                                call.linked.insert(user);
                            } else {
                                call.linked.remove(&user);
                            }
                        }
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
            guild_id,
            channel_id,
            session_id: None,
            mode: None,
            status: CallStatus::Connecting,
            linked: HashSet::new(),
            muted,
            deafened,
            engine: None,
            commands: Some(commands),
            speaking: HashSet::new(),
            _task: Some(task),
        });
        self.watch_speaking(cx);
        cx.notify();
    }

    /// Hangs up.
    pub fn leave_voice(&mut self, cx: &mut Context<Self>) {
        let Some(call) = self.call.take() else { return };
        if let Some(commands) = &call.commands {
            let _ = commands.send(Command::Leave);
        }
        if let Some(api) = self.api_for_guild(&call.guild_id) {
            let channel = call.channel_id.clone();
            self.io.spawn(async move {
                let _ = api.voice_leave(&channel).await;
            });
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
            let (guild, channel) = (call.guild_id.clone(), call.channel_id.clone());
            if let Some(api) = self.api_for_guild(&guild) {
                self.io.spawn(async move {
                    let _ = api.voice_state(&channel, Some(muted), Some(deafened), None).await;
                });
            }
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
                .and_then(|c| Some(VoiceClaim { room_id: c.guild_id.clone(), session_id: c.session_id.clone()? }))
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
        self.voice_members(&call.guild_id, &call.channel_id).into_iter().map(|s| s.user_id).collect()
    }

    /// Called for every voice state change of a guild: the mesh follows the roster, and a call
    /// the server moved or ended follows the server.
    pub(super) fn on_voice_roster(&mut self, state: &rhein_proto::VoiceState, cx: &mut Context<Self>) {
        let Some(call) = &self.call else { return };
        if state.guild_id.as_deref() != Some(call.guild_id.as_str()) {
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
                Some(channel) if channel != call.channel_id => {
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

    /// Subscribes to (or drops) someone's screen share on an SFU call.
    pub fn watch_screen(&self, user_id: &str, on: bool) {
        if let Some(commands) = self.call.as_ref().and_then(|c| c.commands.as_ref()) {
            let _ = commands.send(Command::Watch { user: user_id.to_owned(), on });
        }
    }

    /// Polls the engine for who is audible, and redraws when that changes.
    fn watch_speaking(&mut self, cx: &mut Context<Self>) {
        self.speaking_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let Ok(alive) = this.update(cx, |this, cx| {
                    let me = this.home.as_ref().map(|h| h.user_id.clone()).unwrap_or_default();
                    let Some(call) = this.call.as_mut() else { return false };
                    let now: HashSet<String> = call
                        .engine
                        .as_ref()
                        .map(|e| e.speaking().into_iter().map(|k| if k.is_empty() { me.clone() } else { k }).collect())
                        .unwrap_or_default();
                    if now != call.speaking {
                        call.speaking = now;
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

/// The media side of one call, on the tokio runtime. Reconnects when the server points the call
/// somewhere else (`VOICE_SERVER_UPDATE`).
#[allow(clippy::too_many_arguments)]
async fn drive(
    me: String,
    channel: String,
    api: Api,
    http: reqwest::Client,
    mut server: VoiceServer,
    engine: Arc<AudioEngine>,
    mut roster: Vec<String>,
    mut commands: mpsc::UnboundedReceiver<Command>,
    updates: mpsc::UnboundedSender<Update>,
) {
    // Signals leave one at a time and in order: an answer overtaking its offer is a failed call.
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel::<(String, Value)>();
    {
        let (api, channel) = (api.clone(), channel.clone());
        tokio::spawn(async move {
            while let Some((target, payload)) = signal_rx.recv().await {
                if let Err(err) = api.voice_signal(&channel, &target, &payload).await {
                    log::debug!("voice signal to {target}: {err}");
                }
            }
        });
    }
    loop {
        let next = match server {
            VoiceServer::Mesh { ice_servers, relay, .. } => {
                let factory = PeerConnectionFactory::default();
                let microphone: MediaStreamTrack = factory.create_audio_track("microphone", engine.microphone()).into();
                let (mut call, mut events) = Mesh::new(&me, factory, mesh::ice_servers(&ice_servers), false);
                let _ = relay;
                call.set_source(Slot::Mic, Some(microphone));
                call.sync(roster.iter().map(String::as_str));
                let _ = updates.send(Update::Connected(CallMode::Mesh));
                loop {
                    tokio::select! {
                        command = commands.recv() => match command {
                            Some(Command::Signal { from, payload }) => call.accept(&from, payload),
                            Some(Command::Roster(users)) => {
                                call.sync(users.iter().map(String::as_str));
                                roster = users;
                            }
                            Some(Command::Server(new)) => break Some(new),
                            Some(Command::Watch { .. }) => {}
                            Some(Command::Leave) | None => break None,
                        },
                        Some(event) = events.recv() => match event {
                            CallEvent::Signal { target, payload } => { let _ = signal_tx.send((target, payload)); }
                            event => media(&engine, &updates, event),
                        },
                    }
                }
            }
            VoiceServer::Sfu { endpoint, token, region, .. } => {
                let joined = match sfu::session(&http, &endpoint, &token).await {
                    Ok(session) => SfuCall::connect(session, engine.microphone(), None).await,
                    Err(err) => Err(err),
                };
                match joined {
                    Ok((mut call, mut events)) => {
                        let _ = updates.send(Update::Connected(CallMode::Sfu { region }));
                        let next = loop {
                            tokio::select! {
                                command = commands.recv() => match command {
                                    Some(Command::Watch { user, on }) => call.watch(&user, on),
                                    Some(Command::Server(new)) => break Some(new),
                                    Some(Command::Signal { .. }) => {}
                                    Some(Command::Roster(users)) => roster = users,
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
        CallEvent::Link { user, up } => {
            let _ = updates.send(Update::Link { user, up });
        }
        CallEvent::Error(err) => log::warn!("call: {err}"),
        // Video arrives with M4.2.
        _ => {}
    }
}
