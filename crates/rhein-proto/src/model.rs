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
    /// Media reference of the wide picture across the top of the profile card.
    pub banner_ref: Option<String>,
    /// 0xRRGGBB; fills the banner area when there is no banner picture.
    pub accent_color: Option<u32>,
    /// 0xRRGGBB top and bottom of the card's border and background gradient.
    pub theme_colors: Option<[u32; 2]>,
    /// The public profile page (`/u/{user}`) is served; on unless turned off.
    pub public_page_enabled: Option<bool>,
    /// It shows the presence and custom status; off unless turned on.
    pub public_status_enabled: Option<bool>,
    /// It shows the activity; off unless turned on.
    pub public_activity_enabled: Option<bool>,
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
        matches!(self.kind.as_str(), "text" | "announcement" | "thread")
    }

    pub fn is_thread(&self) -> bool {
        self.kind == "thread"
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
    pub mentionable: bool,
    /// Held by a bot for its own use; not assignable by hand.
    pub managed: bool,
    pub member_count: Option<i64>,
    /// `permissions` as the server names them (`MANAGE_ROLES`, …).
    pub permission_names: Vec<String>,
}

impl Role {
    /// The colour as 0xRRGGBB; `None` for "no colour" (stored as 0).
    pub fn rgb(&self) -> Option<u32> {
        let n = match self.color.as_ref()? {
            Value::Number(n) => n.as_u64()?,
            Value::String(s) => s.trim_start_matches('#').parse::<u64>().ok().or_else(|| u64::from_str_radix(s.trim_start_matches('#'), 16).ok())?,
            _ => return None,
        };
        (n != 0).then_some((n & 0xff_ffff) as u32)
    }

    /// The permission bits (they cross the wire as a decimal string).
    pub fn bits(&self) -> u64 {
        match self.permissions.as_ref() {
            Some(Value::String(s)) => s.parse().unwrap_or(0),
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Guild {
    pub id: String,
    pub name: String,
    pub icon_ref: Option<String>,
    pub owner: Option<String>,
    /// `#name:server`, when the guild has an address of its own.
    pub alias: Option<String>,
    /// The longest the guild keeps media, in seconds; `None` for as long as the server does.
    pub media_expiry_limit_secs: Option<i64>,
    pub created_at: Option<Millis>,
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
    /// Set instead of `guild_id` for a call in a conversation.
    pub conversation_id: Option<String>,
    pub user_id: String,
    pub channel_id: Option<String>,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub self_video: bool,
    pub server_mute: bool,
    pub server_deaf: bool,
    pub streaming: bool,
    pub session_id: Option<String>,
    pub region: Option<String>,
    pub stream_preset: Option<String>,
    pub joined_at: Option<i64>,
    /// Holds the key of a guild call on an SFU (a member of the call's MLS group).
    pub e2ee_member: bool,
}

/// One row of a channel's screen-watch table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VoiceWatcher {
    pub viewer: String,
    pub streamer: String,
    pub quality: String,
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

    pub fn activities(&self) -> Vec<crate::Activity> {
        self.presence.as_ref().map(crate::activity::activities_of).unwrap_or_default()
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
    fn profile_card_look_decodes() {
        let p: Profile = serde_json::from_str(
            r#"{"user_id":"@a:x","banner_ref":"h?cap=c","accent_color":7101439,"theme_colors":[11111375,3048688]}"#,
        )
        .unwrap();
        assert_eq!(p.banner_ref.as_deref(), Some("h?cap=c"));
        assert_eq!(p.accent_color, Some(0x6c5bff));
        assert_eq!(p.theme_colors, Some([0xa98bcf, 0x2e84f0]));
        let bare: Profile = serde_json::from_str(r#"{"user_id":"@a:x","theme_colors":null}"#).unwrap();
        assert_eq!(bare.theme_colors, None);
    }

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

/// How to dial a call: the `voice_server` of a join answer, a token refresh or a
/// `VOICE_SERVER_UPDATE`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum VoiceServer {
    /// Peer to peer, signalled through `/voice/signal`.
    Mesh {
        #[serde(default)]
        channel_id: Option<String>,
        #[serde(default)]
        max_participants: Option<u32>,
        #[serde(default)]
        max_bitrate: Option<u64>,
        /// Whether `ice_servers` carries a TURN relay.
        #[serde(default)]
        relay: bool,
        #[serde(default)]
        ice_servers: Vec<IceServer>,
        /// Bumped by a conversation's sequencer on every topology change.
        #[serde(default)]
        generation: Option<u64>,
    },
    /// Through a Telium SFU: `token` is spent at `{endpoint}/session`.
    Sfu {
        #[serde(default)]
        channel_id: Option<String>,
        #[serde(default)]
        region: Option<String>,
        #[serde(default, deserialize_with = "null_as_empty")]
        endpoint: String,
        #[serde(default, deserialize_with = "null_as_empty")]
        token: String,
        #[serde(default)]
        token_ttl: Option<u64>,
        #[serde(default)]
        max_bitrate: Option<u64>,
        #[serde(default)]
        ice_servers: Vec<IceServer>,
        /// A conversation's own SFU: the address is in the MLS group, not in `endpoint`.
        #[serde(default)]
        custom: bool,
        /// Whether our home has a TURN relay.
        #[serde(default)]
        relay: bool,
        #[serde(default)]
        generation: Option<u64>,
        /// `"required"` for conversation calls: media must be end-to-end encrypted.
        #[serde(default)]
        e2ee: Option<String>,
    },
}

/// One entry of an ICE server list, as WebRTC's `RTCIceServer`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IceServer {
    /// A string or a list of strings.
    pub urls: Value,
    pub username: Option<String>,
    pub credential: Option<String>,
}

impl IceServer {
    pub fn urls(&self) -> Vec<String> {
        match &self.urls {
            Value::String(url) => vec![url.clone()],
            Value::Array(urls) => urls.iter().filter_map(|u| u.as_str().map(str::to_owned)).collect(),
            _ => Vec::new(),
        }
    }
}

/// The answer to `POST /channels/{id}/voice/join`.
#[derive(Debug, Clone, Deserialize)]
pub struct VoiceJoin {
    pub voice_state: VoiceState,
    pub voice_server: VoiceServer,
    /// A guild call on an SFU: its MLS group, and whether we found it.
    #[serde(default)]
    pub e2ee: Option<GuildE2ee>,
}

/// Which MLS group encrypts a guild call, and whether this device is its first member.
#[derive(Debug, Clone, Deserialize)]
pub struct GuildE2ee {
    pub group_id: String,
    #[serde(default)]
    pub epoch: u64,
    #[serde(default)]
    pub member: bool,
}

/// What `POST {sfu}/session` answers with: where the LiveKit room is and how to get in.
#[derive(Debug, Clone, Deserialize)]
pub struct SfuSession {
    pub url: String,
    pub token: String,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub max_bitrate: Option<u64>,
}

fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

#[cfg(test)]
mod voice_tests {
    use super::*;

    #[test]
    fn a_conversation_sfu_with_a_custom_endpoint_parses() {
        let server: VoiceServer = serde_json::from_value(serde_json::json!({
            "mode": "sfu", "custom": true, "endpoint": null, "token": "t", "e2ee": "required", "relay": true, "generation": 2,
        }))
        .unwrap();
        let VoiceServer::Sfu { endpoint, custom, e2ee, .. } = server else { panic!() };
        assert!(endpoint.is_empty() && custom && e2ee.as_deref() == Some("required"));
    }
}
