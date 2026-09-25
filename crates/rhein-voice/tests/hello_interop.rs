//! Call hellos between Rhein and the web client's own `callmode.js` (run under bun from the
//! clone): each side opens what the other sealed. Skipped without bun or the clone.

use std::path::PathBuf;
use std::process::Command;

use rhein_voice::posture::{self, Hello, Posture};

fn bun(args: &[&str]) -> Option<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !root.join(".dev/telium/web/src/lib/callmode.js").exists() {
        return None;
    }
    let bun = std::env::var("BUN").unwrap_or_else(|_| format!("{}/.bun/bin/bun", std::env::var("HOME").unwrap_or_default()));
    let out = Command::new(bun).arg(root.join("scripts/web-interop/hello.mjs")).args(args).output().ok()?;
    assert!(out.status.success(), "bun: {}", String::from_utf8_lossy(&out.stderr));
    Some(String::from_utf8(out.stdout).unwrap().trim().to_owned())
}

#[test]
fn hellos_cross_between_rhein_and_the_web_client() {
    let key = [0x5au8; 32];
    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();

    // The web client seals, Rhein opens.
    let Some(sealed) = bun(&["seal", &hex, "conv1", "@web:a", "sfu", "1790000000000", "4"]) else {
        eprintln!("no bun or no clone; skipping");
        return;
    };
    let sealed: serde_json::Value = serde_json::from_str(&sealed).unwrap();
    assert_eq!(
        posture::open_hello(&key, "conv1", "@web:a", Some(4), &sealed),
        Some(Hello { posture: Posture::Sfu, counter: 1_790_000_000_000, epoch: 4 })
    );
    assert_eq!(posture::open_hello(&key, "conv1", "@rhein:a", Some(4), &sealed), None);

    // Rhein seals, the web client opens.
    let ours = posture::seal_hello(&key, "conv1", "@rhein:a", Posture::Relay, 42, 4).to_string();
    let opened = bun(&["open", &hex, "conv1", "@rhein:a", "4", &ours]).unwrap();
    let opened: serde_json::Value = serde_json::from_str(&opened).unwrap();
    assert_eq!(opened, serde_json::json!({ "posture": "relay", "counter": 42, "epoch": 4 }));
    let wrong = bun(&["open", &hex, "conv1", "@someone:a", "4", &ours]).unwrap();
    assert_eq!(wrong, "null", "the web client refuses it under another sender");
}
