//! Files from messages: fetched (and in DMs decrypted) to be played, or saved straight into the
//! downloads folder, never handed to a browser.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::conversations::Engine;

/// Where an attachment's bytes come from.
#[derive(Clone)]
pub enum MediaSource {
    /// A guild attachment.
    Url(String),
    /// An encrypted DM attachment: the conversation, the message, its index.
    Dm { conversation: String, message: String, index: usize },
}

/// Ticks a few times a second while a sound or video plays or a file downloads, for the views
/// showing how far along it is: they watch this instead of the whole client.
pub struct MediaTick;

/// A download under way: the bytes so far, and how many there are when the server said.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DownloadView {
    pub got: u64,
    pub total: Option<u64>,
}

#[derive(Default)]
pub(super) struct Download {
    got: Arc<AtomicU64>,
    /// 0 while unknown.
    total: Arc<AtomicU64>,
}

/// What fetching an attachment needs, taken off the client to move onto the runtime.
#[derive(Clone)]
pub(super) struct Fetcher {
    http: reqwest::Client,
    engine: Option<dm_view::SharedEngine>,
    api: Option<Api>,
}

impl Fetcher {
    /// The attachment's bytes, opened when it is encrypted.
    pub(super) async fn fetch(&self, source: MediaSource) -> Result<Vec<u8>> {
        match source {
            MediaSource::Url(url) => Ok(self.http.get(&url).send().await?.error_for_status()?.bytes().await?.to_vec()),
            MediaSource::Dm { conversation, message, index } => {
                let (attachment, api) = self.dm_attachment(&conversation, &message, index).await?;
                Engine::fetch_attachment(&api, &attachment).await
            }
        }
    }

    async fn dm_attachment(&self, conversation: &str, message: &str, index: usize) -> Result<(serde_json::Value, Api)> {
        let (Some(engine), Some(api)) = (&self.engine, &self.api) else { anyhow::bail!("encrypted messages are not open on this device") };
        let engine = engine.lock().await;
        let attachment = engine
            .conversation(conversation)
            .and_then(|c| c.messages.iter().find(|m| m.id == message))
            .and_then(|m| m.body.attachments.get(index).cloned())
            .ok_or_else(|| anyhow!("that attachment is gone"))?;
        Ok((attachment, api.clone()))
    }

    /// Saves the attachment as `name` in the downloads folder, counting the bytes into
    /// `progress` as they come. A guild file goes to disk as it arrives; an encrypted one is
    /// opened whole first.
    async fn save(&self, source: MediaSource, name: &str, progress: &Download) -> Result<PathBuf> {
        use tokio::io::AsyncWriteExt as _;
        let (path, part) = reserve(name)?;
        let written = async {
            let mut file = tokio::fs::File::create(&part).await?;
            match source {
                MediaSource::Url(url) => {
                    let mut response = self.http.get(&url).send().await?.error_for_status()?;
                    progress.total.store(response.content_length().unwrap_or(0), Ordering::Relaxed);
                    while let Some(chunk) = response.chunk().await? {
                        file.write_all(&chunk).await?;
                        progress.got.fetch_add(chunk.len() as u64, Ordering::Relaxed);
                    }
                }
                source => {
                    let bytes = self.fetch(source).await?;
                    progress.got.store(bytes.len() as u64, Ordering::Relaxed);
                    file.write_all(&bytes).await?;
                }
            }
            file.flush().await?;
            tokio::fs::rename(&part, &path).await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(err) = written {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(err);
        }
        Ok(path)
    }
}

/// A file name safe to save under: no folders, not hidden, not empty.
pub fn safe_file_name(name: &str) -> String {
    let name: String = name.chars().filter(|c| !matches!(c, '/' | '\\' | '\0')).collect();
    let name = name.trim();
    match name {
        "" => "attachment".to_owned(),
        n if n.starts_with('.') => format!("attachment{n}"),
        n => n.to_owned(),
    }
}

/// `name` in the downloads folder, numbered when that file exists already.
pub fn free_download_path(name: &str) -> PathBuf {
    let dir = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
    free_path_in(&dir, name)
}

fn free_path_in(dir: &Path, name: &str) -> PathBuf {
    let taken = |p: &Path| p.exists() || part_of(p).exists();
    let mut path = dir.join(name);
    let mut n = 1;
    while taken(&path) {
        path = dir.join(format!("{n}-{name}"));
        n += 1;
    }
    path
}

/// Where a download is written until it is complete.
fn part_of(path: &Path) -> PathBuf {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    PathBuf::from(part)
}

/// A free path for `name` and its `.part` file, which is created at once so a second download
/// of the same name picks another.
fn reserve(name: &str) -> Result<(PathBuf, PathBuf)> {
    let path = free_download_path(&safe_file_name(name));
    let part = part_of(&path);
    std::fs::OpenOptions::new().write(true).create_new(true).open(&part).with_context(|| format!("couldn't write {}", part.display()))?;
    Ok((path, part))
}

impl Client {
    pub(super) fn fetcher(&self) -> Fetcher {
        Fetcher { http: self.http.clone(), engine: self.dm_engine.clone(), api: self.home.as_ref().map(|h| h.api.clone()) }
    }

    /// How far the download of `key` (message id and attachment index) has come, while it runs.
    pub fn download(&self, key: &str) -> Option<DownloadView> {
        let download = self.downloads.get(key)?;
        let total = download.total.load(Ordering::Relaxed);
        Some(DownloadView { got: download.got.load(Ordering::Relaxed), total: (total > 0).then_some(total) })
    }

    /// Saves an attachment into the downloads folder.
    pub fn download_file(&mut self, key: String, source: MediaSource, filename: String, cx: &mut Context<Self>) {
        if self.downloads.contains_key(&key) {
            return;
        }
        let download = Download::default();
        let progress = Download { got: download.got.clone(), total: download.total.clone() };
        self.downloads.insert(key.clone(), download);
        let (io, fetcher) = (self.io.clone(), self.fetcher());
        let tick = self.media_tick.clone();
        cx.spawn(async move |this, cx| {
            let saving = io.spawn(async move { fetcher.save(source, &filename, &progress).await });
            // The views showing the progress, a few times a second while it runs.
            let ticker = cx.spawn({
                let tick = tick.clone();
                async move |cx| {
                    loop {
                        cx.background_executor().timer(Duration::from_millis(250)).await;
                        tick.update(cx, |_, cx| cx.notify());
                    }
                }
            });
            let result = join(saving).await;
            drop(ticker);
            this.update(cx, |this, cx| {
                this.downloads.remove(&key);
                match result {
                    Ok(path) => this.flash_notice(format!("Saved to {}", path.display()), cx),
                    Err(err) => {
                        this.error = Some(format!("Couldn't download the file: {}", friendly(&err)).into());
                        cx.notify();
                    }
                }
                tick.update(cx, |_, cx| cx.notify());
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_file_name("../../etc/passwd"), "attachment....etcpasswd");
        assert_eq!(safe_file_name("  "), "attachment");
        assert_eq!(safe_file_name(".bashrc"), "attachment.bashrc");
        assert_eq!(safe_file_name("clip.mp4"), "clip.mp4");
    }

    #[test]
    fn taken_names_are_numbered() {
        let dir = std::env::temp_dir().join(format!("rhein-free-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"").unwrap();
        std::fs::write(dir.join("1-a.txt.part"), b"").unwrap();
        assert_eq!(free_path_in(&dir, "a.txt"), dir.join("2-a.txt"));
        assert_eq!(free_path_in(&dir, "b.txt"), dir.join("b.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
