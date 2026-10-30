//! The `muxrd` network layer — everything that speaks gRPC to a server, in
//! the order a connection uses it: [`tls`] (trust posture) → [`channel`]
//! (the `Channel` for a `(host, port, TrustConfig)`) → [`auth`] (session
//! token + bearer interceptor) → [`client`] ([`MuxrClient`]) → [`error`]
//! (`tonic::Status` → [`AppFailure`](crate::failure::AppFailure)) →
//! [`control`] (the `ControlEvent.kind` strings the proto can't type).
//! [`wake`] is the app-scoped "retry now" signal beside that chain.
//!
//! App-level infrastructure beside `connection.rs`/`failure.rs`, not a
//! feature's `data` layer — shared across `sessions`, `settings`, `terminal`
//! and `connection.rs`'s own `NetConnectionBackend`. Framework-free like a
//! `data` layer (no signals, no widgets); `frust_plugin::android`'s JNI
//! handle accessor for the system-CA verifier is the one exception.
//!
//! Dependency direction is one way: features import `net`, and nothing under
//! `src/net` (fixture included) imports `crate::features::*` — kept
//! greppable at zero `features::` imports in this directory. `TrustMode`
//! (in `connection.rs`) is deliberately not referenced here: mapping a
//! user-facing posture onto a [`TrustConfig`] is the connect seam's job.

pub mod auth;
pub mod channel;
pub mod client;
pub mod control;
pub mod error;
pub mod proto;
pub mod tls;
pub mod wake;

/// The in-process `muxrd` fixture the crate's own tests connect to. See its
/// module docs for why it is `#[cfg(test)]` rather than feature-gated.
#[cfg(test)]
pub mod testing;

pub use client::MuxrClient;
pub use tls::{PinError, TrustConfig};
