//! Which links are GIFs, as the web client decides it (`web/src/lib/gifs.js`). A GIF is the one
//! kind of link a client loads itself: the sender chose the media URL and the picture is the
//! message, so it never goes through the server's link preview. Share pages of Tenor, Giphy and
//! Klipy are HTML; they are turned into their media URL (Giphy by its id, Tenor and Klipy through
//! their public APIs).

use url::Url;

/// The hosts whose links are GIFs even without a `.gif` path.
pub const MEDIA_HOSTS: &[&str] = &[
    "klipy.com",
    "static.klipy.com",
    "static.klipy.co",
    "media.klipy.com",
    "media.giphy.com",
    "media1.giphy.com",
    "media2.giphy.com",
    "media3.giphy.com",
    "media4.giphy.com",
    "i.giphy.com",
    "images.giphy.com",
    "media.tenor.com",
    "media1.tenor.com",
    "media.tenor.co",
    "media1.tenor.co",
    "c.tenor.com",
];

/// Tenor's public web key, which the web client ships too.
pub const TENOR_API_KEY: &str = "AIzaSyCZt6SSh5VgVPzD9fhyzG1DprdPRhtoaR4";
pub const TENOR_CLIENT_KEY: &str = "tenor_web";
pub const TENOR_API_BASE: &str = "https://tenor.googleapis.com/v2";

/// A share page, and what it takes to find its GIF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Share {
    Tenor(String),
    Giphy(String),
    Klipy(String),
}

impl Share {
    pub fn provider(&self) -> &'static str {
        match self {
            Share::Tenor(_) => "Tenor",
            Share::Giphy(_) => "Giphy",
            Share::Klipy(_) => "Klipy",
        }
    }
}

fn https(value: &str) -> Option<Url> {
    Url::parse(value.trim()).ok().filter(|u| u.scheme() == "https")
}

fn decoded_path(url: &Url) -> String {
    urlencoding::decode(url.path()).map(|p| p.into_owned()).unwrap_or_else(|_| url.path().to_owned())
}

/// The share page a link is, if it is one.
pub fn share(value: &str) -> Option<Share> {
    let url = https(value)?;
    let host = url.host_str()?.to_ascii_lowercase();
    let path = decoded_path(&url);
    let host = host.strip_prefix("www.").unwrap_or(&host);
    match host {
        // `/view/title-gif-6459198981897272921`, or an id alone at the end.
        "tenor.com" => {
            let trimmed = path.trim_end_matches('/');
            let last = trimmed.rsplit('/').next().unwrap_or_default();
            let digits = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.to_owned());
            let lower = last.to_ascii_lowercase();
            let id = lower
                .rsplit_once("gif-")
                .filter(|(before, _)| before.is_empty() || before.ends_with('-'))
                .and_then(|(_, id)| digits(id))
                .or_else(|| digits(last))?;
            Some(Share::Tenor(id))
        }
        // `/gifs/Some-title-GAXXHdS0zXawVLOJLY`: the id is the last word of the slug.
        "giphy.com" => {
            let segments: Vec<&str> = path.trim_end_matches('/').split('/').filter(|s| !s.is_empty()).collect();
            let [first, slug] = segments[..] else { return None };
            if !first.eq_ignore_ascii_case("gifs") {
                return None;
            }
            let alnum = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
            let id = match slug.rsplit_once('-') {
                Some((_, id)) if alnum(id) => id,
                Some(_) => return None,
                None if alnum(slug) => slug,
                None => return None,
            };
            Some(Share::Giphy(id.to_owned()))
        }
        // `/gif/<slug>` or `/gifs/<slug>`.
        "klipy.com" if url.host_str()?.eq_ignore_ascii_case("klipy.com") => {
            let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
            let [first, slug] = segments[..] else { return None };
            (first.eq_ignore_ascii_case("gif") || first.eq_ignore_ascii_case("gifs")).then(|| Share::Klipy(slug.to_owned()))
        }
        _ => None,
    }
}

/// Giphy serves every GIF at a stable media URL made from its id.
pub fn giphy_media(id: &str) -> String {
    format!("https://media.giphy.com/media/{}/giphy.gif", urlencoding::encode(id))
}

fn host_matches(host: &str, hosts: &[&str]) -> bool {
    hosts.iter().any(|h| host == *h || host.strip_suffix(h).is_some_and(|rest| rest.ends_with('.')))
}

/// Whether a link is a GIF: a share page, a `.gif` path, or a GIF host.
pub fn is_gif(value: &str) -> bool {
    let Some(url) = https(value) else { return false };
    if share(value).is_some() {
        return true;
    }
    let tail = format!("{}{}", url.path(), url.query().map(|q| format!("?{q}")).unwrap_or_default()).to_ascii_lowercase();
    if tail.ends_with(".gif") || tail.contains(".gif?") || tail.contains(".gif#") {
        return true;
    }
    url.host_str().is_some_and(|h| host_matches(&h.to_ascii_lowercase(), MEDIA_HOSTS))
}

/// The GIF links in a message, in order, each once.
pub fn gifs_in(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in content.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'')) {
        let Some(at) = raw.find("https://") else { continue };
        let cleaned = raw[at..].trim_end_matches(|c: char| ")]}>.,;:!?'\"".contains(c));
        let Some(url) = https(cleaned) else { continue };
        let href = url.to_string();
        if is_gif(&href) && !out.contains(&href) {
            out.push(href);
        }
    }
    out
}

/// Whether a message is a single GIF link and nothing else (its text is then not shown).
pub fn is_gif_only(content: &str) -> bool {
    let text = content.trim();
    let gifs = gifs_in(text);
    gifs.len() == 1 && https(text).is_some_and(|u| u.to_string() == gifs[0])
}

/// The GIF of a Tenor `posts` answer for the post `id` (older share links carry the legacy id).
pub fn tenor_media(payload: &serde_json::Value, id: &str) -> Option<String> {
    let item = payload["results"].as_array()?.iter().find(|r| {
        let as_text = |v: &serde_json::Value| v.as_str().map(str::to_owned).or_else(|| v.as_u64().map(|n| n.to_string()));
        as_text(&r["id"]).as_deref() == Some(id) || as_text(&r["legacy_info"]["post_id"]).as_deref() == Some(id)
    })?;
    ["mediumgif", "gif", "tinygif", "nanogif"]
        .iter()
        .find_map(|f| item["media_formats"][f]["url"].as_str())
        .and_then(https)
        .map(|u| u.to_string())
}

/// The GIF of a Klipy search answer whose slug is `slug`.
pub fn klipy_media(payload: &serde_json::Value, slug: &str) -> Option<String> {
    let items = [&payload["data"]["data"], &payload["data"], &payload["results"], &payload["gifs"]].into_iter().find_map(|v| v.as_array())?;
    let wanted = slug.to_lowercase();
    let item = items.iter().find(|i| {
        i["slug"].as_str().map(str::to_lowercase).is_some_and(|s| s == wanted || s.starts_with(&format!("{wanted}--")))
    })?;
    [
        &item["file"]["md"]["gif"]["url"],
        &item["file"]["hd"]["gif"]["url"],
        &item["file"]["sm"]["gif"]["url"],
        &item["media_formats"]["gif"]["url"],
        &item["images"]["original"]["url"],
        &item["gif"]["url"],
        &item["url"],
    ]
    .into_iter()
    .find_map(|v| v.as_str().and_then(https))
    .map(|u| u.to_string())
}

/// The GIF links in a favourites export (Discord's, as the web client reads it): a list of
/// links, or an object of link → `{order, …}`, in their order; https links only.
pub fn favourites_in_export(document: &serde_json::Value) -> Vec<String> {
    let mut entries: Vec<(f64, String)> = match document {
        serde_json::Value::Array(items) => items.iter().enumerate().filter_map(|(i, v)| v.as_str().map(|u| (i as f64, u.to_owned()))).collect(),
        serde_json::Value::Object(map) => map
            .iter()
            .enumerate()
            .map(|(i, (url, meta))| (meta["order"].as_f64().unwrap_or(i as f64), url.clone()))
            .collect(),
        _ => Vec::new(),
    };
    entries.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<String> = Vec::new();
    for (_, url) in entries {
        if let Some(url) = https(&url).map(|u| u.to_string())
            && !out.contains(&url)
        {
            out.push(url);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn share_pages_as_the_web_client_reads_them() {
        assert_eq!(
            share("https://tenor.com/view/hypergamy-admin-learns-about-hypergamy-oy-vey-admin-gif-6459198981897272921"),
            Some(Share::Tenor("6459198981897272921".into()))
        );
        assert_eq!(share("https://tenor.com/view/12345"), Some(Share::Tenor("12345".into())));
        assert_eq!(
            share("https://giphy.com/gifs/RespectiveCollective-side-eye-cute-cat-fluffy-GAXXHdS0zXawVLOJLY"),
            Some(Share::Giphy("GAXXHdS0zXawVLOJLY".into()))
        );
        assert_eq!(share("https://klipy.com/gifs/big-brother-is-watching-you-5"), Some(Share::Klipy("big-brother-is-watching-you-5".into())));
        assert_eq!(share("http://tenor.com/view/x-gif-1"), None);
        assert_eq!(share("https://tenor.com/search/cats"), None);
        assert_eq!(giphy_media("GAXXHdS0zXawVLOJLY"), "https://media.giphy.com/media/GAXXHdS0zXawVLOJLY/giphy.gif");
    }

    #[test]
    fn gif_links_in_text() {
        let text = "look https://media1.tenor.com/m/abc/x.gif, and (https://example.com/a.gif?x=1) https://example.com/page https://c.tenor.com/q";
        assert_eq!(gifs_in(text), ["https://media1.tenor.com/m/abc/x.gif", "https://example.com/a.gif?x=1", "https://c.tenor.com/q"]);
        assert!(is_gif_only(" https://media.klipy.com/test/celebrate.gif "));
        assert!(!is_gif_only("lol https://media.klipy.com/test/celebrate.gif"));
        assert!(!is_gif("https://evil-tenor.com/x"));
    }

    #[test]
    fn a_discord_export_in_its_order() {
        let export = json!({
            "https://tenor.com/view/second-gif-2": {"order": 20, "src": "https://media.tenor.com/second.mp4"},
            "https://media.tenor.com/first.gif": {"order": 10},
            "http://insecure.example/x.gif": {"order": 5},
        });
        assert_eq!(favourites_in_export(&export), ["https://media.tenor.com/first.gif", "https://tenor.com/view/second-gif-2"]);
        assert_eq!(favourites_in_export(&json!(["https://a.example/1.gif", "nope", "https://a.example/1.gif"])), ["https://a.example/1.gif"]);
    }

    #[test]
    fn provider_answers() {
        let tenor = json!({"results":[{"id":"99","legacy_info":{"post_id":"6459"},"media_formats":{"gif":{"url":"https://media1.tenor.com/full.gif"},"mediumgif":{"url":"https://media1.tenor.com/m.gif"}}}]});
        assert_eq!(tenor_media(&tenor, "6459").as_deref(), Some("https://media1.tenor.com/m.gif"));
        assert_eq!(tenor_media(&tenor, "1"), None);
        let klipy = json!({"data":{"data":[{"slug":"other","file":{"md":{"gif":{"url":"https://media.klipy.com/o.gif"}}}},{"slug":"big-5--x","file":{"hd":{"gif":{"url":"https://media.klipy.com/b.gif"}}}}]}});
        assert_eq!(klipy_media(&klipy, "big-5").as_deref(), Some("https://media.klipy.com/b.gif"));
    }
}
