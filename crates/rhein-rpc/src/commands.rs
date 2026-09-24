//! Discord RPC commands, independent of the transport. Behaves like arRPC, which the Telium
//! desktop client runs: the same READY payload, the same `SET_ACTIVITY` translation and replies.

use serde_json::{Value, json};

use crate::{PRIORITY_RPC, Report, Reports, SourceEvent};

/// Sent right after a successful handshake. Apps only check that it arrives; the user is the
/// stand-in arRPC uses, since there is no Discord account behind it.
pub fn ready() -> Value {
    json!({
        "cmd": "DISPATCH",
        "evt": "READY",
        "nonce": null,
        "data": {
            "v": 1,
            "config": {
                "cdn_host": "cdn.discordapp.com",
                "api_endpoint": "//discord.com/api",
                "environment": "production",
            },
            "user": {
                "id": "1045800378228281345",
                "username": "arrpc",
                "discriminator": "0",
                "global_name": "arRPC",
                "avatar": "cfefa4d9839fb4bdf030f91c2a13e95c",
                "avatar_decoration_data": null,
                "bot": false,
                "flags": 0,
                "premium_type": 0,
            },
        },
    })
}

/// One connected RPC client.
pub struct Session {
    pub key: String,
    pub client_id: String,
    reports: Reports,
}

impl Session {
    pub fn open(key: String, client_id: String, reports: Reports) -> Session {
        let _ = reports.send(SourceEvent::Client { key: key.clone(), client_id: Some(client_id.clone()) });
        Session { key, client_id, reports }
    }

    /// Handles one command frame and returns the reply, if the command gets one.
    pub fn handle(&self, frame: &Value) -> Option<Value> {
        let cmd = frame.get("cmd").and_then(Value::as_str).unwrap_or_default();
        let nonce = frame.get("nonce").cloned().unwrap_or(Value::Null);
        let args = frame.get("args").cloned().unwrap_or(Value::Null);
        match cmd {
            "SET_ACTIVITY" => {
                let activity = args.get("activity").filter(|a| a.is_object()).cloned();
                let Some(activity) = activity else {
                    self.report(None);
                    return Some(json!({ "cmd": cmd, "data": null, "evt": null, "nonce": nonce }));
                };
                self.report(Some(translate(activity.clone(), &self.client_id)));
                let mut echo = activity;
                if let Some(obj) = echo.as_object_mut() {
                    obj.insert("name".into(), json!(""));
                    obj.insert("application_id".into(), json!(self.client_id));
                    obj.insert("type".into(), json!(0));
                }
                Some(json!({ "cmd": cmd, "data": echo, "evt": null, "nonce": nonce }))
            }
            "CLEAR_ACTIVITY" => {
                self.report(None);
                Some(json!({ "cmd": cmd, "data": null, "evt": null, "nonce": nonce }))
            }
            "SUBSCRIBE" | "UNSUBSCRIBE" => {
                let evt = frame.get("evt").cloned().unwrap_or(Value::Null);
                Some(json!({ "cmd": cmd, "data": { "evt": evt }, "evt": null, "nonce": nonce }))
            }
            // Invites and templates are Discord's; there is nothing here to open them in.
            "INVITE_BROWSER" | "GUILD_TEMPLATE_BROWSER" => {
                let code = args.get("code").cloned().unwrap_or(Value::Null);
                let (error, what) = if cmd == "INVITE_BROWSER" { (4011, "invite") } else { (4017, "guild template") };
                Some(json!({
                    "cmd": cmd,
                    "data": { "code": error, "message": format!("Invalid {what} id: {}", code.as_str().unwrap_or_default()) },
                    "evt": "ERROR",
                    "nonce": nonce,
                }))
            }
            "DEEP_LINK" => Some(json!({ "cmd": cmd, "data": null, "evt": "ERROR", "nonce": nonce })),
            _ => Some(json!({
                "cmd": cmd,
                "data": { "code": 1000, "message": format!("Unknown command: {cmd}") },
                "evt": "ERROR",
                "nonce": nonce,
            })),
        }
    }

    fn report(&self, activity: Option<Value>) {
        let _ = self.reports.send(SourceEvent::Report(Report { key: self.key.clone(), activity, priority: PRIORITY_RPC }));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.report(None);
        let _ = self.reports.send(SourceEvent::Client { key: self.key.clone(), client_id: None });
    }
}

/// What Discord makes of a `SET_ACTIVITY`: the app's id attached, timestamps in milliseconds
/// (apps often send seconds), game type unless one is given. Secrets, instance flags and join
/// data are dropped here and never leave the machine.
pub fn translate(mut activity: Value, client_id: &str) -> Value {
    let Some(obj) = activity.as_object_mut() else { return activity };
    obj.insert("application_id".into(), json!(client_id));
    obj.entry("type").or_insert(json!(0));
    for secret in ["secrets", "instance", "match", "join", "spectate"] {
        obj.remove(secret);
    }
    if let Some(stamps) = obj.get_mut("timestamps").and_then(Value::as_object_mut) {
        for key in ["start", "end"] {
            if let Some(n) = stamps.get(key).and_then(Value::as_f64)
                && n > 0.
                && n < 100_000_000_000.
            {
                stamps.insert(key.into(), json!((n * 1000.) as u64));
            }
        }
    }
    activity
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_attaches_the_app_and_uses_milliseconds() {
        let out = translate(
            json!({ "details": "x", "timestamps": { "start": 1_700_000_000, "end": 1_700_000_000_000u64 }, "secrets": { "join": "s" } }),
            "42",
        );
        assert_eq!(out["application_id"], "42");
        assert_eq!(out["type"], 0);
        assert_eq!(out["timestamps"]["start"], 1_700_000_000_000u64);
        assert_eq!(out["timestamps"]["end"], 1_700_000_000_000u64);
        assert!(out.get("secrets").is_none());
    }

    #[test]
    fn commands_reply_like_arrpc() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let session = Session::open("ipc:1".into(), "42".into(), tx);
        let reply = session
            .handle(&json!({ "cmd": "SET_ACTIVITY", "nonce": "n1", "args": { "pid": 7, "activity": { "state": "In menu" } } }))
            .unwrap();
        assert_eq!(reply["nonce"], "n1");
        assert_eq!(reply["data"]["application_id"], "42");
        assert!(matches!(rx.try_recv(), Ok(SourceEvent::Client { .. })));
        match rx.try_recv() {
            Ok(SourceEvent::Report(r)) => assert_eq!(r.activity.unwrap()["state"], "In menu"),
            other => panic!("expected a report, got {other:?}"),
        }
        let unknown = session.handle(&json!({ "cmd": "AUTHORIZE", "nonce": "n2" })).unwrap();
        assert_eq!(unknown["evt"], "ERROR");
        drop(session);
        // Leaving clears the activity.
        assert!(matches!(rx.try_recv(), Ok(SourceEvent::Report(Report { activity: None, .. }))));
    }
}
