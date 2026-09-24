//! App state for Rhein: GPUI entities on top of `rhein-net`. Views live in the `rhein` crate and
//! only ever read these entities and call their methods.

pub mod autocomplete;
pub mod client;
pub mod embed;
pub mod emoji;
pub mod io;
pub mod markdown;
pub mod media;
pub mod nav;
pub mod perms;
pub mod persist;
pub mod prefs;
pub mod social;

pub use client::{Client, ClientEvent, Divider, EmbedState, FriendAction, GuildEntry, Phase, ReadMark, SearchState, ServerStatus, Staged, StagedState};
pub use io::Io;
