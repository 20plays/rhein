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
/// Message text for bot sessions; people always receive it.
pub const INTENT_MESSAGE_CONTENT: u64 = 1 << 8;

/// Every intent a user (not bot) session may ask for, which is what the web client sends.
pub const USER_INTENTS: u64 = 0xff;

/// The protocol window this client speaks (`docs.telium.chat/reference/compatibility`).
pub const PROTOCOL_VERSION: u32 = 1;
pub const MIN_SUPPORTED_PROTOCOL_VERSION: u32 = 1;

/// Whether a peer's advertised window overlaps ours. A peer that advertises nothing is version 1,
/// minimum 1.
pub fn compatible(peer: Option<&crate::rest::ProtocolWindow>) -> bool {
    let (version, min) = peer.map_or((1, 1), |w| (w.version.max(1), w.min_supported.max(1)));
    min <= PROTOCOL_VERSION && version >= MIN_SUPPORTED_PROTOCOL_VERSION
}

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
    pub protocol: Option<crate::rest::ProtocolWindow>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReadyUser {
    pub user_id: String,
    pub device_id: Option<String>,
    pub is_bot: bool,
    pub application_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Ready {
    pub session_id: Value,
    pub user: ReadyUser,
    pub server_name: String,
    pub guilds: Vec<Guild>,
    pub protocol: Option<crate::rest::ProtocolWindow>,
    /// The intents the server granted (bot sessions are masked).
    pub intents: Option<u64>,
    /// Calls named in IDENTIFY that the server kept for this device across its restart.
    pub voice_reclaimed: Value,
}

/// What this connection says about the user.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PresenceState {
    /// online, idle, dnd or invisible.
    pub status: String,
    pub custom_status: Option<String>,
    /// `None` leaves activity to whichever other session reports it (the server keeps the last
    /// one); `Some` is ours, and an empty list clears it.
    pub activities: Option<Vec<crate::Activity>>,
}

impl PresenceState {
    pub fn to_json(&self) -> Value {
        let mut d = json!({ "status": self.status, "custom_status": self.custom_status });
        if let Some(list) = &self.activities {
            // `activity` carries the first one as the v1 envelope for older servers and clients.
            d["activity"] = json!(list.first().and_then(crate::activity::encode_envelope));
            d["rich_activities"] = Value::Array(list.iter().map(crate::Activity::to_value).collect());
        }
        d
    }
}

/// A call this device believes it is still in, named in IDENTIFY so a restarted server keeps
/// its seat (`docs/guild-calls.md`, "Surviving a restart").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceClaim {
    /// The guild for a guild call, the conversation for a DM call.
    pub room_id: String,
    pub session_id: String,
}

pub fn identify(token: &str, intents: u64, presence: &PresenceState, voice: &[VoiceClaim]) -> Value {
    let mut d = json!({
        "token": token,
        "intents": intents,
        "presence": presence.to_json(),
        "protocol": { "version": PROTOCOL_VERSION, "min_supported": MIN_SUPPORTED_PROTOCOL_VERSION, "capabilities": [] },
    });
    if !voice.is_empty() {
        d["voice"] = voice.iter().map(|c| json!({ "room_id": c.room_id, "session_id": c.session_id })).collect();
    }
    json!({ "op": "IDENTIFY", "d": d })
}

pub fn presence(presence: &PresenceState) -> Value {
    json!({ "op": "PRESENCE_UPDATE", "d": presence.to_json() })
}

pub fn heartbeat() -> Value {
    json!({ "op": "HEARTBEAT" })
}

pub fn dm_typing(conversation_id: &str, active: bool) -> Value {
    json!({ "op": if active { "DM_TYPING_START" } else { "DM_TYPING_STOP" }, "d": { "conversation_id": conversation_id } })
}

pub fn typing(channel_id: &str, active: bool) -> Value {
    json!({ "op": if active { "TYPING" } else { "TYPING_STOP" }, "d": { "channel_id": channel_id } })
}

/// Asks for the channels this session may see now; answered by `CHANNEL_VISIBILITY`.
pub fn request_guild_channels(guild_id: &str) -> Value {
    json!({ "op": "REQUEST_GUILD_CHANNELS", "d": { "guild_id": guild_id } })
}

pub fn subscribe_members(guild_id: &str, first: u32, last: u32) -> Value {
    json!({ "op": "SUBSCRIBE", "d": { "guild_id": guild_id, "member_ranges": [[first, last]] } })
}

/// A `DISPATCH` the store understands; anything else stays `Other` and is ignored.
#[derive(Debug, Clone)]
pub enum Event {
    MessageCreate(crate::model::Message),
    /// An edit; a bot's update may also replace its components and cards (absent: unchanged).
    MessageUpdate {
        id: String,
        channel_id: String,
        content: String,
        edited_at: Option<i64>,
        components: Option<Vec<crate::model::Component>>,
        cards: Option<Vec<crate::model::Card>>,
    },
    /// A bot took a press and will answer later: the presser sees it pending.
    InteractionDeferred { channel_id: String, message_id: String },
    /// A bot answered a press with a form for us to fill in.
    ModalOpen(Value),
    /// A message only we see was rewritten (or first shown).
    EphemeralUpdate(crate::model::Message),
    MessageDelete { id: String, channel_id: String },
    ReactionAdd { message_id: String, channel_id: String, emoji: String, user_id: String },
    ReactionRemove { message_id: String, channel_id: String, emoji: String, user_id: String },
    TypingStart { channel_id: String, user_id: String },
    /// Someone stopped typing (in a channel, or a conversation keyed as one).
    TypingStop { channel_id: String, user_id: String },
    /// What this session may see in a guild changed: its channels are fetched again.
    ChannelVisibility { guild_id: String },
    /// The server ended this session (a revoked device, signed out everywhere, a moved account).
    SessionRevoked(Value),
    /// AutoMod acted on a message (moderators only).
    AutomodAction(Value),
    /// A move of this account or an owned guild was scheduled or retried.
    MigrationAlert(Value),
    /// The account's recovery email changed state (confirmed, bounced).
    RecoveryEmailUpdate(Value),
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
    /// Where to dial a call: the join answer again, pushed on region failover.
    VoiceServerUpdate(Value),
    /// A mesh relay payload from another participant.
    VoiceSignal { channel_id: String, from: String, payload: Value },
    /// A guild call's MLS group moved: `{op: "commit" | "reset", group_id, epoch, ...}`.
    VoiceE2ee { channel_id: String, data: Value },
    /// A channel's whole screen-watch table.
    VoiceWatchersUpdate { channel_id: String, watchers: Vec<crate::model::VoiceWatcher> },
    PinsUpdate { channel_id: String, message_id: String, pinned: bool },
    ThreadUpsert(crate::rest::Thread),
    ThreadDelete { id: String, parent_id: Option<String> },
    EmojiUpdate { guild_id: String },
    /// A role was created, changed, moved or deleted.
    RoleChange { guild_id: String },
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
                components: d.get("components").and_then(|c| decode(c).ok()),
                cards: d.get("cards").and_then(|c| decode(c).ok()),
            },
            "INTERACTION_DEFERRED" => Event::InteractionDeferred {
                channel_id: str_of(&d, "channel_id"),
                message_id: str_of(&d, "message_id"),
            },
            "MODAL_OPEN" => Event::ModalOpen(d),
            "EPHEMERAL_MESSAGE_UPDATE" | "EPHEMERAL_MESSAGE_CREATE" => match decode::<crate::model::Message>(&d) {
                Ok(mut m) => {
                    m.ephemeral = true;
                    Event::EphemeralUpdate(m)
                }
                Err(_) => Event::Other { t: t.into(), d },
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
            // Typing in an encrypted conversation is a plaintext gateway signal, keyed by the
            // conversation as a channel would be.
            "DM_TYPING_START" => Event::TypingStart {
                channel_id: str_of(&d, "conversation_id"),
                user_id: str_of(&d, "user_id"),
            },
            "TYPING_STOP" => Event::TypingStop {
                channel_id: str_of(&d, "channel_id"),
                user_id: str_of(&d, "user_id"),
            },
            "CHANNEL_VISIBILITY" => Event::ChannelVisibility { guild_id: str_of(&d, "guild_id") },
            "SESSION_REVOKED" => Event::SessionRevoked(d),
            "AUTOMOD_ACTION" => Event::AutomodAction(d),
            "MIGRATION_ALERT" => Event::MigrationAlert(d),
            "RECOVERY_EMAIL_UPDATE" => Event::RecoveryEmailUpdate(d),
            "DM_TYPING_STOP" => Event::TypingStop {
                channel_id: str_of(&d, "conversation_id"),
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
            "ROLE_CREATE" | "ROLE_UPDATE" | "ROLE_DELETE" => Event::RoleChange { guild_id: str_of(&d, "guild_id") },
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
            "VOICE_SERVER_UPDATE" => Event::VoiceServerUpdate(d),
            "VOICE_E2EE" => Event::VoiceE2ee { channel_id: str_of(&d, "channel_id"), data: d },
            "VOICE_SIGNAL" => Event::VoiceSignal {
                channel_id: str_of(&d, "channel_id"),
                from: str_of(&d, "from"),
                payload: d.get("payload").cloned().unwrap_or(Value::Null),
            },
            "VOICE_WATCHERS_UPDATE" => Event::VoiceWatchersUpdate {
                channel_id: str_of(&d, "channel_id"),
                watchers: d.get("watchers").and_then(|w| decode(w).ok()).unwrap_or_default(),
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
        let v = identify("tok", USER_INTENTS, &PresenceState { status: "online".into(), ..Default::default() }, &[]);
        assert_eq!(v["op"], "IDENTIFY");
        assert_eq!(v["d"]["intents"], 255);
        assert_eq!(v["d"]["protocol"]["min_supported"], MIN_SUPPORTED_PROTOCOL_VERSION);
        // Without activities of our own, the field is left out so another session's stay.
        assert!(v["d"]["presence"].get("activity").is_none());
        assert!(v["d"].get("voice").is_none());
        let claim = VoiceClaim { room_id: "g1".into(), session_id: "s1".into() };
        let v = identify("tok", USER_INTENTS, &PresenceState::default(), &[claim]);
        assert_eq!(v["d"]["voice"], json!([{ "room_id": "g1", "session_id": "s1" }]));
    }

    #[test]
    fn protocol_windows() {
        use crate::rest::ProtocolWindow;
        let w = |version, min_supported| ProtocolWindow { version, min_supported, capabilities: Vec::new() };
        assert!(compatible(None));
        assert!(compatible(Some(&w(1, 1))));
        assert!(compatible(Some(&w(3, 1))));
        assert!(!compatible(Some(&w(3, 2))));
        // Nothing advertised counts as version 1.
        assert!(compatible(Some(&w(0, 0))));
    }

    #[test]
    fn presence_carries_activities_both_ways() {
        let game = crate::Activity::normalize(&json!({ "name": "Factorio" })).unwrap();
        let mut state = PresenceState { status: "online".into(), activities: Some(vec![game]), ..Default::default() };
        let d = presence(&state)["d"].clone();
        assert!(d["activity"].as_str().unwrap().starts_with(crate::activity::ENVELOPE_PREFIX));
        assert_eq!(d["rich_activities"][0]["name"], "Factorio");
        // An empty list is an explicit clear.
        state.activities = Some(Vec::new());
        let d = presence(&state)["d"].clone();
        assert!(d["activity"].is_null());
        assert_eq!(d["rich_activities"], json!([]));
    }
}
