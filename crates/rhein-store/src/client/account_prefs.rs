//! The account's own preferences, shared with the web client and every other device: the
//! object under `prefs.v1` in the sealed sync store (`web/src/lib/store/prefs.js`). Rhein reads
//! and writes the fields it has settings for and leaves every other one as it found it, so the
//! web's own preferences survive a change made here: each write reads the newest copy first and
//! changes only its one field (last write wins per key, as everywhere in the store).

use serde_json::Value;

use super::*;
use crate::sync_store::{Loaded, PREFS, set_path, update_prefs};

impl Client {
    /// A field of the shared preferences, by path (`["signals", "read"]`).
    pub fn account_pref(&self, path: &[&str]) -> Option<&Value> {
        let mut at = &self.account_prefs;
        for key in path {
            at = at.get(*key)?;
        }
        Some(at)
    }

    /// A boolean preference, with the web client's default where it has none.
    pub fn account_flag(&self, path: &[&str], default: bool) -> bool {
        self.account_pref(path).and_then(Value::as_bool).unwrap_or(default)
    }

    /// Reads the shared preferences (with the GIF favourites, once the account's keys are open).
    pub(super) fn load_account_prefs(&mut self, cx: &mut Context<Self>) {
        let (Some(store), Some(api)) = (self.sync.clone(), self.home.as_ref().map(|h| h.api.clone())) else { return };
        let io = self.io.clone();
        cx.spawn(async move |this, cx| {
            let loaded = join(io.spawn(async move { store.lock().await.load(&api, PREFS).await })).await;
            this.update(cx, |this, cx| match loaded {
                Ok(Loaded::Value(value)) if value.is_object() => {
                    this.account_prefs = value;
                    cx.notify();
                }
                Ok(_) => {}
                Err(err) => log::warn!("account preferences: {err:#}"),
            })
            .ok();
        })
        .detach();
    }

    /// Sets (or with `None` removes) one field of the shared preferences, here at once and on
    /// the account in the background.
    pub fn set_account_pref(&mut self, path: &[&str], value: Option<Value>, cx: &mut Context<Self>) {
        if path.is_empty() {
            return;
        }
        let path: Vec<String> = path.iter().map(|s| (*s).to_owned()).collect();
        set_path(&mut self.account_prefs, &path, value.clone());
        cx.notify();
        let (Some(store), Some(api)) = (self.sync.clone(), self.home.as_ref().map(|h| h.api.clone())) else {
            self.warning = Some("Settings shared with your account apply once this device has your keys (sign in with your password once).".into());
            return;
        };
        let local = self.account_prefs.clone();
        let io = self.io.clone();
        cx.spawn(async move |this, cx| {
            // The newest copy is read first, so a change made on another device meanwhile is kept.
            let saved = join(io.spawn(async move { update_prefs(&mut *store.lock().await, &api, local, &path, value).await })).await;
            this.update(cx, |this, cx| match saved {
                Ok(prefs) => {
                    this.account_prefs = prefs;
                    cx.notify();
                }
                Err(err) => log::warn!("saving account preferences: {err:#}"),
            })
            .ok();
        })
        .detach();
    }
}
