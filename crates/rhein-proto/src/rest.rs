use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Attachment, Channel, Guild, Member, Message, Millis, Profile};

/// `GET /.well-known/telium/server`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Discovery {
    pub server_name: String,
    pub api: String,
    pub gateway: Option<String>,
    pub client: Option<String>,
    pub fallback: Option<String>,
    pub moved_to: Option<String>,
    pub relay: Option<Value>,
    pub protocol: Option<ProtocolWindow>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProtocolWindow {
    pub version: u32,
    pub min_supported: u32,
    pub capabilities: Vec<String>,
}

/// `GET /_telium/v1/capabilities`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub server_name: String,
    pub gateway: Option<String>,
    pub roles: Value,
    pub registration_open: bool,
    pub registration_min_password_len: Option<u32>,
    pub auth: AuthCapabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthCapabilities {
    pub opaque: bool,
    pub recovery_key_reset: bool,
    pub protocol: Option<String>,
}

/// The `{error, code, message}` body every non-2xx answer carries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ErrorBody {
    pub error: String,
    pub code: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisterStart<'a> {
    pub localpart: &'a str,
    pub registration_request: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegisterStartResponse {
    pub registration_response: String,
    #[serde(default)]
    pub min_password_len: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisterFinish<'a> {
    pub localpart: &'a str,
    pub registration_record: String,
    pub device_name: &'a str,
    pub device_pubkey: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginStart<'a> {
    pub user: &'a str,
    pub start_login_request: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoginStartResponse {
    pub login_id: String,
    pub login_response: String,
    #[serde(default)]
    pub legacy_upgrade_required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginFinish<'a> {
    pub login_id: &'a str,
    pub finish_login_request: String,
    pub device_name: &'a str,
    pub device_pubkey: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mfa_code: Option<&'a str>,
}

/// What both `register/opaque/finish` and `login/opaque/finish` answer with.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HomeSession {
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
    pub expires_at: Option<Millis>,
    pub server_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WhoAmI {
    pub user_id: String,
    pub device_id: String,
    pub server_name: String,
    pub admin: bool,
    pub profile: Option<Profile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AssertionRequest<'a> {
    pub audience: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssertionResponse {
    pub assertion: String,
    #[serde(default)]
    pub expires_at: Option<Millis>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionRequest<'a> {
    pub assertion: &'a str,
}

/// `POST /_telium/v1/sessions` on a guild server.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GuildSession {
    pub session_token: String,
    pub user_id: String,
    pub expires_at: Option<Millis>,
    pub revalidate_at: Option<Millis>,
    pub gateway: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GuildList {
    pub guilds: Vec<Guild>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ChannelList {
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MessageList {
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct MemberList {
    pub members: Vec<Member>,
    pub member_count: Option<i64>,
    pub online_count: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SendMessage {
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditMessage<'a> {
    pub content: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateGuild<'a> {
    pub name: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ack<'a> {
    pub message_id: &'a str,
}

/// `GET /guilds/{id}/read-state`: per channel, how far this user has read.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ReadStateList {
    pub guild_id: String,
    pub channels: Vec<ChannelReadState>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ChannelReadState {
    pub channel_id: String,
    pub last_read_id: Option<String>,
    pub muted: bool,
    pub unread: u32,
    pub mentions: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PinList {
    pub pins: Vec<Message>,
}

/// A thread: a channel of type `thread` under a text channel.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Thread {
    pub id: String,
    pub parent_id: String,
    pub guild_id: Option<String>,
    pub starter_id: Option<String>,
    pub owner: Option<String>,
    pub title: String,
    pub archived: bool,
    pub locked: bool,
    pub message_count: i64,
    pub last_active: Option<Millis>,
    pub created_at: Option<Millis>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ThreadList {
    pub threads: Vec<Thread>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateThread<'a> {
    pub title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<&'a str>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub channel_id: Option<String>,
    pub author: Option<String>,
    pub has: Option<String>,
    pub offset: u32,
    pub limit: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SearchResults {
    pub results: Vec<Message>,
    pub count: u32,
    pub total: u32,
    pub has_more: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomEmoji {
    pub id: String,
    pub name: String,
    pub animated: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Expressions {
    pub emojis: Vec<CustomEmoji>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Device {
    pub device_id: String,
    pub name: Option<String>,
    pub created_at: Option<Millis>,
    pub last_seen: Option<Millis>,
    pub active: bool,
    pub current: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DeviceList {
    pub devices: Vec<Device>,
}
