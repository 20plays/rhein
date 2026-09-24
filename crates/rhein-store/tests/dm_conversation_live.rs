//! Two Rhein accounts in an encrypted DM against a running dev server (`RHEIN_LIVE`), then a new
//! device of one of them letting itself in from its own sealed GroupInfo.

use std::time::Duration;

use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_mls::payload::Payload;
use rhein_mls::vault;
use rhein_net::{Api, auth, discover};
use rhein_store::conversations::Engine;
use rhein_store::dm::{Secrets, bring_up};

const PASSWORD: &str = "correct horse battery";

async fn register(home: &Api, localpart: &str) -> auth::SignedIn {
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    for _ in 0..20 {
        match auth::register(home, localpart, PASSWORD, &DeviceKey::generate(), "rhein dm test", &ksf).await {
            Ok(signed) => return signed,
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    panic!("could not register {localpart}");
}

/// A signed-in device: its API, its engine.
async fn device(home: &Api, signed: &auth::SignedIn) -> Engine {
    let (user, device) = (&signed.session.user_id, &signed.session.device_id);
    let status = bring_up(home, user, device, Some(vault::recovery_wrapper(&signed.export_key))).await.expect("keys");
    assert!(status.locked.is_none(), "{status:?}");
    let secrets = Secrets::load(user);
    let (private, public) = secrets.mls_signature.clone().unwrap();
    let mut engine = Engine::open(user, device, (&vault::unb64(&private).unwrap(), &vault::unb64(&public).unwrap()), secrets.keyring.clone()).unwrap();
    engine.replenish(home).await.expect("key packages");
    engine
}

fn texts(engine: &Engine, conversation: &str) -> Vec<String> {
    engine.conversation(conversation).map(|c| c.messages.iter().map(|m| m.edited.clone().unwrap_or_else(|| m.body.text.clone())).collect()).unwrap_or_default()
}

#[tokio::test]
async fn two_accounts_and_a_new_device() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rhein-dm-conv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("RHEIN_CONFIG_DIR", &dir) };

    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let (a_home, b_home) = (Api::new(http.clone(), &doc.api, None), Api::new(http.clone(), &doc.api, None));
    let tag = std::process::id();
    let a = register(&a_home, &format!("dma{tag}")).await;
    let b = register(&b_home, &format!("dmb{tag}")).await;
    let (a_user, b_user) = (a.session.user_id.clone(), b.session.user_id.clone());
    let mut ea = device(&a_home, &a).await;
    let mut eb = device(&b_home, &b).await;

    // A starts a DM with B and says hello.
    let convo = a_home.create_conversation("dm", std::slice::from_ref(&b_user), None).await.expect("conversation");
    let id = convo["id"].as_str().unwrap().to_owned();
    let refused = ea.establish(&a_home, &id, &[b_user.clone(), a_user.clone()], None).await.expect("establish");
    assert!(refused.refused.is_empty(), "{refused:?}");
    let hello = ea.send(&a_home, &id, Payload::Message("hello B".into())).await.expect("send");

    // B joins by the Welcome, reads it and answers; A reads the answer.
    let _ = b_home.accept_message_request(&id).await;
    let synced = eb.sync(&b_home, &id).await.expect("b sync");
    assert_eq!(synced.opened, 1);
    assert_eq!(texts(&eb, &id), ["hello B"]);
    eb.send(&b_home, &id, Payload::Message("hi A".into())).await.unwrap();
    eb.send(&b_home, &id, Payload::Reaction { target: hello.clone(), emoji: "👍".into(), add: true }).await.unwrap();
    ea.sync(&a_home, &id).await.expect("a sync");
    assert_eq!(texts(&ea, &id), ["hello B", "hi A"]);
    assert_eq!(ea.conversation(&id).unwrap().reactions[&hello]["👍"], std::slice::from_ref(&b_user));

    // Edits are the author's: A's edit lands, B trying to edit A's message does not.
    ea.send(&a_home, &id, Payload::Edit { target: hello.clone(), text: "hello, B".into() }).await.unwrap();
    eb.send(&b_home, &id, Payload::Edit { target: hello.clone(), text: "forged".into() }).await.unwrap();
    eb.sync(&b_home, &id).await.unwrap();
    ea.sync(&a_home, &id).await.unwrap();
    assert_eq!(texts(&eb, &id)[0], "hello, B");
    assert_eq!(texts(&ea, &id)[0], "hello, B");

    // An encrypted attachment: sealed, uploaded, opened on the other side.
    ea.send_message(&a_home, &id, "a file", vec![("note.txt".into(), "text/plain".into(), b"secret bytes".to_vec())], None).await.unwrap();
    eb.sync(&b_home, &id).await.unwrap();
    let last = eb.conversation(&id).unwrap().messages.last().unwrap().clone();
    assert_eq!(last.body.text, "a file");
    assert_eq!(last.body.attachments[0]["name"], "note.txt");
    assert_eq!(Engine::fetch_attachment(&b_home, &last.body.attachments[0]).await.unwrap(), b"secret bytes");

    // A signs in on a new machine: nothing kept locally, a new device id. It lets itself in
    // from the GroupInfo its first device sealed, and reads what B says from then on.
    drop(ea);
    Engine::remove(&a_user);
    Secrets::remove(&a_user);
    let a2_home = Api::new(http.clone(), &doc.api, None);
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    let a2 = auth::login(&a2_home, &a_user, PASSWORD, &DeviceKey::generate(), "rhein dm test 2", &ksf).await.expect("second sign-in");
    assert_ne!(a2.session.device_id, a.session.device_id);
    let mut ea2 = device(&a2_home, &a2).await;
    ea2.sync(&a2_home, &id).await.expect("self-join");
    assert!(ea2.has_group(&id), "the new device let itself in");
    // History from before this device joined comes back from the account's sealed mirror, with
    // A's edit applied and B's reaction.
    assert_eq!(texts(&ea2, &id), ["hello, B", "hi A", "a file"]);
    assert_eq!(ea2.conversation(&id).unwrap().reactions[&hello]["👍"], std::slice::from_ref(&b_user));
    eb.sync(&b_home, &id).await.expect("b sees the external commit");
    eb.send(&b_home, &id, Payload::Message("welcome back".into())).await.unwrap();
    ea2.sync(&a2_home, &id).await.unwrap();
    assert_eq!(texts(&ea2, &id).last().map(String::as_str), Some("welcome back"));
    ea2.send(&a2_home, &id, Payload::Message("from the new device".into())).await.unwrap();
    eb.sync(&b_home, &id).await.unwrap();
    assert_eq!(texts(&eb, &id).last().map(String::as_str), Some("from the new device"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// A group chat across two federated servers (`RHEIN_LIVE` and `RHEIN_LIVE_B`): the sequencer
/// lives on the creator's home server, the other members reach it through theirs.
#[tokio::test]
async fn group_across_servers() {
    let (Ok(server), Ok(other)) = (std::env::var("RHEIN_LIVE"), std::env::var("RHEIN_LIVE_B")) else {
        eprintln!("RHEIN_LIVE / RHEIN_LIVE_B not set; skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rhein-dm-fed-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: tests here set the same variable only when it is unset, to a per-process dir.
    if std::env::var("RHEIN_CONFIG_DIR").is_err() {
        unsafe { std::env::set_var("RHEIN_CONFIG_DIR", &dir) };
    }
    let http = reqwest::Client::new();
    let (doc_a, doc_b) = (discover(&http, &server).await.unwrap(), discover(&http, &other).await.unwrap());
    let tag = std::process::id();
    let (home_a, home_b, home_c) = (
        Api::new(http.clone(), &doc_a.api, None),
        Api::new(http.clone(), &doc_b.api, None),
        Api::new(http.clone(), &doc_a.api, None),
    );
    let a = register(&home_a, &format!("fa{tag}")).await;
    let b = register(&home_b, &format!("fb{tag}")).await;
    let c = register(&home_c, &format!("fc{tag}")).await;
    let (mut ea, mut eb, mut ec) = (device(&home_a, &a).await, device(&home_b, &b).await, device(&home_c, &c).await);
    let (ua, ub, uc) = (a.session.user_id.clone(), b.session.user_id.clone(), c.session.user_id.clone());

    let convo = home_a.create_conversation("gc", &[ub.clone(), uc.clone()], None).await.expect("group");
    let id = convo["id"].as_str().unwrap().to_owned();
    let claimed = ea.establish(&home_a, &id, &[ua.clone(), ub.clone(), uc.clone()], None).await.unwrap();
    assert!(claimed.refused.is_empty() && claimed.missing.is_empty(), "{claimed:?}");
    ea.send(&home_a, &id, Payload::Message("hello, both servers".into())).await.unwrap();
    for (engine, home) in [(&mut eb, &home_b), (&mut ec, &home_c)] {
        let _ = home.accept_message_request(&id).await;
        // Frames reach another server's members through federation, which takes a moment.
        for _ in 0..20 {
            engine.sync(home, &id).await.unwrap();
            if !texts(engine, &id).is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert_eq!(texts(engine, &id), ["hello, both servers"], "{:?}", engine.conversation(&id));
    }
    eb.send(&home_b, &id, Payload::Message("from the other server".into())).await.unwrap();
    for (engine, home) in [(&mut ea, &home_a), (&mut ec, &home_c)] {
        for _ in 0..20 {
            engine.sync(home, &id).await.unwrap();
            if texts(engine, &id).len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert_eq!(texts(engine, &id), ["hello, both servers", "from the other server"]);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
