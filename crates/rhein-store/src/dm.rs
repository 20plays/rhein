//! Keys and trust for end-to-end encrypted DMs (milestone M3), brought up at sign-in the way the
//! web client does it (`store/lifecycle.js` → `vault.load` → `Trust.start`):
//!
//! 1. open the recovery keyring R from the home server's vault, with the OPAQUE-derived wrapper
//!    when this session came from a password sign-in, else with the copy this device kept;
//! 2. keep one MLS signature key per account on this device (the web client does the same, so a
//!    re-login is the same correspondent rather than a stranger);
//! 3. open the account's cross-signing secret with R, check it really belongs to the published
//!    identity key, vouch for this device's MLS key, and make sure the identity is anchored in its
//!    continuity document, pinning what we saw.
//!
//! Secrets live in a 0600 file per account in the config directory; nothing here is ever written
//! to the server except what the web client writes too (a fresh vault for an account that has
//! none, the device cross-signature and the identity anchor).

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use rhein_mls::trust::{self, IdentityKey, Pin};
use rhein_mls::vault::{self, Key, Keyring, Opened};
use rhein_net::Api;
use serde::{Deserialize, Serialize};

/// What this device keeps for one account.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Secrets {
    pub keyring: Option<Keyring>,
    /// The account's cross-signing secret, once opened (base64).
    pub cross_signing: Option<String>,
    /// This account's MLS signature key on this device (base64 private, public).
    pub mls_signature: Option<(String, String)>,
    /// Identity pins by user id.
    pub pins: BTreeMap<String, Pin>,
}

fn secrets_path(user_id: &str) -> PathBuf {
    let safe: String = user_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    crate::persist::path().with_file_name("dm").join(format!("{safe}.json"))
}

impl Secrets {
    pub fn load(user_id: &str) -> Secrets {
        std::fs::read(secrets_path(user_id)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn store(&self, user_id: &str) -> Result<()> {
        let path = secrets_path(user_id);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("dm secrets dir")?;
        }
        crate::persist::write_private(&path, &serde_json::to_vec_pretty(self)?)
    }

    /// Forgets everything kept for an account (it signed out on this device).
    pub fn remove(user_id: &str) {
        let _ = std::fs::remove_file(secrets_path(user_id));
    }
}

/// How far DM encryption came up, for the settings page and the DM view.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DmStatus {
    /// `opaque_custody`, `device_custody`, …, or `none`.
    pub vault_mode: String,
    /// Why the recovery keyring isn't available, if it isn't.
    pub locked: Option<String>,
    /// The account's cross-signing identity key, if it has one.
    pub identity_key: Option<String>,
    pub immutable_id: Option<String>,
    /// This device's MLS key is vouched for by the identity key.
    pub cross_signed: bool,
    /// Something is wrong with the published identity (a replaced key, a broken chain).
    pub identity_problem: Option<String>,
    /// This device's MLS signature key (base64), as the ratchet tree and cross-signature carry it.
    pub mls_key: String,
}

impl DmStatus {
    /// A short fingerprint of this device's MLS key: 8 groups of 4 hex digits of its SHA-256.
    pub fn fingerprint(&self) -> String {
        use sha2::Digest as _;
        let Ok(raw) = vault::unb64(&self.mls_key) else { return String::new() };
        let digest = sha2::Sha256::digest(raw);
        digest[..16].chunks(2).map(|c| format!("{:02x}{:02x}", c[0], c[1])).collect::<Vec<_>>().join(" ")
    }
}

/// Brings DM keys and trust up for an account. `wrapper` is the OPAQUE-derived vault key from a
/// password sign-in in this session, if there was one.
pub async fn bring_up(api: &Api, user_id: &str, device_id: &str, wrapper: Option<Key>) -> Result<DmStatus> {
    let mut secrets = Secrets::load(user_id);
    let mut status = DmStatus::default();

    // 1. The recovery keyring.
    let doc = api.vault().await.context("vault")?;
    status.vault_mode = doc["mode"].as_str().unwrap_or("none").to_owned();
    match vault::open_vault(&doc, wrapper.as_ref(), secrets.keyring.as_ref())? {
        Opened::Ring { keyring, mode } => {
            status.vault_mode = mode;
            secrets.keyring = Some(keyring);
        }
        Opened::Locked { reason, .. } => status.locked = Some(reason.to_owned()),
        Opened::Missing => match &wrapper {
            // An account without DMs set up yet (one registered with Rhein): create the vault the
            // way the web client does on its first sign-in.
            Some(wrapper) => {
                let keyring = Keyring::generate();
                api.put_vault(&vault::new_vault_body(&keyring, wrapper)).await.context("creating the vault")?;
                status.vault_mode = "opaque_custody".into();
                secrets.keyring = Some(keyring);
            }
            None => status.locked = Some("sign in with your password once on this device".into()),
        },
    }

    // 2. This account's MLS signature key on this device.
    let (_, public) = match &secrets.mls_signature {
        Some(pair) => pair.clone(),
        None => {
            let pair = rhein_mls::new_signature_key()?;
            let pair = (vault::b64(&pair.0), vault::b64(&pair.1));
            secrets.mls_signature = Some(pair.clone());
            pair
        }
    };
    status.mls_key = public.clone();

    // 3. Cross-signing and continuity.
    let cs = api.cross_signing().await.context("cross-signing")?;
    if cs["exists"].as_bool().unwrap_or(false) {
        let published = cs["identity_key"].as_str().unwrap_or_default().to_owned();
        status.identity_key = Some(published.clone());
        let held = secrets.cross_signing.as_deref().and_then(|s| vault::unb64(s).ok()).and_then(|b| IdentityKey::from_secret(&b).ok());
        let opened = held.or_else(|| {
            let ring = secrets.keyring.as_ref()?;
            trust::open_cross_signing_secret(cs["sealed_secret"].as_str().unwrap_or_default(), ring)
        });
        match opened {
            Some(key) if key.binds(user_id, &published) => {
                secrets.cross_signing = Some(vault::b64(key.secret()));
                // Continuity: anchor the identity if it never was, verify it, pin it.
                let mut identity = api.own_identity().await.ok();
                if identity.as_ref().is_some_and(|d| !d["exists"].as_bool().unwrap_or(false)) {
                    let tid = trust::immutable_id(&published)?;
                    let signature = vault::b64url(&key.sign(&trust::anchor_payload(user_id, &tid, &published)));
                    identity = api.anchor_identity(&tid, &published, &signature).await.ok();
                }
                match identity.as_ref().map(|d| trust::verify_identity_document(d, Some(user_id))) {
                    Some(Ok(Some(c))) => match trust::check_pin(secrets.pins.get(user_id), &c) {
                        Ok(pin) if c.current_key == published => {
                            status.immutable_id = Some(c.immutable_id.clone());
                            secrets.pins.insert(user_id.to_owned(), pin);
                        }
                        Ok(_) => status.identity_problem = Some("the published identity key is not the head of its continuity chain".into()),
                        Err(reason) => status.identity_problem = Some(reason.into()),
                    },
                    Some(Err(reason)) => status.identity_problem = Some(reason.into()),
                    // An older home server without continuity documents.
                    Some(Ok(None)) | None => {}
                }
                if status.identity_problem.is_none() {
                    let already = cs["devices"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|d| d["device_id"] == device_id && d["mls_key"] == public.as_str() && d["cross_sig"].is_string());
                    if !already {
                        let signature = key.vouch_for_device(user_id, device_id, &public);
                        api.sign_device(device_id, &public, &signature).await.context("vouching for this device")?;
                    }
                    status.cross_signed = true;
                }
            }
            Some(_) => status.identity_problem = Some("the cross-signing secret does not match the published identity key".into()),
            None => {}
        }
    }

    secrets.store(user_id)?;
    Ok(status)
}
