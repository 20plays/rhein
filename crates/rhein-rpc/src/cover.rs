//! Album covers for players that only offer a local file (`file://…` over MPRIS, which nobody
//! else could load): looked up by artist and album, or artist and title, in public music
//! catalogues (iTunes' search, then Deezer's). A result counts only when its artist matches, so
//! a wrong cover is rarer than none. Each lookup is asked once and remembered.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

/// Letters and digits only, lower case: how names are compared.
fn plain(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Whether a catalogue's artist is the one playing (either name containing the other).
fn same_artist(found: &str, wanted: &str) -> bool {
    let (found, wanted) = (plain(found), plain(wanted));
    !found.is_empty() && !wanted.is_empty() && (found.contains(&wanted) || wanted.contains(&found))
}

/// The cover in an iTunes search answer, large.
pub fn from_itunes(answer: &Value, artist: &str) -> Option<String> {
    answer["results"].as_array()?.iter().find(|r| r["artistName"].as_str().is_some_and(|a| same_artist(a, artist))).and_then(|r| {
        let art = r["artworkUrl100"].as_str()?;
        Some(art.replace("100x100bb", "600x600bb")).filter(|u| u.starts_with("https://"))
    })
}

/// The cover in a Deezer search answer.
pub fn from_deezer(answer: &Value, artist: &str) -> Option<String> {
    answer["data"].as_array()?.iter().find(|r| r["artist"]["name"].as_str().is_some_and(|a| same_artist(a, artist))).and_then(|r| {
        let album = if r["album"].is_object() { &r["album"] } else { r };
        album["cover_xl"].as_str().or_else(|| album["cover_big"].as_str()).map(str::to_owned).filter(|u| u.starts_with("https://"))
    })
}

#[derive(Default)]
pub struct Covers {
    http: reqwest::Client,
    known: HashMap<(String, String), Option<String>>,
}

impl Covers {
    /// The cover for a track, or `None` when the catalogues don't have it.
    pub async fn find(&mut self, artist: &str, album: Option<&str>, title: Option<&str>) -> Option<String> {
        let what = album.or(title)?.to_owned();
        let key = (plain(artist), plain(&what));
        if let Some(known) = self.known.get(&key) {
            return known.clone();
        }
        let found = self.search(artist, album, title).await;
        self.known.insert(key, found.clone());
        found
    }

    async fn search(&self, artist: &str, album: Option<&str>, title: Option<&str>) -> Option<String> {
        let mut asks = Vec::new();
        if let Some(album) = album {
            asks.push(("album", format!("{artist} {album}")));
        }
        if let Some(title) = title {
            asks.push(("song", format!("{artist} {title}")));
        }
        for (entity, term) in &asks {
            let url = format!("https://itunes.apple.com/search?media=music&entity={entity}&limit=5&term={}", urlencoding::encode(term));
            if let Some(cover) = self.get(&url).await.and_then(|a| from_itunes(&a, artist)) {
                return Some(cover);
            }
        }
        let query = match (album, title) {
            (Some(album), _) => format!("artist:\"{artist}\" album:\"{album}\""),
            (None, Some(title)) => format!("artist:\"{artist}\" track:\"{title}\""),
            _ => return None,
        };
        let path = if album.is_some() { "album" } else { "track" };
        let url = format!("https://api.deezer.com/search/{path}?limit=5&q={}", urlencoding::encode(&query));
        self.get(&url).await.and_then(|a| from_deezer(&a, artist))
    }

    async fn get(&self, url: &str) -> Option<Value> {
        let response = self.http.get(url).timeout(Duration::from_secs(6)).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json().await.ok()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn only_the_right_artist_counts() {
        let itunes = json!({"results": [
            {"artistName": "Someone Else", "artworkUrl100": "https://is1.mzstatic.com/a/100x100bb.jpg"},
            {"artistName": "G.A.B.B.E.R.", "artworkUrl100": "https://is1.mzstatic.com/b/100x100bb.jpg"},
        ]});
        assert_eq!(super::from_itunes(&itunes, "GABBER").as_deref(), Some("https://is1.mzstatic.com/b/600x600bb.jpg"));
        assert_eq!(super::from_itunes(&itunes, "Nobody"), None);
        let deezer = json!({"data": [{"artist": {"name": "Dither"}, "cover_xl": "https://e-cdns-images.dzcdn.net/x/1000x1000.jpg"}]});
        assert_eq!(super::from_deezer(&deezer, "dither").as_deref(), Some("https://e-cdns-images.dzcdn.net/x/1000x1000.jpg"));
        let deezer_track = json!({"data": [{"artist": {"name": "Dither"}, "album": {"cover_xl": "https://e-cdns-images.dzcdn.net/y.jpg"}}]});
        assert_eq!(super::from_deezer(&deezer_track, "Dither").as_deref(), Some("https://e-cdns-images.dzcdn.net/y.jpg"));
    }
}
