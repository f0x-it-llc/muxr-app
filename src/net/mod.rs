//! The `muxrd` network layer — everything that speaks gRPC to a server.
//!
//! [`proto`] is the generated `muxr.v1` service `build.rs` compiles from the
//! vendored `proto/muxr.proto`; the rest of this module is the transport around
//! it, in the order a connection uses it:
//!
//! - [`tls`] — [`TrustConfig`] and the `rustls` verifier each of its three modes
//!   installs (system CA, accept-any, leaf-DER SHA-256 pin).
//! - [`channel`] — the endpoint/`Channel` for a `(host, port, TrustConfig)`
//!   triple, including the keepalive settings that mirror `muxrd`'s own.
//! - [`auth`] — the swappable session token and the interceptor stamping
//!   `authorization: Bearer <token>` onto the authenticated stub.
//! - [`client`] — [`MuxrClient`], the handle everything above this layer holds.
//! - [`error`] — the one place a `tonic::Status` becomes an
//!   [`AppFailure`](crate::failure::AppFailure).
//!
//! No feature's `data` layer consumes it yet — every repository in the app is
//! still the in-memory mock `docs/ARCHITECTURE.md`'s Planned section describes,
//! and the repository implementations that replace them are what joins this
//! module next.
//!
//! **Layer position.** This is app-level infrastructure, beside
//! `connection.rs`/`failure.rs`, not a feature's `data` layer: more than one
//! feature's repositories will be built over the same channel, and
//! `docs/ARCHITECTURE.md`'s cross-feature rule puts shared, app-scoped
//! machinery here rather than inside whichever slice happened to need it first.
//! It stays framework-free in the same sense a `data` layer is: no signals, no
//! widgets, no `frust::` reactive vocabulary — a platform *handle* accessor
//! (`frust_plugin::android`, for the system-CA verifier's JNI initialization)
//! is not that, and is the one framework crate this module may name.
//!
//! `connection.rs`'s own `TrustMode` is deliberately **not** referenced here:
//! it is a mock connection's user-facing posture label, and mapping it onto
//! [`TrustConfig`] belongs to whoever builds a real connect request.

pub mod auth;
pub mod channel;
pub mod client;
pub mod error;
pub mod proto;
pub mod tls;

/// The in-process `muxrd` fixture the crate's own tests connect to. See its
/// module docs for why it is `#[cfg(test)]` rather than feature-gated.
#[cfg(test)]
pub mod testing;

pub use client::MuxrClient;
pub use tls::{PinError, TrustConfig};
