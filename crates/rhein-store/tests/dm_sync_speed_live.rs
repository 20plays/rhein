//! How long opening a DM again takes (a sync with nothing new), against a running server
//! (`RHEIN_LIVE`): with the keyring and without it (locked), on a conversation with a long
//! history. Prints the times; fails when a sync with nothing new takes as long as fetching it all.

use std::time::{Duration, Instant};

use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_mls::payload::Payload;
use rhein_mls::vault;
use rhein_net::{Api, auth, discover};
use rhein_store::conversations::Engine;
use rhein_store::dm::{Secrets, bring_up};

const PASSWORD: &str = "correct horse battery";

/// Messages in the conversation; `RHEIN_LIVE_MESSAGES` for more.
fn messages() -> usize {
    std::env::var("RHEIN_LIVE_MESSAGES").ok().and_then(|n| n.parse().ok()).unwrap_or(200)
}

async fn register(home: &Api, localpart: &str) -> auth::SignedIn {
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    for _ in 0..20 {
        match auth::register(home, localpart, PASSWORD, &DeviceKey::generate(), "rhein sync speed test", &ksf).await {
            Ok(signed) => return signed,
            Err(err) => {
                eprintln!("register {localpart}: {err}");
                tokio::time::sleep(Duration::from_millis(500)).await
            }
        }
    }
    panic!("could not register {localpart}");
}

/// A signed-in device's engine, with its keyring or (`locked`) without.
async fn device(home: &Api, signed: &auth::SignedIn, locked: bool) -> Engine {
    let (user, device) = (&signed.session.user_id, &signed.session.device_id);
    let status = bring_up(home, user, device, Some(vault::recovery_wrapper(&signed.export_key))).await.expect("keys");
    assert!(status.locked.is_none(), "{status:?}");
    let secrets = Secrets::load(user);
    let (private, public) = secrets.mls_signature.clone().unwrap();
    let keyring = if locked { None } else { secrets.keyring.clone() };
    let mut engine = Engine::open(user, device, (&vault::unb64(&private).unwrap(), &vault::unb64(&public).unwrap()), keyring).unwrap();
    engine.replenish(home).await.expect("key packages");
    engine
}

async fn timed_syncs(engine: &mut Engine, home: &Api, id: &str, what: &str) -> Vec<Duration> {
    let mut times = Vec::new();
    for n in 0..4 {
        let since = Instant::now();
        engine.sync(home, id).await.expect("sync");
        let took = since.elapsed();
        eprintln!("{what}: sync {n} took {took:?}");
        times.push(took);
    }
    times
}

#[tokio::test]
async fn opening_a_dm_again_is_quick() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rhein-dm-speed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("RHEIN_CONFIG_DIR", &dir) };

    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let (a_home, b_home) = (Api::new(http.clone(), &doc.api, None), Api::new(http.clone(), &doc.api, None));
    let tag = std::process::id();
    let a = register(&a_home, &format!("rheinspeeda{tag}")).await;
    let b = register(&b_home, &format!("rheinspeedb{tag}")).await;
    let (a_user, b_user) = (a.session.user_id.clone(), b.session.user_id.clone());
    let mut ea = device(&a_home, &a, false).await;
    // B's device is there first, so the Welcome lets it in.
    let mut eb = device(&b_home, &b, false).await;

    let convo = a_home.create_conversation("dm", std::slice::from_ref(&b_user), None).await.expect("conversation");
    let id = convo["id"].as_str().unwrap().to_owned();
    ea.establish(&a_home, &id, &[b_user.clone(), a_user.clone()], None).await.expect("establish");
    let since = Instant::now();
    for n in 0..messages() {
        ea.send(&a_home, &id, Payload::Message(format!("message {n}"))).await.expect("send");
    }
    eprintln!("sent {} messages in {:?}", messages(), since.elapsed());
    let _ = b_home.accept_message_request(&id).await;

    // B unlocked: the first sync reads it all, the next ones have nothing new.
    let unlocked = timed_syncs(&mut eb, &b_home, &id, "unlocked").await;
    eprintln!("B holds {:?} messages", eb.conversation(&id).map(|c| c.messages.len()));
    drop(eb);

    // B again, the keyring locked (as when the password was never given on this device).
    let mut eb = device(&b_home, &b, true).await;
    eprintln!("B (locked) holds {:?} messages", eb.conversation(&id).map(|c| c.messages.len()));
    let locked = timed_syncs(&mut eb, &b_home, &id, "locked").await;

    let _ = std::fs::remove_dir_all(&dir);
    for (what, times) in [("unlocked", &unlocked), ("locked", &locked)] {
        let again = times[1..].iter().max().unwrap();
        assert!(*again < times[0] / 2 || *again < Duration::from_millis(400), "{what}: a sync with nothing new took {again:?}, the first {:?}", times[0]);
    }
}
