//! A Discord RPC client, as a game would run it, talking to Rhein's IPC socket.

use std::time::Duration;

use rhein_rpc::ipc::{OP_FRAME, OP_HANDSHAKE, OP_PING, OP_PONG, read_frame, write_frame};
use rhein_rpc::{Config, start};
use serde_json::json;
use tokio::net::UnixStream;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rhein-rpc-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn a_game_sets_and_clears_its_activity() {
    let dir = scratch("ipc");
    let config = Config {
        ipc_dir: Some(dir.clone()),
        websocket: false,
        detect_games: false,
        media: false,
        ..Config::new(dir.clone())
    };
    let handle = start(config);
    let mut activities = handle.activities.clone();
    let mut status = handle.status.clone();
    let socket = tokio::time::timeout(Duration::from_secs(5), status.wait_for(|s| s.socket.is_some())).await.unwrap().unwrap().socket.clone().unwrap();
    assert_eq!(socket, dir.join("discord-ipc-0"));

    let mut stream = UnixStream::connect(&socket).await.unwrap();
    // Not a Discord snowflake, so nothing is looked up online during the test.
    write_frame(&mut stream, OP_HANDSHAKE, &json!({ "v": 1, "client_id": "test-app" })).await.unwrap();
    let (op, ready) = read_frame(&mut stream).await.unwrap();
    assert_eq!(op, OP_FRAME);
    assert_eq!(ready["evt"], "READY");

    write_frame(&mut stream, OP_PING, &json!({ "n": 1 })).await.unwrap();
    assert_eq!(read_frame(&mut stream).await.unwrap(), (OP_PONG, json!({ "n": 1 })));

    let set = json!({
        "cmd": "SET_ACTIVITY",
        "nonce": "abc",
        "args": {
            "pid": 1,
            "activity": {
                "name": "Test Game",
                "details": "Level 2",
                "timestamps": { "start": 1_700_000_000 },
                "buttons": [{ "label": "Site", "url": "https://example.org" }],
                "secrets": { "join": "never shared" },
            },
        },
    });
    write_frame(&mut stream, OP_FRAME, &set).await.unwrap();
    let (_, reply) = read_frame(&mut stream).await.unwrap();
    assert_eq!(reply["nonce"], "abc");
    assert_eq!(reply["data"]["application_id"], "test-app");

    let shared = tokio::time::timeout(Duration::from_secs(5), activities.wait_for(|a| !a.is_empty())).await.unwrap().unwrap().clone();
    assert_eq!(shared.len(), 1);
    assert_eq!(shared[0].name, "Test Game");
    assert_eq!(shared[0].details.as_deref(), Some("Level 2"));
    assert_eq!(shared[0].timestamps.as_ref().and_then(|t| t.start), Some(1_700_000_000_000));
    assert_eq!(shared[0].buttons.len(), 1);
    assert!(status.borrow().clients.contains(&"test-app".to_owned()));

    // The game quits: its activity goes with it.
    drop(stream);
    tokio::time::timeout(Duration::from_secs(5), activities.wait_for(|a| a.is_empty())).await.unwrap().unwrap();

    drop(handle);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_must_handshake_first() {
    let dir = scratch("handshake");
    let config = Config { ipc_dir: Some(dir.clone()), websocket: false, detect_games: false, media: false, ..Config::new(dir.clone()) };
    let handle = start(config);
    let mut status = handle.status.clone();
    let socket = tokio::time::timeout(Duration::from_secs(5), status.wait_for(|s| s.socket.is_some())).await.unwrap().unwrap().socket.clone().unwrap();
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    write_frame(&mut stream, OP_FRAME, &json!({ "cmd": "SET_ACTIVITY" })).await.unwrap();
    let (op, close) = read_frame(&mut stream).await.unwrap();
    assert_eq!(op, rhein_rpc::ipc::OP_CLOSE);
    assert_eq!(close["code"], 1003);
    drop(handle);
    let _ = std::fs::remove_dir_all(&dir);
}
