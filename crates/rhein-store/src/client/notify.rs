//! Alerts, as the web client gives them (`web/src/lib/notify-sound.js`,
//! `views/user-settings/NotificationsPane.jsx`), each behind the account's shared switch:
//! - a desktop notification (the freedesktop notification service) for a mention or a private
//!   message while this window is not in front, its text shown in full, as the sender only, or
//!   not at all;
//! - the ping for those, and a softer sound for any other message, both opt-in;
//! - the ringtone for a call starting in one of your conversations, on unless turned off, for
//!   thirty seconds or until it is answered, declined or over.
//!
//! Nothing alerts under "do not disturb".

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use rhein_voice::playback::Playback;

use super::*;

const PING: &[u8] = include_bytes!("../../assets/sounds/ping.mp3");
const MESSAGE: &[u8] = include_bytes!("../../assets/sounds/message.mp3");
const RING: &[u8] = include_bytes!("../../assets/sounds/ring.mp3");

/// The quiet between two sounds of a kind (a burst is heard once).
const PING_GAP: Duration = Duration::from_millis(1200);
const MESSAGE_GAP: Duration = Duration::from_millis(3000);
/// How long an unanswered call rings.
const RING_FOR: Duration = Duration::from_secs(30);

/// What an alert is about.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    Mention,
    Direct,
}

/// A call ringing in a conversation.
pub struct Ringing {
    pub conversation: String,
    pub from: String,
    _task: Task<()>,
}

#[derive(Default)]
pub struct Alerts {
    last_ping: Option<Instant>,
    last_message: Option<Instant>,
    /// The sound playing now (dropping it stops it).
    playing: Option<Arc<Playback>>,
    pub ringing: Option<Ringing>,
    /// Conversations whose ringing call was declined, until that call is over.
    declined: HashSet<String>,
}

impl Client {
    /// Whether this window is in front (alerts only come while it is not).
    pub fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        let woke = active && !self.window_active;
        self.window_active = active;
        // Back after a while (a laptop lid, say): sessions that ran out meanwhile are renewed.
        if woke {
            self.freshen_sessions(cx);
        }
        // Back in front of an open conversation: it has been read.
        if let Some(id) = self.home_conversation.clone().filter(|_| active) {
            self.note_dm_read(&id, cx);
        }
    }

    fn quiet(&self) -> bool {
        self.prefs.status == "dnd"
    }

    /// A mention or a private message arrived: a desktop notification and the ping, as the
    /// switches allow.
    pub(super) fn alert(&mut self, kind: Alert, sender: &str, place: Option<String>, text: &str, cx: &mut Context<Self>) {
        if self.quiet() {
            return;
        }
        let wanted = match kind {
            Alert::Mention => self.account_flag(&["notifyMentions"], true),
            Alert::Direct => self.account_flag(&["notifyDms"], true),
        };
        if wanted && !self.window_active {
            let preview = self.account_pref(&["notificationPreview"]).and_then(|v| v.as_str()).unwrap_or("full").to_owned();
            let (title, body) = match preview.as_str() {
                "hidden" => ("Rhein".to_owned(), "New message".to_owned()),
                "sender" => (sender.to_owned(), "New message".to_owned()),
                _ => (
                    match &place {
                        Some(p) => format!("{sender} ({p})"),
                        None => sender.to_owned(),
                    },
                    shorten(text),
                ),
            };
            self.io.spawn(async move {
                if let Err(err) = desktop_notification(&title, &body).await {
                    log::debug!("desktop notification: {err:#}");
                }
            });
        }
        if self.account_flag(&["notifySounds"], false) && !gap(&mut self.alerts.last_ping, PING_GAP) {
            self.play(PING, cx);
        }
    }

    /// Any other message from someone else, in a channel not in front of you.
    pub(super) fn message_sound(&mut self, cx: &mut Context<Self>) {
        if self.quiet() || !self.account_flag(&["notifySounds"], false) || !self.account_flag(&["notifySoundMessages"], false) {
            return;
        }
        // A ping just now outranks it.
        if self.alerts.last_ping.is_some_and(|t| t.elapsed() < MESSAGE_GAP) || gap(&mut self.alerts.last_message, MESSAGE_GAP) {
            return;
        }
        self.play(MESSAGE, cx);
    }

    fn play(&mut self, sound: &'static [u8], cx: &mut Context<Self>) {
        let voice = self.voice_prefs();
        let device = voice.null_devices.then(|| "alsa:null".to_owned()).or(voice.output.clone());
        let io = self.io.clone();
        cx.spawn(async move |this, cx| {
            let started = join(io.spawn(async move { tokio::task::spawn_blocking(move || Playback::start(sound.to_vec(), Some("mp3"), device)).await? })).await;
            match started {
                Ok(playback) => {
                    this.update(cx, |this, _| this.alerts.playing = Some(Arc::new(playback))).ok();
                }
                Err(err) => log::debug!("alert sound: {err:#}"),
            }
        })
        .detach();
    }

    /// The call ringing now, if any: the conversation and who is calling.
    pub fn ringing(&self) -> Option<(&str, &str)> {
        self.alerts.ringing.as_ref().map(|r| (r.conversation.as_str(), r.from.as_str()))
    }

    /// Stops the ringing without answering; it stays quiet until that call is over.
    pub fn decline_ring(&mut self, cx: &mut Context<Self>) {
        if let Some(ring) = self.alerts.ringing.take() {
            self.alerts.declined.insert(ring.conversation);
        }
        self.alerts.playing = None;
        cx.notify();
    }

    fn stop_ring(&mut self, conversation: &str, cx: &mut Context<Self>) {
        if self.alerts.ringing.as_ref().is_some_and(|r| r.conversation == conversation) {
            self.alerts.ringing = None;
            self.alerts.playing = None;
            cx.notify();
        }
    }

    /// A conversation's call roster changed: `before` and `after` are how many were in it.
    pub(super) fn ring_on_roster(&mut self, state: &rhein_proto::VoiceState, before: usize, after: usize, cx: &mut Context<Self>) {
        let Some(conversation) = state.conversation_id.clone() else { return };
        let me = self.user_id().map(str::to_owned);
        if me.as_deref() == Some(state.user_id.as_str()) {
            if state.channel_id.is_some() {
                self.stop_ring(&conversation, cx);
            }
            return;
        }
        if after == 0 {
            self.alerts.declined.remove(&conversation);
            self.stop_ring(&conversation, cx);
            return;
        }
        if before > 0 || self.alerts.declined.contains(&conversation) || self.alerts.ringing.is_some() || self.quiet() || self.is_request(&conversation) {
            return;
        }
        if self.call.as_ref().is_some_and(|c| c.room_id == conversation) {
            return;
        }
        let name = self.display_name(None, &state.user_id);
        let conv = conversation.clone();
        let ring = self.account_flag(&["notifyCallRing"], true);
        let task = cx.spawn(async move |this, cx| {
            let started = Instant::now();
            while started.elapsed() < RING_FOR {
                let going = this
                    .update(cx, |this, cx| {
                        let still = this.alerts.ringing.as_ref().is_some_and(|r| r.conversation == conv);
                        // The ringtone again whenever it has played out.
                        if still && ring && this.alerts.playing.as_ref().is_none_or(|p| p.ended()) {
                            this.play(RING, cx);
                        }
                        still
                    })
                    .unwrap_or(false);
                if !going {
                    return;
                }
                cx.background_executor().timer(Duration::from_millis(500)).await;
            }
            // Missed.
            this.update(cx, |this, cx| this.stop_ring(&conv, cx)).ok();
        });
        self.alerts.ringing = Some(Ringing { conversation, from: state.user_id.clone(), _task: task });
        if !self.window_active {
            let body = format!("{name} is calling you");
            self.io.spawn(async move {
                if let Err(err) = desktop_notification("Incoming call", &body).await {
                    log::debug!("desktop notification: {err:#}");
                }
            });
        }
        cx.notify();
    }
}

/// Whether the last time is within `gap` (and if not, now becomes the last time).
fn gap(last: &mut Option<Instant>, gap: Duration) -> bool {
    if last.is_some_and(|t| t.elapsed() < gap) {
        return true;
    }
    *last = Some(Instant::now());
    false
}

fn shorten(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() > 200 { format!("{}…", line.chars().take(200).collect::<String>()) } else { line }
}

/// Shows a notification through the desktop's notification service.
async fn desktop_notification(title: &str, body: &str) -> anyhow::Result<()> {
    use std::collections::HashMap;
    let connection = zbus::Connection::session().await?;
    let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::from([("desktop-entry", zbus::zvariant::Value::from("cc.rheinium.Rhein"))]);
    connection
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &("Rhein", 0u32, "", title, body, Vec::<&str>::new(), hints, -1i32),
        )
        .await?;
    Ok(())
}
