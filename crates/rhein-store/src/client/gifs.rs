//! GIF favourites: kept in the account's sealed sync store (`gif.favorites.v1`, shared with the
//! web client and every other device), loaded once the account's keys are open on this device.

use std::sync::Arc;

use tokio::sync::Mutex;

use super::*;
use crate::gif_links;
use crate::sync_store::{GIF_FAVORITES, Loaded, MAX_GIF_FAVORITES, SyncStore, clean_favorites};

impl Client {
    /// The account's favourite GIFs, newest first.
    pub fn gif_favorites(&self) -> &[String] {
        &self.gif_favorites
    }

    pub fn is_gif_favorite(&self, url: &str) -> bool {
        self.gif_favorites.iter().any(|u| u == url)
    }

    /// Whether favourites can be changed yet (the account's keys are open here).
    pub fn gif_favorites_ready(&self) -> bool {
        self.sync.is_some()
    }

    /// Opens the sync store with the recovery keyring and reads the favourites.
    pub(super) fn load_gif_favorites(&mut self, cx: &mut Context<Self>) {
        let Some(home) = &self.home else { return };
        let Some(ring) = crate::dm::Secrets::load(&home.user_id).keyring else { return };
        let (api, io) = (home.api.clone(), self.io.clone());
        let store = Arc::new(Mutex::new(SyncStore::new(&home.user_id, &ring)));
        self.sync = Some(store.clone());
        cx.spawn(async move |this, cx| {
            let loaded = join(io.spawn(async move { store.lock().await.load(&api, GIF_FAVORITES).await })).await;
            this.update(cx, |this, cx| match loaded {
                Ok(Loaded::Value(value)) => {
                    this.gif_favorites = clean_favorites(&value);
                    cx.notify();
                }
                Ok(Loaded::Missing) => {}
                // An older copy than this device has seen: ours stands, and is written back.
                Ok(Loaded::RolledBack) => this.save_gif_favorites(cx),
                Err(err) => log::warn!("GIF favourites: {err:#}"),
            })
            .ok();
        })
        .detach();
    }

    /// Adds a GIF to the favourites, or takes it out again.
    pub fn toggle_gif_favorite(&mut self, url: &str, cx: &mut Context<Self>) {
        if self.sync.is_none() {
            self.warning = Some("Favourites are kept encrypted with your account; they open once this device has your keys (sign in with your password once).".into());
            cx.notify();
            return;
        }
        if let Some(at) = self.gif_favorites.iter().position(|u| u == url) {
            self.gif_favorites.remove(at);
        } else if url.starts_with("https://") {
            self.gif_favorites.insert(0, url.to_owned());
            self.gif_favorites.truncate(MAX_GIF_FAVORITES);
        }
        self.save_gif_favorites(cx);
        cx.notify();
    }

    fn save_gif_favorites(&mut self, cx: &mut Context<Self>) {
        let (Some(store), Some(api)) = (self.sync.clone(), self.home.as_ref().map(|h| h.api.clone())) else { return };
        let value = serde_json::json!(self.gif_favorites);
        let io = self.io.clone();
        cx.spawn(async move |_, _| {
            if let Err(err) = join(io.spawn(async move { store.lock().await.save(&api, GIF_FAVORITES, &value).await })).await {
                log::warn!("saving GIF favourites: {err:#}");
            }
        })
        .detach();
    }
}

/// A GIF share page on its way to being a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GifShare {
    Loading,
    Ready(String),
    /// The provider had no GIF for it; the page is offered as a link.
    Failed,
}

impl Client {
    /// What a GIF share page turned out to be; `None` until someone asks its provider
    /// (`request_gif_share`).
    pub fn gif_share(&self, url: &str) -> Option<GifShare> {
        if let Some(gif_links::Share::Giphy(id)) = gif_links::share(url) {
            return Some(GifShare::Ready(gif_links::giphy_media(&id)));
        }
        self.gif_shares.get(url).cloned()
    }

    /// Asks a share page's provider for its GIF, once.
    pub fn request_gif_share(&mut self, url: &str, cx: &mut Context<Self>) {
        if self.gif_share(url).is_some() {
            return;
        }
        let Some(share) = gif_links::share(url) else { return };
        self.gif_shares.insert(url.to_owned(), GifShare::Loading);
        let (io, http, api) = (self.io.clone(), self.http.clone(), self.home.as_ref().map(|h| h.api.clone()));
        let key = url.to_owned();
        cx.spawn(async move |this, cx| {
            let found = join(io.spawn(async move { resolve_share(&http, api, &share).await })).await;
            this.update(cx, |this, cx| {
                let state = match found {
                    Ok(Some(media)) => GifShare::Ready(media),
                    Ok(None) => GifShare::Failed,
                    Err(err) => {
                        log::debug!("resolving {key}: {err:#}");
                        GifShare::Failed
                    }
                };
                this.gif_shares.insert(key, state);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

/// Asks Tenor or Klipy (Klipy with the key the home server publishes) for a share page's GIF.
async fn resolve_share(http: &reqwest::Client, api: Option<rhein_net::Api>, share: &gif_links::Share) -> anyhow::Result<Option<String>> {
    let get = |url: String| async move {
        let answer = http.get(url).header("Accept", "application/json").send().await?.error_for_status()?;
        anyhow::Ok(answer.json::<serde_json::Value>().await?)
    };
    match share {
        gif_links::Share::Giphy(id) => Ok(Some(gif_links::giphy_media(id))),
        gif_links::Share::Tenor(id) => {
            let url = format!(
                "{}/posts?ids={id}&key={}&client_key={}&media_filter=gif,mediumgif,tinygif,nanogif",
                gif_links::TENOR_API_BASE,
                gif_links::TENOR_API_KEY,
                gif_links::TENOR_CLIENT_KEY
            );
            Ok(gif_links::tenor_media(&get(url).await?, id))
        }
        gif_links::Share::Klipy(slug) => {
            let Some(api) = api else { return Ok(None) };
            let caps: serde_json::Value = api.get("/capabilities").await.map_err(|e| anyhow!("{e}"))?;
            let provider = &caps["gif_provider"];
            let (Some(key), true) = (provider["api_key"].as_str().filter(|k| !k.is_empty()), provider["enabled"].as_bool() != Some(false)) else {
                return Ok(None);
            };
            let base = provider["api_base_url"].as_str().unwrap_or("https://api.klipy.com/api/v1").trim_end_matches('/');
            let url = format!(
                "{base}/{}/gifs/search?page=1&limit=24&per_page=24&q={}",
                urlencoding::encode(key),
                urlencoding::encode(slug)
            );
            Ok(gif_links::klipy_media(&get(url).await?, slug))
        }
    }
}
