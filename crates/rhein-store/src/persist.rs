//! The signed-in sessions on disk: enough to reconnect without asking for the password again.
//! Several accounts can be saved; one is active.
//!
//! The file holds bearer tokens and device key seeds, so it is created 0600 in the user's config
//! directory. The OPAQUE export key is never written.

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
    /// Remembered for the account list, so it needs no network.
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Accounts {
    active: String,
    accounts: Vec<Saved>,
}

/// The configuration directory's single-session file of earlier versions; everything else is
/// stored next to it.
pub fn path() -> PathBuf {
    if let Ok(dir) = std::env::var("RHEIN_CONFIG_DIR") {
        return PathBuf::from(dir).join("session.json");
    }
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("rhein")
        .join("session.json")
}

pub fn accounts_path() -> PathBuf {
    path().with_file_name("accounts.json")
}

/// Where caches live (the detectable-games list, resolved application names).
pub fn cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RHEIN_CONFIG_DIR") {
        return PathBuf::from(dir).join("cache");
    }
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("rhein")
}

fn read() -> Accounts {
    if let Some(all) = std::fs::read_to_string(accounts_path()).ok().and_then(|t| serde_json::from_str(&t).ok()) {
        return all;
    }
    // Earlier versions kept one session in session.json; it becomes the first account.
    let legacy: Option<Saved> = std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok());
    match legacy {
        Some(saved) => {
            let all = Accounts { active: saved.user_id.clone(), accounts: vec![saved] };
            if write(&all).is_ok() {
                let _ = std::fs::remove_file(path());
            }
            all
        }
        None => Accounts::default(),
    }
}

fn write(all: &Accounts) -> Result<()> {
    let path = accounts_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("config dir")?;
    }
    write_private(&path, &serde_json::to_vec_pretty(all)?)
}

/// The active account.
pub fn load() -> Option<Saved> {
    let all = read();
    all.accounts.into_iter().find(|a| a.user_id == all.active)
}

/// Every saved account, the active one included.
pub fn accounts() -> Vec<Saved> {
    read().accounts
}

/// Saves (or updates) an account and makes it the active one.
pub fn store(saved: &Saved) -> Result<()> {
    let mut all = read();
    match all.accounts.iter_mut().find(|a| a.user_id == saved.user_id) {
        Some(existing) => *existing = saved.clone(),
        None => all.accounts.push(saved.clone()),
    }
    all.active = saved.user_id.clone();
    write(&all)
}

pub fn set_active(user_id: &str) -> Result<()> {
    let mut all = read();
    anyhow::ensure!(all.accounts.iter().any(|a| a.user_id == user_id), "{user_id} is not a saved account");
    all.active = user_id.to_owned();
    write(&all)
}

/// Forgets an account. If it was the active one, the next saved account (if any) becomes active
/// and is returned.
pub fn remove(user_id: &str) -> Option<Saved> {
    let mut all = read();
    all.accounts.retain(|a| a.user_id != user_id);
    if all.active == user_id {
        all.active = all.accounts.first().map(|a| a.user_id.clone()).unwrap_or_default();
    }
    let _ = write(&all);
    all.accounts.into_iter().find(|a| a.user_id == all.active)
}

/// Forgets the active account (its session was refused or expired).
pub fn clear() {
    if let Some(active) = load() {
        remove(&active.user_id);
    }
}

#[cfg(unix)]
pub(crate) fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
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
pub(crate) fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).context("session file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: &str) -> Saved {
        Saved { user_id: id.into(), home: "localhost".into(), access_token: format!("token-{id}"), ..Default::default() }
    }

    /// One test, because the location comes from a process-wide environment variable.
    #[test]
    fn several_accounts_with_one_active() {
        let dir = std::env::temp_dir().join(format!("rhein-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: no other test in this crate reads RHEIN_CONFIG_DIR.
        unsafe { std::env::set_var("RHEIN_CONFIG_DIR", &dir) };

        // A session file from before becomes the first account.
        std::fs::write(dir.join("session.json"), serde_json::to_vec(&account("@anna:x")).unwrap()).unwrap();
        assert_eq!(load().unwrap().user_id, "@anna:x");
        assert!(!dir.join("session.json").exists());

        store(&account("@ben:x")).unwrap();
        assert_eq!(load().unwrap().user_id, "@ben:x");
        assert_eq!(accounts().len(), 2);
        set_active("@anna:x").unwrap();
        assert_eq!(load().unwrap().access_token, "token-@anna:x");
        assert!(set_active("@nobody:x").is_err());

        // Updating keeps one entry.
        store(&Saved { access_token: "new".into(), ..account("@anna:x") }).unwrap();
        assert_eq!(accounts().len(), 2);

        // Removing the active account hands over to the next one.
        assert_eq!(remove("@anna:x").map(|a| a.user_id), Some("@ben:x".to_owned()));
        assert_eq!(load().unwrap().user_id, "@ben:x");
        clear();
        assert!(load().is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(accounts_path()).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
