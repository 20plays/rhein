//! Discord's local IPC: a Unix socket named `discord-ipc-N` in the runtime directory, speaking
//! frames of `op: u32 LE, length: u32 LE, JSON`. Rhein takes the first free slot, the way arRPC
//! does, so a running Discord keeps its own.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};

use crate::Reports;
use crate::commands::{Session, ready};

pub const OP_HANDSHAKE: u32 = 0;
pub const OP_FRAME: u32 = 1;
pub const OP_CLOSE: u32 = 2;
pub const OP_PING: u32 = 3;
pub const OP_PONG: u32 = 4;

/// Frames bigger than this are refused; activities are a few hundred bytes.
const MAX_FRAME: usize = 64 * 1024;
const INVALID_CLIENT_ID: u32 = 4000;
const INVALID_VERSION: u32 = 4004;
const CLOSE_UNSUPPORTED: u32 = 1003;

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Where Discord clients look: `$XDG_RUNTIME_DIR` (or the temp directory).
pub fn runtime_dir() -> PathBuf {
    ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .iter()
        .find_map(|k| std::env::var_os(k).filter(|v| !v.is_empty()))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

/// Binds the first free `discord-ipc-{0..9}` in `dir`. A socket that answers belongs to a
/// running Discord (or another bridge) and is left alone; a dead one is replaced.
pub async fn bind(dir: &Path) -> anyhow::Result<(UnixListener, PathBuf)> {
    for i in 0..10 {
        let path = dir.join(format!("discord-ipc-{i}"));
        if path.exists() {
            let alive = tokio::time::timeout(Duration::from_millis(300), UnixStream::connect(&path)).await;
            if matches!(alive, Ok(Ok(_))) {
                continue;
            }
            let _ = std::fs::remove_file(&path);
        }
        match UnixListener::bind(&path) {
            Ok(listener) => return Ok((listener, path)),
            Err(err) => log::debug!("rpc: {}: {err}", path.display()),
        }
    }
    anyhow::bail!("no free discord-ipc socket in {}", dir.display())
}

/// Accepts clients until the listener fails. The socket file is removed when this ends.
pub async fn serve(listener: UnixListener, path: PathBuf, reports: Reports) {
    let _cleanup = RemoveOnDrop(path);
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let reports = reports.clone();
                tokio::spawn(async move {
                    if let Err(err) = client(stream, reports).await {
                        log::debug!("rpc ipc client: {err}");
                    }
                });
            }
            Err(err) => {
                log::warn!("rpc ipc accept: {err}");
                return;
            }
        }
    }
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub async fn read_frame(stream: &mut UnixStream) -> anyhow::Result<(u32, Value)> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).await?;
    let op = u32::from_le_bytes(header[..4].try_into()?);
    let len = u32::from_le_bytes(header[4..].try_into()?) as usize;
    anyhow::ensure!(op <= OP_PONG, "invalid op {op}");
    anyhow::ensure!(len <= MAX_FRAME, "frame too large ({len} bytes)");
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).await?;
    let value = if body.is_empty() { Value::Null } else { serde_json::from_slice(&body)? };
    Ok((op, value))
}

pub async fn write_frame(stream: &mut UnixStream, op: u32, value: &Value) -> anyhow::Result<()> {
    let body = serde_json::to_vec(value)?;
    let mut frame = Vec::with_capacity(body.len() + 8);
    frame.extend_from_slice(&op.to_le_bytes());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    stream.write_all(&frame).await?;
    Ok(())
}

async fn close(stream: &mut UnixStream, code: u32, message: &str) {
    let _ = write_frame(stream, OP_CLOSE, &json!({ "code": code, "message": message })).await;
    let _ = stream.shutdown().await;
}

async fn client(mut stream: UnixStream, reports: Reports) -> anyhow::Result<()> {
    let mut session: Option<Session> = None;
    loop {
        let (op, value) = match read_frame(&mut stream).await {
            Ok(frame) => frame,
            Err(err) => {
                // A clean disconnect ends here too; anything else gets a close frame first.
                if !err.downcast_ref::<std::io::Error>().is_some_and(|e| e.kind() == std::io::ErrorKind::UnexpectedEof) {
                    close(&mut stream, CLOSE_UNSUPPORTED, &err.to_string()).await;
                }
                return Ok(());
            }
        };
        match op {
            OP_PING => write_frame(&mut stream, OP_PONG, &value).await?,
            OP_PONG => {}
            OP_CLOSE => {
                let _ = stream.shutdown().await;
                return Ok(());
            }
            OP_HANDSHAKE if session.is_none() => {
                let version = value.get("v").and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok())).unwrap_or(1);
                let client_id = value.get("client_id").and_then(Value::as_str).unwrap_or_default().to_owned();
                if version != 1 {
                    close(&mut stream, INVALID_VERSION, "unsupported version").await;
                    return Ok(());
                }
                if client_id.is_empty() {
                    close(&mut stream, INVALID_CLIENT_ID, "client id required").await;
                    return Ok(());
                }
                let key = format!("ipc:{}", NEXT.fetch_add(1, Ordering::Relaxed));
                session = Some(Session::open(key, client_id, reports.clone()));
                write_frame(&mut stream, OP_FRAME, &ready()).await?;
            }
            OP_FRAME => {
                let Some(session) = &session else {
                    close(&mut stream, CLOSE_UNSUPPORTED, "need to handshake first").await;
                    return Ok(());
                };
                if let Some(reply) = session.handle(&value) {
                    write_frame(&mut stream, OP_FRAME, &reply).await?;
                }
            }
            _ => {
                close(&mut stream, CLOSE_UNSUPPORTED, "unexpected frame").await;
                return Ok(());
            }
        }
    }
}
