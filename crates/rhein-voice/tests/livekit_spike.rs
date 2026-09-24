//! M4.0(a): the LiveKit Rust SDK against a real `livekit-server`. One participant publishes a
//! 440 Hz tone as its microphone, the other subscribes and hears it. Starts
//! `.dev/bin/livekit-server --dev` itself; skipped when the binary is missing. M4.0(b): the same
//! with Telium's DM call encryption.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures::StreamExt as _;
use livekit::options::TrackPublishOptions;
use livekit::track::{LocalAudioTrack, LocalTrack, RemoteTrack, TrackSource};
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::prelude::{AudioFrame, AudioSourceOptions, RtcAudioSource};
use livekit::{Room, RoomEvent, RoomOptions};
use livekit_api::access_token::{AccessToken, VideoGrants};
use rhein_voice::e2ee;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn start_livekit(port: u16) -> Option<Server> {
    let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.dev/bin/livekit-server");
    if !bin.exists() {
        eprintln!("no livekit-server in .dev/bin; skipping");
        return None;
    }
    let child = Command::new(bin)
        .args(["--dev", "--bind", "127.0.0.1", "--port", &port.to_string(), "--rtc.tcp_port", &(port + 1).to_string()])
        .args(["--rtc.port_range_start", &(port as u32 * 10 - 28000).to_string(), "--rtc.port_range_end", &(port as u32 * 10 - 27900).to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    Some(Server(child))
}

fn token(identity: &str, room: &str) -> String {
    // `--dev` runs with the key "devkey" and secret "secret".
    AccessToken::with_api_key("devkey", "secret")
        .with_identity(identity)
        .with_grants(VideoGrants { room_join: true, room: room.into(), ..Default::default() })
        .to_jwt()
        .unwrap()
}

async fn connect(port: u16, identity: &str, options: impl Fn() -> RoomOptions) -> (Room, tokio::sync::mpsc::UnboundedReceiver<RoomEvent>) {
    let mut last = None;
    for _ in 0..40 {
        match Room::connect(&format!("ws://127.0.0.1:{port}"), &token(identity, "rhein-spike"), options()).await {
            Ok(pair) => return pair,
            Err(err) => last = Some(err),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("could not reach livekit-server: {last:?}");
}

/// Publishes a 440 Hz tone from `speaker` until the returned task is aborted.
async fn publish_tone(speaker: &Room) -> tokio::task::JoinHandle<()> {
    // A 48 kHz mono source fed 10 ms frames of a 440 Hz sine.
    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 1, 1000);
    let track = LocalAudioTrack::create_audio_track("mic", RtcAudioSource::Native(source.clone()));
    speaker
        .local_participant()
        .publish_track(LocalTrack::Audio(track), TrackPublishOptions { source: TrackSource::Microphone, ..Default::default() })
        .await
        .expect("publish");
    tokio::spawn(async move {
        let mut phase = 0f32;
        loop {
            let samples: Vec<i16> = (0..480)
                .map(|_| {
                    phase += 2. * std::f32::consts::PI * 440. / 48_000.;
                    (phase.sin() * 8000.) as i16
                })
                .collect();
            let frame = AudioFrame { data: samples.into(), num_channels: 1, sample_rate: 48_000, samples_per_channel: 480 };
            if source.capture_frame(&frame).await.is_err() {
                break;
            }
        }
    })
}

/// How much of a frame is the 440 Hz tone: about 1 for the tone, well below for noise (which is
/// what Opus makes of a frame it cannot decrypt, passed through by a cryptor not yet running).
fn tone_share(samples: &[i16]) -> f64 {
    let (mut re, mut im, mut energy) = (0f64, 0f64, 0f64);
    for (i, &x) in samples.iter().enumerate() {
        let t = 2. * std::f64::consts::PI * 440. * i as f64 / 48_000.;
        re += x as f64 * t.cos();
        im += x as f64 * t.sin();
        energy += (x as f64).powi(2);
    }
    2. * (re * re + im * im) / (energy.max(1.) * samples.len() as f64)
}

/// Waits for the speaker's track, then for the loudest peak of the tone heard within a while.
async fn loudest(events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>, listen: Duration) -> i16 {
    let track = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(event) = events.recv().await {
            if let RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(track), participant, .. } = event {
                assert_eq!(participant.identity().as_str(), "speaker");
                return track;
            }
        }
        panic!("events ended");
    })
    .await
    .expect("subscribed to the tone");
    let mut stream = NativeAudioStream::new(track.rtc_track(), 48_000, 1);
    let mut peak = 0;
    let _ = tokio::time::timeout(listen, async {
        while let Some(frame) = stream.next().await {
            if tone_share(&frame.data) > 0.9 {
                peak = peak.max(frame.data.iter().map(|s| s.saturating_abs()).max().unwrap_or(0));
            }
            if peak > 2000 {
                break;
            }
        }
    })
    .await;
    peak
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tone_crosses_the_sfu() {
    let Some(_server) = start_livekit(7980) else { return };
    let (speaker, _speaker_events) = connect(7980, "speaker", RoomOptions::default).await;
    let (listener, mut events) = connect(7980, "listener", RoomOptions::default).await;
    let feeder = publish_tone(&speaker).await;
    let peak = loudest(&mut events, Duration::from_secs(15)).await;
    assert!(peak > 2000, "the tone is audible (peak {peak})");
    feeder.abort();
    listener.close().await.ok();
    speaker.close().await.ok();
}

/// M4.0(b): media encrypted the way DM calls do it (a shared HKDF key in slot `epoch % 16`).
/// A participant with the same secret hears the tone, one with another secret never does (not
/// even from the first frames, which the sender's cryptor would pass on in the clear if it were
/// not running yet).
#[tokio::test(flavor = "multi_thread")]
async fn an_encrypted_tone_needs_the_epoch_key() {
    let Some(_server) = start_livekit(7990) else { return };
    let secret = [7u8; 32];
    let encrypted = |secret: [u8; 32]| {
        move || {
            let mut options = RoomOptions::default();
            e2ee::encrypt(&mut options, e2ee::key_provider(&secret, 19));
            options
        }
    };
    let (speaker, _speaker_events) = connect(7990, "speaker", encrypted(secret)).await;
    let (friend, mut friend_events) = connect(7990, "friend", encrypted(secret)).await;
    let (stranger, mut stranger_events) = connect(7990, "stranger", encrypted([8u8; 32])).await;
    let feeder = publish_tone(&speaker).await;
    let heard = loudest(&mut friend_events, Duration::from_secs(15)).await;
    assert!(heard > 2000, "the tone is audible with the epoch key (peak {heard})");
    let overheard = loudest(&mut stranger_events, Duration::from_secs(4)).await;
    assert_eq!(overheard, 0, "another key never hears the tone");
    feeder.abort();
    for room in [friend, stranger, speaker] {
        room.close().await.ok();
    }
}
