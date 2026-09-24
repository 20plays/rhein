//! What media players are playing, over D-Bus MPRIS: a song becomes "Listening to", a video in a
//! browser "Watching". Polled every few seconds; only real changes are reported.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};
use zbus::zvariant::{OwnedValue, Value as ZValue};
use zbus::{Connection, Proxy};

use crate::detect::now_ms;
use crate::{PRIORITY_DETECTED, Report, Reports, SourceEvent};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
pub const POLL_EVERY: Duration = Duration::from_secs(5);
/// Position jitter below this is not a seek, so the timestamps stay put.
const SEEK_TOLERANCE_MS: u64 = 2500;
const BROWSERS: [&str; 9] = ["firefox", "mozilla", "chrom", "brave", "vivaldi", "opera", "edge", "zen", "librewolf"];
const MUSIC_SITES: [&str; 6] = ["music.youtube.com", "open.spotify.com", "soundcloud.com", "bandcamp.com", "music.apple.com", "deezer.com"];
const VIDEO_SITES: [&str; 6] = ["youtube.com", "youtu.be", "twitch.tv", "netflix.com", "vimeo.com", "primevideo.com"];

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Track {
    pub player: String,
    pub title: Option<String>,
    pub artists: Vec<String>,
    pub album: Option<String>,
    pub art: Option<String>,
    pub url: Option<String>,
    pub length_ms: Option<u64>,
    pub position_ms: u64,
}

/// A playing track as an activity. `start` is when the track would have begun, so viewers can
/// draw progress.
pub fn activity(track: &Track, start: u64) -> Option<Value> {
    let title = track.title.clone()?;
    // What plays in a browser is usually a video, and what someone watches is more private than
    // a song, so browsers count as video unless the page is a known music site. Dedicated players
    // count as music unless the page is a known video site.
    let host = track.url.as_deref().and_then(|u| url::Url::parse(u).ok()).and_then(|u| u.host_str().map(str::to_owned));
    let on = |sites: &[&str]| host.as_deref().is_some_and(|h| sites.iter().any(|s| h == *s || h.ends_with(&format!(".{s}"))));
    let browser = BROWSERS.iter().any(|b| track.player.to_lowercase().contains(b));
    let video = if browser { !on(&MUSIC_SITES) } else { on(&VIDEO_SITES) };
    let mut timestamps = json!({ "start": start });
    if let Some(length) = track.length_ms.filter(|l| *l > 0) {
        timestamps["end"] = json!(start + length);
    }
    let mut activity = json!({
        "name": track.player,
        "type": if video { 3 } else { 2 },
        "details": title,
        "timestamps": timestamps,
    });
    if !track.artists.is_empty() {
        activity["state"] = json!(track.artists.join(", "));
    }
    // Only web artwork can be shared; a file:// cover exists on this machine alone.
    if let Some(art) = track.art.as_deref().filter(|a| a.starts_with("https://") || a.starts_with("http://")) {
        activity["assets"] = json!({ "large_image": art, "large_text": track.album });
    }
    Some(activity)
}

fn text(v: &ZValue<'_>) -> Option<String> {
    match v {
        ZValue::Str(s) => Some(s.to_string()).filter(|s| !s.trim().is_empty()),
        ZValue::ObjectPath(p) => Some(p.to_string()),
        ZValue::Value(inner) => text(inner),
        _ => None,
    }
}

fn texts(v: &ZValue<'_>) -> Vec<String> {
    match v {
        ZValue::Array(a) => a.iter().filter_map(text).collect(),
        ZValue::Value(inner) => texts(inner),
        other => text(other).into_iter().collect(),
    }
}

fn number(v: &ZValue<'_>) -> Option<u64> {
    match v {
        ZValue::I64(n) => u64::try_from(*n).ok(),
        ZValue::U64(n) => Some(*n),
        ZValue::I32(n) => u64::try_from(*n).ok(),
        ZValue::U32(n) => Some(u64::from(*n)),
        ZValue::F64(n) if *n >= 0. => Some(*n as u64),
        ZValue::Value(inner) => number(inner),
        _ => None,
    }
}

/// The first player reporting `Playing`, with its track.
async fn playing(conn: &Connection) -> zbus::Result<(Vec<String>, Option<Track>)> {
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    let names: Vec<String> = dbus.list_names().await?.into_iter().map(|n| n.to_string()).filter(|n| n.starts_with(PREFIX)).collect();
    let mut players = Vec::new();
    let mut found = None;
    for name in &names {
        let root = Proxy::new(conn, name.as_str(), PATH, "org.mpris.MediaPlayer2").await?;
        let identity: String = root.get_property("Identity").await.unwrap_or_else(|_| name.trim_start_matches(PREFIX).to_owned());
        players.push(identity.clone());
        if found.is_some() {
            continue;
        }
        let player = Proxy::new(conn, name.as_str(), PATH, "org.mpris.MediaPlayer2.Player").await?;
        let status: String = player.get_property("PlaybackStatus").await.unwrap_or_default();
        if status != "Playing" {
            continue;
        }
        let meta: HashMap<String, OwnedValue> = player.get_property("Metadata").await.unwrap_or_default();
        let get = |key: &str| meta.get(key).map(|v| &**v);
        let position: i64 = player.get_property("Position").await.unwrap_or(0);
        found = Some(Track {
            player: identity,
            title: get("xesam:title").and_then(text),
            artists: get("xesam:artist").map(texts).unwrap_or_default(),
            album: get("xesam:album").and_then(text),
            art: get("mpris:artUrl").and_then(text),
            url: get("xesam:url").and_then(text),
            length_ms: get("mpris:length").and_then(number).map(|us| us / 1000),
            position_ms: u64::try_from(position).unwrap_or(0) / 1000,
        });
    }
    Ok((players, found))
}

pub async fn run(reports: Reports) {
    let conn = match Connection::session().await {
        Ok(conn) => conn,
        Err(err) => {
            log::info!("rpc: no session bus for MPRIS: {err}");
            return;
        }
    };
    let mut last: Option<(Track, u64)> = None;
    loop {
        match playing(&conn).await {
            Ok((players, track)) => {
                let _ = reports.send(SourceEvent::Players(players));
                let next = track.map(|t| {
                    let start = now_ms().saturating_sub(t.position_ms);
                    // Same track, no seek: keep the old start so viewers' clocks don't jitter. A
                    // player that never reports a position (many browsers) can't seek either.
                    let keep = last.as_ref().filter(|(prev, s)| {
                        let same = Track { position_ms: 0, ..prev.clone() } == Track { position_ms: 0, ..t.clone() };
                        same && (t.position_ms == 0 || s.abs_diff(start) < SEEK_TOLERANCE_MS)
                    });
                    let start = keep.map_or(start, |(_, s)| *s);
                    (t, start)
                });
                if next != last {
                    let activity = next.as_ref().and_then(|(t, start)| activity(t, *start));
                    let _ = reports.send(SourceEvent::Report(Report { key: "mpris".into(), activity, priority: PRIORITY_DETECTED }));
                    last = next;
                }
            }
            Err(err) => log::debug!("rpc: mpris: {err}"),
        }
        tokio::time::sleep(POLL_EVERY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track() -> Track {
        Track {
            player: "Spotify".into(),
            title: Some("Song".into()),
            artists: vec!["A".into(), "B".into()],
            album: Some("Album".into()),
            art: Some("https://i.scdn.co/image/x".into()),
            url: None,
            length_ms: Some(200_000),
            position_ms: 0,
        }
    }

    #[test]
    fn a_song_is_listening_with_progress() {
        let a = activity(&track(), 1000).unwrap();
        assert_eq!(a["type"], 2);
        assert_eq!(a["name"], "Spotify");
        assert_eq!(a["state"], "A, B");
        assert_eq!(a["timestamps"]["end"], 201_000);
        assert_eq!(a["assets"]["large_image"], "https://i.scdn.co/image/x");
    }

    #[test]
    fn browsers_are_video_unless_music() {
        let firefox = Track { player: "Mozilla Firefox".into(), url: Some("https://example.org/talk".into()), ..track() };
        assert_eq!(activity(&firefox, 0).unwrap()["type"], 3);
        let spotify_web = Track { url: Some("https://open.spotify.com/track/x".into()), ..firefox.clone() };
        assert_eq!(activity(&spotify_web, 0).unwrap()["type"], 2);
        assert_eq!(activity(&Track { url: None, ..firefox }, 0).unwrap()["type"], 3);
    }

    #[test]
    fn a_browser_video_is_watching_and_local_art_stays_home() {
        let t = Track { url: Some("https://www.youtube.com/watch?v=x".into()), art: Some("file:///tmp/c.png".into()), ..track() };
        let a = activity(&t, 0).unwrap();
        assert_eq!(a["type"], 3);
        assert!(a.get("assets").is_none());
        assert!(activity(&Track { title: None, ..track() }, 0).is_none());
    }
}
