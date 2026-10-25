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
    /// Show every change instantly: no fades, glides or blinking cursor.
    pub reduce_motion: bool,
    /// Keep the preview of our own screen share off our own stage.
    pub hide_own_screen: bool,
    /// Share what you are playing, listening to or watching (rich presence).
    pub share_activity: bool,
    pub share_games: bool,
    pub share_music: bool,
    pub share_videos: bool,
    /// Activities never shared, by application name.
    pub hidden_activities: Vec<String>,
    pub voice: VoicePrefs,
    /// Incoming friend requests already looked at (`social::seen_key`), so the Direct messages
    /// row marks only new ones.
    pub seen_requests: Vec<String>,
    /// Which activity is shown first when there are several (`Slot::key`); none: a game, then
    /// music, then a video.
    pub primary_activity: Option<String>,
    /// Unfold links into previews in the chat.
    pub link_previews: bool,
    /// Tell the others while you type.
    pub send_typing: bool,
    /// Leave muted channels out of the sidebar (unless open or mentioning you).
    pub hide_muted: bool,
    /// The coloured dot beside each server in the sidebar.
    pub server_dots: bool,
    /// "Copy … ID" entries in menus.
    pub developer_mode: bool,
}

/// Calls: devices, processing, push-to-talk, the gate, and what stays between calls.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoicePrefs {
    /// cpal device ids; `None` for the system default.
    pub input: Option<String>,
    pub output: Option<String>,
    pub push_to_talk: bool,
    /// The noise gate's threshold in dBFS, `None` for no gate.
    pub gate: Option<f32>,
    /// Input gain in percent (100 when unset).
    pub input_gain: Option<u32>,
    /// Plain capture: no echo cancellation, noise suppression or gain control.
    pub raw_input: bool,
    pub muted: bool,
    pub deafened: bool,
    /// Local playback volume per user, 0..200.
    pub volumes: std::collections::HashMap<String, u32>,
    /// Call privacy in conversations: `direct`, `relay` or `sfu` (see `docs/calls.md`).
    pub posture: String,
    /// Open ALSA's null devices instead of real ones (`RHEIN_AUDIO=null`, for screenshots and
    /// demos that must not touch the speakers). Never saved.
    #[serde(skip)]
    pub null_devices: bool,
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
            reduce_motion: false,
            hide_own_screen: false,
            share_activity: true,
            share_games: true,
            // Like video, what someone listens to is opt-in (the web client's default).
            share_music: false,
            // What someone watches in a browser is more private than a game; opt in.
            share_videos: false,
            hidden_activities: Vec::new(),
            voice: VoicePrefs::default(),
            seen_requests: Vec::new(),
            primary_activity: None,
            link_previews: true,
            send_typing: true,
            hide_muted: false,
            server_dots: true,
            developer_mode: true,
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
