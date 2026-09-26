//! "Protect my IP": guild traffic through the account home's relay (`server/src/relay.rs`,
//! `docs/network-privacy.md`), so guild servers see the home's address instead of ours.
//!
//! A [`RelayProxy`] is a small HTTP CONNECT proxy on 127.0.0.1. The guild's REST client uses it
//! as its proxy and the gateway connects through it, so each connection to the guild is
//! `CONNECT host:port` here, and for each one this opens a WebSocket to the relay, names the
//! destination (`{"op": "OPEN", "d": {server_name, host, port}}`), waits for `READY` and then
//! passes bytes through as binary frames both ways. The TLS session to the guild runs inside,
//! end to end: the certificate checked is the guild's, and the relay only ever carries
//! ciphertext. If the relay refuses or can't be reached, the CONNECT fails and so does the
//! connection: protected mode fails closed.

use std::net::SocketAddr;

use futures::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

use crate::Api;

/// A running proxy for one guild server; stops when dropped.
pub struct RelayProxy {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for RelayProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl RelayProxy {
    /// Starts a proxy that tunnels to `server_name`'s hosts through the relay of `home`.
    pub async fn start(home: Api, server_name: String) -> std::io::Result<RelayProxy> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let relay = relay_url(home.base());
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let (home, relay, server) = (home.clone(), relay.clone(), server_name.clone());
                tokio::spawn(async move {
                    if let Err(err) = serve(client, &home, &relay, &server).await {
                        log::info!("relay tunnel to {server}: {err:#}");
                    }
                });
            }
        });
        Ok(RelayProxy { addr, task })
    }

    /// Where the proxy listens.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// A proxy setting for reqwest: everything this client sends goes through the tunnel.
    pub fn reqwest_proxy(&self) -> reqwest::Result<reqwest::Proxy> {
        reqwest::Proxy::all(format!("http://{}", self.addr))
    }
}

/// `https://home.example` → `wss://home.example/_telium/v1/relay/v1`.
pub fn relay_url(home_base: &str) -> String {
    let base = home_base.trim_end_matches('/');
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_owned()
    };
    format!("{ws}/_telium/v1/relay/v1")
}

/// Reads `CONNECT host:port HTTP/1.1` and its headers; the destination.
async fn read_connect(client: &mut TcpStream) -> anyhow::Result<(String, u16)> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(head.len() < 8192, "request head too long");
        let n = client.read(&mut byte).await?;
        anyhow::ensure!(n == 1, "closed before the request");
        head.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&head).lines().next().unwrap_or_default().to_owned();
    let mut parts = line.split_whitespace();
    anyhow::ensure!(parts.next() == Some("CONNECT"), "only CONNECT goes through the relay");
    let target = parts.next().unwrap_or_default();
    let (host, port) = target.rsplit_once(':').ok_or_else(|| anyhow::anyhow!("no port in {target}"))?;
    Ok((host.trim_matches(['[', ']']).to_ascii_lowercase(), port.parse()?))
}

async fn serve(mut client: TcpStream, home: &Api, relay: &str, server_name: &str) -> anyhow::Result<()> {
    let (host, port) = read_connect(&mut client).await?;
    let opened = open(home, relay, server_name, &host, port).await;
    let socket = match opened {
        Ok(socket) => socket,
        Err(err) => {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n").await;
            return Err(err);
        }
    };
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    let (mut sink, mut stream) = socket.split();
    let (mut read, mut write) = client.into_split();
    let up = async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = read.read(&mut buf).await?;
            if n == 0 {
                let _ = sink.send(Message::Close(None)).await;
                return anyhow::Ok(());
            }
            sink.send(Message::Binary(buf[..n].to_vec().into())).await?;
        }
    };
    let down = async move {
        while let Some(message) = stream.next().await {
            match message? {
                Message::Binary(bytes) => write.write_all(&bytes).await?,
                Message::Close(_) => break,
                _ => {}
            }
        }
        let _ = write.shutdown().await;
        anyhow::Ok(())
    };
    // Either side ending ends the tunnel.
    tokio::select! {
        r = up => r,
        r = down => r,
    }
}

type Socket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

/// Opens a relay tunnel to `host:port` of `server_name`, ready for bytes.
async fn open(home: &Api, relay: &str, server_name: &str, host: &str, port: u16) -> anyhow::Result<Socket> {
    let mut request = relay.into_client_request()?;
    let token = home.bearer().await.ok_or_else(|| anyhow::anyhow!("not signed in"))?;
    request.headers_mut().insert("Authorization", format!("Bearer {token}").parse()?);
    let (mut socket, _) = tokio::time::timeout(std::time::Duration::from_secs(15), tokio_tungstenite::connect_async(request)).await??;
    let open = json!({ "op": "OPEN", "d": { "server_name": server_name, "host": host, "port": port } });
    socket.send(Message::Text(open.to_string().into())).await?;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(15), socket.next()).await? {
            Some(Ok(Message::Text(text))) => {
                let answer: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                match answer["op"].as_str() {
                    Some("READY") => return Ok(socket),
                    _ => anyhow::bail!("the relay refused: {}", answer["d"]["message"].as_str().or(answer["d"]["error"].as_str()).unwrap_or(&text)),
                }
            }
            Some(Ok(Message::Close(frame))) => anyhow::bail!("the relay refused: {}", frame.map(|f| f.reason.to_string()).unwrap_or_default()),
            Some(Ok(_)) => {}
            Some(Err(err)) => return Err(err.into()),
            None => anyhow::bail!("the relay closed"),
        }
    }
}

/// Opens a TCP stream to `host:port` through a proxy's CONNECT, for the gateway's WebSocket
/// (which then runs its own TLS inside).
pub async fn connect_via(proxy: SocketAddr, host: &str, port: u16) -> anyhow::Result<TcpStream> {
    let mut stream = TcpStream::connect(proxy).await?;
    stream.write_all(format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").as_bytes()).await?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(head.len() < 8192, "proxy answer too long");
        anyhow::ensure!(stream.read(&mut byte).await? == 1, "the proxy closed");
        head.push(byte[0]);
    }
    let status = String::from_utf8_lossy(&head).lines().next().unwrap_or_default().to_owned();
    anyhow::ensure!(status.split_whitespace().nth(1) == Some("200"), "the relay couldn't reach {host}: {status}");
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_urls() {
        assert_eq!(relay_url("https://home.example/"), "wss://home.example/_telium/v1/relay/v1");
        assert_eq!(relay_url("http://localhost:8448"), "ws://localhost:8448/_telium/v1/relay/v1");
    }

    /// A relay that opens, answers READY and echoes, and a CONNECT through the proxy to it.
    #[tokio::test]
    #[allow(clippy::result_large_err)]
    async fn bytes_go_through_the_relay() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(tcp, |req: &tokio_tungstenite::tungstenite::handshake::server::Request, resp| {
                assert_eq!(req.headers().get("Authorization").unwrap(), "Bearer t0k");
                Ok(resp)
            })
            .await
            .unwrap();
            let Some(Ok(Message::Text(open))) = ws.next().await else { panic!("no OPEN") };
            let open: Value = serde_json::from_str(&open).unwrap();
            assert_eq!((open["op"].as_str(), open["d"]["host"].as_str(), open["d"]["port"].as_u64()), (Some("OPEN"), Some("guild.example"), Some(443)));
            ws.send(Message::Text(json!({ "op": "READY" }).to_string().into())).await.unwrap();
            while let Some(Ok(Message::Binary(b))) = ws.next().await {
                ws.send(Message::Binary(b)).await.unwrap();
            }
        });
        let home = Api::new(reqwest::Client::new(), &format!("http://{relay_addr}"), Some("t0k".into()));
        let proxy = RelayProxy::start(home, "guild.example".into()).await.unwrap();
        let mut stream = connect_via(proxy.addr(), "guild.example", 443).await.unwrap();
        stream.write_all(b"ping through").await.unwrap();
        let mut back = [0u8; 12];
        stream.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"ping through");
    }
}
