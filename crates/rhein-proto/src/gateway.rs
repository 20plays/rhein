use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::model::Guild;

pub const INTENT_GUILD_MESSAGES: u64 = 1 << 0;
pub const INTENT_GUILD_MEMBERS: u64 = 1 << 1;
pub const INTENT_GUILD_PRESENCES: u64 = 1 << 2;
pub const INTENT_GUILD_TYPING: u64 = 1 << 3;
pub const INTENT_GUILD_EXPRESSIONS: u64 = 1 << 4;
pub const INTENT_GUILD_STRUCTURE: u64 = 1 << 5;
pub const INTENT_DIRECT_MESSAGES: u64 = 1 << 6;
pub const INTENT_GUILD_VOICE_STATES: u64 = 1 << 7;

/// Every intent a user (not bot) session may ask for, which is what the web client sends.
pub const USER_INTENTS: u64 = 0xff;

pub const PROTOCOL_VERSION: u32 = 1;

/// Close codes the server uses, and what a client does about each.
pub mod close {
    pub const IDENTIFY_EXPECTED: u16 = 4001;
    pub const MALFORMED: u16 = 4002;
    pub const AUTH_FAILED: u16 = 4004;
    pub const RESTARTING: u16 = 4005;
    pub const IDENTIFY_TIMEOUT: u16 = 4008;
    pub const HEARTBEAT_TIMEOUT: u16 = 4009;
    pub const LAGGING: u16 = 4010;
    pub const TOO_MANY_CONNECTIONS: u16 = 4011;
    pub const PROTOCOL_INCOMPATIBLE: u16 = 4015;
}

/// One frame as it arrives from the server.
#[derive(Debug, Clone, Deserialize)]
pub struct Frame {
    pub op: String,
    #[serde(default)]
    pub s: Option<u64>,
    #[serde(default)]
    pub t: Option<String>,
    #[serde(default)]
    pub d: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Hello {
    pub heartbeat_interval_ms: u64,
    pub gateway_version: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReadyUser {
    pub user_id: String,
    pub device_id: Option<String>,
    pub is_bot: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Ready {
    pub session_id: Value,
    pub user: ReadyUser,
    pub server_name: String,
    pub guilds: Vec<Guild>,
}

pub fn identify(token: &str, intents: u64, status: &str, custom_status: Option<&str>) -> Value {
    json!({
        "op": "IDENTIFY",
        "d": {
            "token": token,
            "intents": intents,
            "presence": { "status": status, "custom_status": custom_status },
            "protocol": { "version": PROTOCOL_VERSION },
        }
    })
}

/// `status` is one of online, idle, dnd, invisible.
pub fn presence(status: &str, custom_status: Option<&str>) -> Value {
    json!({ "op": "PRESENCE_UPDATE", "d": { "status": status, "custom_status": custom_status } })
}

pub fn heartbeat() -> Value {
    json!({ "op": "HEARTBEAT" })
}

pub fn typing(channel_id: &str) -> Value {
    json!({ "op": "TYPING", "d": { "channel_id": channel_id } })
}

pub fn subscribe_members(guild_id: &str, first: u32, last: u32) -> Value {
    json!({ "op": "SUBSCRIBE", "d": { "guild_id": guild_id, "member_ranges": [[first, last]] } })
}

/// A `DISPATCH` the store understands; anything else stays `Other` and is ignored.
#[derive(Debug, Clone)]
pub enum Event {
    MessageCreate(crate::model::Message),
    MessageUpdate { id: String, channel_id: String, content: String, edited_at: Option<i64> },
    MessageDelete { id: String, channel_id: String },
    ReactionAdd { message_id: String, channel_id: String, emoji: String, user_id: String },
    ReactionRemove { message_id: String, channel_id: String, emoji: String, user_id: String },
    TypingStart { channel_id: String, user_id: String },
    ChannelCreate(crate::model::Channel),
    ChannelUpdate(crate::model::Channel),
    ChannelDelete { id: String, guild_id: Option<String> },
    GuildUpdate(Value),
    GuildDelete { id: String },
    MemberAdd(Value),
    MemberUpdate(Value),
    MemberRemove(Value),
    MemberListUpdate(Value),
    /// `presence` is the whole object: status, custom status and activities.
    PresenceUpdate { user_id: String, presence: Value },
    VoiceStateUpdate(crate::model::VoiceState),
    PinsUpdate { channel_id: String, message_id: String, pinned: bool },
    ThreadUpsert(crate::rest::Thread),
    ThreadDelete { id: String, parent_id: Option<String> },
    EmojiUpdate { guild_id: String },
    FriendUpdate(crate::rest::Friend),
    FriendRemove { user_id: String },
    /// Something happened in a private conversation (a new frame, a request decision, a member leaving).
    ConversationActivity { conversation_id: String, message: bool },
    Other { t: String, d: Value },
}

fn decode<T: DeserializeOwned>(d: &Value) -> Result<T, serde_json::Error> {
    serde_json::from_value(d.clone())
}

fn str_of(d: &Value, key: &str) -> String {
    d.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

impl Event {
    pub fn parse(t: &str, d: Value) -> Event {
        match t {
            "MESSAGE_CREATE" => match decode(&d) {
                Ok(m) => Event::MessageCreate(m),
                Err(_) => Event::Other { t: t.into(), d },
            },
            "MESSAGE_UPDATE" => Event::MessageUpdate {
                id: str_of(&d, "id"),
                channel_id: str_of(&d, "channel_id"),
                content: str_of(&d, "content"),
                edited_at: d.get("edited_at").and_then(Value::as_i64),
            },
            "MESSAGE_DELETE" => Event::MessageDelete {
                id: str_of(&d, "id"),
                channel_id: str_of(&d, "channel_id"),
            },
            "MESSAGE_REACTION_ADD" | "MESSAGE_REACTION_REMOVE" => {
                let (message_id, channel_id, emoji, user_id) = (
                    str_of(&d, "message_id"),
                    str_of(&d, "channel_id"),
                    str_of(&d, "emoji"),
                    str_of(&d, "user_id"),
                );
                if t == "MESSAGE_REACTION_ADD" {
                    Event::ReactionAdd { message_id, channel_id, emoji, user_id }
                } else {
                    Event::ReactionRemove { message_id, channel_id, emoji, user_id }
                }
            }
            "TYPING_START" => Event::TypingStart {
                channel_id: str_of(&d, "channel_id"),
                user_id: str_of(&d, "user_id"),
            },
            "CHANNEL_CREATE" | "CHANNEL_UPDATE" => match decode(&d) {
                Ok(c) if t == "CHANNEL_CREATE" => Event::ChannelCreate(c),
                Ok(c) => Event::ChannelUpdate(c),
                Err(_) => Event::Other { t: t.into(), d },
            },
            "CHANNEL_DELETE" => Event::ChannelDelete {
                id: str_of(&d, "id"),
                guild_id: d.get("guild_id").and_then(Value::as_str).map(str::to_owned),
            },
            "GUILD_UPDATE" => Event::GuildUpdate(d),
            "GUILD_DELETE" => Event::GuildDelete { id: str_of(&d, "id") },
            "MEMBER_ADD" => Event::MemberAdd(d),
            "MEMBER_UPDATE" => Event::MemberUpdate(d),
            "MEMBER_REMOVE" => Event::MemberRemove(d),
            "MEMBER_LIST_UPDATE" => Event::MemberListUpdate(d),
            "PRESENCE_UPDATE" => Event::PresenceUpdate { user_id: str_of(&d, "user_id"), presence: d },
            "CHANNEL_PINS_UPDATE" => Event::PinsUpdate {
                channel_id: str_of(&d, "channel_id"),
                message_id: str_of(&d, "message_id"),
                pinned: d.get("pinned").and_then(Value::as_bool).unwrap_or(false),
            },
            "THREAD_CREATE" | "THREAD_UPDATE" => match decode(&d) {
                Ok(t) => Event::ThreadUpsert(t),
                Err(_) => Event::Other { t: t.into(), d },
            },
            "THREAD_DELETE" => Event::ThreadDelete {
                id: str_of(&d, "id"),
                parent_id: d.get("parent_id").and_then(Value::as_str).map(str::to_owned),
            },
            "FRIEND_UPDATE" => match decode(&d) {
                Ok(f) => Event::FriendUpdate(f),
                Err(_) => Event::Other { t: t.into(), d },
            },
            "FRIEND_REMOVE" => Event::FriendRemove { user_id: str_of(&d, "user_id") },
            "DM_MESSAGE" | "DM_REQUEST_UPDATE" | "DM_MEMBER_LEAVE" => Event::ConversationActivity {
                conversation_id: d
                    .get("conversation_id")
                    .or_else(|| d.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                message: t == "DM_MESSAGE",
            },
            "GUILD_EMOJI_UPDATE" => Event::EmojiUpdate { guild_id: str_of(&d, "guild_id") },
            "VOICE_STATE_UPDATE" => match decode(&d) {
                Ok(v) => Event::VoiceStateUpdate(v),
                Err(_) => Event::Other { t: t.into(), d },
            },
            _ => Event::Other { t: t.into(), d },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_message_create() {
        let d = json!({"id":"m","channel_id":"c","guild_id":"g","author":"@a:x","content":"yo","created_at":5});
        match Event::parse("MESSAGE_CREATE", d) {
            Event::MessageCreate(m) => assert_eq!(m.content, "yo"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn identify_shape() {
        let v = identify("tok", USER_INTENTS, "online", None);
        assert_eq!(v["op"], "IDENTIFY");
        assert_eq!(v["d"]["intents"], 255);
    }
}
