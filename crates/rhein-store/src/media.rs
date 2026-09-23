//! Turning media references into fetchable URLs, as the web client does (`web/src/lib/media.js`).
//!
//! A reference is `"{hash}?cap={capability}"`. Media owned by this server is served locally;
//! media owned by another server is fetched through this server's read-through cache, so the
//! client only ever talks to servers it already has a session with.

/// `base` is the API base of the server asked; `origin` is the owning server when it is not
/// that one.
pub fn media_url(base: &str, reference: &str, origin: Option<&str>) -> String {
    let (hash, query) = match reference.split_once('?') {
        Some((hash, query)) => (hash, Some(query)),
        None => (reference, None),
    };
    let hash = urlencoding::encode(hash);
    let mut url = match origin {
        Some(origin) => format!("{base}/_telium/v1/media/remote/{}/{hash}", urlencoding::encode(origin)),
        None => format!("{base}/_telium/v1/media/{hash}"),
    };
    if let Some(query) = query.filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(query);
    }
    url
}

/// The owning server of a user's media, when it is not `local_domain`.
pub fn remote_origin<'a>(user_id: &'a str, local_domain: &str) -> Option<&'a str> {
    rhein_proto::domain_of(user_id).filter(|d| *d != local_domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_and_remote() {
        assert_eq!(media_url("https://a.x", "abc?cap=k1", None), "https://a.x/_telium/v1/media/abc?cap=k1");
        assert_eq!(
            media_url("https://a.x", "abc?cap=k1", Some("b.x:8449")),
            "https://a.x/_telium/v1/media/remote/b.x%3A8449/abc?cap=k1"
        );
        assert_eq!(media_url("https://a.x", "abc", None), "https://a.x/_telium/v1/media/abc");
        assert_eq!(remote_origin("@u:b.x", "a.x"), Some("b.x"));
        assert_eq!(remote_origin("@u:a.x", "a.x"), None);
    }
}
