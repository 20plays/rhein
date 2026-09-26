//! App state for Rhein: GPUI entities on top of `rhein-net`. Views live in the `rhein` crate and
//! only ever read these entities and call their methods.

pub mod autocomplete;
pub mod client;
pub mod conversations;
pub mod crop;
pub mod guild_keys;
pub mod dm;
pub mod embed;
pub mod emoji;
pub mod gif_links;
pub mod io;
pub mod markdown;
pub mod media;
pub mod nav;
pub mod perms;
pub mod persist;
pub mod prefs;
pub mod social;
pub mod sync_store;

pub use client::{ActiveCall, CallMode, CallStatus, Client, ClientEvent, MediaProbe, Slot, SoundSource, SoundView, VoicePrefs, Divider, EmbedState, FriendAction, GuildEntry, Phase, ReadMark, SearchState, ServerStatus, Staged, StagedState};
pub use io::Io;
