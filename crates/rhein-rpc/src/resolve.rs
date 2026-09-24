//! Turns what an RPC app sends into something a viewer can show: `SET_ACTIVITY` usually carries
//! only the application id and asset keys, and Discord fills in the name and pictures. Rhein asks
//! Discord's public application endpoints the same way the Telium desktop client does, and keeps
//! the answers on disk.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const API: &str = "https://discord.com/api/v10";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: Option<String>,
    /// Icon hash on Discord's CDN.
    pub icon: Option<String>,
    /// Asset name → asset id.
    pub assets: HashMap<String, String>,
}

pub struct Resolver {
    http: reqwest::Client,
    file: PathBuf,
    apps: Mutex<HashMap<String, AppInfo>>,
}

pub fn icon_url(app_id: &str, hash: &str) -> String {
    format!("https://cdn.discordapp.com/app-icons/{app_id}/{hash}.png?size=256")
}

/// An asset reference as apps send it: a URL, an `mp:` media proxy path, an asset name or id.
pub fn asset_url(app_id: &str, key: &str, info: &AppInfo) -> Option<String> {
    if key.starts_with("https://") || key.starts_with("http://") {
        return Some(key.to_owned());
    }
    if let Some(path) = key.strip_prefix("mp:") {
        return Some(format!("https://media.discordapp.net/{path}"));
    }
    let id = info.assets.get(key).cloned().or_else(|| key.chars().all(|c| c.is_ascii_digit()).then(|| key.to_owned()))?;
    Some(format!("https://cdn.discordapp.com/app-assets/{app_id}/{id}.png?size=256"))
}

impl Resolver {
    pub fn new(cache_dir: PathBuf) -> Resolver {
        let file = cache_dir.join("rpc-apps.json");
        let apps = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Resolver { http: reqwest::Client::new(), file, apps: Mutex::new(apps) }
    }

    /// What Discord says about an application; empty when it doesn't know it or can't be reached.
    pub async fn app(&self, id: &str) -> AppInfo {
        if !valid_id(id) {
            return AppInfo::default();
        }
        if let Some(info) = self.apps.lock().unwrap().get(id).cloned() {
            return info;
        }
        let rpc: Option<Value> = self.get(&format!("{API}/applications/{id}/rpc")).await;
        let assets: Option<Vec<Value>> = self.get(&format!("{API}/oauth2/applications/{id}/assets")).await;
        let info = AppInfo {
            name: rpc.as_ref().and_then(|r| r.get("name")).and_then(Value::as_str).map(str::to_owned),
            icon: rpc.as_ref().and_then(|r| r.get("icon")).and_then(Value::as_str).map(str::to_owned),
            assets: assets
                .unwrap_or_default()
                .iter()
                .filter_map(|a| Some((a.get("name")?.as_str()?.to_owned(), a.get("id")?.as_str()?.to_owned())))
                .collect(),
        };
        // Only a real answer is remembered; a network hiccup is asked again next time.
        if rpc.is_some() {
            let mut apps = self.apps.lock().unwrap();
            apps.insert(id.to_owned(), info.clone());
            if let Ok(bytes) = serde_json::to_vec(&*apps) {
                let _ = std::fs::write(&self.file, bytes);
            }
        }
        info
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, url: &str) -> Option<T> {
        let response = self.http.get(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json().await.ok()
    }

    /// Fills in the application's name and turns asset keys into picture URLs. Without a large
    /// picture of its own, the application's icon stands in, as in Discord.
    pub async fn complete(&self, mut activity: Value) -> Value {
        let Some(app_id) = activity.get("application_id").and_then(Value::as_str).map(str::to_owned) else {
            return activity;
        };
        let info = self.app(&app_id).await;
        let Some(obj) = activity.as_object_mut() else { return activity };
        let has_name = obj.get("name").and_then(Value::as_str).is_some_and(|n| !n.trim().is_empty());
        if !has_name && let Some(name) = &info.name {
            obj.insert("name".into(), json!(name));
        }
        let mut assets = obj.get("assets").and_then(Value::as_object).cloned().unwrap_or_default();
        for key in ["large_image", "small_image"] {
            let resolved = assets.get(key).and_then(Value::as_str).and_then(|k| asset_url(&app_id, k, &info));
            match resolved {
                Some(url) => assets.insert(key.into(), json!(url)),
                None => assets.remove(key),
            };
        }
        if !assets.contains_key("large_image")
            && let Some(icon) = &info.icon
        {
            assets.insert("large_image".into(), json!(icon_url(&app_id, icon)));
        }
        if !assets.is_empty() {
            obj.insert("assets".into(), Value::Object(assets));
        }
        activity
    }
}

/// Discord ids are snowflakes; anything else never reaches a URL.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_references() {
        let mut info = AppInfo::default();
        info.assets.insert("logo".into(), "999".into());
        assert_eq!(asset_url("1", "logo", &info).as_deref(), Some("https://cdn.discordapp.com/app-assets/1/999.png?size=256"));
        assert_eq!(asset_url("1", "123", &info).as_deref(), Some("https://cdn.discordapp.com/app-assets/1/123.png?size=256"));
        assert_eq!(asset_url("1", "mp:external/abc/x.png", &info).as_deref(), Some("https://media.discordapp.net/external/abc/x.png"));
        assert_eq!(asset_url("1", "https://i.scdn.co/x", &info).as_deref(), Some("https://i.scdn.co/x"));
        assert_eq!(asset_url("1", "unknown-name", &info), None);
        assert!(!valid_id("../x"));
    }
}
