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
use rhein_voice::mesh::{Mesh, MeshEvent, Slot};
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
async fn pumping<T>(a: &mut Mesh, ea: &mut UnboundedReceiver<MeshEvent>, b: &mut Mesh, eb: &mut UnboundedReceiver<MeshEvent>, work: impl std::future::Future<Output = T>) -> T {
    tokio::pin!(work);
    loop {
        tokio::select! {
            out = &mut work => return out,
            Some(MeshEvent::Signal { payload, .. }) = ea.recv() => b.accept("@a:test", payload),
            Some(MeshEvent::Signal { payload, .. }) = eb.recv() => a.accept("@b:test", payload),
        }
    }
}

/// Relays signals between the two until `stop` accepts an event (seen by `a` if the flag is
/// true, else by `b`), returning that event.
async fn pump(
    a: &mut Mesh,
    ea: &mut UnboundedReceiver<MeshEvent>,
    b: &mut Mesh,
    eb: &mut UnboundedReceiver<MeshEvent>,
    mut stop: impl FnMut(bool, &MeshEvent) -> bool,
) -> MeshEvent {
    let wait = async {
        loop {
            let (at_a, event) = tokio::select! {
                Some(e) = ea.recv() => (true, e),
                Some(e) = eb.recv() => (false, e),
            };
            match event {
                MeshEvent::Signal { payload, .. } => {
                    if at_a { b.accept("@a:test", payload) } else { a.accept("@b:test", payload) }
                }
                MeshEvent::Error(err) => eprintln!("{} error: {err}", if at_a { "a" } else { "b" }),
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
    let got = pump(&mut a, &mut ea, &mut b, &mut eb, |at_a, e| !at_a && matches!(e, MeshEvent::Track { slot: Slot::Mic, .. })).await;
    let MeshEvent::Track { from, track, .. } = got else { unreachable!() };
    assert_eq!(from, "@a:test");
    assert!(pumping(&mut a, &mut ea, &mut b, &mut eb, hears(track, 440.)).await, "b hears a's tone");

    // b opens its microphone late: the m-line exists already, it is turned around.
    let (tone_b, feed_b) = tone(b.factory(), 660.);
    b.set_source(Slot::Mic, Some(tone_b));
    let got = pump(&mut a, &mut ea, &mut b, &mut eb, |at_a, e| at_a && matches!(e, MeshEvent::Track { slot: Slot::Mic, .. })).await;
    let MeshEvent::Track { from, track, .. } = got else { unreachable!() };
    assert_eq!(from, "@b:test");
    assert!(pumping(&mut a, &mut ea, &mut b, &mut eb, hears(track, 660.)).await, "a hears b's tone");

    // And closes it again: the track ends on a's side.
    b.set_source(Slot::Mic, None);
    let got = pump(&mut a, &mut ea, &mut b, &mut eb, |at_a, e| at_a && matches!(e, MeshEvent::TrackEnded { .. })).await;
    assert!(matches!(got, MeshEvent::TrackEnded { slot: Slot::Mic, .. }));

    feed_a.abort();
    feed_b.abort();
    a.close();
    b.close();
}
