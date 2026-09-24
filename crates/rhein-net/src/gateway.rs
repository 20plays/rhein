//! One realtime connection per server. HELLO → IDENTIFY → READY, heartbeats, and reconnect with
//! backoff. There is no resume in Telium: every reconnect identifies afresh and the consumer must
//! treat the next READY as a full replacement of that server's state.

use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};
use rhein_proto::{Event, Frame, Hello, Ready, close};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayStatus {
    Connecting,
    Ready,
    /// Waiting to reconnect after a drop.
    Backoff,
    /// The token was refused; the owner has to mint a new session and call `reconnect`.
    Rejected,
    /// The server speaks a protocol window this client does not; retrying will not help.
    Incompatible,
}

#[derive(Debug)]
pub enum GatewayEvent {
    Status(GatewayStatus),
    Ready(Box<Ready>),
    Dispatch(Box<Event>),
}

enum Command {
    Send(Value),
    Reconnect(String),
    /// Remember a presence for the next IDENTIFY and send it now.
    Presence(String, Option<String>),
    Close,
}

/// The owner's side of a connection. Dropping it closes the socket.
pub struct GatewayHandle {
    commands: mpsc::UnboundedSender<Command>,
}

impl GatewayHandle {
    pub fn send(&self, frame: Value) {
        let _ = self.commands.send(Command::Send(frame));
    }

    /// Sets this connection's presence now and for every later IDENTIFY.
    pub fn set_presence(&self, status: String, custom_status: Option<String>) {
        let _ = self.commands.send(Command::Presence(status, custom_status));
    }

    /// Reconnects now with a fresh token (after a renewed session).
    pub fn reconnect(&self, token: String) {
        let _ = self.commands.send(Command::Reconnect(token));
    }
}

impl Drop for GatewayHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Close);
    }
}

pub struct GatewayConfig {
    pub url: String,
    pub token: String,
    pub intents: u64,
    pub status: String,
    pub custom_status: Option<String>,
}

/// Starts the connection task on the current tokio runtime. Events arrive on the returned
/// receiver until the handle is dropped.
pub fn connect(config: GatewayConfig) -> (GatewayHandle, mpsc::UnboundedReceiver<GatewayEvent>) {
    let (commands, rx_commands) = mpsc::unbounded_channel();
    let (events, rx_events) = mpsc::unbounded_channel();
    tokio::spawn(run(config, rx_commands, events));
    (GatewayHandle { commands }, rx_events)
}

enum Outcome {
    /// Retry after a backoff.
    Retry { quick: bool },
    Rejected,
    Incompatible,
    Closed,
}

async fn run(
    mut config: GatewayConfig,
    mut commands: mpsc::UnboundedReceiver<Command>,
    events: mpsc::UnboundedSender<GatewayEvent>,
) {
    let mut failures: u32 = 0;
    loop {
        let _ = events.send(GatewayEvent::Status(GatewayStatus::Connecting));
        let outcome = session(&mut config, &mut commands, &events, &mut failures).await;
        let wait = match outcome {
            Outcome::Closed => return,
            Outcome::Retry { quick: true } => Duration::from_millis(500 + jitter(2500)),
            Outcome::Retry { quick: false } => {
                failures = failures.saturating_add(1);
                let base = 1000u64.saturating_mul(1 << failures.min(6)).min(60_000);
                Duration::from_millis(base / 2 + jitter(base / 2))
            }
            Outcome::Rejected | Outcome::Incompatible => {
                let status = if matches!(outcome, Outcome::Rejected) {
                    GatewayStatus::Rejected
                } else {
                    GatewayStatus::Incompatible
                };
                let _ = events.send(GatewayEvent::Status(status));
                // Park until the owner hands over a new token or goes away.
                loop {
                    match commands.recv().await {
                        Some(Command::Reconnect(token)) => {
                            config.token = token;
                            failures = 0;
                            break;
                        }
                        Some(Command::Send(_)) => {}
                        Some(Command::Presence(status, custom)) => {
                            config.status = status;
                            config.custom_status = custom;
                        }
                        Some(Command::Close) | None => return,
                    }
                }
                continue;
            }
        };
        let _ = events.send(GatewayEvent::Status(GatewayStatus::Backoff));
        let deadline = Instant::now() + wait;
        loop {
            tokio::select! {
                _ = sleep_until(deadline) => break,
                cmd = commands.recv() => match cmd {
                    Some(Command::Reconnect(token)) => { config.token = token; break; }
                    Some(Command::Send(_)) => {}
                    Some(Command::Presence(status, custom)) => {
                        config.status = status;
                        config.custom_status = custom;
                    }
                    Some(Command::Close) | None => return,
                },
            }
        }
    }
}

fn jitter(max_ms: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos() as u64);
    if max_ms == 0 { 0 } else { nanos % max_ms }
}

async fn session(
    config: &mut GatewayConfig,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    events: &mpsc::UnboundedSender<GatewayEvent>,
    failures: &mut u32,
) -> Outcome {
    let (socket, _) = match timeout(Duration::from_secs(15), tokio_tungstenite::connect_async(&config.url)).await {
        Ok(Ok(ok)) => ok,
        Ok(Err(err)) => {
            log::warn!("gateway {}: {err}", config.url);
            return Outcome::Retry { quick: false };
        }
        Err(_) => return Outcome::Retry { quick: false },
    };
    let (mut sink, mut stream) = socket.split();
    let mut heartbeat_every = Duration::from_secs(45);
    let mut next_beat: Option<Instant> = None;
    let mut queued: Vec<Value> = Vec::new();
    let mut ready = false;

    loop {
        let beat = async {
            match next_beat {
                Some(at) => sleep_until(at).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            _ = beat => {
                if sink.send(WsMessage::text(rhein_proto::heartbeat().to_string())).await.is_err() {
                    return Outcome::Retry { quick: false };
                }
                next_beat = Some(Instant::now() + heartbeat_every);
            }
            cmd = commands.recv() => match cmd {
                Some(Command::Presence(status, custom)) => {
                    let frame = rhein_proto::presence(&status, custom.as_deref());
                    config.status = status;
                    config.custom_status = custom;
                    if ready && sink.send(WsMessage::text(frame.to_string())).await.is_err() {
                        return Outcome::Retry { quick: false };
                    }
                }
                Some(Command::Send(frame)) => {
                    if ready {
                        if sink.send(WsMessage::text(frame.to_string())).await.is_err() {
                            return Outcome::Retry { quick: false };
                        }
                    } else {
                        queued.push(frame);
                    }
                }
                Some(Command::Reconnect(_)) => {
                    // A new token mid-session: drop this socket and identify again with it.
                    let _ = sink.close().await;
                    return Outcome::Retry { quick: true };
                }
                Some(Command::Close) | None => {
                    let _ = sink.close().await;
                    return Outcome::Closed;
                }
            },
            msg = stream.next() => {
                let msg = match msg {
                    Some(Ok(msg)) => msg,
                    Some(Err(err)) => {
                        log::warn!("gateway {}: {err}", config.url);
                        return Outcome::Retry { quick: false };
                    }
                    None => return Outcome::Retry { quick: false },
                };
                match msg {
                    WsMessage::Text(text) => {
                        let Ok(frame) = serde_json::from_str::<Frame>(&text) else { continue };
                        match frame.op.as_str() {
                            "HELLO" => {
                                let hello: Hello = serde_json::from_value(frame.d).unwrap_or_default();
                                if hello.heartbeat_interval_ms > 0 {
                                    heartbeat_every = Duration::from_millis(hello.heartbeat_interval_ms);
                                }
                                let identify = rhein_proto::identify(&config.token, config.intents, &config.status, config.custom_status.as_deref());
                                if sink.send(WsMessage::text(identify.to_string())).await.is_err() {
                                    return Outcome::Retry { quick: false };
                                }
                            }
                            "READY" => {
                                let data: Ready = match serde_json::from_value(frame.d) {
                                    Ok(r) => r,
                                    Err(err) => {
                                        log::error!("gateway {}: bad READY: {err}", config.url);
                                        return Outcome::Retry { quick: false };
                                    }
                                };
                                ready = true;
                                *failures = 0;
                                next_beat = Some(Instant::now() + heartbeat_every);
                                let _ = events.send(GatewayEvent::Ready(Box::new(data)));
                                let _ = events.send(GatewayEvent::Status(GatewayStatus::Ready));
                                for frame in queued.drain(..) {
                                    let _ = sink.send(WsMessage::text(frame.to_string())).await;
                                }
                            }
                            "DISPATCH" => {
                                if let Some(t) = frame.t {
                                    let _ = events.send(GatewayEvent::Dispatch(Box::new(Event::parse(&t, frame.d))));
                                }
                            }
                            "INVALID_SESSION" => return Outcome::Rejected,
                            _ => {}
                        }
                    }
                    WsMessage::Close(close) => {
                        let code = close.map(|c| u16::from(c.code)).unwrap_or(1006);
                        return classify(code);
                    }
                    WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Binary(_) | WsMessage::Frame(_) => {}
                }
            }
        }
    }
}

fn classify(code: u16) -> Outcome {
    match code {
        close::AUTH_FAILED | close::IDENTIFY_EXPECTED => Outcome::Rejected,
        close::PROTOCOL_INCOMPATIBLE | close::MALFORMED => Outcome::Incompatible,
        close::RESTARTING | close::LAGGING => Outcome::Retry { quick: true },
        _ => Outcome::Retry { quick: false },
    }
}
