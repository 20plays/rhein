//! Voice and video for Rhein: Telium's mesh calls and its LiveKit-based SFU.

pub mod budget;
pub mod buffered;
pub mod audio;
pub mod e2ee;
pub mod mesh;
pub mod movie;
pub mod playback;
pub mod posture;
pub mod region;
pub mod screen;
pub mod sfu;
pub mod video;

pub use mesh::{CallEvent, Slot};
/// The LiveKit SDK (and its `webrtc`) these calls are built on, for callers that handle tracks.
pub use livekit;

/// Calls run on tokio tasks and the UI reads the engine from its own thread.
#[allow(dead_code)]
fn assert_threading() {
    fn send<T: Send>() {}
    fn sync<T: Sync>() {}
    send::<mesh::Mesh>();
    send::<sfu::SfuCall>();
    send::<audio::AudioEngine>();
    sync::<audio::AudioEngine>();
}
