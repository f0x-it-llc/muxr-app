//! The `connect` feature slice — muxr's `/` route: the connect flow (server
//! address + login token, TLS posture, saved servers, QR pairing).
//!
//! `{domain, data, presentation}`, dependencies pointing inward only
//! (`presentation` → `domain` ← `data`); [`di`] is the composition-root wiring
//! seam `crate::MuxrApp::init` calls into, not part of that slice.
//!
//! Two stores, one key: a remembered server's metadata and its secret never
//! share a store — the list is plain device storage, the auth token is behind
//! the device's biometric gate, and only a `host:port` key joins them (see
//! [`domain::entities`]).

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
