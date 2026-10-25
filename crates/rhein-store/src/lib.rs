//! App state for Rhein: GPUI entities on top of `rhein-net`. Views live in the `rhein` crate and
//! only ever read these entities and call their methods.

pub mod autocomplete;
pub mod client;
/// The server API, for views that call a server directly (the guild settings).
pub use rhein_net;
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

pub use client::banners::{Banner, BannerAction};
pub use client::{now_ms, BotModal, ModalField, ActiveCall, CallMode, CallSpeaking, CallStatus, CallVideo, Client, ClientEvent, MediaProbe, Slot, DownloadView, MediaSource, MediaTick, MoviePlayer, MovieView, SoundView, free_download_path, safe_file_name, VoicePrefs, Divider, EmbedState, FriendAction, GuildEntry, Phase, ReadMark, SearchState, ServerStatus, Staged, StagedState};
pub use io::Io;
