//! The `muxrd` network layer — everything that speaks gRPC to a server.
//!
//! Today this module is **codegen only**: [`proto`] is the generated `muxr.v1`
//! service (messages, client stubs, server stubs) that `build.rs` compiles from
//! the vendored `proto/muxr.proto`. Nothing here opens a socket yet, and no
//! feature's `data` layer consumes it — every repository in the app is still
//! the in-memory mock `docs/ARCHITECTURE.md`'s Planned section describes.
//!
//! What joins it, in order: the channel builder (endpoint + keepalive knobs,
//! and the three `TrustMode` arms — pinned via a leaf-DER SHA-256 verifier,
//! trust-on-first-use, and system-CA through `rustls-platform-verifier`), then
//! the bearer-token interceptor that stamps `authorization: Bearer <session
//! token>` on every authenticated call, then the repository implementations
//! that replace the mocks behind each feature's existing `domain` traits.
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

pub mod proto;
