//! Networking against Telium servers: discovery, the REST client, sign-in, guild sessions and the
//! realtime gateway. Plain async Rust on tokio; nothing here knows about the UI.

pub mod api;
pub mod auth;
pub mod discovery;
pub mod gateway;

pub use api::{Api, ApiError, ApiResult};
pub mod tunnel;
pub use discovery::discover;
pub use gateway::{GatewayEvent, GatewayHandle, GatewayStatus};
