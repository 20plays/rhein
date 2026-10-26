//! Seeds a throwaway account for timing DMs in the app: registers it and three partners on a
//! server, starts a DM with each (of `RHEIN_SEED_MESSAGES`, default 300, 60 and 10 messages,
//! both sides speaking), and saves the account as a sign-in would, in `RHEIN_CONFIG_DIR`.
//!
//!   RHEIN_CONFIG_DIR=/tmp/x cargo run -p rhein-store --example dm_bench_seed -- staging.telium.chat
//!
//! Then `RHEIN_CONFIG_DIR=/tmp/x RHEIN_DEV_OPEN=cycle-dm rhein` (a debug build) opens them in turn.

use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_mls::payload::Payload;
use rhein_mls::vault;
use rhein_net::{Api, auth, discover};
use rhein_store::conversations::Engine;
use rhein_store::dm::{Secrets, bring_up};
use rhein_store::persist;

const PASSWORD: &str = "rhein bench password";

async fn register(home: &Api, localpart: &str, device: &DeviceKey) -> auth::SignedIn {
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    auth::register(home, localpart, PASSWORD, device, "rhein bench", &ksf).await.expect("register")
}

async fn engine(home: &Api, signed: &auth::SignedIn) -> Engine {
    let (user, device) = (&signed.session.user_id, &signed.session.device_id);
    bring_up(home, user, device, Some(vault::recovery_wrapper(&signed.export_key))).await.expect("keys");
    let secrets = Secrets::load(user);
    let (private, public) = secrets.mls_signature.clone().unwrap();
    let mut engine = Engine::open(user, device, (&vault::unb64(&private).unwrap(), &vault::unb64(&public).unwrap()), secrets.keyring.clone()).unwrap();
    engine.replenish(home).await.expect("key packages");
    engine
}

#[tokio::main]
async fn main() {
    let server = std::env::args().nth(1).expect("the server, e.g. staging.telium.chat");
    assert!(std::env::var("RHEIN_CONFIG_DIR").is_ok(), "set RHEIN_CONFIG_DIR to where the app should find the account");
    let counts: Vec<usize> = std::env::var("RHEIN_SEED_MESSAGES")
        .ok()
        .map(|s| s.split(',').filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_else(|| vec![300, 60, 10]);
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let tag = std::process::id();

    let home = Api::new(http.clone(), &doc.api, None);
    let key = DeviceKey::generate();
    let me = register(&home, &format!("rheinbench{tag}"), &key).await;
    let mut mine = engine(&home, &me).await;
    for (n, count) in counts.iter().enumerate() {
        let their_home = Api::new(http.clone(), &doc.api, None);
        let them = register(&their_home, &format!("rheinbench{tag}p{n}"), &DeviceKey::generate()).await;
        let mut theirs = engine(&their_home, &them).await;
        let convo = home.create_conversation("dm", std::slice::from_ref(&them.session.user_id), None).await.expect("conversation");
        let id = convo["id"].as_str().unwrap().to_owned();
        mine.establish(&home, &id, &[them.session.user_id.clone(), me.session.user_id.clone()], None).await.expect("establish");
        let _ = their_home.accept_message_request(&id).await;
        theirs.sync(&their_home, &id).await.expect("their sync");
        for i in 0..*count {
            // Mostly us, every third message them, some of them long.
            let text = if i % 7 == 0 { format!("message {i}: {}", "a longer line of text, ".repeat(8)) } else { format!("message {i}") };
            if i % 3 == 2 {
                theirs.send(&their_home, &id, Payload::Message(text)).await.expect("their send");
            } else {
                mine.sync(&home, &id).await.expect("sync");
                mine.send(&home, &id, Payload::Message(text)).await.expect("send");
            }
        }
        mine.sync(&home, &id).await.expect("sync");
        eprintln!("{id}: {count} messages with {}", them.session.user_id);
    }
    persist::store(&persist::Saved {
        home: server.clone(),
        user_id: me.session.user_id.clone(),
        device_id: me.session.device_id.clone(),
        access_token: me.session.access_token.clone(),
        expires_at: me.session.expires_at,
        device_seed: rhein_auth::b64u(&key.seed()),
        ..Default::default()
    })
    .expect("save the session");
    persist::set_active(&me.session.user_id).expect("make it the active account");
    eprintln!("seeded {} in {}", me.session.user_id, std::env::var("RHEIN_CONFIG_DIR").unwrap());
}
