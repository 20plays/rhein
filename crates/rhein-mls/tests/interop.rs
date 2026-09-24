//! Rhein (openmls) against the Telium web client's MLS layer (ts-mls 1.6.4, driven through
//! `scripts/mls-interop/harness.mjs`, which makes exactly the calls `web/src/lib/mls.js` makes).
//! Skipped when Node or the harness's modules are missing (`cd scripts/mls-interop && npm install`).

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rhein_mls::policy::Rule;
use rhein_mls::{Device, Policy, Processed, members, policy_of};
use serde_json::{Value, json};

struct Web {
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Web {
    fn start() -> Option<Web> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/mls-interop");
        if !dir.join("node_modules/ts-mls").exists() {
            eprintln!("ts-mls harness not installed; skipping");
            return None;
        }
        let mut child = Command::new("node").arg("harness.mjs").current_dir(&dir).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().ok()?;
        let input = child.stdin.take()?;
        let output = BufReader::new(child.stdout.take()?);
        Some(Web { _child: child, input, output })
    }

    fn call(&mut self, request: Value) -> Value {
        writeln!(self.input, "{request}").unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        let answer: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(answer["ok"], true, "{} failed: {}", request["op"], answer["error"]);
        answer
    }

    fn bytes(answer: &Value, key: &str) -> Vec<u8> {
        B64.decode(answer[key].as_str().unwrap_or_else(|| panic!("no {key} in {answer}"))).unwrap()
    }
}

fn b64(bytes: &[u8]) -> String {
    B64.encode(bytes)
}

fn policy(creator: &str) -> Policy {
    Policy { v: 1, creator: creator.into(), add: Rule::Anyone, remove: Rule::Creator, policy: Rule::Creator, admins: vec![] }
}

fn accept(group: &openmls::prelude::MlsGroup, committer: Option<&(String, String)>, staged: &openmls::prelude::StagedCommit) -> anyhow::Result<()> {
    rhein_mls::judge_commit(group, committer, staged)
}

fn text(processed: Processed) -> String {
    match processed {
        Processed::Application { plaintext, .. } => {
            assert_eq!(plaintext[0], 0x00, "a message payload");
            String::from_utf8(plaintext[1..].to_vec()).unwrap()
        }
        other => panic!("expected an application message, got {other:?}"),
    }
}

fn message(text: &str) -> Vec<u8> {
    let mut out = vec![0x00];
    out.extend_from_slice(text.as_bytes());
    out
}

/// The web client founds the group; Rhein joins by Welcome, and they talk both ways.
#[test]
fn web_founds_rhein_joins() {
    let Some(mut web) = Web::start() else { return };
    let rhein = Device::new("@rhein:x|r1").unwrap();
    web.call(json!({ "op": "new", "id": "w", "identity": "@web:x|w1" }));

    let kp = rhein.key_package().unwrap();
    let created = web.call(json!({
        "op": "create_group", "id": "w", "group_id": "conv-a",
        "policy": policy("@web:x"), "key_packages": [b64(&kp)],
    }));
    let mut group = rhein.join(&Web::bytes(&created, "welcome")).unwrap();
    assert_eq!(group.group_id().as_slice(), b"conv-a");
    assert_eq!(policy_of(&group).unwrap(), Some(policy("@web:x")));
    assert_eq!(members(&group), ["@rhein:x|r1", "@web:x|w1"]);

    let from_web = web.call(json!({ "op": "send", "id": "w", "group_id": "conv-a", "text": "hello from the web" }));
    let got = rhein.process(&mut group, &Web::bytes(&from_web, "message"), accept).unwrap();
    assert_eq!(text(got), "hello from the web");

    let from_rhein = rhein.send(&mut group, &message("hello from Rhein")).unwrap();
    let got = web.call(json!({ "op": "process", "id": "w", "group_id": "conv-a", "message": b64(&from_rhein) }));
    assert_eq!(got["kind"], "application");
    assert_eq!(got["tag"], 0);
    assert_eq!(got["text"], "hello from Rhein");
}

/// Rhein founds the group with a policy; the web client joins, reads the policy and talks.
#[test]
fn rhein_founds_web_joins() {
    let Some(mut web) = Web::start() else { return };
    let rhein = Device::new("@rhein:x|r1").unwrap();
    web.call(json!({ "op": "new", "id": "w", "identity": "@web:x|w1" }));
    let kp = web.call(json!({ "op": "key_package", "id": "w" }));

    let (mut group, welcome) = rhein.create_group("conv-b", Some(&policy("@rhein:x")), &[Web::bytes(&kp, "key_package")]).unwrap();
    let joined = web.call(json!({ "op": "join", "id": "w", "welcome": b64(&welcome.unwrap()) }));
    assert_eq!(joined["group_id"], "conv-b");
    assert_eq!(joined["policy"]["creator"], "@rhein:x");
    assert_eq!(joined["policy"]["remove"], "creator");

    let out = rhein.send(&mut group, &message("first")).unwrap();
    let got = web.call(json!({ "op": "process", "id": "w", "group_id": "conv-b", "message": b64(&out) }));
    assert_eq!(got["text"], "first");
    let back = web.call(json!({ "op": "send", "id": "w", "group_id": "conv-b", "text": "second" }));
    assert_eq!(text(rhein.process(&mut group, &Web::bytes(&back, "message"), accept).unwrap()), "second");
}

/// Commits in both directions: the web adds a member, Rhein refreshes its path; both keep
/// the same tree and keep talking at the new epochs.
#[test]
fn commits_both_ways() {
    let Some(mut web) = Web::start() else { return };
    let rhein = Device::new("@rhein:x|r1").unwrap();
    web.call(json!({ "op": "new", "id": "w", "identity": "@web:x|w1" }));
    web.call(json!({ "op": "new", "id": "w2", "identity": "@other:x|o1" }));
    let created = web.call(json!({ "op": "create_group", "id": "w", "group_id": "conv-c", "key_packages": [b64(&rhein.key_package().unwrap())] }));
    let mut group = rhein.join(&Web::bytes(&created, "welcome")).unwrap();

    let kp2 = web.call(json!({ "op": "key_package", "id": "w2" }));
    let added = web.call(json!({ "op": "add", "id": "w", "group_id": "conv-c", "key_packages": [kp2["key_package"]] }));
    web.call(json!({ "op": "join", "id": "w2", "welcome": added["welcome"] }));
    assert!(matches!(rhein.process(&mut group, &Web::bytes(&added, "commit"), accept).unwrap(), Processed::Commit { .. }));
    assert_eq!(members(&group), ["@other:x|o1", "@rhein:x|r1", "@web:x|w1"]);
    assert_eq!(group.epoch().as_u64(), added["epoch"].as_u64().unwrap());

    let commit = rhein.update(&mut group).unwrap();
    for id in ["w", "w2"] {
        let got = web.call(json!({ "op": "process", "id": id, "group_id": "conv-c", "message": b64(&commit) }));
        assert_eq!(got["kind"], "handshake");
        assert_eq!(got["epoch"].as_u64().unwrap(), group.epoch().as_u64());
    }
    let out = rhein.send(&mut group, &message("after the update")).unwrap();
    let got = web.call(json!({ "op": "process", "id": "w2", "group_id": "conv-c", "message": b64(&out) }));
    assert_eq!(got["text"], "after the update");

    // Rhein adds a web device, and the web device is welcomed into the same epoch.
    web.call(json!({ "op": "new", "id": "w3", "identity": "@web:x|w2" }));
    let kp3 = web.call(json!({ "op": "key_package", "id": "w3" }));
    let (commit, welcome) = rhein.add(&mut group, &[Web::bytes(&kp3, "key_package")]).unwrap();
    for id in ["w", "w2"] {
        web.call(json!({ "op": "process", "id": id, "group_id": "conv-c", "message": b64(&commit) }));
    }
    let joined = web.call(json!({ "op": "join", "id": "w3", "welcome": b64(&welcome) }));
    assert_eq!(joined["epoch"].as_u64().unwrap(), group.epoch().as_u64());
}

/// The new-device path, both ways: an external commit from a GroupInfo.
#[test]
fn external_commits_both_ways() {
    let Some(mut web) = Web::start() else { return };
    let rhein = Device::new("@rhein:x|r1").unwrap();
    web.call(json!({ "op": "new", "id": "w", "identity": "@web:x|w1" }));
    let created = web.call(json!({ "op": "create_group", "id": "w", "group_id": "conv-d", "key_packages": [b64(&rhein.key_package().unwrap())] }));
    let mut group = rhein.join(&Web::bytes(&created, "welcome")).unwrap();

    // A second web device of the same account lets itself in from Rhein's GroupInfo.
    web.call(json!({ "op": "new", "id": "w-new", "identity": "@web:x|w9" }));
    let info = rhein.group_info(&group).unwrap();
    let joined = web.call(json!({ "op": "join_external", "id": "w-new", "group_info": b64(&info) }));
    assert!(matches!(rhein.process(&mut group, &Web::bytes(&joined, "commit"), accept).unwrap(), Processed::Commit { .. }));
    web.call(json!({ "op": "process", "id": "w", "group_id": "conv-d", "message": joined["commit"] }));
    assert_eq!(members(&group), ["@rhein:x|r1", "@web:x|w1", "@web:x|w9"]);

    // A new Rhein device does the same from the web's GroupInfo.
    let info = web.call(json!({ "op": "group_info", "id": "w", "group_id": "conv-d" }));
    let newcomer = Device::new("@rhein:x|r2").unwrap();
    let (mut second, commit) = newcomer.join_external(&Web::bytes(&info, "group_info")).unwrap();
    for id in ["w", "w-new"] {
        let got = web.call(json!({ "op": "process", "id": id, "group_id": "conv-d", "message": b64(&commit) }));
        assert_eq!(got["members"].as_array().unwrap().len(), 4);
    }
    rhein.process(&mut group, &commit, accept).unwrap();
    let hello = web.call(json!({ "op": "send", "id": "w", "group_id": "conv-d", "text": "welcome, new device" }));
    assert_eq!(text(newcomer.process(&mut second, &Web::bytes(&hello, "message"), accept).unwrap()), "welcome, new device");
    assert_eq!(text(rhein.process(&mut group, &Web::bytes(&hello, "message"), accept).unwrap()), "welcome, new device");
}

/// Call keys come from the MLS exporter; both sides must derive the same bytes.
#[test]
fn exporter_matches() {
    let Some(mut web) = Web::start() else { return };
    let rhein = Device::new("@rhein:x|r1").unwrap();
    web.call(json!({ "op": "new", "id": "w", "identity": "@web:x|w1" }));
    let created = web.call(json!({ "op": "create_group", "id": "w", "group_id": "conv-e", "key_packages": [b64(&rhein.key_package().unwrap())] }));
    let group = rhein.join(&Web::bytes(&created, "welcome")).unwrap();
    for label in ["telium call media v1", "telium call signal v1"] {
        let theirs = web.call(json!({ "op": "exporter", "id": "w", "group_id": "conv-e", "label": label, "context": "conv-e", "length": 32 }));
        let ours = rhein.export(&group, label, b"conv-e", 32).unwrap();
        assert_eq!(b64(&ours), theirs["secret"].as_str().unwrap(), "{label}");
    }
}


/// A device whose signature key was kept from an earlier session (as the store keeps it) still
/// interoperates: the key round-trips through its raw form.
#[test]
fn kept_signature_key_interoperates() {
    let Some(mut web) = Web::start() else { return };
    let (private, public) = rhein_mls::new_signature_key().unwrap();
    let signer = rhein_mls::signature_key(&private, &public);
    let provider = openmls_rust_crypto::OpenMlsRustCrypto::default();
    let rhein = Device::with_signer(provider, signer, "@rhein:x|r7").unwrap();
    assert_eq!(rhein.signature_public_key(), public.as_slice());
    web.call(json!({ "op": "new", "id": "w", "identity": "@web:x|w1" }));
    let kp = web.call(json!({ "op": "key_package", "id": "w" }));
    let (mut group, welcome) = rhein.create_group("conv-k", None, &[Web::bytes(&kp, "key_package")]).unwrap();
    web.call(json!({ "op": "join", "id": "w", "welcome": b64(&welcome.unwrap()) }));
    let out = rhein.send(&mut group, &message("kept key")).unwrap();
    let got = web.call(json!({ "op": "process", "id": "w", "group_id": "conv-k", "message": b64(&out) }));
    assert_eq!(got["text"], "kept key");
}

/// The membership policy is enforced on commits from the web: a member who may not add people
/// is refused, the creator is not.
#[test]
fn policy_is_enforced() {
    let Some(mut web) = Web::start() else { return };
    let rhein = Device::new("@rhein:x|r1").unwrap();
    web.call(json!({ "op": "new", "id": "creator", "identity": "@creator:x|c1" }));
    web.call(json!({ "op": "new", "id": "member", "identity": "@member:x|m1" }));
    web.call(json!({ "op": "new", "id": "late", "identity": "@late:x|l1" }));
    let strict = Policy { v: 1, creator: "@creator:x".into(), add: Rule::Creator, remove: Rule::Creator, policy: Rule::Creator, admins: vec![] };
    let kp_member = web.call(json!({ "op": "key_package", "id": "member" }));
    let created = web.call(json!({
        "op": "create_group", "id": "creator", "group_id": "conv-p", "policy": strict,
        "key_packages": [b64(&rhein.key_package().unwrap()), kp_member["key_package"]],
    }));
    let mut group = rhein.join(&Web::bytes(&created, "welcome")).unwrap();
    web.call(json!({ "op": "join", "id": "member", "welcome": created["welcome"] }));

    // The member (not the creator) adds somebody: refused, and the group stays where it was.
    let kp_late = web.call(json!({ "op": "key_package", "id": "late" }));
    let sneaky = web.call(json!({ "op": "add", "id": "member", "group_id": "conv-p", "key_packages": [kp_late["key_package"]] }));
    let epoch = group.epoch();
    let refused = rhein.process(&mut group, &Web::bytes(&sneaky, "commit"), accept).unwrap_err();
    assert!(refused.to_string().contains("may not add"), "{refused}");
    assert_eq!(group.epoch(), epoch);
}
