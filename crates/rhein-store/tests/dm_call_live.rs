//! A call in an encrypted conversation between two Rhein accounts, against a dev server with an
//! SFU region (`RHEIN_LIVE`, `scripts/dev-sfu.sh`, `RHEIN_SFU=1 scripts/dev-server.sh a`):
//! both sides derive the same call secrets from the MLS group and open each other's hellos,
//! asking for an SFU puts the call on it with end-to-end encryption required, and a tone crosses
//! the SFU sealed with the group's media key.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_mls::payload::Payload;
use rhein_mls::vault;
use rhein_net::{Api, auth, discover};
use rhein_proto::VoiceServer;
use rhein_store::conversations::Engine;
use rhein_store::dm::{Secrets, bring_up};
use rhein_voice::audio::AudioEngine;
use rhein_voice::livekit::webrtc::media_stream_track::MediaStreamTrack;
use rhein_voice::posture::{self, Posture};
use rhein_voice::sfu::{self, SfuCall};
use rhein_voice::{CallEvent, Slot, e2ee};

const PASSWORD: &str = "correct horse battery";

async fn register(home: &Api, localpart: &str) -> auth::SignedIn {
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    for _ in 0..20 {
        match auth::register(home, localpart, PASSWORD, &DeviceKey::generate(), "rhein call test", &ksf).await {
            Ok(signed) => return signed,
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    panic!("could not register {localpart}");
}

async fn device(home: &Api, signed: &auth::SignedIn) -> Engine {
    let (user, device) = (&signed.session.user_id, &signed.session.device_id);
    bring_up(home, user, device, Some(vault::recovery_wrapper(&signed.export_key))).await.expect("keys");
    let secrets = Secrets::load(user);
    let (private, public) = secrets.mls_signature.clone().unwrap();
    let mut engine = Engine::open(user, device, (&vault::unb64(&private).unwrap(), &vault::unb64(&public).unwrap()), secrets.keyring.clone()).unwrap();
    engine.replenish(home).await.expect("key packages");
    engine
}

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

#[tokio::test(flavor = "multi_thread")]
async fn an_encrypted_call_through_the_sfu() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rhein-dm-call-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("RHEIN_CONFIG_DIR", &dir) };
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let (a_home, b_home) = (Api::new(http.clone(), &doc.api, None), Api::new(http.clone(), &doc.api, None));
    let tag = std::process::id();
    let (a, b) = (register(&a_home, &format!("ca{tag}")).await, register(&b_home, &format!("cb{tag}")).await);
    let (a_user, b_user) = (a.session.user_id.clone(), b.session.user_id.clone());
    let (mut ea, mut eb) = (device(&a_home, &a).await, device(&b_home, &b).await);

    // A conversation both are in.
    let convo = a_home.create_conversation("dm", std::slice::from_ref(&b_user), None).await.expect("conversation");
    let id = convo["id"].as_str().unwrap().to_owned();
    ea.establish(&a_home, &id, &[b_user.clone(), a_user.clone()], None).await.expect("establish");
    ea.send(&a_home, &id, Payload::Message("call me".into())).await.unwrap();
    let _ = b_home.accept_message_request(&id).await;
    eb.sync(&b_home, &id).await.expect("b joins the group");

    // Both hold the same call secrets for the same epoch, and open each other's hellos.
    let (sa, sb) = (ea.call_secret(&id, posture::SIGNAL_LABEL).unwrap(), eb.call_secret(&id, posture::SIGNAL_LABEL).unwrap());
    assert_eq!(sa, sb, "one hello key per epoch");
    let hello = posture::seal_hello(&sa.0, &id, &a_user, Posture::Sfu, 1, sa.1);
    assert_eq!(posture::open_hello(&sb.0, &id, &a_user, Some(sb.1), &hello).map(|h| h.posture), Some(Posture::Sfu));
    let (ma, mb) = (ea.call_secret(&id, e2ee::MEDIA_EXPORTER_LABEL).unwrap(), eb.call_secret(&id, e2ee::MEDIA_EXPORTER_LABEL).unwrap());
    assert_eq!(ma, mb, "one media key per epoch");
    assert_ne!(ma.0, sa.0);

    // Asking for an SFU (posture `sfu`) puts the call on the dev SFU, encrypted end to end.
    let engines = [Arc::new(AudioEngine::headless().unwrap()), Arc::new(AudioEngine::headless().unwrap())];
    let mut calls = Vec::new();
    for ((home, secret), engine) in [(&a_home, &ma), (&b_home, &mb)].into_iter().zip(&engines) {
        let joined = home.conversation_voice_join(&id, false, false, true, false).await.expect("join the call");
        let VoiceServer::Sfu { endpoint, token, e2ee: required, .. } = joined.voice_server else {
            eprintln!("the server has no SFU region; skipping the SFU part");
            return;
        };
        assert_eq!(required.as_deref(), Some("required"));
        let session = sfu::session(&http, &endpoint, &token).await.expect("SFU session");
        let provider = e2ee::key_provider(&secret.0, secret.1);
        calls.push(SfuCall::connect(session, engine.microphone(), Some(provider)).await.expect("join the room"));
    }

    let speaking = Arc::new(AtomicBool::new(true));
    let feeder = {
        let (speaking, engine) = (speaking.clone(), engines[0].clone());
        std::thread::spawn(move || {
            let tone: Vec<i16> = (0..480 * 100).map(|i| ((i as f32 * 2. * std::f32::consts::PI * 440. / 48_000.).sin() * 8000.) as i16).collect();
            let mut chunks = tone.chunks(480).cycle();
            while speaking.load(Ordering::Relaxed) {
                engine.feed(chunks.next().unwrap());
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    let track = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(event) = calls[1].1.recv().await {
            if let CallEvent::Track { slot: Slot::Mic, track: MediaStreamTrack::Audio(track), .. } = event {
                return track;
            }
        }
        panic!("the call ended");
    })
    .await
    .expect("b is sent a's microphone");
    engines[1].add_voice(&a_user, track);
    let tap = engines[1].tap();
    let heard = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match tap.try_recv() {
                Ok(frame) if is_tone(&frame, 440.) => return true,
                Ok(_) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(heard, "b hears a through the SFU, decrypted with the group's media key");

    speaking.store(false, Ordering::Relaxed);
    feeder.join().unwrap();
    for (call, _) in calls {
        call.close().await;
    }
    let _ = a_home.conversation_voice_leave(&id).await;
    let _ = b_home.conversation_voice_leave(&id).await;
    let _ = std::fs::remove_dir_all(&dir);
}
