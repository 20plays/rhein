//! Small per-install preferences kept next to the session file.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Most recent first; unicode characters or `<name:id>` tokens.
    pub recent_emoji: Vec<String>,
    pub skin_tone: u8,
    /// online, idle, dnd or invisible.
    pub status: String,
    pub custom_status: Option<String>,
    /// UI text size in percent of the default (80–150).
    pub text_scale: u16,
    pub compact: bool,
    pub clock_24h: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            recent_emoji: Vec::new(),
            skin_tone: 0,
            status: "online".into(),
            custom_status: None,
            text_scale: 100,
            compact: false,
            clock_24h: true,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let p: Prefs = serde_json::from_str(r#"{"recent_emoji":["🎉"]}"#).unwrap();
        assert_eq!(p.recent_emoji, vec!["🎉"]);
        assert_eq!(p.status, "online");
        assert_eq!(p.text_scale, 100);
        let round: Prefs = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(round.status, p.status);
    }
}
