//! The connect flow: server address + login token, TLS posture, saved servers, QR pairing.
//! Metadata (plain device storage) and the auth token (behind the biometric gate)
//! never share a store; only a `host:port` key joins them.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
