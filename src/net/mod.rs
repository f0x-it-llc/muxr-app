//! The `muxrd` gRPC layer: [`tls`] → [`channel`] → [`auth`] → [`client`] → [`error`].
//! Framework-free except the Android JNI handle the system-CA verifier needs; nothing under
//! `src/net` imports `crate::features::*`.

pub mod agent_layouts;
pub mod auth;
pub mod channel;
pub mod client;
pub mod control;
pub mod error;
pub mod proto;
pub mod tls;
pub mod wake;

#[cfg(test)]
pub mod testing;

pub use client::MuxrClient;
pub use tls::{PinError, TrustConfig};
