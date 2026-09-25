//! What a conversation is told about its reader (`web/src/lib/store/signals.js`): read
//! receipts, delivery acknowledgements and typing, each an account setting shared with the web
//! client, which a single conversation can override. Read receipts are off until turned on, and
//! go out once the conversation has been in front of you for a moment.

use super::*;
use crate::conversations::Signals;

/// How long a conversation has to be in front of you, quietly, before its receipt is sent.
const READ_AFTER: Duration = Duration::from_secs(4);

impl Client {
    /// One of `read`, `delivered` or `typing`, for a conversation (its own choice first).
    pub fn signal(&self, kind: &str, conversation: Option<&str>, default: bool) -> bool {
        conversation
            .and_then(|c| self.account_pref(&["signals", "conversations", c, kind]))
            .or_else(|| self.account_pref(&["signals", kind]))
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    }

    pub(super) fn signals_for(&self, conversation: &str) -> Signals {
        Signals { read: self.signal("read", Some(conversation), false), delivered: self.signal("delivered", Some(conversation), true) }
    }

    /// Our newest message in a conversation someone else has read (read receipts on only).
    pub fn dm_seen(&self, conversation: &str) -> Option<&str> {
        self.dm_seen.get(conversation).map(String::as_str)
    }

    /// The conversation on screen, in a window in front: its receipt goes out after a moment
    /// (the latest call wins, so a burst of messages is one receipt).
    pub(super) fn note_dm_read(&mut self, conversation: &str, cx: &mut Context<Self>) {
        if !self.window_active || self.home_conversation.as_deref() != Some(conversation) || !self.signal("read", Some(conversation), false) {
            return;
        }
        let (Some(engine), Some(api)) = (self.dm_engine.clone(), self.home.as_ref().map(|h| h.api.clone())) else { return };
        let (io, id, signals) = (self.io.clone(), conversation.to_owned(), self.signals_for(conversation));
        self.read_flush = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(READ_AFTER).await;
            let still = this.update(cx, |this, _| this.window_active && this.home_conversation.as_deref() == Some(id.as_str())).unwrap_or(false);
            if !still {
                return;
            }
            let sent = join(io.spawn(async move {
                let mut engine = engine.lock().await;
                engine.signals = signals;
                engine.mark_read(&api, &id).await
            }))
            .await;
            if let Err(err) = sent {
                log::info!("read receipt: {err:#}");
            }
        }));
    }
}
