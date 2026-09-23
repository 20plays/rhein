use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Milliseconds since the Unix epoch, as every Telium timestamp is.
pub type Millis = i64;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub user_id: String,
    pub display_name: Option<String>,
    pub avatar_ref: Option<String>,
    pub bio: Option<String>,
    pub pronouns: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Overwrite {
    pub target_type: String,
    pub target_id: String,
    /// u64 bitfields travel as decimal strings.
    pub allow: String,
    pub deny: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Channel {
    pub id: String,
    pub guild_id: Option<String>,
    /// `text`, `voice`, `forum`, `announcement` or `category`.
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub topic: Option<String>,
    pub position: i64,
    pub parent_id: Option<String>,
    pub strip_embeds: bool,
    pub permission_overwrites: Vec<Overwrite>,
}

impl Channel {
    pub fn is_category(&self) -> bool {
        self.kind == "category"
    }

    pub fn is_voice(&self) -> bool {
        self.kind == "voice"
    }

    pub fn is_textual(&self) -> bool {
        matches!(self.kind.as_str(), "text" | "announcement")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Role {
    pub id: String,
    pub name: String,
    pub color: Option<Value>,
    pub position: i64,
    pub permissions: Option<Value>,
    pub hoist: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Guild {
    pub id: String,
    pub name: String,
    pub icon_ref: Option<String>,
    pub owner: Option<String>,
    pub moved_to: Option<Value>,
    pub channels: Vec<Channel>,
    pub roles: Vec<Role>,
    pub voice_states: Vec<VoiceState>,
    pub member_count: Option<i64>,
    pub me: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceState {
    pub guild_id: Option<String>,
    pub user_id: String,
    pub channel_id: Option<String>,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub self_video: bool,
    pub server_mute: bool,
    pub server_deaf: bool,
    pub streaming: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reaction {
    pub emoji: String,
    pub count: i64,
    pub me: bool,
    pub users: Vec<String>,
}

/// The descriptor `POST /media` answers with, and what a message carries per attachment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Attachment {
    pub reference: Option<String>,
    pub origin: String,
    pub hash: String,
    pub capability: Option<String>,
    pub url: Option<String>,
    pub filename: String,
    pub mime: String,
    pub size: i64,
    pub expires_at: Option<Millis>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub thumbhash: Option<String>,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Message {
    pub id: String,
    pub channel_id: String,
    pub guild_id: Option<String>,
    pub author: String,
    pub content: String,
    pub attachments: Vec<Attachment>,
    pub reply_to: Option<String>,
    pub reply_author: Option<String>,
    pub thread_id: Option<String>,
    pub created_at: Millis,
    pub edited_at: Option<Millis>,
    pub reactions: Vec<Reaction>,
    pub nonce: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Member {
    pub user_id: String,
    pub nickname: Option<String>,
    pub display_name: Option<String>,
    pub avatar_ref: Option<String>,
    pub joined_at: Option<Millis>,
    pub roles: Vec<String>,
    pub presence: Option<Value>,
    pub is_bot: bool,
}

impl Member {
    pub fn shown_name(&self) -> &str {
        self.nickname
            .as_deref()
            .or(self.display_name.as_deref())
            .unwrap_or_else(|| localpart(&self.user_id))
    }

    pub fn status(&self) -> &str {
        self.presence
            .as_ref()
            .and_then(|p| p.get("status"))
            .and_then(Value::as_str)
            .unwrap_or("offline")
    }
}

/// `@alice:a.example` → `alice`.
pub fn localpart(user_id: &str) -> &str {
    let id = user_id.strip_prefix('@').unwrap_or(user_id);
    id.split_once(':').map_or(id, |(local, _)| local)
}

/// `@alice:a.example` → `a.example`.
pub fn domain_of(user_id: &str) -> Option<&str> {
    user_id.split_once(':').map(|(_, domain)| domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_id_parts() {
        assert_eq!(localpart("@alice:a.example"), "alice");
        assert_eq!(domain_of("@alice:a.example"), Some("a.example"));
        assert_eq!(domain_of("@alice:localhost:8448"), Some("localhost:8448"));
    }

    #[test]
    fn message_decodes_with_missing_fields() {
        let m: Message = serde_json::from_str(
            r#"{"id":"01J","channel_id":"c","author":"@a:x","content":"hi","created_at":1,
                "reactions":[{"emoji":"👍","count":1,"me":true,"users":["@a:x"]}],"future":1}"#,
        )
        .unwrap();
        assert_eq!(m.content, "hi");
        assert_eq!(m.reactions[0].count, 1);
    }
}
