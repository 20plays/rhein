//! Channels ready before they are opened: once a server is connected, the newest page of each of
//! its text channels is fetched in the background, a few at a time, and the last page of every
//! channel read is kept on disk, so a channel opened shows its messages at once (after a restart
//! too) while the server is asked what came since.

use std::collections::VecDeque;
use std::path::PathBuf;

use super::*;

/// Channel pages fetched at once in the background.
const AT_ONCE: usize = 2;

/// At most this many channels of a server are fetched ahead.
const PER_SERVER: usize = 80;

/// The newest messages of a channel kept on disk.
const KEPT_ON_DISK: usize = 50;

/// The background fetching of channels' newest pages.
#[derive(Default)]
pub(super) struct Preload {
    queue: VecDeque<String>,
    /// Under way now.
    running: HashSet<String>,
}

fn kept_path(channel_id: &str) -> PathBuf {
    let name: String = channel_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(96).collect();
    crate::persist::cache_dir().join("messages").join(format!("{name}.json"))
}

/// The messages of `channel_id` kept on disk, if any.
pub(super) fn read_kept(channel_id: &str) -> Vec<Message> {
    std::fs::read(kept_path(channel_id)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

impl Client {
    /// Fetches the newest page of the text channels of `server`'s guilds in the background: the
    /// ones visited first, then those of the guild shown, then the rest in order.
    pub(super) fn preload_server(&mut self, server: &str, cx: &mut Context<Self>) {
        let selected = self.selected_guild.clone();
        let mut channels: Vec<(u8, usize, String)> = Vec::new();
        for entry in self.guilds.iter().filter(|g| g.server == server) {
            for (n, channel) in entry.ordered_channels().into_iter().filter(|c| c.is_textual()).enumerate() {
                let rank = match self.visited.iter().position(|v| *v == channel.id) {
                    Some(_) => 0,
                    None if selected.as_deref() == Some(entry.guild.id.as_str()) => 1,
                    None => 2,
                };
                let order = self.visited.iter().position(|v| *v == channel.id).unwrap_or(n);
                channels.push((rank, order, channel.id.clone()));
            }
        }
        channels.sort();
        for (_, _, channel) in channels.into_iter().take(PER_SERVER) {
            if !self.preload.queue.contains(&channel) {
                self.preload.queue.push_back(channel);
            }
        }
        self.pump_preload(cx);
    }

    /// Starts the next fetches ahead while fewer than `AT_ONCE` run.
    pub(super) fn pump_preload(&mut self, cx: &mut Context<Self>) {
        while self.preload.running.len() < AT_ONCE {
            let Some(channel) = self.preload.queue.pop_front() else { return };
            let needed = self.logs.get(&channel).is_none_or(|l| !l.loaded && !l.loading);
            if !needed {
                continue;
            }
            self.preload.running.insert(channel.clone());
            self.load_page(channel.clone(), false, true, cx);
            if !self.logs.get(&channel).is_some_and(|l| l.loading) {
                self.preload.running.remove(&channel);
            }
        }
    }

    /// A page fetched ahead has come (or failed): the next one starts.
    pub(super) fn preloaded(&mut self, channel_id: &str, cx: &mut Context<Self>) {
        if self.preload.running.remove(channel_id) {
            self.pump_preload(cx);
        }
    }

    /// Keeps the newest messages of `channel_id` on disk, for showing at once next time.
    pub(super) fn keep_on_disk(&self, channel_id: &str) {
        let Some(log) = self.logs.get(channel_id).filter(|l| l.loaded) else { return };
        let messages: Vec<&Message> = log.messages.iter().filter(|m| !m.id.starts_with("pending:")).collect();
        let newest = &messages[messages.len().saturating_sub(KEPT_ON_DISK)..];
        let Ok(json) = serde_json::to_vec(newest) else { return };
        let path = kept_path(channel_id);
        self.io.spawn(async move {
            let written = async {
                if let Some(dir) = path.parent() {
                    tokio::fs::create_dir_all(dir).await?;
                }
                let part = path.with_extension("part");
                tokio::fs::write(&part, json).await?;
                tokio::fs::rename(&part, &path).await
            };
            if let Err(err) = written.await {
                log::debug!("keeping {}: {err}", path.display());
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_messages_round_trip() {
        let dir = std::env::temp_dir().join(format!("rhein-kept-{}", std::process::id()));
        // SAFETY: tests touching RHEIN_CONFIG_DIR run in this one test only.
        unsafe { std::env::set_var("RHEIN_CONFIG_DIR", &dir) };
        let path = kept_path("chan/../1");
        assert!(path.starts_with(dir.join("cache").join("messages")));
        assert!(read_kept("nothing-here").is_empty());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let message = Message { id: "1".into(), content: "hi".into(), ..Default::default() };
        std::fs::write(&path, serde_json::to_vec(&[&message]).unwrap()).unwrap();
        assert_eq!(read_kept("chan/../1").iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), ["hi"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
