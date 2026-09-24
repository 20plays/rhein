//! Rhein and the real Telium web client (its own `mls.js`, `vault.js`, `verify.js`, run by
//! `scripts/web-interop/web-client.mjs` under bun) in encrypted DMs, against a running dev server.
//! Needs `RHEIN_LIVE`, bun, and `bun install` in `.dev/telium/web`; skipped otherwise.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_mls::payload::Payload;
use rhein_mls::vault;
use rhein_net::{Api, auth, discover};
use rhein_store::conversations::Engine;
use rhein_store::dm::{Secrets, bring_up};
use serde_json::{Value, json};

const PASSWORD: &str = "correct horse battery";

struct WebClient {
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl WebClient {
    fn start() -> Option<WebClient> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !root.join(".dev/telium/web/node_modules/ts-mls").exists() {
            eprintln!("web client dependencies not installed; skipping");
            return None;
        }
        let bun = std::env::var("BUN").unwrap_or_else(|_| format!("{}/.bun/bin/bun", std::env::var("HOME").unwrap_or_default()));
        let mut child = Command::new(bun)
            .arg(root.join("scripts/web-interop/web-client.mjs"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .ok()?;
        let input = child.stdin.take()?;
        let output = BufReader::new(child.stdout.take()?);
        Some(WebClient { _child: child, input, output })
    }

    fn call(&mut self, request: Value) -> Value {
        writeln!(self.input, "{request}").unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        let answer: Value = serde_json::from_str(&line).unwrap_or_else(|_| panic!("bad answer: {line}"));
        assert_eq!(answer["ok"], true, "web {} failed: {}", request["op"], answer["error"].as_str().unwrap_or_default());
        answer
    }

    fn texts(&mut self, id: &str) -> Vec<String> {
        let synced = self.call(json!({ "op": "sync", "id": id }));
        assert!(synced["error"].is_null(), "web sync: {}", synced["error"]);
        synced["texts"].as_array().unwrap().iter().map(|t| t["text"].as_str().unwrap_or_default().to_owned()).collect()
    }
}

/// With the web client's own key-stretching parameters, so it can sign in to the account too.
async fn register(home: &Api, localpart: &str) -> auth::SignedIn {
    let ksf = SerenityKsf::memory_constrained();
    for _ in 0..20 {
        match auth::register(home, localpart, PASSWORD, &DeviceKey::generate(), "rhein interop", &ksf).await {
            Ok(signed) => return signed,
            Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    panic!("could not register {localpart}");
}

fn rhein_texts(engine: &Engine, id: &str) -> Vec<String> {
    engine.conversation(id).map(|c| c.messages.iter().map(|m| m.edited.clone().unwrap_or_else(|| m.body.text.clone())).collect()).unwrap_or_default()
}

#[tokio::test]
async fn rhein_and_the_web_client() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let Some(mut web) = WebClient::start() else { return };
    let dir = std::env::temp_dir().join(format!("rhein-web-interop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: the only test in this binary.
    unsafe { std::env::set_var("RHEIN_CONFIG_DIR", &dir) };

    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let tag = std::process::id();

    // The web user signs in with the web client: it creates its vault, its cross-signing
    // identity (anchored in continuity) and vouches for its device.
    let web_home = Api::new(http.clone(), &doc.api, None);
    let w = register(&web_home, &format!("web{tag}")).await;
    let signed_in = web.call(json!({ "op": "signin", "base": doc.api, "user": w.session.user_id, "password": PASSWORD }));
    assert!(signed_in["identity_key"].is_string(), "the web client set up an identity: {signed_in}");
    let web_user = w.session.user_id.clone();

    // The Rhein user.
    let r_home = Api::new(http.clone(), &doc.api, None);
    let r = register(&r_home, &format!("rhn{tag}")).await;
    let rhein_user = r.session.user_id.clone();
    bring_up(&r_home, &rhein_user, &r.session.device_id, Some(vault::recovery_wrapper(&r.export_key))).await.unwrap();
    let secrets = Secrets::load(&rhein_user);
    let (private, public) = secrets.mls_signature.clone().unwrap();
    let mut rhein = Engine::open(&rhein_user, &r.session.device_id, (&vault::unb64(&private).unwrap(), &vault::unb64(&public).unwrap()), secrets.keyring.clone()).unwrap();
    rhein.replenish(&r_home).await.unwrap();

    // 1. The web client starts a DM with Rhein.
    let convo = web.call(json!({ "op": "create_conversation", "members": [rhein_user] }));
    assert!(convo["rejected"].as_array().unwrap().is_empty(), "web refused Rhein's device: {convo}");
    let id = convo["id"].as_str().unwrap().to_owned();
    web.call(json!({ "op": "send", "id": id, "text": "hello from the web client" }));
    let _ = r_home.accept_message_request(&id).await;
    rhein.sync(&r_home, &id).await.unwrap();
    assert_eq!(rhein_texts(&rhein, &id), ["hello from the web client"]);
    let first = rhein.conversation(&id).unwrap().messages[0].id.clone();
    rhein.send(&r_home, &id, Payload::Message("hello from Rhein".into())).await.unwrap();
    rhein.send(&r_home, &id, Payload::Reaction { target: first.clone(), emoji: "🌊".into(), add: true }).await.unwrap();
    assert_eq!(web.texts(&id), ["hello from the web client", "hello from Rhein"]);
    let synced = web.call(json!({ "op": "sync", "id": id }));
    assert_eq!(synced["reactions"][&first][0]["emoji"], "🌊", "{synced}");

    // 2. Rhein starts a group chat with the web user (a second one-to-one would be the same
    //    conversation). The web user has a cross-signing identity, so Rhein only takes the web
    //    device's key package because its identity vouches for it.
    let convo = r_home.create_conversation("gc", std::slice::from_ref(&web_user), None).await.unwrap();
    let id2 = convo["id"].as_str().unwrap().to_owned();
    assert_ne!(id2, id);
    let refused = rhein.establish(&r_home, &id2, &[web_user.clone(), rhein_user.clone()], None).await.unwrap();
    assert!(refused.refused.is_empty(), "{refused:?}");
    rhein.send(&r_home, &id2, Payload::Message("Rhein started this one".into())).await.unwrap();
    web.call(json!({ "op": "accept", "id": id2 }));
    assert_eq!(web.texts(&id2), ["Rhein started this one"]);
    web.call(json!({ "op": "send", "id": id2, "text": "and the web client answers" }));
    rhein.sync(&r_home, &id2).await.unwrap();
    assert_eq!(rhein_texts(&rhein, &id2), ["Rhein started this one", "and the web client answers"]);

    // 3. The same account in both clients. The Rhein user signs in with the web client too; it
    //    opens the vault Rhein created, sets up a cross-signing identity for the account and
    //    vouches for its own device. Rhein's next start vouches for Rhein's device with it...
    let Some(mut same) = WebClient::start() else { return };
    let signed = same.call(json!({ "op": "signin", "base": doc.api, "user": rhein_user, "password": PASSWORD }));
    let identity = signed["identity_key"].as_str().expect("the web client made an identity for the account").to_owned();
    let status = bring_up(&r_home, &rhein_user, &r.session.device_id, None).await.unwrap();
    assert!(status.cross_signed, "{status:?}");
    assert_eq!(status.identity_key.as_deref(), Some(identity.as_str()));

    // ...so a web peer now takes Rhein's device only because the account vouches for it, and
    // all three devices (web user, the account on the web, the account in Rhein) share a group.
    rhein.replenish(&r_home).await.unwrap();
    let group = web.call(json!({ "op": "create_conversation", "kind": "gc", "members": [rhein_user] }));
    assert!(group["rejected"].as_array().unwrap().is_empty(), "the web client refused a device: {group}");
    let id3 = group["id"].as_str().unwrap().to_owned();
    web.call(json!({ "op": "send", "id": id3, "text": "all of us" }));
    rhein.sync(&r_home, &id3).await.unwrap();
    assert_eq!(rhein_texts(&rhein, &id3), ["all of us"]);
    same.call(json!({ "op": "accept", "id": id3 }));
    assert_eq!(same.texts(&id3), ["all of us"]);
    rhein.send(&r_home, &id3, Payload::Message("Rhein, same account".into())).await.unwrap();
    assert_eq!(web.texts(&id3), ["all of us", "Rhein, same account"]);
    // The account's web device sees what its Rhein device sent (another device of its own).
    assert_eq!(same.texts(&id3), ["all of us", "Rhein, same account"]);

    let _ = std::fs::remove_dir_all(&dir);
}
