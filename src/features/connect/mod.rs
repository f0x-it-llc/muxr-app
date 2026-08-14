//! The `connect` feature slice — muxr's `/` route: the connect flow (server
//! address + login token, TLS posture, saved servers, QR pairing).
//!
//! `{domain, data, presentation}`, dependencies pointing inward only
//! (`presentation` → `domain` ← `data`); [`di`] is the composition-root wiring
//! seam `crate::MuxrApp::init` calls into, not part of that slice.
//!
//! | Layer | What it holds |
//! |---|---|
//! | [`domain`] | `SavedServer`, the two store contracts (metadata + gated secret), the `host:port` validation, the two credential use cases |
//! | [`data`] | The real preferences-backed saved-server store and the real biometric-gated credential store, each beside its in-memory double |
//! | [`presentation`] | The `ConnectScreen` `Component`, its `ConnectController`, and the widgets |
//! | [`di`] | Constructs both stores once and publishes them into context |
//!
//! **Two stores, one key.** A remembered server's *metadata* and its *secret*
//! never share a store: the list is plain device storage, the auth token is behind
//! the device's biometric gate, and only a `host:port` key joins them (see
//! [`domain::entities`]). The QR sheet scans a `muxr://pair` code through
//! `frust-camera`, prefills the form and offers the explicit pinned connect; a
//! successful pairing then stores its scanned token behind one biometric prompt.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
