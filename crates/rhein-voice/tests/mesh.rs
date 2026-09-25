//! M4.0(c): mesh calls on libwebrtc's own `PeerConnectionFactory`. Two participants in one
//! process, their relay payloads handed straight across: both offer at once (perfect
//! negotiation has to settle it), one hears the other's tone, a microphone opened later arrives
//! by turning its m-line around, and closing it ends the track on the far side.

use std::time::Duration;

use futures::StreamExt as _;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::media_stream_track::MediaStreamTrack;
use livekit::webrtc::peer_connection_factory::PeerConnectionFactory;
use livekit::webrtc::peer_connection_factory::native::PeerConnectionFactoryExt as _;
use livekit::webrtc::prelude::{AudioFrame, AudioSourceOptions};
use rhein_voice::mesh::{Mesh, CallEvent, Slot};
use tokio::sync::mpsc::UnboundedReceiver;

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

fn tone_share(samples: &[i16], hz: f64) -> f64 {
    let (mut re, mut im, mut energy) = (0f64, 0f64, 0f64);
    for (i, &x) in samples.iter().enumerate() {
        let t = 2. * std::f64::consts::PI * hz * i as f64 / 48_000.;
        re += x as f64 * t.cos();
        im += x as f64 * t.sin();
        energy += (x as f64).powi(2);
    }
    2. * (re * re + im * im) / (energy.max(1.) * samples.len() as f64)
}

/// Whether `track` carries a clear `hz` tone within a few seconds.
async fn hears(track: MediaStreamTrack, hz: f64) -> bool {
    let MediaStreamTrack::Audio(track) = track else { return false };
    let mut stream = NativeAudioStream::new(track, 48_000, 1);
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(frame) = stream.next().await {
            let peak = frame.data.iter().map(|s| s.saturating_abs()).max().unwrap_or(0);
            if peak > 2000 && tone_share(&frame.data, hz) > 0.9 {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

/// Relays signals between the two while `work` runs.
async fn pumping<T>(a: &mut Mesh, ea: &mut UnboundedReceiver<CallEvent>, b: &mut Mesh, eb: &mut UnboundedReceiver<CallEvent>, work: impl std::future::Future<Output = T>) -> T {
    tokio::pin!(work);
    loop {
        tokio::select! {
            out = &mut work => return out,
            Some(CallEvent::Signal { payload, .. }) = ea.recv() => b.accept("@a:test", payload),
            Some(CallEvent::Signal { payload, .. }) = eb.recv() => a.accept("@b:test", payload),
        }
    }
}

/// Relays signals between the two until `stop` accepts an event (seen by `a` if the flag is
/// true, else by `b`), returning that event.
async fn pump(
    a: &mut Mesh,
    ea: &mut UnboundedReceiver<CallEvent>,
    b: &mut Mesh,
    eb: &mut UnboundedReceiver<CallEvent>,
    mut stop: impl FnMut(bool, &CallEvent) -> bool,
) -> CallEvent {
    let wait = async {
        loop {
            let (at_a, event) = tokio::select! {
                Some(e) = ea.recv() => (true, e),
                Some(e) = eb.recv() => (false, e),
            };
            match event {
                CallEvent::Signal { payload, .. } => {
                    if at_a { b.accept("@a:test", payload) } else { a.accept("@b:test", payload) }
                }
                CallEvent::Error(err) => eprintln!("{} error: {err}", if at_a { "a" } else { "b" }),
                event if stop(at_a, &event) => return event,
                _ => {}
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(20), wait).await.expect("the mesh got there")
}

#[tokio::test(flavor = "multi_thread")]
async fn two_peers_negotiate_and_hear_each_other() {
    let _ = env_logger::try_init();
    // Two factories, as two processes would have.
    let (fa, fb) = (PeerConnectionFactory::default(), PeerConnectionFactory::default());
    let (tone_a, feed_a) = tone(&fa, 440.);
    let (mut a, mut ea) = Mesh::new("@a:test", fa, vec![], false);
    let (mut b, mut eb) = Mesh::new("@b:test", fb, vec![], false);
    a.set_source(Slot::Mic, Some(tone_a));

    // Both learn of each other at once, so both offer: glare, which the polite side yields.
    a.sync(["@a:test", "@b:test"]);
    b.sync(["@a:test", "@b:test"]);
    let got = pump(&mut a, &mut ea, &mut b, &mut eb, |at_a, e| !at_a && matches!(e, CallEvent::Track { slot: Slot::Mic, .. })).await;
    let CallEvent::Track { from, track, .. } = got else { unreachable!() };
    assert_eq!(from, "@a:test");
    assert!(pumping(&mut a, &mut ea, &mut b, &mut eb, hears(track, 440.)).await, "b hears a's tone");

    // b opens its microphone late: the m-line exists already, it is turned around.
    let (tone_b, feed_b) = tone(b.factory(), 660.);
    b.set_source(Slot::Mic, Some(tone_b));
    let got = pump(&mut a, &mut ea, &mut b, &mut eb, |at_a, e| at_a && matches!(e, CallEvent::Track { slot: Slot::Mic, .. })).await;
    let CallEvent::Track { from, track, .. } = got else { unreachable!() };
    assert_eq!(from, "@b:test");
    assert!(pumping(&mut a, &mut ea, &mut b, &mut eb, hears(track, 660.)).await, "a hears b's tone");

    // And closes it again: the track ends on a's side.
    b.set_source(Slot::Mic, None);
    let got = pump(&mut a, &mut ea, &mut b, &mut eb, |at_a, e| at_a && matches!(e, CallEvent::TrackEnded { .. })).await;
    assert!(matches!(got, CallEvent::TrackEnded { slot: Slot::Mic, .. }));

    feed_a.abort();
    feed_b.abort();
    a.close();
    b.close();
}

/// The audio engines on both ends: a's microphone (fed a tone, as if captured) goes out through
/// the mesh, b plays it into its mix and sees a speaking.
#[tokio::test(flavor = "multi_thread")]
async fn engines_carry_a_voice_through_the_mesh() {
    use rhein_voice::audio::AudioEngine;
    let (fa, fb) = (PeerConnectionFactory::default(), PeerConnectionFactory::default());
    let (engine_a, engine_b) = (AudioEngine::headless().unwrap(), AudioEngine::headless().unwrap());
    let mic: MediaStreamTrack = fa.create_audio_track("mic", engine_a.microphone()).into();
    let (mut a, mut ea) = Mesh::new("@a:test", fa, vec![], false);
    let (mut b, mut eb) = Mesh::new("@b:test", fb, vec![], false);
    a.set_source(Slot::Mic, Some(mic));
    a.sync(["@a:test", "@b:test"]);
    b.sync(["@a:test", "@b:test"]);

    // a "speaks" in real time.
    let speaking = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let feeder = {
        let speaking = speaking.clone();
        let tone: Vec<i16> = (0..480 * 100).map(|i| ((i as f32 * 2. * std::f32::consts::PI * 440. / 48_000.).sin() * 8000.) as i16).collect();
        let engine_a = std::sync::Arc::new(engine_a);
        let feeding = engine_a.clone();
        let thread = std::thread::spawn(move || {
            let mut chunks = tone.chunks(480).cycle();
            while speaking.load(std::sync::atomic::Ordering::Relaxed) {
                feeding.feed(chunks.next().unwrap());
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        (thread, engine_a)
    };

    let got = pump(&mut a, &mut ea, &mut b, &mut eb, |at_a, e| !at_a && matches!(e, CallEvent::Track { slot: Slot::Mic, .. })).await;
    let CallEvent::Track { track: MediaStreamTrack::Audio(track), .. } = got else { panic!("audio track") };
    engine_b.add_voice("@a:test", track);
    let tap = engine_b.tap();
    let heard = pumping(&mut a, &mut ea, &mut b, &mut eb, async {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match tap.try_recv() {
                Ok(frame) if tone_share(&frame, 440.) > 0.9 && frame.iter().map(|s| s.saturating_abs()).max().unwrap_or(0) > 2000 => return true,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
        false
    })
    .await;
    assert!(heard, "b's mix carries a's tone");
    assert!(engine_b.speaking().iter().any(|k| k == "@a:test"), "a shows as speaking at b");

    // Deafened, b plays nothing.
    engine_b.set_deafened(true);
    tokio::time::sleep(Duration::from_millis(100)).await;
    while tap.try_recv().is_ok() {}
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(tap.try_iter().all(|f| f.iter().all(|&s| s == 0)), "deafened is silent");

    speaking.store(false, std::sync::atomic::Ordering::Relaxed);
    feeder.0.join().unwrap();
    a.close();
    b.close();
}
