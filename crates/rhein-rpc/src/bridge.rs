//! Activities another RPC server already holds. Discord mods such as Equibop and Vesktop run
//! arRPC, which takes `discord-ipc-0` before Rhein can; apps built on Discord's RPC libraries
//! only ever talk to that first socket, so their activity (Sonora's song, say, under the name
//! chosen in its settings) never reaches Rhein's own server. arRPC hands every activity on to its
//! clients over a WebSocket on 127.0.0.1:1337; Rhein listens there as one more such client, and
//! never sends it anything.
//!
//! A message is `{"activity": {...} | null, "pid": n, "socketId": "3"}`: one per RPC connection,
//! the current ones again on every (re)connect.

use std::collections::HashSet;
use std::time::Duration;

use futures::StreamExt as _;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use crate::{PRIORITY_RPC, Report, Reports, SourceEvent};

/// arRPC's bridge; loopback only.
const BRIDGE: &str = "ws://127.0.0.1:1337";
/// How often to look for the bridge again while it is not there.
const RETRY: Duration = Duration::from_secs(15);

/// Listens to the bridge for as long as the service runs, reconnecting when it goes away.
pub async fn run(reports: Reports) {
    loop {
        match tokio_tungstenite::connect_async(BRIDGE).await {
            Ok((mut socket, _)) => {
                log::info!("rpc: reading activities from the arRPC bridge at {BRIDGE}");
                let _ = reports.send(SourceEvent::Bridge(true));
                let mut keys = HashSet::new();
                while let Some(Ok(message)) = socket.next().await {
                    let Message::Text(text) = message else { continue };
                    let Some((key, activity)) = serde_json::from_str::<Value>(&text).ok().as_ref().and_then(parse) else {
                        continue;
                    };
                    keys.insert(key.clone());
                    let _ = reports.send(SourceEvent::Report(Report { key, activity, priority: PRIORITY_RPC }));
                }
                // Gone: so is everything it said.
                for key in keys {
                    let _ = reports.send(SourceEvent::Report(Report { key, activity: None, priority: PRIORITY_RPC }));
                }
                let _ = reports.send(SourceEvent::Bridge(false));
                log::info!("rpc: the arRPC bridge closed");
            }
            Err(err) => log::debug!("rpc: no arRPC bridge at {BRIDGE}: {err}"),
        }
        tokio::time::sleep(RETRY).await;
    }
}

/// One bridge message as a report key and the activity in the shape `SET_ACTIVITY` carries
/// (`None` clears it); `None` for anything that is not a bridge message.
pub fn parse(message: &Value) -> Option<(String, Option<Value>)> {
    let socket = match message.get("socketId")? {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let key = format!("bridge:{socket}");
    let activity = match message.get("activity")? {
        Value::Null => None,
        Value::Object(_) => Some(plain(message["activity"].clone())),
        _ => return None,
    };
    Some((key, activity))
}

/// Undoes arRPC's reshaping: its buttons are labels with their links in `metadata.button_urls`,
/// and it adds `metadata` and `flags` of its own. Secrets and join data go, as for our own RPC.
fn plain(mut activity: Value) -> Value {
    let Some(obj) = activity.as_object_mut() else { return activity };
    let urls: Vec<Value> = obj
        .get("metadata")
        .and_then(|m| m.get("button_urls"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let labels = obj.get("buttons").and_then(Value::as_array).filter(|b| b.iter().all(Value::is_string)).cloned();
    if let Some(labels) = labels {
        let buttons: Vec<Value> = labels.iter().zip(&urls).map(|(label, url)| json!({ "label": label, "url": url })).collect();
        obj.insert("buttons".into(), Value::Array(buttons));
    }
    for extra in ["metadata", "flags", "secrets", "instance", "match", "join", "spectate"] {
        obj.remove(extra);
    }
    activity
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bridge_message_becomes_a_plain_activity() {
        let message = json!({
            "activity": {
                "application_id": "1547350467904806923", "name": "Spotify", "type": 2, "flags": 0,
                "metadata": { "button_urls": ["https://sonorahq.org"] }, "buttons": ["Get Sonora"],
                "details": "10", "state": "Dean Blunt, Joanne Robertson",
                "timestamps": { "start": 1790383321000u64 }, "secrets": { "join": "x" }
            },
            "pid": 462443, "socketId": "3"
        });
        let (key, activity) = parse(&message).unwrap();
        assert_eq!(key, "bridge:3");
        let a = activity.unwrap();
        assert_eq!(a["name"], "Spotify");
        assert_eq!(a["buttons"], json!([{ "label": "Get Sonora", "url": "https://sonorahq.org" }]));
        assert!(a.get("metadata").is_none() && a.get("flags").is_none() && a.get("secrets").is_none());
        // What the rest of Rhein makes of it: the name Sonora was told to show.
        let normal = rhein_proto::Activity::normalize(&a).unwrap();
        assert_eq!(normal.name, "Spotify");
        assert_eq!(normal.buttons.len(), 1);
    }

    #[test]
    fn a_cleared_or_odd_message() {
        assert_eq!(parse(&json!({ "activity": null, "pid": 1, "socketId": 4 })), Some(("bridge:4".into(), None)));
        assert_eq!(parse(&json!({ "activity": null })), None);
        assert_eq!(parse(&json!({ "activity": "x", "socketId": "1" })), None);
        assert_eq!(parse(&json!([1, 2])), None);
    }
}
