//! End-to-end against a running dev homeserver (`scripts/dev-server.sh`).
//!
//!   RHEIN_LIVE=localhost:8448 cargo test -p rhein-net --test live -- --nocapture
//!
//! Skipped (passes trivially) when `RHEIN_LIVE` is not set.

use std::time::Duration;

use rhein_auth::{DeviceKey, SerenityKsf};
use rhein_net::gateway::{self, GatewayConfig};
use rhein_proto::PresenceState;
use rhein_net::{Api, GatewayEvent, auth, discover};
use rhein_proto::{Event, SendMessage, USER_INTENTS};


/// Registers, waiting out the server's registration rate limit (429) instead of failing.
async fn register_retrying(home: &Api, localpart: &str, ksf: &SerenityKsf) -> auth::SignedIn {
    for _ in 0..30 {
        match auth::register(home, localpart, "correct horse battery", &DeviceKey::generate(), "rhein test", ksf).await {
            Ok(signed) => return signed,
            Err(err) if err.downcast_ref::<rhein_net::ApiError>().and_then(|e| e.status()) == Some(429) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(err) => panic!("register: {err:#}"),
        }
    }
    panic!("register: still rate limited")
}

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
    let registered = register_retrying(&home, &localpart, &ksf).await;
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
        presence: online(),
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
    register_retrying(&home, &localpart, &ksf).await;

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
        presence: online(),
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
    register_retrying(&home, &format!("embed{}", std::process::id()), &ksf).await;
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

/// Pins, threads, search and the expressions list against a live server.
#[tokio::test]
async fn pins_threads_search() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let home = Api::new(http.clone(), &doc.api, None);
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    register_retrying(&home, &format!("p4{}", std::process::id()), &ksf).await;
    let api = Api::new(http, &doc.api, None);
    auth::guild_session(&home, &api, &doc.server_name).await.expect("session");
    let guild = api.create_guild("Phase four").await.expect("guild");
    let text = api.channels(&guild.id).await.unwrap().into_iter().find(|c| c.is_textual()).unwrap();
    let marker = format!("needle{}", std::process::id());
    let sent = api
        .send_message(&text.id, &SendMessage { content: format!("find the {marker} here"), ..Default::default() })
        .await
        .expect("send");

    api.set_pinned(&text.id, &sent.id, true).await.expect("pin");
    assert!(api.pins(&text.id).await.expect("pins").iter().any(|m| m.id == sent.id));
    api.set_pinned(&text.id, &sent.id, false).await.expect("unpin");
    assert!(api.pins(&text.id).await.expect("pins").is_empty());

    let thread = api.create_thread(&text.id, "A thread", Some(&sent.id)).await.expect("thread");
    api.send_message(&thread.id, &SendMessage { content: "inside the thread".into(), ..Default::default() })
        .await
        .expect("post in thread");
    assert!(api.threads(&text.id, false).await.expect("threads").iter().any(|t| t.id == thread.id));
    assert_eq!(api.messages(&thread.id, None, 50).await.expect("thread history").len(), 1);

    // The search index may lag the write slightly.
    let mut found = false;
    for _ in 0..20 {
        let results = api
            .search(&guild.id, &rhein_proto::SearchQuery { q: Some(marker.clone()), limit: 10, ..Default::default() })
            .await
            .expect("search");
        if results.results.iter().any(|m| m.id == sent.id) {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "search did not find the message");
    assert!(api.expressions(&guild.id).await.expect("expressions").is_empty());
    println!("ok: pins, thread {} and search work", thread.id);
}

/// Profile edits, the device list, channel management and leaving a guild.
#[tokio::test]
async fn settings_and_management() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    let owner_home = Api::new(http.clone(), &doc.api, None);
    register_retrying(&owner_home, &format!("own{}", std::process::id()), &ksf).await;

    owner_home
        .patch_profile(&serde_json::json!({ "display_name": "Owner", "bio": "hello", "pronouns": "they/them" }))
        .await
        .expect("patch profile");
    let me = owner_home.whoami().await.expect("whoami");
    let profile = me.profile.expect("profile");
    assert_eq!(profile.display_name.as_deref(), Some("Owner"));
    assert_eq!(profile.bio.as_deref(), Some("hello"));
    let devices = owner_home.devices().await.expect("devices");
    assert!(devices.iter().any(|d| d.current), "current device listed");

    let owner = Api::new(http.clone(), &doc.api, None);
    auth::guild_session(&owner_home, &owner, &doc.server_name).await.expect("session");
    let guild = owner.create_guild("Managed").await.expect("guild");
    let chan = owner.create_channel(&guild.id, "news", "text", None).await.expect("create channel");
    owner.patch_channel(&chan.id, &serde_json::json!({ "name": "updates", "topic": "what changed" })).await.expect("patch channel");
    let renamed = owner.channels(&guild.id).await.unwrap().into_iter().find(|c| c.id == chan.id).expect("channel");
    assert_eq!(renamed.name, "updates");
    owner.delete_channel(&chan.id).await.expect("delete channel");
    assert!(!owner.channels(&guild.id).await.unwrap().iter().any(|c| c.id == chan.id));
    owner.patch_guild(&guild.id, &serde_json::json!({ "name": "Renamed" })).await.expect("rename guild");

    // A second account joins through an invite and leaves again.
    let invite = owner.create_invite(&guild.id, None).await.expect("invite");
    let code = invite["code"].as_str().expect("code").to_owned();
    let guest_home = Api::new(http.clone(), &doc.api, None);
    register_retrying(&guest_home, &format!("gst{}", std::process::id()), &ksf).await;
    let guest = Api::new(http, &doc.api, None);
    auth::guild_session(&guest_home, &guest, &doc.server_name).await.expect("guest session");
    guest.join_invite(&code).await.expect("join");
    assert!(guest.guilds().await.unwrap().iter().any(|g| g.id == guild.id));
    guest.leave_guild(&guild.id).await.expect("leave");
    assert!(!guest.guilds().await.unwrap().iter().any(|g| g.id == guild.id));
    println!("ok: profile, devices, channels, guild rename and leave work");
}

/// Friend requests between two accounts, blocking, and the conversation list.
#[tokio::test]
async fn friends_and_conversations() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    let a = Api::new(http.clone(), &doc.api, None);
    let b = Api::new(http, &doc.api, None);
    let a_id = register_retrying(&a, &format!("fra{}", std::process::id()), &ksf).await.session.user_id;
    let b_id = register_retrying(&b, &format!("frb{}", std::process::id()), &ksf).await.session.user_id;

    a.request_friend(&b_id).await.expect("request");
    let state = |list: Vec<rhein_proto::Friend>, who: &str| list.into_iter().find(|f| f.user_id == who).map(|f| f.state);
    assert_eq!(state(a.friends().await.unwrap(), &b_id).as_deref(), Some("pending_out"));
    assert_eq!(state(b.friends().await.unwrap(), &a_id).as_deref(), Some("pending_in"));
    b.accept_friend(&a_id).await.expect("accept");
    assert_eq!(state(a.friends().await.unwrap(), &b_id).as_deref(), Some("accepted"));
    assert_eq!(state(b.friends().await.unwrap(), &a_id).as_deref(), Some("accepted"));

    a.set_blocked(&b_id, true).await.expect("block");
    assert_eq!(state(a.friends().await.unwrap(), &b_id).as_deref(), Some("blocked"));
    a.set_blocked(&b_id, false).await.expect("unblock");
    a.remove_friend(&b_id).await.ok();
    assert!(a.conversations().await.expect("conversations").is_empty());
    println!("ok: friend request, accept, block, unblock and conversation list work");
}

/// Rich presence end to end: an activity set on one account's connection reaches another member
/// of the same guild as a PRESENCE_UPDATE, in the shape the web client reads.
#[tokio::test]
async fn rich_presence_reaches_guild_members() {
    let Ok(server) = std::env::var("RHEIN_LIVE") else {
        eprintln!("RHEIN_LIVE not set; skipping");
        return;
    };
    let http = reqwest::Client::new();
    let doc = discover(&http, &server).await.expect("discovery");
    let ksf = SerenityKsf::argon2id(1, 8, 1);
    let gateway_of = |s: &rhein_proto::GuildSession| s.gateway.clone().or(doc.gateway.clone()).expect("gateway");

    let watcher_home = Api::new(http.clone(), &doc.api, None);
    register_retrying(&watcher_home, &format!("rpw{}", std::process::id()), &ksf).await;
    let watcher = Api::new(http.clone(), &doc.api, None);
    let watcher_session = auth::guild_session(&watcher_home, &watcher, &doc.server_name).await.expect("session");
    let guild = watcher.create_guild("Presence").await.expect("guild");
    let code = watcher.create_invite(&guild.id, None).await.expect("invite")["code"].as_str().unwrap().to_owned();

    let player_home = Api::new(http.clone(), &doc.api, None);
    let player_id = register_retrying(&player_home, &format!("rpp{}", std::process::id()), &ksf).await.session.user_id;
    let player = Api::new(http.clone(), &doc.api, None);
    let player_session = auth::guild_session(&player_home, &player, &doc.server_name).await.expect("session");
    player.join_invite(&code).await.expect("join");

    let (_watch, mut events) = gateway::connect(GatewayConfig {
        url: gateway_of(&watcher_session),
        token: watcher_session.session_token.clone(),
        intents: USER_INTENTS,
        presence: online(),
    });
    let game = rhein_proto::Activity::normalize(&serde_json::json!({
        "name": "Factorio",
        "details": "Building a rail network",
        "timestamps": { "start": 1_700_000_000_000u64 },
    }))
    .unwrap();
    let (play, mut play_events) = gateway::connect(GatewayConfig {
        url: gateway_of(&player_session),
        token: player_session.session_token.clone(),
        intents: USER_INTENTS,
        presence: online(),
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while !matches!(play_events.recv().await, Some(GatewayEvent::Ready(_))) {}
    })
    .await
    .expect("player READY");
    play.set_presence(PresenceState { activities: Some(vec![game.clone()]), ..online() });

    let seen = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(GatewayEvent::Dispatch(event)) = events.recv().await
                && let Event::PresenceUpdate { user_id, presence } = *event
                && user_id == player_id
            {
                let activities = rhein_proto::activity::activities_of(&presence);
                if !activities.is_empty() {
                    return activities;
                }
            }
        }
    })
    .await
    .expect("PRESENCE_UPDATE with the activity");
    assert_eq!(seen, vec![game]);
    println!("ok: rich presence reached the other member");
}

fn online() -> PresenceState {
    PresenceState { status: "online".into(), ..Default::default() }
}
