//! Wire types for the Telium client protocol (REST, gateway, discovery).
//!
//! Every struct keeps unknown fields out of the way with `#[serde(default)]` so a server that
//! grows new fields never breaks decoding. Field names follow the server's JSON exactly.

pub mod gateway;
pub mod model;
pub mod rest;

pub use gateway::*;
pub use model::*;
pub use rest::*;
