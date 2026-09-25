//! Call privacy in conversations (`docs/calls.md`, `web/src/lib/callmode.js`): each person
//! holds a posture (`direct`, `relay`, `sfu`, in rising strictness), the call is as strict as its
//! strictest participant, and postures travel only in hellos sealed with a key exported from the
//! conversation's MLS group, so no server learns them.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

/// The exporter label of the hello key (context: the conversation id, 32 bytes).
pub const SIGNAL_LABEL: &str = "telium call signal v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Posture {
    /// Peers connect straight to each other: they learn your address.
    #[default]
    Direct,
    /// Every connection through a TURN relay: nobody in the call learns your address.
    Relay,
    /// Through an SFU, end-to-end encrypted: only the SFU learns your address.
    Sfu,
}

impl Posture {
    pub const ALL: [Posture; 3] = [Posture::Direct, Posture::Relay, Posture::Sfu];

    pub fn as_str(self) -> &'static str {
        match self {
            Posture::Direct => "direct",
            Posture::Relay => "relay",
            Posture::Sfu => "sfu",
        }
    }

    /// Unknown names read as `direct`, as the web client's `normalize` does.
    pub fn parse(name: &str) -> Posture {
        match name {
            "relay" => Posture::Relay,
            "sfu" => Posture::Sfu,
            _ => Posture::Direct,
        }
    }
}

/// A hello opened from someone in the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hello {
    pub posture: Posture,
    pub counter: u64,
    pub epoch: u64,
}

/// The call's posture: ours and everyone else's in the roster, where anyone whose hello is
/// missing, unopened or from another epoch counts as `relay` (the strict side of not knowing).
pub fn effective(own: Posture, me: &str, roster: &[String], hellos: &std::collections::HashMap<String, Hello>, epoch: Option<u64>) -> Posture {
    roster
        .iter()
        .filter(|u| *u != me)
        .map(|u| match hellos.get(u) {
            Some(h) if epoch.is_none_or(|e| h.epoch == e) => h.posture,
            _ => Posture::Relay,
        })
        .fold(own, Posture::max)
}

/// The one bit the sequencer is told: this posture cannot be met without an SFU.
pub fn wants_sfu(own: Posture, relay_available: bool) -> bool {
    own == Posture::Sfu || (own == Posture::Relay && !relay_available)
}

/// Whether mesh connections must go through TURN: always for our own strict posture, and for
/// someone else's when a relay is there to use.
pub fn relay_only(own: Posture, effective: Posture, relay_available: bool) -> bool {
    own >= Posture::Relay || (effective >= Posture::Relay && relay_available)
}

fn aad(conversation: &str, sender: &str) -> Vec<u8> {
    format!("telium call hello|{conversation}|{sender}").into_bytes()
}

/// Seals our posture for the others in the call: `{v: 1, epoch, iv, ct}`.
pub fn seal_hello(key: &[u8], conversation: &str, sender: &str, posture: Posture, counter: u64, epoch: u64) -> Value {
    let mut iv = [0u8; 12];
    getrandom::fill(&mut iv).expect("the system has randomness");
    let body = json!({ "posture": posture.as_str(), "counter": counter }).to_string();
    let aad = aad(conversation, sender);
    let ct = Aes256Gcm::new(key.into())
        .encrypt(Nonce::from_slice(&iv), Payload { msg: body.as_bytes(), aad: &aad })
        .expect("AES-GCM seals any length");
    json!({ "v": 1, "epoch": epoch, "iv": STANDARD.encode(iv), "ct": STANDARD.encode(ct) })
}

/// Opens someone's hello; `None` for anything that does not open, parse or match the epoch.
pub fn open_hello(key: &[u8], conversation: &str, sender: &str, epoch: Option<u64>, sealed: &Value) -> Option<Hello> {
    if sealed["v"].as_u64() != Some(1) {
        return None;
    }
    let sealed_epoch = sealed["epoch"].as_u64()?;
    if epoch.is_some_and(|e| e != sealed_epoch) {
        return None;
    }
    let iv = STANDARD.decode(sealed["iv"].as_str()?).ok()?;
    let ct = STANDARD.decode(sealed["ct"].as_str()?).ok()?;
    if iv.len() != 12 {
        return None;
    }
    let aad = aad(conversation, sender);
    let plain = Aes256Gcm::new_from_slice(key).ok()?.decrypt(Nonce::from_slice(&iv), Payload { msg: &ct, aad: &aad }).ok()?;
    let body: Value = serde_json::from_slice(&plain).ok()?;
    let posture = body["posture"].as_str().filter(|p| ["direct", "relay", "sfu"].contains(p)).map(Posture::parse)?;
    let counter = body["counter"].as_f64().filter(|c| c.is_finite())? as u64;
    Some(Hello { posture, counter, epoch: sealed_epoch })
}

/// Keeps the newest hello per sender (a replayed one never wins); whether anything changed.
pub fn note_hello(hellos: &mut std::collections::HashMap<String, Hello>, sender: &str, opened: Option<Hello>) -> bool {
    let Some(opened) = opened else { return false };
    if hellos.get(sender).is_some_and(|held| held.epoch == opened.epoch && held.counter >= opened.counter) {
        return false;
    }
    hellos.insert(sender.to_owned(), opened);
    true
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn the_strictest_known_posture_wins_and_the_unknown_is_relay() {
        let roster = vec!["@me:a".to_owned(), "@you:b".to_owned()];
        let mut hellos = HashMap::new();
        assert_eq!(effective(Posture::Direct, "@me:a", &roster, &hellos, Some(3)), Posture::Relay, "no hello yet");
        hellos.insert("@you:b".to_owned(), Hello { posture: Posture::Direct, counter: 1, epoch: 3 });
        assert_eq!(effective(Posture::Direct, "@me:a", &roster, &hellos, Some(3)), Posture::Direct);
        assert_eq!(effective(Posture::Direct, "@me:a", &roster, &hellos, Some(4)), Posture::Relay, "an old epoch's hello");
        assert_eq!(effective(Posture::Sfu, "@me:a", &roster, &hellos, Some(3)), Posture::Sfu);
        assert!(wants_sfu(Posture::Relay, false) && !wants_sfu(Posture::Relay, true) && wants_sfu(Posture::Sfu, true));
        assert!(relay_only(Posture::Relay, Posture::Relay, false));
        assert!(!relay_only(Posture::Direct, Posture::Relay, false), "no relay to use");
        assert!(relay_only(Posture::Direct, Posture::Relay, true));
    }

    #[test]
    fn hellos_open_only_for_their_sender_and_epoch() {
        let key = [9u8; 32];
        let sealed = seal_hello(&key, "conv", "@you:b", Posture::Sfu, 7, 3);
        assert_eq!(open_hello(&key, "conv", "@you:b", Some(3), &sealed), Some(Hello { posture: Posture::Sfu, counter: 7, epoch: 3 }));
        assert_eq!(open_hello(&key, "conv", "@other:b", Some(3), &sealed), None, "bound to its sender");
        assert_eq!(open_hello(&key, "other", "@you:b", Some(3), &sealed), None, "bound to its conversation");
        assert_eq!(open_hello(&key, "conv", "@you:b", Some(4), &sealed), None, "another epoch");
        let mut hellos = HashMap::new();
        assert!(note_hello(&mut hellos, "@you:b", open_hello(&key, "conv", "@you:b", None, &sealed)));
        let older = seal_hello(&key, "conv", "@you:b", Posture::Direct, 6, 3);
        assert!(!note_hello(&mut hellos, "@you:b", open_hello(&key, "conv", "@you:b", None, &older)), "a replay never wins");
    }
}
