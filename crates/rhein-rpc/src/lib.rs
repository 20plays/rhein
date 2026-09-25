//! Where Rhein learns what you are doing, so it can share it as rich presence:
//!
//! - [`ipc`]: a Discord-compatible RPC server on `discord-ipc-N`, so games and apps built with
//!   Discord Rich Presence report their activity to Rhein as they would to Discord;
//! - [`ws`]: the same commands over Discord's local WebSocket (ports 6463–6472);
//! - [`detect`]: finds running games by their executables, from Discord's list of detectable
//!   applications;
//! - [`mpris`]: the song or video a media player is playing (D-Bus MPRIS).
//!
//! [`service::start`] runs them together and publishes at most one game, one music and one
//! video activity, normalised the way the Telium web client does. This crate knows nothing about
//! Telium servers; the store decides what to share and sends it.

mod cover;
pub mod commands;
pub mod detect;
pub mod ipc;
pub mod mpris;
pub mod resolve;
pub mod service;
pub mod ws;

pub use service::{Config, Handle, Status, start};

/// What a source reports: `None` clears that source's activity.
#[derive(Debug, Clone)]
pub struct Report {
    /// Identifies the source: one RPC connection, one detected game, the media player.
    pub key: String,
    /// The raw activity as the source knows it, before names and artwork are resolved.
    pub activity: Option<serde_json::Value>,
    /// An explicit RPC activity outranks a detected one in the same slot.
    pub priority: u8,
}

pub const PRIORITY_DETECTED: u8 = 1;
pub const PRIORITY_RPC: u8 = 2;

/// Sources talk to the service through this.
pub type Reports = tokio::sync::mpsc::UnboundedSender<SourceEvent>;

#[derive(Debug, Clone)]
pub enum SourceEvent {
    Report(Report),
    /// An RPC client connected (`Some(client_id)`) or left (`None`), keyed like its reports.
    Client { key: String, client_id: Option<String> },
    Detected(Vec<String>),
    Players(Vec<String>),
}
