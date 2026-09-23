//! App state for Rhein: GPUI entities on top of `rhein-net`. Views live in the `rhein` crate and
//! only ever read these entities and call their methods.

pub mod client;
pub mod embed;
pub mod io;
pub mod markdown;
pub mod media;
pub mod persist;

pub use client::{Client, ClientEvent, Divider, EmbedState, GuildEntry, Phase, ReadMark, ServerStatus, Staged, StagedState};
pub use io::Io;
