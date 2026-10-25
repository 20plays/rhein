//! Signed JSON documents (signing family A in `docs.telium.chat/reference/protocol/signing`):
//! key documents, media vouchers, relocation records, tombstones and the public account lookups.
//!
//! The signature covers the document without `signature` and `key_id`, serialised canonically:
//! no whitespace, object keys sorted by their UTF-8 bytes at every depth, and only `"`, `\` and
//! C0 controls escaped. A document holding a non-integer number is never signed, so it never
//! verifies either. Unknown members are covered like any other, and are kept as received.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, SigningKey, VerifyingKey};
use serde_json::Value;

/// The canonical serialisation, or `None` for JSON that holds a non-integer number.
pub fn canonical(value: &Value) -> Option<String> {
    let mut out = String::new();
    write(value, &mut out)?;
    Some(out)
}

fn write(value: &Value, out: &mut String) -> Option<()> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if !(n.is_i64() || n.is_u64()) {
                return None;
            }
            out.push_str(&n.to_string());
        }
        Value::String(s) => string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                string(key, out);
                out.push(':');
                write(&map[key], out)?;
            }
            out.push('}');
        }
    }
    Some(())
}

fn string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// What the signature of a document covers: the canonical form without `signature` and
/// `key_id`. `None` when it is not an object or holds a non-integer number.
pub fn signing_bytes(document: &Value) -> Option<Vec<u8>> {
    let mut body = document.as_object()?.clone();
    body.remove("signature");
    body.remove("key_id");
    canonical(&Value::Object(body)).map(String::into_bytes)
}

/// Base64 in any alphabet, padded or not, as identity inputs are accepted.
pub fn decode_any_b64(text: &str) -> Option<Vec<u8>> {
    [&URL_SAFE_NO_PAD, &URL_SAFE, &STANDARD_NO_PAD, &STANDARD].iter().find_map(|engine| engine.decode(text.trim()).ok())
}

/// Whether `document` carries a valid signature by the Ed25519 key `key_b64`. Strict: a
/// non-canonical signature or a small-order key fails. A valid signature is not authorisation;
/// audiences, expiry and the key's validity window are the caller's to check.
pub fn verify(document: &Value, key_b64: &str) -> bool {
    let Some(signature) = document.get("signature").and_then(Value::as_str).and_then(decode_any_b64) else { return false };
    let Some(bytes) = signing_bytes(document) else { return false };
    let Some(key) = decode_any_b64(key_b64).and_then(|k| <[u8; 32]>::try_from(k).ok()) else { return false };
    let Ok(key) = VerifyingKey::from_bytes(&key) else { return false };
    if key.is_weak() {
        return false;
    }
    let Ok(signature) = <[u8; 64]>::try_from(signature) else { return false };
    key.verify_strict(&bytes, &Signature::from_bytes(&signature)).is_ok()
}

/// Signs a document as a server would (tests and local fixtures).
pub fn sign(document: &Value, key: &SigningKey, key_id: &str) -> Option<Value> {
    use ed25519_dalek::Signer as _;
    let bytes = signing_bytes(document)?;
    let mut signed = document.as_object()?.clone();
    signed.insert("signature".into(), Value::String(URL_SAFE_NO_PAD.encode(key.sign(&bytes).to_bytes())));
    signed.insert("key_id".into(), Value::String(key_id.into()));
    Some(Value::Object(signed))
}

/// The key a server's key document (`GET /_telium/v1/keys`) lists under `key_id`, if it was
/// valid at `at_ms` (`valid_until` is exclusive). The document must be signed by one of its own
/// keys: the one it names as `key_id`.
pub fn key_from_document(keys: &Value, key_id: &str, at_ms: i64) -> Option<String> {
    let entry = |id: &str| keys.get("verify_keys")?.get(id).cloned();
    let signer = keys.get("key_id").and_then(Value::as_str)?;
    let signer_key = entry(signer)?;
    if !verify(keys, signer_key.get("key").and_then(Value::as_str)?) {
        return None;
    }
    let wanted = entry(key_id)?;
    let from = wanted.get("valid_from").and_then(Value::as_i64).unwrap_or(i64::MIN);
    let until = wanted.get("valid_until").and_then(Value::as_i64).unwrap_or(i64::MAX);
    (from <= at_ms && at_ms < until).then(|| wanted.get("key").and_then(Value::as_str).map(str::to_owned)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_form() {
        let v = json!({"b": 1, "a": {"z": [true, null], "é": "x/y\u{1}\"\\\n"}, "A": -3});
        assert_eq!(canonical(&v).unwrap(), "{\"A\":-3,\"a\":{\"z\":[true,null],\"é\":\"x/y\\u0001\\\"\\\\\\n\"},\"b\":1}");
        // Byte order, not code point order of a case-folded view.
        assert_eq!(canonical(&json!({"é": 1, "z": 2})).unwrap(), "{\"z\":2,\"é\":1}");
        assert!(canonical(&json!({"a": 1.5})).is_none());
        assert!(canonical(&json!({"a": [ {"b": 0.1} ]})).is_none());
    }

    #[test]
    fn sign_and_verify() {
        let key = SigningKey::from_bytes(&[7; 32]);
        let public = URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes());
        let doc = sign(&json!({"server_name": "a.example", "n": 3, "extra": {"y": 1}}), &key, "ed25519:1").unwrap();
        assert!(verify(&doc, &public));
        // key_id is not signed: rewriting it changes nothing.
        let mut renamed = doc.clone();
        renamed["key_id"] = json!("other");
        assert!(verify(&renamed, &public));
        // Every member is covered, unknown ones included; null is not absence.
        for (k, v) in [("n", json!(4)), ("extra", json!({"y": 2})), ("added", json!(null))] {
            let mut tampered = doc.clone();
            tampered[k] = v;
            assert!(!verify(&tampered, &public), "{k}");
        }
        let mut dropped = doc.clone();
        dropped.as_object_mut().unwrap().remove("extra");
        assert!(!verify(&dropped, &public));
        // A float anywhere: never signed, never verified.
        assert!(sign(&json!({"x": 0.5}), &key, "k").is_none());
        // Received key order does not matter.
        let reordered: Value = serde_json::from_str(&doc.to_string()).unwrap();
        assert!(verify(&reordered, &public));
    }

    #[test]
    fn key_documents() {
        let key = SigningKey::from_bytes(&[9; 32]);
        let public = URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes());
        let doc = sign(
            &json!({
                "server_name": "a.example",
                "current_key_id": "ed25519:2",
                "verify_keys": {
                    "ed25519:2": {"key": public, "valid_from": 1000, "valid_until": 5000},
                    "ed25519:1": {"key": "old", "valid_from": 0, "valid_until": 1000},
                },
            }),
            &key,
            "ed25519:2",
        )
        .unwrap();
        assert_eq!(key_from_document(&doc, "ed25519:2", 1000).as_deref(), Some(public.as_str()));
        assert!(key_from_document(&doc, "ed25519:2", 5000).is_none());
        assert_eq!(key_from_document(&doc, "ed25519:1", 999).as_deref(), Some("old"));
        assert!(key_from_document(&doc, "ed25519:3", 2000).is_none());
    }
}
