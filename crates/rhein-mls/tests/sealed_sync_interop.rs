//! Sealed sync values between Rhein and the web client's own `sealed-sync.js` (under bun, from
//! the clone): each side opens what the other sealed, under the same key name and keyring.
//! Skipped without bun or the clone.

use std::path::PathBuf;
use std::process::Command;

use rhein_mls::sealed_sync::{self, SyncKeys};
use rhein_mls::vault::Keyring;
use serde_json::{Value, json};

fn bun(args: &[&str]) -> Option<String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !root.join(".dev/telium/web/src/lib/sealed-sync.js").exists() {
        return None;
    }
    let bun = std::env::var("BUN").unwrap_or_else(|_| format!("{}/.bun/bin/bun", std::env::var("HOME").unwrap_or_default()));
    let out = Command::new(bun).arg(root.join("scripts/web-interop/sealed-sync.mjs")).args(args).output().ok()?;
    assert!(out.status.success(), "bun: {}", String::from_utf8_lossy(&out.stderr));
    Some(String::from_utf8(out.stdout).unwrap().trim().to_owned())
}

#[test]
fn sealed_values_cross_between_rhein_and_the_web_client() {
    let ring = Keyring { key: [0x42; 32], retired: Vec::new() };
    let hex: String = ring.key.iter().map(|b| format!("{b:02x}")).collect();
    let keys = SyncKeys::new(&ring);
    let favorites = json!(["https://media.example/cat.gif", "https://media.example/dog.gif"]);

    // The web client seals, Rhein opens.
    let doc = json!({ "n": 7, "t": 1_790_000_000_000u64, "v": favorites }).to_string();
    let Some(sealed) = bun(&["seal", &hex, "gif.favorites.v1", &doc]) else {
        eprintln!("no bun or no clone; skipping");
        return;
    };
    let sealed: Value = serde_json::from_str(&sealed).unwrap();
    assert_eq!(sealed["kid"], keys.kid, "the same key id from the same keyring");
    let opened = sealed_sync::open(&keys, "gif.favorites.v1", &sealed).unwrap();
    assert_eq!((opened.n, opened.v.clone()), (7, favorites.clone()));

    // Rhein seals, the web client opens.
    let ours = sealed_sync::seal(&keys, "gif.favorites.v1", 8, 1_790_000_000_001, &favorites).to_string();
    let doc: Value = serde_json::from_str(&bun(&["open", &hex, "gif.favorites.v1", &ours]).unwrap()).unwrap();
    assert_eq!(doc, json!({ "n": 8, "t": 1_790_000_000_001u64, "v": favorites }));
}
