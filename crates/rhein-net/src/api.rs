use std::sync::Arc;

use rhein_proto::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::RwLock;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{status}: {}", .body.message.as_deref().unwrap_or(&.body.error))]
    Status { status: u16, body: ErrorBody },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Status { status, .. } => Some(*status),
            ApiError::Http(e) => e.status().map(|s| s.as_u16()),
        }
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            ApiError::Status { body, .. } => body.code.as_deref(),
            ApiError::Http(_) => None,
        }
    }

    /// For a rate-limited request, how long to wait before repeating it.
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ApiError::Status { status: 429, body } => Some(std::time::Duration::from_secs_f64(body.retry_after.unwrap_or(1.).clamp(0.1, 10.))),
            _ => None,
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// One server's REST surface under one bearer token. Cheap to clone; the token can be swapped in
/// place when a guild session is renewed, so every clone sees the new one.
#[derive(Clone)]
pub struct Api {
    http: reqwest::Client,
    base: Arc<str>,
    token: Arc<RwLock<Option<String>>>,
}

impl Api {
    pub fn new(http: reqwest::Client, base: &str, token: Option<String>) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').into(),
            token: Arc::new(RwLock::new(token)),
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub async fn set_token(&self, token: String) {
        *self.token.write().await = Some(token);
    }

    pub async fn token(&self) -> Option<String> {
        self.token.read().await.clone()
    }

    fn url(&self, path: &str) -> String {
        format!("{}/_telium/v1{}", self.base, path)
    }

    async fn send<T: DeserializeOwned>(&self, req: reqwest::RequestBuilder) -> ApiResult<T> {
        let req = match self.token.read().await.as_deref() {
            Some(token) => req.bearer_auth(token),
            None => req,
        };
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            // Some routes answer 204 or an empty body; decode that as JSON null.
            let bytes = resp.bytes().await?;
            let text = if bytes.is_empty() { &b"null"[..] } else { &bytes[..] };
            return serde_json::from_slice(text).map_err(|e| ApiError::Status {
                status: status.as_u16(),
                body: ErrorBody { error: "decode".into(), code: None, message: Some(e.to_string()), retry_after: None },
            });
        }
        let body = resp.json::<ErrorBody>().await.unwrap_or_else(|_| ErrorBody {
            error: status.canonical_reason().unwrap_or("error").into(),
            ..Default::default()
        });
        Err(ApiError::Status { status: status.as_u16(), body })
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> ApiResult<T> {
        self.send(self.http.get(self.url(path))).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> ApiResult<T> {
        self.send(self.http.post(self.url(path)).json(body)).await
    }

    pub async fn patch<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> ApiResult<T> {
        self.send(self.http.patch(self.url(path)).json(body)).await
    }

    pub async fn put_empty(&self, path: &str) -> ApiResult<serde_json::Value> {
        self.send(self.http.put(self.url(path))).await
    }

    pub async fn delete(&self, path: &str) -> ApiResult<serde_json::Value> {
        self.send(self.http.delete(self.url(path))).await
    }

    pub async fn put<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> ApiResult<T> {
        self.send(self.http.put(self.url(path)).json(body)).await
    }

    // ---- encrypted DMs: keys, trust and frames (home server) ----

    /// The recovery-keyring vault, sealed (`{exists, mode, ciphertext, …}`).
    pub async fn vault(&self) -> ApiResult<serde_json::Value> {
        self.get("/vault").await
    }

    pub async fn put_vault(&self, body: &serde_json::Value) -> ApiResult<serde_json::Value> {
        self.put("/vault", body).await
    }

    /// This account's cross-signing row: identity key, sealed secret, vouched devices.
    pub async fn cross_signing(&self) -> ApiResult<serde_json::Value> {
        self.get("/crosssigning").await
    }

    pub async fn sign_device(&self, device_id: &str, mls_key: &str, signature: &str) -> ApiResult<serde_json::Value> {
        let body = serde_json::json!({ "mls_key": mls_key, "signature": signature });
        self.put(&format!("/crosssigning/devices/{}", urlencoding::encode(device_id)), &body).await
    }

    /// This account's own identity-continuity document.
    pub async fn own_identity(&self) -> ApiResult<serde_json::Value> {
        self.get("/identity").await
    }

    pub async fn anchor_identity(&self, immutable_id: &str, identity_key: &str, signature: &str) -> ApiResult<serde_json::Value> {
        let body = serde_json::json!({ "immutable_id": immutable_id, "identity_key": identity_key, "signature": signature });
        self.put("/identity/anchor", &body).await
    }

    /// Anyone's identity-continuity document, as their home server publishes it.
    pub async fn identity_document(&self, user_id: &str) -> ApiResult<serde_json::Value> {
        self.get(&format!("/users/{}/identity", urlencoding::encode(user_id))).await
    }

    /// Anyone's device list with their cross-signatures.
    pub async fn device_list(&self, user_id: &str) -> ApiResult<serde_json::Value> {
        self.get(&format!("/users/{}/devices", urlencoding::encode(user_id))).await
    }

    /// How many key packages each of this account's devices has left.
    pub async fn key_packages(&self) -> ApiResult<serde_json::Value> {
        self.get("/keypackages").await
    }

    pub async fn publish_key_packages(&self, packages: &[String]) -> ApiResult<serde_json::Value> {
        self.post("/keypackages", &serde_json::json!({ "packages": packages })).await
    }

    /// One key package for each of a user's devices (consumed on the server).
    pub async fn claim_key_packages(&self, user_id: &str) -> ApiResult<serde_json::Value> {
        self.post(&format!("/users/{}/keypackages/claim", urlencoding::encode(user_id)), &serde_json::json!({})).await
    }

    /// Starts a conversation (`kind` is `dm` or `gc`); a DM with someone we already have one with
    /// comes back as that one.
    pub async fn create_conversation(&self, kind: &str, members: &[String], policy: Option<&serde_json::Value>) -> ApiResult<serde_json::Value> {
        let mut body = serde_json::json!({ "kind": kind, "members": members });
        if let Some(policy) = policy {
            body["policy"] = policy.clone();
        }
        self.post("/conversations", &body).await
    }

    pub async fn accept_message_request(&self, conversation_id: &str) -> ApiResult<serde_json::Value> {
        self.post(&format!("/conversations/{}/request/accept", urlencoding::encode(conversation_id)), &serde_json::json!({})).await
    }

    pub async fn decline_message_request(&self, conversation_id: &str) -> ApiResult<serde_json::Value> {
        self.post(&format!("/conversations/{}/request/decline", urlencoding::encode(conversation_id)), &serde_json::json!({})).await
    }

    /// One page of the sealed history mirror for a conversation (`scope`).
    pub async fn backup_page(&self, scope: &str, after: Option<&str>) -> ApiResult<serde_json::Value> {
        let mut path = format!("/vault/backup?scope={}", urlencoding::encode(scope));
        if let Some(after) = after {
            path.push_str(&format!("&after={}", urlencoding::encode(after)));
        }
        self.get(&path).await
    }

    /// Stores sealed mirror rows `{scope, item, ciphertext}`.
    pub async fn put_backup(&self, items: &[serde_json::Value]) -> ApiResult<serde_json::Value> {
        self.put("/vault/backup", &serde_json::json!({ "items": items })).await
    }

    /// Frames of a conversation after `after` (seq), oldest first.
    pub async fn dm_frames(&self, conversation_id: &str, after: Option<u64>, limit: u32) -> ApiResult<serde_json::Value> {
        let mut path = format!("/conversations/{}/messages?limit={limit}", urlencoding::encode(conversation_id));
        if let Some(after) = after {
            path.push_str(&format!("&after={after}"));
        }
        self.get(&path).await
    }

    pub async fn send_frame(&self, conversation_id: &str, frame: &serde_json::Value) -> ApiResult<serde_json::Value> {
        self.post(&format!("/conversations/{}/messages", urlencoding::encode(conversation_id)), frame).await
    }

    // ---- home server ----

    // --- guild voice (`server/src/routes/voice.rs`) ---

    /// Joins a guild voice channel. `key_package` (base64) is this device's offer for the call's
    /// MLS group; an SFU channel refuses a join without one.
    pub async fn voice_join(&self, channel_id: &str, self_mute: bool, self_deaf: bool, self_video: bool, region: Option<&str>, key_package: Option<&str>) -> ApiResult<VoiceJoin> {
        let mut body = serde_json::json!({ "self_mute": self_mute, "self_deaf": self_deaf, "self_video": self_video });
        if let Some(region) = region {
            body["region"] = region.into();
        }
        if let Some(package) = key_package {
            body["key_package"] = package.into();
        }
        self.post(&format!("/channels/{}/voice/join", urlencoding::encode(channel_id)), &body).await
    }

    pub async fn voice_leave(&self, channel_id: &str) -> ApiResult<serde_json::Value> {
        self.post(&format!("/channels/{}/voice/leave", urlencoding::encode(channel_id)), &serde_json::json!({})).await
    }

    /// Changes this user's self flags; `None` leaves one as it is.
    pub async fn voice_state(&self, channel_id: &str, self_mute: Option<bool>, self_deaf: Option<bool>, self_video: Option<bool>) -> ApiResult<VoiceState> {
        let mut body = serde_json::json!({});
        for (key, value) in [("self_mute", self_mute), ("self_deaf", self_deaf), ("self_video", self_video)] {
            if let Some(value) = value {
                body[key] = value.into();
            }
        }
        self.post(&format!("/channels/{}/voice/state", urlencoding::encode(channel_id)), &body).await
    }

    /// A fresh `voice_server` for a call that is still up (the SFU token lasts minutes).
    pub async fn voice_token(&self, channel_id: &str) -> ApiResult<VoiceServer> {
        self.post(&format!("/channels/{}/voice/token", urlencoding::encode(channel_id)), &serde_json::json!({})).await
    }

    /// One mesh relay payload for another participant. A burst of ICE candidates runs into the
    /// write rate limit, and a dropped candidate can cost the connection, so a rate-limited
    /// signal waits and goes again.
    pub async fn voice_signal(&self, channel_id: &str, target: &str, payload: &serde_json::Value) -> ApiResult<serde_json::Value> {
        let body = serde_json::json!({ "target": target, "payload": payload });
        let path = format!("/channels/{}/voice/signal", urlencoding::encode(channel_id));
        self.post_patiently(&path, &body).await
    }

    /// `post`, repeated while the server says to wait (a few times at most).
    pub async fn post_patiently<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> ApiResult<T> {
        let mut tries = 0;
        loop {
            match self.post(path, body).await {
                Err(err) if tries < 8 => match err.retry_after() {
                    Some(wait) => {
                        tries += 1;
                        tokio::time::sleep(wait).await;
                    }
                    None => return Err(err),
                },
                other => return other,
            }
        }
    }

    /// Starts or stops a screen share; `preset` describes it ("detail 1080p@15").
    pub async fn voice_stream(&self, channel_id: &str, enabled: bool, preset: Option<&str>) -> ApiResult<serde_json::Value> {
        let body = serde_json::json!({ "enabled": enabled, "preset": preset });
        self.post(&format!("/channels/{}/voice/stream", urlencoding::encode(channel_id)), &body).await
    }

    /// Starts or stops watching someone's screen share (on an SFU the server has it forwarded).
    pub async fn voice_watch(&self, channel_id: &str, streamer: &str, watching: bool, quality: Option<&str>) -> ApiResult<serde_json::Value> {
        let body = serde_json::json!({ "streamer": streamer, "watching": watching, "quality": quality });
        self.post(&format!("/channels/{}/voice/watch", urlencoding::encode(channel_id)), &body).await
    }

    /// A guild call's MLS group for its committer: `{group_id, epoch, participants: [{user_id,
    /// device_id, member, key_package}]}`.
    pub async fn voice_e2ee(&self, channel_id: &str) -> ApiResult<serde_json::Value> {
        self.get(&format!("/channels/{}/voice/e2ee", urlencoding::encode(channel_id))).await
    }

    /// Posts a commit for the call's group; a 409 means another commit took this epoch.
    pub async fn voice_e2ee_commit(&self, channel_id: &str, body: &serde_json::Value) -> ApiResult<serde_json::Value> {
        self.post(&format!("/channels/{}/voice/e2ee", urlencoding::encode(channel_id)), body).await
    }

    /// Who is in a voice channel: `{channel_id, voice_states, watchers}`.
    pub async fn voice_channel(&self, channel_id: &str) -> ApiResult<serde_json::Value> {
        self.get(&format!("/channels/{}/voice", urlencoding::encode(channel_id))).await
    }

    // --- conversation calls, on the home server (`server/src/routes/dmvoice.rs`) ---

    /// Joins a conversation's call. `wants_sfu` is the one bit the sequencer learns about our
    /// posture; `custom_sfu` says the group names its own SFU.
    pub async fn conversation_voice_join(&self, id: &str, self_mute: bool, self_deaf: bool, wants_sfu: bool, custom_sfu: bool) -> ApiResult<VoiceJoin> {
        let body = serde_json::json!({
            "self_mute": self_mute, "self_deaf": self_deaf, "self_video": false,
            "wants_sfu": wants_sfu, "custom_sfu": custom_sfu,
        });
        self.post(&format!("/conversations/{}/voice/join", urlencoding::encode(id)), &body).await
    }

    pub async fn conversation_voice_leave(&self, id: &str) -> ApiResult<serde_json::Value> {
        self.post(&format!("/conversations/{}/voice/leave", urlencoding::encode(id)), &serde_json::json!({})).await
    }

    /// Changes flags (`self_mute`, `self_deaf`, `self_video`, `streaming`, `wants_sfu`).
    pub async fn conversation_voice_state(&self, id: &str, patch: &serde_json::Value) -> ApiResult<serde_json::Value> {
        self.post(&format!("/conversations/{}/voice/state", urlencoding::encode(id)), patch).await
    }

    /// One relay payload (a mesh signal or a hello) for another participant.
    pub async fn conversation_voice_signal(&self, id: &str, target: &str, payload: &serde_json::Value) -> ApiResult<serde_json::Value> {
        let body = serde_json::json!({ "target": target, "payload": payload });
        self.post_patiently(&format!("/conversations/{}/voice/signal", urlencoding::encode(id)), &body).await
    }

    /// Where the call is now (after a switch to an SFU): `{voice_server}`.
    pub async fn conversation_voice_server(&self, id: &str) -> ApiResult<serde_json::Value> {
        self.post(&format!("/conversations/{}/voice/server", urlencoding::encode(id)), &serde_json::json!({})).await
    }

    /// Who is in a conversation's call.
    pub async fn conversation_voice(&self, id: &str) -> ApiResult<serde_json::Value> {
        self.get(&format!("/conversations/{}/voice", urlencoding::encode(id))).await
    }

    /// The server's SFU regions: `{default, regions: [{name, sfu, healthy, ...}], ...}`.
    pub async fn voice_regions(&self) -> ApiResult<serde_json::Value> {
        self.get("/voice/regions").await
    }

    pub async fn capabilities(&self) -> ApiResult<Capabilities> {
        self.get("/capabilities").await
    }

    pub async fn whoami(&self) -> ApiResult<WhoAmI> {
        self.get("/whoami").await
    }

    pub async fn assertion(&self, audience: &str) -> ApiResult<AssertionResponse> {
        self.post("/assertions", &AssertionRequest { audience }).await
    }

    pub async fn profile(&self, user_id: &str) -> ApiResult<Profile> {
        self.get(&format!("/users/{}/profile", urlencoding::encode(user_id))).await
    }

    /// Uploads to this (home) server's media origin.
    pub async fn upload(&self, filename: &str, mime: &str, bytes: Vec<u8>) -> ApiResult<Attachment> {
        let req = self
            .http
            .post(self.url("/media"))
            .header("Content-Type", mime)
            .header("X-Telium-Filename", urlencoding::encode(filename).into_owned())
            .header("X-Telium-Purpose", "attachment")
            .body(bytes);
        self.send(req).await
    }

    /// Uploads a profile picture; the result's `hash?cap=capability` becomes the `avatar_ref`.
    pub async fn upload_profile_media(&self, filename: &str, mime: &str, bytes: Vec<u8>) -> ApiResult<Attachment> {
        let req = self
            .http
            .post(self.url("/media"))
            .header("Content-Type", mime)
            .header("X-Telium-Filename", urlencoding::encode(filename).into_owned())
            .header("X-Telium-Purpose", "profile")
            .body(bytes);
        self.send(req).await
    }

    pub async fn patch_profile(&self, patch: &serde_json::Value) -> ApiResult<serde_json::Value> {
        self.patch("/profile", patch).await
    }

    pub async fn devices(&self) -> ApiResult<Vec<Device>> {
        Ok(self.get::<DeviceList>("/devices").await?.devices)
    }

    pub async fn revoke_device(&self, device_id: &str) -> ApiResult<serde_json::Value> {
        self.delete(&format!("/devices/{}", urlencoding::encode(device_id))).await
    }

    pub async fn friends(&self) -> ApiResult<Vec<Friend>> {
        Ok(self.get::<FriendList>("/friends").await?.friends)
    }

    pub async fn request_friend(&self, user_id: &str) -> ApiResult<serde_json::Value> {
        self.post("/friends", &serde_json::json!({ "user_id": user_id })).await
    }

    pub async fn accept_friend(&self, user_id: &str) -> ApiResult<serde_json::Value> {
        self.post(&format!("/friends/{}/accept", urlencoding::encode(user_id)), &serde_json::json!({})).await
    }

    /// Removes a friend, declines an incoming request or cancels an outgoing one.
    pub async fn remove_friend(&self, user_id: &str) -> ApiResult<serde_json::Value> {
        self.delete(&format!("/friends/{}", urlencoding::encode(user_id))).await
    }

    pub async fn set_blocked(&self, user_id: &str, blocked: bool) -> ApiResult<serde_json::Value> {
        let path = format!("/friends/{}/block", urlencoding::encode(user_id));
        if blocked { self.put_empty(&path).await } else { self.delete(&path).await }
    }

    pub async fn conversations(&self) -> ApiResult<Vec<Conversation>> {
        Ok(self.get::<ConversationList>("/conversations").await?.conversations)
    }

    // ---- guild server ----

    pub async fn create_session(&self, assertion: &str) -> ApiResult<GuildSession> {
        self.post("/sessions", &SessionRequest { assertion }).await
    }

    pub async fn local_session(&self) -> ApiResult<GuildSession> {
        self.post("/sessions/local", &serde_json::json!({})).await
    }

    pub async fn guilds(&self) -> ApiResult<Vec<Guild>> {
        Ok(self.get::<GuildList>("/guilds").await?.guilds)
    }

    pub async fn create_guild(&self, name: &str) -> ApiResult<Guild> {
        self.post("/guilds", &CreateGuild { name }).await
    }

    pub async fn channels(&self, guild_id: &str) -> ApiResult<Vec<Channel>> {
        Ok(self.get::<ChannelList>(&format!("/guilds/{guild_id}/channels")).await?.channels)
    }

    pub async fn messages(&self, channel_id: &str, before: Option<&str>, limit: u32) -> ApiResult<Vec<Message>> {
        let mut path = format!("/channels/{channel_id}/messages?limit={limit}");
        if let Some(before) = before {
            path.push_str("&before=");
            path.push_str(before);
        }
        Ok(self.get::<MessageList>(&path).await?.messages)
    }

    /// Uploads raw bytes (a pasted image, for example) to the home media origin.
    pub async fn upload_attachment(&self, filename: &str, mime: &str, bytes: Vec<u8>, channel_id: &str, guild_id: &str) -> ApiResult<Attachment> {
        let req = self
            .http
            .post(self.url("/media"))
            .header("Content-Type", mime)
            .header("X-Telium-Filename", urlencoding::encode(filename).into_owned())
            .header("X-Telium-Purpose", "attachment")
            .header("X-Telium-Channel-Id", channel_id)
            .header("X-Telium-Guild-Id", guild_id)
            .body(bytes);
        self.send(req).await
    }

    pub async fn send_message(&self, channel_id: &str, body: &SendMessage) -> ApiResult<Message> {
        self.post(&format!("/channels/{channel_id}/messages"), body).await
    }

    pub async fn edit_message(&self, channel_id: &str, message_id: &str, content: &str) -> ApiResult<serde_json::Value> {
        self.patch(&format!("/channels/{channel_id}/messages/{message_id}"), &EditMessage { content }).await
    }

    pub async fn delete_message(&self, channel_id: &str, message_id: &str) -> ApiResult<serde_json::Value> {
        self.delete(&format!("/channels/{channel_id}/messages/{message_id}")).await
    }

    pub async fn react(&self, message_id: &str, emoji: &str, add: bool) -> ApiResult<serde_json::Value> {
        let path = format!("/messages/{message_id}/reactions/{}/@me", urlencoding::encode(emoji));
        if add { self.put_empty(&path).await } else { self.delete(&path).await }
    }

    pub async fn ack(&self, channel_id: &str, message_id: &str) -> ApiResult<serde_json::Value> {
        self.post(&format!("/channels/{channel_id}/ack"), &Ack { message_id }).await
    }

    /// The guild server's unfurl of a link (`GET /embed?url=`), as raw JSON.
    pub async fn embed(&self, url: &str) -> ApiResult<serde_json::Value> {
        self.get(&format!("/embed?url={}", urlencoding::encode(url))).await
    }

    pub async fn pins(&self, channel_id: &str) -> ApiResult<Vec<Message>> {
        Ok(self.get::<PinList>(&format!("/channels/{channel_id}/pins")).await?.pins)
    }

    pub async fn set_pinned(&self, channel_id: &str, message_id: &str, pinned: bool) -> ApiResult<serde_json::Value> {
        let path = format!("/channels/{channel_id}/pins/{message_id}");
        if pinned { self.put_empty(&path).await } else { self.delete(&path).await }
    }

    pub async fn threads(&self, channel_id: &str, archived: bool) -> ApiResult<Vec<Thread>> {
        Ok(self.get::<ThreadList>(&format!("/channels/{channel_id}/threads?archived={archived}")).await?.threads)
    }

    pub async fn create_thread(&self, channel_id: &str, title: &str, message_id: Option<&str>) -> ApiResult<Thread> {
        self.post(&format!("/channels/{channel_id}/threads"), &CreateThread { title, message_id }).await
    }

    pub async fn search(&self, guild_id: &str, query: &SearchQuery) -> ApiResult<SearchResults> {
        let mut path = format!("/guilds/{guild_id}/search?limit={}&offset={}", query.limit.clamp(1, 100), query.offset);
        for (key, value) in [("q", &query.q), ("channel_id", &query.channel_id), ("author", &query.author), ("has", &query.has)] {
            if let Some(value) = value.as_deref().filter(|v| !v.is_empty()) {
                path.push_str(&format!("&{key}={}", urlencoding::encode(value)));
            }
        }
        self.get(&path).await
    }

    pub async fn expressions(&self, guild_id: &str) -> ApiResult<Vec<CustomEmoji>> {
        Ok(self.get::<Expressions>(&format!("/guilds/{guild_id}/expressions")).await?.emojis)
    }

    /// Server-side member lookup by name, for autocomplete beyond the loaded roster.
    pub async fn search_members(&self, guild_id: &str, q: &str, limit: u32) -> ApiResult<MemberList> {
        self.get(&format!("/guilds/{guild_id}/members?q={}&limit={limit}", urlencoding::encode(q))).await
    }

    pub async fn patch_guild(&self, guild_id: &str, patch: &serde_json::Value) -> ApiResult<serde_json::Value> {
        self.patch(&format!("/guilds/{guild_id}"), patch).await
    }

    pub async fn leave_guild(&self, guild_id: &str) -> ApiResult<serde_json::Value> {
        self.post(&format!("/guilds/{guild_id}/leave"), &serde_json::json!({})).await
    }

    pub async fn create_channel(&self, guild_id: &str, name: &str, kind: &str, parent_id: Option<&str>) -> ApiResult<Channel> {
        self.post(
            &format!("/guilds/{guild_id}/channels"),
            &serde_json::json!({ "name": name, "type": kind, "parent_id": parent_id }),
        )
        .await
    }

    pub async fn patch_channel(&self, channel_id: &str, patch: &serde_json::Value) -> ApiResult<serde_json::Value> {
        self.patch(&format!("/channels/{channel_id}"), patch).await
    }

    pub async fn delete_channel(&self, channel_id: &str) -> ApiResult<serde_json::Value> {
        self.delete(&format!("/channels/{channel_id}")).await
    }

    pub async fn patch_member(&self, guild_id: &str, user_id: &str, patch: &serde_json::Value) -> ApiResult<serde_json::Value> {
        self.patch(&format!("/guilds/{guild_id}/members/{}", urlencoding::encode(user_id)), patch).await
    }

    pub async fn kick_member(&self, guild_id: &str, user_id: &str) -> ApiResult<serde_json::Value> {
        self.delete(&format!("/guilds/{guild_id}/members/{}", urlencoding::encode(user_id))).await
    }

    pub async fn ban_member(&self, guild_id: &str, user_id: &str, reason: Option<&str>) -> ApiResult<serde_json::Value> {
        let req = self
            .http
            .put(self.url(&format!("/guilds/{guild_id}/bans/{}", urlencoding::encode(user_id))))
            .json(&serde_json::json!({ "reason": reason }));
        self.send(req).await
    }

    pub async fn read_state(&self, guild_id: &str) -> ApiResult<ReadStateList> {
        self.get(&format!("/guilds/{guild_id}/read-state")).await
    }

    pub async fn members(&self, guild_id: &str, offset: u32, limit: u32) -> ApiResult<MemberList> {
        self.get(&format!("/guilds/{guild_id}/members?offset={offset}&limit={limit}")).await
    }

    pub async fn join_invite(&self, code: &str) -> ApiResult<serde_json::Value> {
        self.post(&format!("/invites/{}/join", urlencoding::encode(code)), &serde_json::json!({})).await
    }

    /// Answers the invite object; its `code` is what `join_invite` takes.
    pub async fn create_invite(&self, guild_id: &str, channel_id: Option<&str>) -> ApiResult<serde_json::Value> {
        self.post(&format!("/guilds/{guild_id}/invites"), &serde_json::json!({ "channel_id": channel_id })).await
    }
}
