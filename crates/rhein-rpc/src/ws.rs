//! Discord's local WebSocket RPC on 127.0.0.1:6463–6472, as arRPC serves it: JSON only, version 1,
//! and from a browser only for Discord's own origins.

use std::sync::atomic::{AtomicU64, Ordering};

use futures::{SinkExt as _, StreamExt as _};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

use crate::Reports;
use crate::commands::{Session, ready};

pub const PORTS: std::ops::RangeInclusive<u16> = 6463..=6472;
const ALLOWED_ORIGINS: [&str; 3] = ["https://discord.com", "https://ptb.discord.com", "https://canary.discord.com"];

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Binds the first free port of Discord's range.
pub async fn bind() -> anyhow::Result<(TcpListener, u16)> {
    for port in PORTS {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)).await {
            return Ok((listener, port));
        }
    }
    anyhow::bail!("ports 6463-6472 are all taken")
}

pub async fn serve(listener: TcpListener, reports: Reports) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let reports = reports.clone();
                tokio::spawn(async move {
                    if let Err(err) = client(stream, reports).await {
                        log::debug!("rpc ws client: {err}");
                    }
                });
            }
            Err(err) => {
                log::warn!("rpc ws accept: {err}");
                return;
            }
        }
    }
}

/// The client id from the query string, once the request passes arRPC's checks.
pub fn check(uri: &str, origin: Option<&str>) -> Result<String, &'static str> {
    let query = url::Url::parse(&format!("http://localhost{uri}")).map_err(|_| "bad request")?;
    let param = |name: &str| query.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
    if let Some(origin) = origin.filter(|o| !o.is_empty())
        && !ALLOWED_ORIGINS.contains(&origin)
    {
        return Err("origin not allowed");
    }
    if param("encoding").is_some_and(|e| e != "json") {
        return Err("unsupported encoding");
    }
    if param("v").is_some_and(|v| v != "1") {
        return Err("unsupported version");
    }
    Ok(param("client_id").unwrap_or_default())
}

async fn client(stream: TcpStream, reports: Reports) -> anyhow::Result<()> {
    let mut client_id = String::new();
    // The callback's signature is tungstenite's, large error type and all.
    #[allow(clippy::result_large_err)]
    let callback = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let origin = request.headers().get("origin").and_then(|o| o.to_str().ok());
        match check(&request.uri().to_string(), origin) {
            Ok(id) => {
                client_id = id;
                Ok(response)
            }
            Err(why) => {
                let mut refusal = ErrorResponse::new(Some(why.to_owned()));
                *refusal.status_mut() = StatusCode::FORBIDDEN;
                Err(refusal)
            }
        }
    };
    let mut socket = tokio_tungstenite::accept_hdr_async(stream, callback).await?;
    let key = format!("ws:{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let session = Session::open(key, client_id, reports);
    socket.send(Message::text(ready().to_string())).await?;
    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(text) => {
                let Ok(frame) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(reply) = session.handle(&frame) {
                    socket.send(Message::text(reply.to_string())).await?;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_like_arrpc() {
        assert_eq!(check("/?v=1&client_id=42&encoding=json", None), Ok("42".into()));
        assert_eq!(check("/?v=1&client_id=42", Some("https://discord.com")), Ok("42".into()));
        assert!(check("/?v=1", Some("https://evil.example")).is_err());
        assert!(check("/?v=2", None).is_err());
        assert!(check("/?encoding=etf", None).is_err());
    }
}
