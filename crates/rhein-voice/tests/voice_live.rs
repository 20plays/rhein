//! Guild voice over a running dev server without an SFU region (`RHEIN_LIVE`, for example server
//! b): two accounts join a voice channel, the server answers with a mesh, their relay payloads go
//! through `/voice/signal` and come back as `VOICE_SIGNAL`, the roster follows
//! `VOICE_STATE_UPDATE`, and one hears the other's tone. Leaving takes the peer away on the other
//! side. SFU calls, which are end-to-end encrypted, are in `rhein-store/tests/guild_call_live.rs`.

use std::collections::HashSet;
use std::time::Duration;

use futures::StreamExt as _;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::media_stream_track::MediaStreamTrack;
use livekit::webrtc::peer_connection_factory::PeerConnectionFactory;
use livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt as _;
use livekit::webrtc::prelude::{AudioFrame, AudioSourceOptions};
use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_net::gateway::{self, GatewayConfig, GatewayHandle};
use rhein_net::{Api, GatewayEvent, auth, discover};
use rhein_proto::{Event, PresenceState, USER_INTENTS, VoiceServer};
use rhein_voice::mesh::{self, Mesh, CallEvent, Slot};
use tokio::sync::mpsc::UnboundedReceiver;

async fn register(home: &Api, localpart: &str) -> auth::SignedIn {
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    for _ in 0..30 {
        match auth::register(home, localpart, "correct horse battery", &DeviceKey::generate(), "rhein voice test", &ksf).await {
            Ok(signed) => return signed,
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    panic!("could not register {localpart}");
}

/// One participant: guild API, gateway, and its roster of the voice channel.
struct Member {
    user: String,
    api: Api,
    _gateway: GatewayHandle,
    events: UnboundedReceiver<GatewayEvent>,
    mesh: Option<(Mesh, UnboundedReceiver<CallEvent>)>,
    roster: HashSet<String>,
}

impl Member {
    async fn ready(&mut self) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(event) = self.events.recv().await {
                if matches!(event, GatewayEvent::Ready(_)) {
                    return;
                }
            }
        })
        .await
        .expect("gateway ready");
    }
}

async fn member(http: &reqwest::Client, doc: &rhein_proto::Discovery, signed: &auth::SignedIn, home: &Api) -> Member {
    let api = Api::new(http.clone(), &doc.api, None);
    let session = auth::guild_session(home, &api, &doc.server_name).await.expect("guild session");
    let (gateway, events) = gateway::connect(GatewayConfig {
        url: session.gateway.clone().or(doc.gateway.clone()).expect("gateway"),
        token: session.session_token.clone(),
        intents: USER_INTENTS,
        presence: PresenceState { status: "online".into(), ..Default::default() },
        voice: Vec::new(),
    });
    Member { user: signed.session.user_id.clone(), api, _gateway: gateway, events, mesh: None, roster: HashSet::new() }
}

fn tone(factory: &PeerConnectionFactory, hz: f32) -> (MediaStreamTrack, tokio::task::JoinHandle<()>) {
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 1000);
    let track = factory.create_audio_track("mic", source.clone());
    let feeder = tokio::spawn(async move {
        let mut phase = 0f32;
        loop {
            let samples: Vec<i16> = (0..480)
                .map(|_| {
                    phase += 2. * std::f32::consts::PI * hz / 48_000.;
                    (phase.sin() * 8000.) as i16
                })
                .collect();
            let frame = AudioFrame { data: samples.into(), num_channels: 1, sample_rate: 48_000, samples_per_channel: 480 };
            if source.capture_frame(&frame).await.is_err() {
                break;
            }
        }
    });
    (track.into(), feeder)
}

/// Whether a frame of `samples` is mostly a `hz` tone.
fn is_tone(samples: &[i16], hz: f64) -> bool {
    let (mut re, mut im, mut energy) = (0f64, 0f64, 0f64);
    for (i, &x) in samples.iter().enumerate() {
        let t = 2. * std::f64::consts::PI * hz * i as f64 / 48_000.;
        re += x as f64 * t.cos();
        im += x as f64 * t.sin();
        energy += (x as f64).powi(2);
    }
    let peak = samples.iter().map(|s| s.saturating_abs()).max().unwrap_or(0);
    peak > 2000 && 2. * (re * re + im * im) / (energy.max(1.) * samples.len() as f64) > 0.9
}

/// What one turn of the call loop saw that the test cares about.
enum Seen {
    Track { at: usize, from: String, track: MediaStreamTrack },
    Ended { at: usize, from: String },
}

/// Runs both members' calls for one step: mesh signals go out through the server, gateway
/// signals and roster changes come back into the mesh.
async fn step(members: &mut [Member; 2]) -> Option<Seen> {
    let [a, b] = members;
    let (at, event) = tokio::select! {
        Some(e) = a.events.recv() => (0, Ok(e)),
        Some(e) = b.events.recv() => (1, Ok(e)),
        Some(e) = async { a.mesh.as_mut()?.1.recv().await } => (0, Err(e)),
        Some(e) = async { b.mesh.as_mut()?.1.recv().await } => (1, Err(e)),
    };
    let me = &mut members[at];
    match event {
        Ok(GatewayEvent::Dispatch(event)) => match *event {
            Event::VoiceSignal { from, payload, .. } => {
                if let Some((mesh, _)) = me.mesh.as_mut() {
                    mesh.accept(&from, payload);
                }
            }
            Event::VoiceStateUpdate(state) => {
                if state.channel_id.is_some() {
                    me.roster.insert(state.user_id);
                } else {
                    me.roster.remove(&state.user_id);
                }
                if let Some((mesh, _)) = me.mesh.as_mut() {
                    mesh.sync(me.roster.iter().map(String::as_str));
                }
            }
            _ => {}
        },
        Ok(_) => {}
        Err(CallEvent::Signal { target, payload }) => {
            let channel = CHANNEL.with(|c| c.borrow().clone());
            if let Err(err) = me.api.voice_signal(&channel, &target, &payload).await {
                eprintln!("signal to {target}: {err}");
            }
        }
        Err(CallEvent::Track { from, slot: Slot::Mic, track }) => return Some(Seen::Track { at, from, track }),
        Err(CallEvent::TrackEnded { from, slot: Slot::Mic }) => return Some(Seen::Ended { at, from }),
        Err(CallEvent::Error(err)) => eprintln!("{} mesh: {err}", me.user),
        Err(_) => {}
    }
    None
}

thread_local! {
    static CHANNEL: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Two fresh accounts in a new guild with a voice channel, gateways up; `None` without
/// `RHEIN_LIVE`.
async fn two_in_a_guild(prefix: &str) -> Option<([Member; 2], String, reqwest::Client)> {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return None;
    };
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let tag = std::process::id();
    let (home_a, home_b) = (Api::new(http.clone(), &doc.api, None), Api::new(http.clone(), &doc.api, None));
    let (sa, sb) = (register(&home_a, &format!("{prefix}a{tag}")).await, register(&home_b, &format!("{prefix}b{tag}")).await);

    let a = member(&http, &doc, &sa, &home_a).await;
    let guild = a.api.create_guild("Rhein voice").await.expect("guild");
    let voice = a.api.create_channel(&guild.id, "Lounge", "voice", None).await.expect("voice channel");
    let invite = a.api.create_invite(&guild.id, None).await.expect("invite");
    let b = member(&http, &doc, &sb, &home_b).await;
    b.api.join_invite(invite["code"].as_str().unwrap()).await.expect("join guild");
    // Fresh gateways that cover the guild as it now is.
    let a = member(&http, &doc, &sa, &home_a).await;
    let b = member(&http, &doc, &sb, &home_b).await;
    let mut members = [a, b];
    for m in members.iter_mut() {
        m.ready().await;
    }
    Some((members, voice.id, http))
}

#[tokio::test]
async fn two_members_talk_over_a_server_mesh() {
    let Some((mut members, voice_id, _)) = two_in_a_guild("vm").await else { return };
    CHANNEL.with(|c| *c.borrow_mut() = voice_id.clone());
    // Both join. Without an SFU region (scripts/dev-server.sh without RHEIN_SFU) the answer is a
    // mesh.
    let factory = PeerConnectionFactory::default();
    let (mic, feeder) = tone(&factory, 440.);
    for (i, m) in members.iter_mut().enumerate() {
        let joined = m.api.voice_join(&voice_id, false, false, false, None, None).await.expect("join voice");
        assert_eq!(joined.voice_state.channel_id.as_deref(), Some(voice_id.as_str()));
        assert!(joined.voice_state.session_id.is_some());
        let VoiceServer::Mesh { ice_servers, .. } = joined.voice_server else {
            eprintln!("the server has an SFU region; skipping the mesh test");
            feeder.abort();
            return;
        };
        let (mut call, events) = Mesh::new(&m.user, factory.clone(), mesh::ice_servers(&ice_servers), false);
        if i == 0 {
            call.set_source(Slot::Mic, Some(mic.clone()));
        }
        m.roster.insert(m.user.clone());
        m.mesh = Some((call, events));
    }
    // The first to join learned of the second from VOICE_STATE_UPDATE; the second also reads
    // the channel's roster, as a client does on joining.
    let here = members[1].api.voice_channel(&voice_id).await.unwrap();
    for state in here["voice_states"].as_array().unwrap() {
        members[1].roster.insert(state["user_id"].as_str().unwrap().to_owned());
    }
    let b = &mut members[1];
    b.mesh.as_mut().unwrap().0.sync(b.roster.iter().map(String::as_str));

    let a_user = members[0].user.clone();
    let track = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(Seen::Track { at: 1, from, track }) = step(&mut members).await {
                assert_eq!(from, a_user);
                return track;
            }
        }
    })
    .await
    .expect("b gets a's microphone");

    // b hears the tone while the calls keep running.
    let MediaStreamTrack::Audio(track) = track else { panic!("audio") };
    let mut stream = NativeAudioStream::new(track, 48_000, 1);
    let heard = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            tokio::select! {
                frame = stream.next() => if frame.is_some_and(|f| is_tone(&f.data, 440.)) { return true },
                _ = step(&mut members) => {}
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(heard, "b hears a's tone through the mesh");

    // a leaves: b's roster drops a, and with it the connection and the track.
    let leave = format!("/channels/{voice_id}/voice/leave");
    let _: serde_json::Value = members[0].api.post_patiently(&leave, &serde_json::json!({})).await.expect("leave");
    members[0].mesh = None;
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(Seen::Ended { at: 1, from }) = step(&mut members).await {
                return from;
            }
        }
    })
    .await
    .expect("a's track ends for b");
    assert_eq!(ended, a_user);
    feeder.abort();
    members[1].api.voice_leave(&voice_id).await.ok();
}
