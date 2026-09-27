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
        self.load_account_prefs(cx);
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

/// What a Telium media link is, once its origin has said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaProbe {
    Loading,
    Known(gif_links::MediaKind),
}

impl Client {
    /// What a Telium media link serves; `None` until someone asks (`request_media_kind`).
    pub fn media_kind(&self, url: &str) -> Option<MediaProbe> {
        self.media_kinds.get(url).copied()
    }

    /// The links in a message to show as GIFs: GIF links, and Telium media that proved to be one.
    pub fn gif_links_in(&self, content: &str) -> Vec<String> {
        let mut out = gif_links::gifs_in(content);
        out.extend(
            gif_links::telium_media_in(content)
                .into_iter()
                .filter(|u| self.media_kind(u) == Some(MediaProbe::Known(gif_links::MediaKind::Gif))),
        );
        out
    }

    /// Whether a message is one GIF and nothing else (its text is then not shown).
    pub fn is_gif_only(&self, content: &str) -> bool {
        if gif_links::is_gif_only(content) {
            return true;
        }
        let text = content.trim();
        gif_links::is_telium_media(text)
            && self.media_kind(text) == Some(MediaProbe::Known(gif_links::MediaKind::Gif))
            && gif_links::telium_media_in(text).len() == 1
    }

    /// Asks a Telium media link's origin what it serves, once: a `HEAD`, and a one-byte `GET`
    /// where `HEAD` is refused. The capability in the link is all it needs; no credentials go.
    pub fn request_media_kind(&mut self, url: &str, cx: &mut Context<Self>) {
        if self.media_kinds.contains_key(url) || !gif_links::is_telium_media(url) {
            return;
        }
        self.media_kinds.insert(url.to_owned(), MediaProbe::Loading);
        let (io, http, key) = (self.io.clone(), self.http.clone(), url.to_owned());
        let target = key.clone();
        cx.spawn(async move |this, cx| {
            let found = join(io.spawn(async move { probe(&http, &target).await })).await;
            this.update(cx, |this, cx| {
                let kind = match found {
                    Ok(content_type) => gif_links::media_kind(&content_type),
                    Err(err) => {
                        log::debug!("probing {key}: {err:#}");
                        gif_links::MediaKind::Other
                    }
                };
                this.media_kinds.insert(key, MediaProbe::Known(kind));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

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
                    Ok(None) => {
                        log::debug!("resolving {key}: the provider named no GIF for it");
                        GifShare::Failed
                    }
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
            let key = urlencoding::encode(key);
            // The GIF by its slug; a search for the slug finds it only among the first results.
            let item = format!("{base}/{key}/gifs/{}", urlencoding::encode(slug));
            match get(item).await {
                Ok(payload) if gif_links::klipy_media(&payload, slug).is_some() => return Ok(gif_links::klipy_media(&payload, slug)),
                Ok(_) => {}
                Err(err) => log::debug!("klipy item {slug}: {err:#}"),
            }
            let url = format!("{base}/{key}/gifs/search?page=1&limit=24&per_page=24&q={}", urlencoding::encode(slug));
            Ok(gif_links::klipy_media(&get(url).await?, slug))
        }
    }
}

impl Client {
    /// Adds links from a favourites export after the ones already kept, up to the limit.
    /// Returns how many were added and how many did not fit.
    pub fn import_gif_favorites(&mut self, urls: Vec<String>, cx: &mut Context<Self>) -> (usize, usize) {
        let before = self.gif_favorites.len();
        let fresh: Vec<String> = urls.into_iter().filter(|u| !self.gif_favorites.contains(u)).collect();
        let total = before + fresh.len();
        self.gif_favorites.extend(fresh);
        self.gif_favorites.truncate(MAX_GIF_FAVORITES);
        self.save_gif_favorites(cx);
        cx.notify();
        (self.gif_favorites.len() - before, total.saturating_sub(MAX_GIF_FAVORITES))
    }

    /// Uploads GIF (or MP4) files to the home server and keeps each as a favourite. `done`
    /// hears how many were added and what failed.
    pub fn upload_gif_favorites(
        &mut self,
        files: Vec<std::path::PathBuf>,
        done: impl FnOnce(usize, Vec<String>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(api) = self.home.as_ref().map(|h| h.api.clone()) else { return };
        let io = self.io.clone();
        cx.spawn(async move |this, cx| {
            let uploaded = join(io.spawn(async move {
                let mut urls = Vec::new();
                let mut failed = Vec::new();
                for path in files {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("gif").to_owned();
                    let lower = name.to_ascii_lowercase();
                    let mime = if lower.ends_with(".gif") { "image/gif" } else if lower.ends_with(".mp4") { "video/mp4" } else {
                        failed.push(format!("{name}: not a GIF or MP4 file"));
                        continue;
                    };
                    let result = async {
                        let bytes = tokio::fs::read(&path).await?;
                        let media = api.upload(&name, mime, bytes).await?;
                        media.url.filter(|u| u.starts_with("https://")).ok_or_else(|| anyhow!("the server gave no https link for it"))
                    };
                    match result.await {
                        Ok(url) => urls.push(url),
                        Err(err) => failed.push(format!("{name}: {err:#}")),
                    }
                }
                anyhow::Ok((urls, failed))
            }))
            .await;
            this.update(cx, |this, cx| match uploaded {
                Ok((urls, failed)) => {
                    let (added, _) = this.import_gif_favorites(urls, cx);
                    done(added, failed, cx);
                }
                Err(err) => done(0, vec![friendly(&err)], cx),
            })
            .ok();
        })
        .detach();
    }

    /// Forgets what this device keeps only to be quicker (the game list, app names, link
    /// previews, resolved GIF links); it all comes back when needed.
    pub fn clear_cache(&mut self, cx: &mut Context<Self>) {
        let dir = crate::persist::cache_dir();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let _ = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
            }
        }
        self.embeds.clear();
        self.artwork.clear();
        self.gif_shares.clear();
        self.media_kinds.clear();
        self.notice = Some("Cached data cleared.".into());
        cx.notify();
    }
}

/// The type a media link is served with.
async fn probe(http: &reqwest::Client, url: &str) -> anyhow::Result<String> {
    let timeout = std::time::Duration::from_secs(10);
    let content_type = |r: &reqwest::Response| {
        r.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_owned)
    };
    let head = http.head(url).timeout(timeout).send().await?;
    if head.status().is_success()
        && let Some(t) = content_type(&head)
    {
        return Ok(t);
    }
    let get = http.get(url).header(reqwest::header::RANGE, "bytes=0-0").timeout(timeout).send().await?;
    anyhow::ensure!(get.status().is_success(), "{}", get.status());
    content_type(&get).ok_or_else(|| anyhow::anyhow!("no content type"))
}
