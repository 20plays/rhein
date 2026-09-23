//! The signed-in session on disk: enough to reconnect without asking for the password again.
//!
//! The file holds bearer tokens and the device key seed, so it is created 0600 in the user's
//! config directory. The OPAQUE export key is never written.

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    /// What the user typed for their home server; discovery runs again on every start.
    pub home: String,
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
    pub expires_at: Option<i64>,
    /// base64url Ed25519 seed of this install's device key.
    pub device_seed: String,
    /// Guild servers other than the home server, by the name they were added under.
    pub servers: Vec<String>,
}

pub fn path() -> PathBuf {
    if let Ok(dir) = std::env::var("RHEIN_CONFIG_DIR") {
        return PathBuf::from(dir).join("session.json");
    }
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("rhein")
        .join("session.json")
}

pub fn load() -> Option<Saved> {
    let text = std::fs::read_to_string(path()).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn store(saved: &Saved) -> Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("config dir")?;
    }
    let text = serde_json::to_vec_pretty(saved)?;
    write_private(&path, &text)
}

pub fn clear() {
    let _ = std::fs::remove_file(path());
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .context("session file")?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).context("session file")
}
