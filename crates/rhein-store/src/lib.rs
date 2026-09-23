//! App state for Rhein: GPUI entities on top of `rhein-net`. Views live in the `rhein` crate and
//! only ever read these entities and call their methods.

pub mod client;
pub mod io;
pub mod persist;

pub use client::{Client, ClientEvent, GuildEntry, Phase, ReadMark, ServerStatus};
pub use io::Io;
