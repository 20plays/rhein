//! A guild call's MLS group between Rhein and the web client's own `guild-call-keys.js` (run
//! under bun from the clone by `scripts/web-interop/guild-call.mjs`): a web device founds the
//! group and adds Rhein, Rhein as committer adds a second web device, and every device exports
//! the same media key at every epoch. Skipped without bun or the clone's dependencies.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use rhein_mls::call_group::{CallGroup, package_identity};
use rhein_mls::vault;
use serde_json::{Value, json};

struct Web {
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Web {
    fn start() -> Option<Web> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !root.join(".dev/telium/web/node_modules/ts-mls").exists() || !root.join(".dev/telium/web/src/lib/guild-call-keys.js").exists() {
            return None;
        }
        let bun = std::env::var("BUN").unwrap_or_else(|_| format!("{}/.bun/bin/bun", std::env::var("HOME").unwrap_or_default()));
        let mut child = Command::new(bun).arg(root.join("scripts/web-interop/guild-call.mjs")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().ok()?;
        let input = child.stdin.take()?;
        let output = BufReader::new(child.stdout.take()?);
        Some(Web { _child: child, input, output })
    }

    fn call(&mut self, request: Value) -> Value {
        writeln!(self.input, "{request}").unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        let answer: Value = serde_json::from_str(&line).unwrap_or_else(|_| panic!("bad answer: {line}"));
        assert_eq!(answer["ok"], true, "web {}: {}", request["op"], answer["error"]);
        answer
    }

    fn key(&mut self, name: &str) -> (Vec<u8>, u64) {
        let k = self.call(json!({ "op": "key", "name": name }));
        (vault::unb64(k["key"].as_str().expect("a key")).unwrap(), k["epoch"].as_u64().unwrap())
    }
}

#[test]
fn a_call_group_crosses_between_rhein_and_the_web_client() {
    let Some(mut web) = Web::start() else {
        eprintln!("no bun or no clone dependencies; skipping");
        return;
    };
    let channel = "chan1";
    let group = "01CALLGROUP";
    web.call(json!({ "op": "new", "name": "w1", "user": "@web:a", "device": "w1", "channel": channel }));
    web.call(json!({ "op": "new", "name": "w2", "user": "@web2:a", "device": "w2", "channel": channel }));
    web.call(json!({ "op": "offer", "name": "w1" }));

    // The web device founds the call's group.
    web.call(json!({ "op": "begin", "name": "w1", "group_id": group, "member": true }));
    assert_eq!(web.key("w1").1, 0);

    // Rhein waits with a package; the web committer adds it.
    let (private, public) = rhein_mls::new_signature_key().unwrap();
    let mut rhein = CallGroup::new("@rhein:a", "r1", &private, &public).unwrap();
    let offered = rhein.offer().unwrap();
    assert_eq!(package_identity(&offered).map(|(u, d, _)| (u, d)), Some(("@rhein:a".into(), "r1".into())));
    rhein.begin(group, false).unwrap();
    let view = json!({ "group_id": group, "epoch": 0, "participants": [
        { "user_id": "@web:a", "device_id": "w1", "member": true },
        { "user_id": "@rhein:a", "device_id": "r1", "member": false, "key_package": offered },
    ]});
    let body = web.call(json!({ "op": "change", "name": "w1", "view": view }))["body"].clone();
    assert_eq!(body["added"][0]["user_id"], "@rhein:a");
    rhein.join(body["welcome"].as_str().expect("a welcome")).unwrap();
    let key = rhein.media_key(channel).unwrap();
    assert_eq!(key, web.key("w1"), "the same key at epoch 1");
    assert_eq!(key.1, 1);

    // Rhein commits: it adds a second web device, and the first processes the commit.
    let package = web.call(json!({ "op": "offer", "name": "w2" }))["key_package"].as_str().unwrap().to_owned();
    web.call(json!({ "op": "begin", "name": "w2", "group_id": group, "member": false }));
    let (commit, welcome) = rhein.change(&[], &[package]).unwrap();
    rhein.merge_pending().unwrap();
    let event = json!({ "op": "commit", "channel_id": channel, "group_id": group, "epoch": 2, "commit": commit, "welcome": welcome, "added": ["@web2:a"], "by": "@rhein:a" });
    web.call(json!({ "op": "hear", "name": "w1", "event": event }));
    web.call(json!({ "op": "hear", "name": "w2", "event": event }));
    let key = rhein.media_key(channel).unwrap();
    assert_eq!(key.1, 2);
    assert_eq!(web.key("w1"), key, "the web founder follows Rhein's commit");
    assert_eq!(web.key("w2"), key, "the web device Rhein added joins from its welcome");

    // The web founder removes the second web device; Rhein follows.
    let view = json!({ "group_id": group, "epoch": 2, "participants": [
        { "user_id": "@web:a", "device_id": "w1", "member": true },
        { "user_id": "@rhein:a", "device_id": "r1", "member": true },
    ]});
    let body = web.call(json!({ "op": "change", "name": "w1", "view": view }))["body"].clone();
    assert!(body["commit"].is_string(), "a removal commit: {body}");
    assert!(rhein.process(body["commit"].as_str().unwrap()).unwrap(), "Rhein stays in");
    let key = rhein.media_key(channel).unwrap();
    assert_eq!(key.1, 3);
    assert_eq!(web.key("w1"), key);
}
