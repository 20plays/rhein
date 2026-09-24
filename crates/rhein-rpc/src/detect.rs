//! Running games, found by their executables in Discord's list of detectable applications, the way
//! arRPC scans processes. Works for native games and for Windows games under Wine or Proton.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{PRIORITY_DETECTED, Report, Reports, SourceEvent};

const LIST_URL: &str = "https://discord.com/api/v9/applications/detectable";
/// The list changes slowly; once a day is plenty.
const REFRESH: Duration = Duration::from_secs(24 * 3600);
pub const SCAN_EVERY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Executable {
    pub name: String,
    #[serde(default)]
    pub arguments: Option<String>,
}

/// One detectable application, trimmed to what matching needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detectable {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon_hash: Option<String>,
    pub executables: Vec<Executable>,
}

/// Loads the cached list, downloading it when missing or older than a day. A failed download
/// keeps whatever was cached.
pub async fn load_list(cache_dir: &Path) -> Vec<Detectable> {
    let file = cache_dir.join("detectable.json");
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < REFRESH);
    if !fresh {
        match download().await {
            Ok(list) => {
                if let Ok(bytes) = serde_json::to_vec(&list) {
                    let _ = tokio::fs::write(&file, bytes).await;
                }
                return list;
            }
            Err(err) => log::warn!("rpc: detectable list: {err:#}"),
        }
    }
    tokio::fs::read(&file).await.ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

async fn download() -> anyhow::Result<Vec<Detectable>> {
    let raw: Vec<serde_json::Value> = reqwest::get(LIST_URL).await?.error_for_status()?.json().await?;
    Ok(raw.iter().filter_map(trim).collect())
}

/// Keeps entries with non-launcher executables for Linux or Windows (Wine).
fn trim(entry: &serde_json::Value) -> Option<Detectable> {
    let executables: Vec<Executable> = entry
        .get("executables")?
        .as_array()?
        .iter()
        .filter(|e| !e.get("is_launcher").and_then(|v| v.as_bool()).unwrap_or(false))
        .filter(|e| matches!(e.get("os").and_then(|v| v.as_str()), Some("linux" | "win32")))
        .filter_map(|e| {
            Some(Executable {
                name: e.get("name")?.as_str()?.to_lowercase(),
                arguments: e.get("arguments").and_then(|a| a.as_str()).map(str::to_owned),
            })
        })
        .collect();
    if executables.is_empty() {
        return None;
    }
    Some(Detectable {
        id: entry.get("id")?.as_str()?.to_owned(),
        name: entry.get("name")?.as_str()?.to_owned(),
        icon_hash: entry.get("icon_hash").and_then(|h| h.as_str()).map(str::to_owned),
        executables,
    })
}

/// The suffixes of a process path that an executable name is compared with, as arRPC builds them:
/// `a/b/game.exe` gives `game.exe`, `b/game.exe`, `a/b/game.exe`, plus variants without 64-bit
/// markers.
pub fn candidates(path: &str) -> Vec<String> {
    let path = path.to_lowercase().replace('\\', "/");
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let mut out: Vec<String> = (1..=parts.len()).map(|n| parts[parts.len() - n..].join("/")).collect();
    let base: Vec<String> = out.clone();
    for p in base {
        for marker in ["64", ".x64", "x64", "_64"] {
            if p.contains(marker) {
                out.push(p.replacen(marker, "", 1));
            }
        }
    }
    out
}

pub fn matches(exe: &Executable, candidates: &[String], args: &str) -> bool {
    let hit = match exe.name.strip_prefix('>') {
        Some(exact) => candidates.first().is_some_and(|c| c == exact),
        None => candidates.contains(&exe.name),
    };
    hit && exe.arguments.as_deref().is_none_or(|a| args.contains(a))
}

/// `(pid, path, arguments)` of every process we can read, from `/proc`.
fn processes() -> Vec<(u32, String, String)> {
    let own = std::process::id();
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    dir.filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != own)
        .filter_map(|pid| {
            let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
            let mut parts = raw.split(|b| *b == 0).filter(|p| !p.is_empty()).map(|p| String::from_utf8_lossy(p).into_owned());
            let path = parts.next()?;
            let args = parts.collect::<Vec<_>>().join(" ");
            Some((pid, path, args))
        })
        .collect()
}

/// Scans every [`SCAN_EVERY`] and reports games as they start and stop.
pub async fn run(cache_dir: PathBuf, reports: Reports) {
    let list = load_list(&cache_dir).await;
    log::info!("rpc: {} detectable applications", list.len());
    if list.is_empty() {
        return;
    }
    // Executable name → entries, so a scan is one lookup per path suffix.
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, d) in list.iter().enumerate() {
        for e in &d.executables {
            by_name.entry(e.name.trim_start_matches('>').to_owned()).or_default().push(i);
        }
    }
    let (list, by_name) = (Arc::new(list), Arc::new(by_name));
    let mut running: HashMap<String, u64> = HashMap::new();
    loop {
        let found = tokio::task::spawn_blocking({
            let (list, by_name) = (list.clone(), by_name.clone());
            move || scan(&list, &by_name)
        })
        .await
        .unwrap_or_default();
        let now = now_ms();
        for id in running.keys().filter(|id| !found.contains(*id)).cloned().collect::<Vec<_>>() {
            running.remove(&id);
            let _ = reports.send(SourceEvent::Report(Report { key: format!("detect:{id}"), activity: None, priority: PRIORITY_DETECTED }));
        }
        let mut names = Vec::new();
        for id in &found {
            let Some(d) = list.iter().find(|d| &d.id == id) else { continue };
            names.push(d.name.clone());
            let start = *running.entry(id.clone()).or_insert(now);
            let mut activity = json!({
                "application_id": d.id,
                "name": d.name,
                "type": 0,
                "timestamps": { "start": start },
            });
            if let Some(hash) = &d.icon_hash {
                activity["assets"] = json!({ "large_image": crate::resolve::icon_url(&d.id, hash) });
            }
            let _ = reports.send(SourceEvent::Report(Report { key: format!("detect:{id}"), activity: Some(activity), priority: PRIORITY_DETECTED }));
        }
        names.sort();
        let _ = reports.send(SourceEvent::Detected(names));
        tokio::time::sleep(SCAN_EVERY).await;
    }
}

fn scan(list: &[Detectable], by_name: &HashMap<String, Vec<usize>>) -> HashSet<String> {
    let mut found = HashSet::new();
    for (_, path, args) in processes() {
        let candidates = candidates(&path);
        for c in &candidates {
            for &i in by_name.get(c).into_iter().flatten() {
                let d = &list[i];
                if d.executables.iter().any(|e| matches(e, &candidates, &args)) {
                    found.insert(d.id.clone());
                }
            }
        }
    }
    found
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_cover_suffixes_and_bitness() {
        let c = candidates("Z:\\Games\\Factorio\\bin\\x64\\Factorio.exe");
        assert_eq!(c[0], "factorio.exe");
        assert!(c.contains(&"x64/factorio.exe".to_owned()));
        // Variants without the 64-bit marker, as arRPC makes them.
        assert!(c.contains(&"/factorio.exe".to_owned()));
        assert!(c.contains(&"bin//factorio.exe".to_owned()));
    }

    #[test]
    fn executable_matching() {
        let c = candidates("/home/me/.steam/steamapps/common/Game/game_x64");
        let plain = Executable { name: "game_x64".into(), arguments: None };
        let exact = Executable { name: ">game/game_x64".into(), arguments: None };
        let with_args = Executable { name: "game_x64".into(), arguments: Some("--server".into()) };
        assert!(matches(&plain, &c, ""));
        assert!(!matches(&exact, &c, ""));
        assert!(!matches(&with_args, &c, "--client"));
        assert!(matches(&with_args, &c, "--server --port 1"));
    }

    #[test]
    fn trim_keeps_playable_entries() {
        let entry = json!({
            "id": "1", "name": "Game", "icon_hash": "abc",
            "executables": [
                { "name": "Launcher.exe", "os": "win32", "is_launcher": true },
                { "name": "Game.app", "os": "darwin" },
                { "name": "Game.exe", "os": "win32" },
            ],
        });
        let d = trim(&entry).unwrap();
        assert_eq!(d.executables.len(), 1);
        assert_eq!(d.executables[0].name, "game.exe");
        assert!(trim(&json!({ "id": "2", "name": "Mac only", "executables": [{ "name": "x", "os": "darwin" }] })).is_none());
    }
}
