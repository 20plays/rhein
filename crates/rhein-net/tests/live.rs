//! End-to-end against a running dev homeserver (`scripts/dev-server.sh`).
//!
//!   RHEIN_LIVE=localhost:8448 cargo test -p rhein-net --test live -- --nocapture
//!
//! Skipped (passes trivially) when `RHEIN_LIVE` is not set.

use std::time::Duration;

use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_net::gateway::{self, GatewayConfig};
use rhein_net::{Api, GatewayEvent, auth, discover};
use rhein_proto::{Event, SendMessage, USER_INTENTS};

#[tokio::test]
async fn register_login_session_gateway_message() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    println!("discovered {} at {}", doc.server_name, doc.api);

    let home = Api::new(http.clone(), &doc.api, None);
    let ksf = SerenityKsf::memory_constrained();
    let localpart = format!("rhein{}", std::process::id());
    let device = DeviceKey::generate();
    let registered = auth::register(&home, &localpart, "correct horse battery", &device, "rhein test", &ksf)
        .await
        .expect("register");
    println!("registered {}", registered.session.user_id);

    // A second sign-in from a fresh device must yield the same export key.
    let home2 = Api::new(http.clone(), &doc.api, None);
    let again = auth::login(&home2, &localpart, "correct horse battery", &DeviceKey::generate(), "rhein test 2", &ksf)
        .await
        .expect("login");
    assert_eq!(again.export_key, registered.export_key);
    assert!(
        auth::login(&Api::new(http.clone(), &doc.api, None), &localpart, "wrong password!", &DeviceKey::generate(), "x", &ksf)
            .await
            .is_err()
    );

    let me = home.whoami().await.expect("whoami");
    assert_eq!(me.user_id, registered.session.user_id);

    let guild_api = Api::new(http.clone(), &doc.api, None);
    let session = auth::guild_session(&home, &guild_api, &doc.server_name).await.expect("guild session");
    let guild = guild_api.create_guild("Rhein test").await.expect("create guild");
    let channels = guild_api.channels(&guild.id).await.expect("channels");
    let text = channels.iter().find(|c| c.is_textual()).expect("a text channel");

    let url = session.gateway.clone().or(doc.gateway.clone()).expect("gateway url");
    let (_handle, mut events) = gateway::connect(GatewayConfig {
        url,
        token: session.session_token.clone(),
        intents: USER_INTENTS,
        status: "online".into(),
    });
    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(GatewayEvent::Ready(r)) = events.recv().await {
                return r;
            }
        }
    })
    .await
    .expect("READY");
    assert!(ready.guilds.iter().any(|g| g.id == guild.id));

    let sent = guild_api
        .send_message(&text.id, &SendMessage { content: "hello from Rhein".into(), ..Default::default() })
        .await
        .expect("send");
    let echoed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(GatewayEvent::Dispatch(event)) = events.recv().await
                && let Event::MessageCreate(m) = *event
            {
                return m;
            }
        }
    })
    .await
    .expect("MESSAGE_CREATE");
    assert_eq!(echoed.id, sent.id);

    let history = guild_api.messages(&text.id, None, 50).await.expect("history");
    assert!(history.iter().any(|m| m.id == sent.id));
    guild_api.react(&sent.id, "👍", true).await.expect("react");
    guild_api.edit_message(&text.id, &sent.id, "edited").await.expect("edit");
    guild_api.delete_message(&text.id, &sent.id).await.expect("delete");
    println!("ok: {} messages in history", history.len());
}

/// Logs into an account registered by `@serenity-kit/opaque` (the web client's library) and
/// checks the export key matches. Set `RHEIN_WEB_ACCOUNT=localpart:password:exportKey`.
#[tokio::test]
async fn login_account_made_by_web_client() {
    let (Ok(server), Ok(account)) = (std::env::var("RHEIN_LIVE"), std::env::var("RHEIN_WEB_ACCOUNT")) else {
        eprintln!("RHEIN_LIVE / RHEIN_WEB_ACCOUNT not set; skipping");
        return;
    };
    let mut parts = account.splitn(3, ':');
    let (user, password, export) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let home = Api::new(http, &doc.api, None);
    let signed = auth::login(&home, user, password, &DeviceKey::generate(), "rhein interop", &SerenityKsf::memory_constrained())
        .await
        .expect("login with serenity-made account");
    assert_eq!(rhein_auth::b64u(&signed.export_key), export);
}

/// Federation: an account whose home is `RHEIN_LIVE` uses a guild on `RHEIN_LIVE_B` through an
/// identity assertion, without an account there. Run both `scripts/dev-server.sh a` and `b`.
#[tokio::test]
async fn guild_on_foreign_server() {
    let (Ok(home_name), Ok(other)) = (std::env::var("RHEIN_LIVE"), std::env::var("RHEIN_LIVE_B")) else {
        eprintln!("RHEIN_LIVE / RHEIN_LIVE_B not set; skipping");
        return;
    };
    let http = reqwest::Client::new();
    let home_doc = discover(&http, &home_name).await.expect("home discovery");
    let home = Api::new(http.clone(), &home_doc.api, None);
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    let localpart = format!("fed{}", std::process::id());
    auth::register(&home, &localpart, "correct horse battery", &DeviceKey::generate(), "rhein fed", &ksf)
        .await
        .expect("register at home");

    let doc = discover(&http, &other).await.expect("foreign discovery");
    assert_ne!(doc.server_name, home_doc.server_name);
    let foreign = Api::new(http.clone(), &doc.api, None);
    let session = auth::guild_session(&home, &foreign, &doc.server_name).await.expect("foreign session");
    let guild = foreign.create_guild("Federated").await.expect("guild on foreign server");
    let text = foreign.channels(&guild.id).await.unwrap().into_iter().find(|c| c.is_textual()).unwrap();

    let (_handle, mut events) = gateway::connect(GatewayConfig {
        url: session.gateway.clone().or(doc.gateway.clone()).unwrap(),
        token: session.session_token,
        intents: USER_INTENTS,
        status: "online".into(),
    });
    let ready = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(GatewayEvent::Ready(r)) = events.recv().await {
                return r;
            }
        }
    })
    .await
    .expect("READY from foreign server");
    assert!(ready.guilds.iter().any(|g| g.id == guild.id));
    let sent = foreign
        .send_message(&text.id, &SendMessage { content: "across server boundaries".into(), ..Default::default() })
        .await
        .expect("send on foreign server");
    assert!(sent.author.ends_with(&format!(":{}", home_doc.server_name)), "author {} keeps its home", sent.author);
    println!("ok: {} posted in {} on {}", sent.author, guild.id, doc.server_name);
}

/// The guild server unfurls links for its members (`GET /embed?url=`). Needs outbound internet on
/// the dev server; a server that refuses previews (403) passes too.
#[tokio::test]
async fn embed_preview() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let home = Api::new(http.clone(), &doc.api, None);
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    auth::register(&home, &format!("embed{}", std::process::id()), "correct horse battery", &DeviceKey::generate(), "rhein", &ksf)
        .await
        .expect("register");
    let guild = Api::new(http, &doc.api, None);
    auth::guild_session(&home, &guild, &doc.server_name).await.expect("session");
    match guild.embed("https://example.com").await {
        Ok(card) => {
            assert!(card.get("status").is_some(), "unexpected embed answer: {card}");
            println!("ok: embed status {}", card["status"]);
        }
        Err(err) if err.status() == Some(403) => println!("ok: server refuses previews"),
        Err(err) => panic!("embed: {err}"),
    }
}
