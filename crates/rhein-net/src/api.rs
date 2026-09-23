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
                body: ErrorBody { error: "decode".into(), code: None, message: Some(e.to_string()) },
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

    // ---- home server ----

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
