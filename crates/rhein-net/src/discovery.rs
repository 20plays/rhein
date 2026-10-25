use anyhow::{Context as _, Result, bail};
use rhein_proto::Discovery;

/// Maps a logical server name to its API base and gateway, the way the web client does: HTTPS
/// first, plain HTTP as a fallback for local development, following `moved_to` a bounded number
/// of times.
pub async fn discover(http: &reqwest::Client, domain: &str) -> Result<Discovery> {
    let start = normalise(domain);
    let mut name = start.clone();
    for _ in 0..4 {
        let mut doc = fetch(http, &name).await?;
        match doc.moved_to.as_deref() {
            Some(next) if !next.is_empty() && normalise(next) != name => name = normalise(next),
            _ => {
                if name != start {
                    doc.relocated_to = Some(name);
                }
                return Ok(doc);
            }
        }
    }
    bail!("{domain}: too many moved_to redirects")
}

/// Accepts `a.example`, `https://a.example/` or `localhost:8448` and returns a bare authority,
/// keeping an explicit scheme when one was given.
pub fn normalise(input: &str) -> String {
    input.trim().trim_end_matches('/').to_owned()
}

async fn fetch(http: &reqwest::Client, name: &str) -> Result<Discovery> {
    let candidates: Vec<String> = if name.starts_with("http://") || name.starts_with("https://") {
        vec![name.to_owned()]
    } else {
        vec![format!("https://{name}"), format!("http://{name}")]
    };
    let mut last = None;
    for base in candidates {
        let url = format!("{base}/.well-known/telium/server");
        match http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let mut doc: Discovery =
                    resp.json().await.with_context(|| format!("{url}: not a Telium server"))?;
                if doc.api.is_empty() {
                    doc.api = base.clone();
                }
                doc.api = doc.api.trim_end_matches('/').to_owned();
                if doc.server_name.is_empty() {
                    bail!("{url}: discovery document has no server_name");
                }
                return Ok(doc);
            }
            Ok(resp) => last = Some(anyhow::anyhow!("{url}: HTTP {}", resp.status())),
            Err(err) => last = Some(anyhow::anyhow!("{url}: {err}")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("{name}: unreachable")))
}
