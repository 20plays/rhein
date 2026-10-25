//! Notices about the servers themselves, shown as a strip above the chat until dismissed: a move
//! of this account that is scheduled, a home server shutting down or served from its standby,
//! and a server this build cannot talk to.

use gpui::Context;
use serde_json::Value;

use super::{Client, ServerStatus};

/// What a banner's button does.
#[derive(Debug, Clone, PartialEq)]
pub enum BannerAction {
    /// Cancels a scheduled move (`POST /migrate/{id}/cancel`), possible until it runs.
    CancelMove(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Banner {
    /// Stays the same while the banner means the same thing, so dismissing it sticks.
    pub key: String,
    pub text: String,
    pub urgent: bool,
    pub action: Option<(String, BannerAction)>,
}

/// A time in the local zone, for deadlines.
fn local_time(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .ok()
        .map(|ts| ts.to_zoned(jiff::tz::TimeZone::system()).strftime("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// A scheduled move as a banner: `MIGRATION_ALERT`, or a pending row of `GET /migrate`.
fn move_banner(d: &Value) -> Option<Banner> {
    let id = d["migration_id"].as_str().or(d["id"].as_str())?.to_owned();
    if d["state"].as_str().is_some_and(|s| s != "pending") {
        return None;
    }
    let to = d["destination"].as_str().or(d["to"].as_str()).or(d["new_id"].as_str()).unwrap_or("another server");
    let when = d["executes_at"].as_i64().map(local_time).filter(|w| !w.is_empty());
    let text = match when {
        Some(when) => format!("A move to {to} is scheduled for {when}. If you did not ask for it, cancel it now."),
        None => format!("A move to {to} is scheduled. If you did not ask for it, cancel it now."),
    };
    Some(Banner { key: format!("move:{id}"), text, urgent: true, action: Some(("Cancel move".into(), BannerAction::CancelMove(id))) })
}

impl Client {
    /// The banners to show now, most urgent first, without the dismissed ones.
    pub fn banners(&self) -> Vec<Banner> {
        let mut out = Vec::new();
        out.extend(self.migration_alert.as_ref().and_then(move_banner));
        if let Some(home) = &self.home {
            let doc = &home.doc;
            if let Some(sunset) = doc.sunset.as_ref().filter(|s| !s.is_null() && s["mode"].as_str() != Some("none")) {
                let when = ["shutdown_at", "executes_at", "at"].iter().find_map(|k| sunset[*k].as_i64()).map(local_time);
                let text = match (sunset["message"].as_str(), when) {
                    (Some(message), _) => format!("{} is shutting down: {message}", doc.server_name),
                    (None, Some(when)) => format!("{} shuts down on {when}. Move your account to another server before then.", doc.server_name),
                    (None, None) => format!("{} is shutting down. Move your account to another server.", doc.server_name),
                };
                out.push(Banner { key: format!("sunset:{}", doc.server_name), text, urgent: true, action: None });
            }
            if let Some(authority) = doc.authority.as_deref().filter(|a| !a.is_empty() && *a != doc.server_name) {
                out.push(Banner {
                    key: format!("authority:{authority}"),
                    text: format!("{} is now served by {authority}.", doc.server_name),
                    urgent: false,
                    action: None,
                });
            }
        }
        for server in &self.servers {
            if server.status == ServerStatus::Incompatible {
                let reason = server.error.clone().unwrap_or_else(|| "it speaks a protocol version this build of Rhein does not".into());
                out.push(Banner { key: format!("protocol:{}", server.name), text: format!("Can't connect to {}: {reason}. Update Rhein.", server.name), urgent: true, action: None });
            }
        }
        out.retain(|b| !self.dismissed_banners.contains(&b.key));
        out
    }

    pub fn dismiss_banner(&mut self, key: &str, cx: &mut Context<Self>) {
        self.dismissed_banners.insert(key.to_owned());
        cx.notify();
    }

    pub fn banner_action(&mut self, action: BannerAction, cx: &mut Context<Self>) {
        match action {
            BannerAction::CancelMove(id) => {
                let Some(api) = self.home.as_ref().map(|h| h.api.clone()) else { return };
                let io = self.io.clone();
                cx.spawn(async move |this, cx| {
                    let path = format!("/migrate/{}/cancel", urlencoding::encode(&id));
                    let result = io.spawn(async move { api.post::<_, Value>(&path, &serde_json::json!({})).await }).await;
                    this.update(cx, |this, cx| {
                        match result {
                            Ok(Ok(_)) => {
                                this.migration_alert = None;
                                this.flash_notice("The move was cancelled.", cx);
                            }
                            Ok(Err(err)) => this.flash_notice(format!("Couldn't cancel the move: {err}"), cx),
                            Err(err) => this.flash_notice(format!("Couldn't cancel the move: {err}"), cx),
                        }
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// Asks the home server for moves already scheduled (an alert may have come while this
    /// device was off).
    pub(super) fn load_pending_moves(&mut self, cx: &mut Context<Self>) {
        let Some(api) = self.home.as_ref().map(|h| h.api.clone()) else { return };
        let io = self.io.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(answer)) = io.spawn(async move { api.get::<Value>("/migrate").await }).await else { return };
            let rows = match &answer {
                Value::Array(rows) => rows.clone(),
                other => other["migrations"].as_array().or(other["moves"].as_array()).cloned().unwrap_or_default(),
            };
            if let Some(pending) = rows.into_iter().find(|r| move_banner(r).is_some()) {
                this.update(cx, |this, cx| {
                    this.migration_alert = Some(pending);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn moves() {
        let b = move_banner(&json!({ "id": "m1", "state": "pending", "destination": "new.example" })).unwrap();
        assert_eq!(b.action, Some(("Cancel move".into(), BannerAction::CancelMove("m1".into()))));
        assert!(b.text.contains("new.example"));
        assert!(move_banner(&json!({ "id": "m1", "state": "completed" })).is_none());
        assert!(move_banner(&json!({ "destination": "x" })).is_none());
    }
}
