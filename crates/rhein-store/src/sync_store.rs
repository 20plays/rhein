//! The account's sealed sync store on its home server, as the web client keeps it
//! (`web/src/lib/sync.js`): values sealed under a key from the recovery keyring
//! (`rhein_mls::sealed_sync`), each with a counter so a server handing back an older value than
//! this device has seen is caught, last write winning otherwise. Rhein uses the GIF favourites
//! so far (`gif.favorites.v1`).

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use rhein_mls::sealed_sync::{self, SyncKeys};
use rhein_mls::vault::Keyring;
use rhein_net::Api;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const GIF_FAVORITES: &str = "gif.favorites.v1";
/// What the web client keeps at most.
pub const MAX_GIF_FAVORITES: usize = 500;

/// The highest counter this device has seen per key, kept beside the DM secrets.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Marks {
    pub seen: HashMap<String, u64>,
}

fn marks_path(user_id: &str) -> std::path::PathBuf {
    let safe: String = user_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    crate::persist::path().with_file_name("dm").join(format!("{safe}.sync.json"))
}

impl Marks {
    pub fn load(user_id: &str) -> Marks {
        std::fs::read_to_string(marks_path(user_id)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn store(&self, user_id: &str) -> Result<()> {
        crate::persist::write_private(&marks_path(user_id), &serde_json::to_vec(self)?)
    }
}

/// A sync store of one account.
pub struct SyncStore {
    user: String,
    keys: SyncKeys,
    marks: Marks,
}

/// What loading found for a key: its value, or that the server's copy is older than one this
/// device has seen (and should be written back).
pub enum Loaded {
    Value(Value),
    Missing,
    RolledBack,
}

impl SyncStore {
    pub fn new(user: &str, ring: &Keyring) -> SyncStore {
        SyncStore { user: user.to_owned(), keys: SyncKeys::new(ring), marks: Marks::load(user) }
    }

    /// Reads one key from the server, checking it against what this device has seen.
    pub async fn load(&mut self, api: &Api, key: &str) -> Result<Loaded> {
        let all = api.sync_all().await.map_err(|e| anyhow!("{e}"))?;
        let Some(stored) = all.get(key).map(|row| row.get("value").cloned().unwrap_or_else(|| row.clone())) else {
            return Ok(Loaded::Missing);
        };
        if !sealed_sync::is_sealed(&stored) {
            // Plaintext from before sealing counts only where this device has never seen a
            // sealed value for the key.
            return Ok(if self.marks.seen.contains_key(key) { Loaded::RolledBack } else { Loaded::Value(stored) });
        }
        let opened = sealed_sync::open(&self.keys, key, &stored)?;
        let seen = self.marks.seen.get(key).copied().unwrap_or(0);
        if opened.n < seen {
            return Ok(Loaded::RolledBack);
        }
        self.marks.seen.insert(key.to_owned(), opened.n);
        let _ = self.marks.store(&self.user);
        Ok(Loaded::Value(opened.v))
    }

    /// Seals and writes one key, one counter past anything seen for it.
    pub async fn save(&mut self, api: &Api, key: &str, value: &Value) -> Result<()> {
        let n = self.marks.seen.get(key).copied().unwrap_or(0) + 1;
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let envelope = sealed_sync::seal(&self.keys, key, n, t, value);
        api.sync_put(key, &envelope).await.map_err(|e| anyhow!("{e}"))?;
        self.marks.seen.insert(key.to_owned(), n);
        let _ = self.marks.store(&self.user);
        Ok(())
    }
}

/// GIF favourites as the web client keeps them: https URLs, no duplicates, at most 500.
pub fn clean_favorites(value: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in value.as_array().into_iter().flatten() {
        let Some(url) = raw.as_str().map(str::trim) else { continue };
        if url.starts_with("https://") && !out.iter().any(|u| u == url) {
            out.push(url.to_owned());
        }
        if out.len() >= MAX_GIF_FAVORITES {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn favorites_are_https_urls_without_duplicates() {
        let value = json!(["https://a/1.gif", "http://a/2.gif", "https://a/1.gif", 7, " https://a/3.gif "]);
        assert_eq!(super::clean_favorites(&value), ["https://a/1.gif", "https://a/3.gif"]);
    }
}
