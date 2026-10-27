//! Link previews, validated the way the web client's `readEmbed` does.
//!
//! The guild server fetches the page and re-hosts its picture or video under its own `/camo/`
//! path; the client accepts media only from there, so a message can never make a member's client
//! contact an arbitrary host.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbedKind {
    Link,
    Image,
    Video,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Embed {
    pub url: String,
    pub site: String,
    pub kind: EmbedKind,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    pub video: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

const TITLE_MAX: usize = 256;
const DESCRIPTION_MAX: usize = 512;

/// `api_base` is the server that answered; `url` is the link we asked about. The card's `url`
/// and `site` come from our own link, never from the payload.
pub fn read(payload: &Value, api_base: &str, url: &str) -> Option<Embed> {
    if payload.get("status").and_then(Value::as_str) != Some("ok") {
        return None;
    }
    let kind = match payload.get("kind").and_then(Value::as_str) {
        Some("image") => EmbedKind::Image,
        Some("video") => EmbedKind::Video,
        _ => EmbedKind::Link,
    };
    let text = |key: &str, max: usize| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(max).collect::<String>())
    };
    let media = |key: &str| payload.get(key).and_then(Value::as_str).filter(|u| is_own_media(u, api_base)).map(str::to_owned);
    let dimension = |key: &str| {
        payload.get(key).and_then(Value::as_u64).filter(|v| (1..=16384).contains(v)).map(|v| v as u32)
    };
    let site = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_default();
    let embed = Embed {
        url: url.to_owned(),
        site,
        kind,
        title: text("title", TITLE_MAX),
        description: text("description", DESCRIPTION_MAX),
        image: media("image"),
        video: media("video"),
        width: dimension("video_width"),
        height: dimension("video_height"),
    };
    // A card with nothing to show is no card.
    let empty = embed.title.is_none() && embed.description.is_none() && embed.image.is_none() && embed.video.is_none();
    (!empty).then_some(embed)
}

/// Absolute, same origin as the answering server, under its camo or media path, no userinfo.
fn is_own_media(candidate: &str, api_base: &str) -> bool {
    let Some(media) = on_server(candidate, api_base) else { return false };
    media.path().starts_with("/_telium/v1/camo/") || media.path().starts_with("/_telium/v1/media/")
}

/// One of the answering server's own media files as it is: same origin, no userinfo, and a media
/// hash path (`media/{hash}` or `media/remote/{origin}/{hash}`), nothing else under it.
pub fn is_own_media_file(candidate: &str, api_base: &str) -> bool {
    on_server(candidate, api_base).is_some() && crate::gif_links::is_telium_media(candidate)
}

/// `candidate` parsed, when it is absolute, same origin as `api_base` and carries no userinfo.
fn on_server(candidate: &str, api_base: &str) -> Option<url::Url> {
    let (media, base) = (url::Url::parse(candidate).ok()?, url::Url::parse(api_base).ok()?);
    (media.origin() == base.origin() && media.username().is_empty() && media.password().is_none()).then_some(media)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_own_camo_only() {
        let base = "https://g.x";
        let ok = json!({"status":"ok","kind":"link","title":"T","description":"D",
            "image":"https://g.x/_telium/v1/camo/abc","site":"evil.example"});
        let e = read(&ok, base, "https://news.example/a").unwrap();
        assert_eq!(e.site, "news.example");
        assert_eq!(e.image.as_deref(), Some("https://g.x/_telium/v1/camo/abc"));

        let foreign = json!({"status":"ok","title":"T","image":"https://tracker.example/pixel.png"});
        assert_eq!(read(&foreign, base, "https://a.example").unwrap().image, None);
        let userinfo = json!({"status":"ok","title":"T","image":"https://u:p@g.x/_telium/v1/camo/a"});
        assert_eq!(read(&userinfo, base, "https://a.example").unwrap().image, None);
    }

    #[test]
    fn refuses_failures_and_empty_cards() {
        assert!(read(&json!({"status":"error","title":"T"}), "https://g.x", "https://a.example").is_none());
        assert!(read(&json!({"status":"ok"}), "https://g.x", "https://a.example").is_none());
    }

    #[test]
    fn clamps_text_and_kind() {
        let long = "x".repeat(600);
        let e = read(&json!({"status":"ok","kind":"weird","title":long,"description":long}), "https://g.x", "https://a.example").unwrap();
        assert_eq!(e.kind, EmbedKind::Link);
        assert_eq!(e.title.unwrap().len(), 256);
        assert_eq!(e.description.unwrap().len(), 512);
    }
}
