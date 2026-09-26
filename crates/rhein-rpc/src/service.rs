//! Runs the sources together and publishes the activities to share: per slot (game, music,
//! video) the best one, where an app's own RPC activity beats a detected one and newer beats
//! older.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rhein_proto::Activity;
use rhein_proto::activity::Slot;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::resolve::Resolver;
use crate::{Report, SourceEvent, bridge, detect, ipc, mpris, ws};

/// Changes arriving together (a game starting while its RPC connects) are published once.
const SETTLE: Duration = Duration::from_millis(750);

#[derive(Debug, Clone)]
pub struct Config {
    /// Where the detectable list and resolved application names are cached.
    pub cache_dir: PathBuf,
    /// Directory for the `discord-ipc-N` socket; the runtime directory by default.
    pub ipc_dir: Option<PathBuf>,
    pub ipc: bool,
    pub websocket: bool,
    pub detect_games: bool,
    pub media: bool,
    /// Also read the activities an arRPC holding `discord-ipc-0` receives.
    pub bridge: bool,
}

impl Config {
    pub fn new(cache_dir: PathBuf) -> Config {
        Config { cache_dir, ipc_dir: None, ipc: true, websocket: true, detect_games: true, media: true, bridge: true }
    }
}

/// What the settings page shows about the sources.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub socket: Option<PathBuf>,
    pub websocket_port: Option<u16>,
    /// Application ids of connected RPC clients.
    pub clients: Vec<String>,
    /// Names of the games found running.
    pub detected: Vec<String>,
    /// Media players on the bus.
    pub players: Vec<String>,
    /// Whether arRPC's bridge is connected.
    pub bridge: bool,
}

/// Dropping it stops every source and removes the socket.
pub struct Handle {
    pub activities: watch::Receiver<Vec<Activity>>,
    pub status: watch::Receiver<Status>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Handle {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

struct Entry {
    activity: Activity,
    priority: u8,
    sequence: u64,
}

/// Picks one activity per slot: highest priority, then most recent. Stable order: game, music,
/// video.
fn select(entries: &HashMap<String, Entry>) -> Vec<Activity> {
    let mut best: HashMap<Slot, &Entry> = HashMap::new();
    for entry in entries.values() {
        let slot = entry.activity.slot();
        let better = best.get(&slot).is_none_or(|b| (entry.priority, entry.sequence) > (b.priority, b.sequence));
        if better {
            best.insert(slot, entry);
        }
    }
    [Slot::Game, Slot::Music, Slot::Video].iter().filter_map(|s| best.get(s).map(|e| e.activity.clone())).collect()
}

/// Starts the enabled sources. Call from inside a Tokio runtime.
pub fn start(config: Config) -> Handle {
    let _ = std::fs::create_dir_all(&config.cache_dir);
    let (reports, mut events) = mpsc::unbounded_channel::<SourceEvent>();
    let (activities_tx, activities) = watch::channel(Vec::new());
    let (status_tx, status) = watch::channel(Status::default());
    let status_tx = Arc::new(status_tx);
    let mut tasks = Vec::new();

    if config.ipc {
        let (reports, status_tx) = (reports.clone(), status_tx.clone());
        let dir = config.ipc_dir.clone().unwrap_or_else(ipc::runtime_dir);
        tasks.push(tokio::spawn(async move {
            match ipc::bind(&dir).await {
                Ok((listener, path)) => {
                    log::info!("rpc: listening on {}", path.display());
                    status_tx.send_modify(|s| s.socket = Some(path.clone()));
                    ipc::serve(listener, path, reports).await;
                }
                Err(err) => log::warn!("rpc: {err:#}"),
            }
        }));
    }
    if config.websocket {
        let (reports, status_tx) = (reports.clone(), status_tx.clone());
        tasks.push(tokio::spawn(async move {
            match ws::bind().await {
                Ok((listener, port)) => {
                    status_tx.send_modify(|s| s.websocket_port = Some(port));
                    ws::serve(listener, reports).await;
                }
                Err(err) => log::info!("rpc websocket: {err:#}"),
            }
        }));
    }
    if config.detect_games {
        let (reports, dir) = (reports.clone(), config.cache_dir.clone());
        tasks.push(tokio::spawn(detect::run(dir, reports)));
    }
    if config.media {
        tasks.push(tokio::spawn(mpris::run(reports.clone())));
    }
    if config.bridge {
        tasks.push(tokio::spawn(bridge::run(reports.clone())));
    }
    drop(reports);

    let resolver = Arc::new(Resolver::new(config.cache_dir.clone()));
    tasks.push(tokio::spawn(async move {
        let mut entries: HashMap<String, Entry> = HashMap::new();
        let mut clients: HashMap<String, String> = HashMap::new();
        let mut sequence = 0u64;
        while let Some(first) = events.recv().await {
            // Take everything that arrives within the settle window as one change.
            let mut batch = vec![first];
            let deadline = tokio::time::Instant::now() + SETTLE;
            while let Ok(Some(more)) = tokio::time::timeout_at(deadline, events.recv()).await {
                batch.push(more);
            }
            for event in batch {
                match event {
                    SourceEvent::Report(Report { key, activity, priority }) => match activity {
                        Some(raw) => {
                            let raw = resolver.complete(raw).await;
                            match Activity::normalize(&raw) {
                                Some(mut activity) => {
                                    // A media player's own cover file (never another program's:
                                    // that could name any picture on the disk). The client
                                    // uploads it and shares the copy, never the path.
                                    if key == "mpris"
                                        && let Some(local) = raw["assets"]["large_image"].as_str().filter(|u| u.starts_with("file://"))
                                    {
                                        let assets = activity.assets.get_or_insert_with(Default::default);
                                        assets.large_image = Some(local.to_owned());
                                        assets.large_text = raw["assets"]["large_text"].as_str().map(|t| t.chars().take(128).collect());
                                    }
                                    // A detected game re-reports every scan; keep its place in line.
                                    let same = entries.get(&key).is_some_and(|e| e.activity == activity);
                                    if !same {
                                        sequence += 1;
                                        entries.insert(key, Entry { activity, priority, sequence });
                                    }
                                }
                                None => {
                                    entries.remove(&key);
                                }
                            }
                        }
                        None => {
                            entries.remove(&key);
                        }
                    },
                    SourceEvent::Client { key, client_id } => {
                        match client_id {
                            Some(id) => clients.insert(key, id),
                            None => clients.remove(&key),
                        };
                        let mut ids: Vec<String> = clients.values().cloned().collect();
                        ids.sort();
                        status_tx.send_modify(|s| s.clients = ids);
                    }
                    SourceEvent::Detected(names) => {
                        status_tx.send_if_modified(|s| {
                            let changed = s.detected != names;
                            s.detected = names;
                            changed
                        });
                    }
                    SourceEvent::Bridge(up) => {
                        status_tx.send_if_modified(|s| std::mem::replace(&mut s.bridge, up) != up);
                    }
                    SourceEvent::Players(names) => {
                        status_tx.send_if_modified(|s| {
                            let changed = s.players != names;
                            s.players = names;
                            changed
                        });
                    }
                }
            }
            let next = select(&entries);
            activities_tx.send_if_modified(|current| {
                let changed = *current != next;
                *current = next;
                changed
            });
        }
    }));

    Handle { activities, status, tasks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(v: serde_json::Value, priority: u8, sequence: u64) -> Entry {
        Entry { activity: Activity::normalize(&v).unwrap(), priority, sequence }
    }

    #[test]
    fn rpc_beats_detection_and_slots_stay_ordered() {
        let mut entries = HashMap::new();
        entries.insert("detect:1".into(), entry(json!({ "name": "Detected game" }), 1, 5));
        entries.insert("ipc:1".into(), entry(json!({ "name": "RPC game" }), 2, 1));
        entries.insert("mpris".into(), entry(json!({ "name": "Spotify", "type": 2 }), 1, 3));
        let out = select(&entries);
        let names: Vec<&str> = out.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["RPC game", "Spotify"]);
    }
}
