//! Small per-install preferences kept next to the session file.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Most recent first; unicode characters or `<name:id>` tokens.
    pub recent_emoji: Vec<String>,
    pub skin_tone: u8,
}

fn path() -> std::path::PathBuf {
    crate::persist::path().with_file_name("prefs.json")
}

pub fn load() -> Prefs {
    std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn store(prefs: &Prefs) {
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_vec_pretty(prefs) {
        let _ = std::fs::write(path, text);
    }
}
