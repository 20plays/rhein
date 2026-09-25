//! The sealed client sync store (`docs/sync-encryption.md`, `web/src/lib/sealed-sync.js`): the
//! account's small key-value store on its home server, every value sealed before it leaves the
//! device.
//!
//! - K_sync = HKDF-SHA256(IKM = the recovery keyring, salt `telium/sealed-sync/v1`, info `aead`),
//!   and its id `kid` = the first 8 bytes of info `key-id`, base64url. Each retired ring key
//!   gives another pair, for opening.
//! - The envelope is `{sealed: "v1", kid, iv, ct}`: AES-256-GCM with a 96-bit nonce and the
//!   associated data `telium/sealed-sync/v1 LF kid LF key-name`.
//! - The plaintext is a 4-byte big-endian length, the JSON `{"n", "t", "v"}`, then zeros up to a
//!   padding bucket.

use std::collections::HashMap;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use serde_json::{Value, json};
use sha2::Sha256;

use crate::vault::Keyring;

const LABEL: &str = "telium/sealed-sync/v1";
pub const ENVELOPE_VERSION: &str = "v1";

/// The sync keys of an account's keyring: the live one seals, all of them open.
#[derive(Clone)]
pub struct SyncKeys {
    pub kid: String,
    key: [u8; 32],
    by_kid: HashMap<String, [u8; 32]>,
}

fn derive(raw: &[u8]) -> (String, [u8; 32]) {
    let hk = Hkdf::<Sha256>::new(Some(LABEL.as_bytes()), raw);
    let mut key = [0u8; 32];
    hk.expand(b"aead", &mut key).expect("32 bytes is a valid HKDF length");
    let mut id = [0u8; 8];
    hk.expand(b"key-id", &mut id).expect("8 bytes is a valid HKDF length");
    (URL_SAFE_NO_PAD.encode(id), key)
}

impl SyncKeys {
    pub fn new(ring: &Keyring) -> SyncKeys {
        let (kid, key) = derive(&ring.key);
        let mut by_kid = HashMap::from([(kid.clone(), key)]);
        for old in &ring.retired {
            let (id, k) = derive(old);
            by_kid.entry(id).or_insert(k);
        }
        SyncKeys { kid, key, by_kid }
    }
}

fn associated(kid: &str, name: &str) -> Vec<u8> {
    format!("{LABEL}\n{kid}\n{name}").into_bytes()
}

/// The padding bucket of a plaintext of `n` bytes (length prefix included).
pub fn padded_length(n: usize) -> usize {
    if n <= 256 {
        256
    } else if n <= 65536 {
        n.next_power_of_two()
    } else {
        n.div_ceil(65536) * 65536
    }
}

/// Whether a stored value is a sealed envelope.
pub fn is_sealed(value: &Value) -> bool {
    value["sealed"].is_string() && value["ct"].is_string()
}

/// Seals `{n, t, v}` for the key `name`.
pub fn seal(keys: &SyncKeys, name: &str, n: u64, t: u64, v: &Value) -> Value {
    let body = json!({ "n": n, "t": t, "v": v }).to_string().into_bytes();
    let mut plain = vec![0u8; padded_length(body.len() + 4)];
    plain[..4].copy_from_slice(&(body.len() as u32).to_be_bytes());
    plain[4..4 + body.len()].copy_from_slice(&body);
    let mut iv = [0u8; 12];
    getrandom::fill(&mut iv).expect("the system has randomness");
    let aad = associated(&keys.kid, name);
    let ct = Aes256Gcm::new((&keys.key).into())
        .encrypt(Nonce::from_slice(&iv), Payload { msg: &plain, aad: &aad })
        .expect("AES-GCM seals any length");
    json!({ "sealed": ENVELOPE_VERSION, "kid": keys.kid, "iv": URL_SAFE_NO_PAD.encode(iv), "ct": URL_SAFE_NO_PAD.encode(ct) })
}

/// What a sealed value held: its counter, when it was written (ms) and the value.
#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    pub n: u64,
    pub t: f64,
    pub v: Value,
    pub kid: String,
}

/// Opens an envelope stored under the key `name`.
pub fn open(keys: &SyncKeys, name: &str, envelope: &Value) -> Result<Opened> {
    if !is_sealed(envelope) {
        bail!("not a sealed value");
    }
    if envelope["sealed"].as_str() != Some(ENVELOPE_VERSION) {
        bail!("sealed with a newer version");
    }
    let kid = envelope["kid"].as_str().unwrap_or_default();
    let key = keys.by_kid.get(kid).ok_or_else(|| anyhow!("sealed under a key this device does not hold"))?;
    let iv = URL_SAFE_NO_PAD.decode(envelope["iv"].as_str().unwrap_or_default()).map_err(|_| anyhow!("bad nonce"))?;
    if iv.len() != 12 {
        bail!("bad nonce");
    }
    let ct = URL_SAFE_NO_PAD.decode(envelope["ct"].as_str().unwrap_or_default()).map_err(|_| anyhow!("bad ciphertext"))?;
    let aad = associated(kid, name);
    let plain = Aes256Gcm::new(key.into())
        .decrypt(Nonce::from_slice(&iv), Payload { msg: &ct, aad: &aad })
        .map_err(|_| anyhow!("{name} does not authenticate"))?;
    if plain.len() < 4 {
        bail!("short plaintext");
    }
    let len = u32::from_be_bytes(plain[..4].try_into().unwrap()) as usize;
    if len > plain.len() - 4 {
        bail!("bad padding");
    }
    let doc: Value = serde_json::from_slice(&plain[4..4 + len]).map_err(|_| anyhow!("unreadable plaintext"))?;
    let n = doc["n"].as_u64().filter(|n| *n >= 1).ok_or_else(|| anyhow!("unexpected plaintext"))?;
    let t = doc["t"].as_f64().ok_or_else(|| anyhow!("unexpected plaintext"))?;
    let v = doc.get("v").cloned().ok_or_else(|| anyhow!("unexpected plaintext"))?;
    Ok(Opened { n, t, v, kid: kid.to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring(key: u8, retired: &[u8]) -> Keyring {
        Keyring { key: [key; 32], retired: retired.iter().map(|k| [*k; 32]).collect() }
    }

    #[test]
    fn a_value_opens_only_under_its_name_and_its_account() {
        let keys = SyncKeys::new(&ring(1, &[]));
        let sealed = seal(&keys, "gif.favorites.v1", 3, 1_790_000_000_000, &json!(["https://x/a.gif"]));
        let opened = open(&keys, "gif.favorites.v1", &sealed).unwrap();
        assert_eq!((opened.n, opened.v), (3, json!(["https://x/a.gif"])));
        assert!(open(&keys, "prefs.v1", &sealed).is_err(), "bound to its key name");
        assert!(open(&SyncKeys::new(&ring(2, &[])), "gif.favorites.v1", &sealed).is_err(), "bound to its account");
        // A ring that rotated still opens what the old key sealed.
        let rotated = SyncKeys::new(&ring(2, &[1]));
        assert_eq!(open(&rotated, "gif.favorites.v1", &sealed).unwrap().n, 3);
        assert_eq!(URL_SAFE_NO_PAD.decode(&keys.kid).unwrap().len(), 8);
    }

    #[test]
    fn padding_follows_the_web_client_buckets() {
        assert_eq!(padded_length(10), 256);
        assert_eq!(padded_length(257), 512);
        assert_eq!(padded_length(65536), 65536);
        assert_eq!(padded_length(65537), 131072);
    }
}
