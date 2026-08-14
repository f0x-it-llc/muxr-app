//! `connect` use cases — one file per `<verb><noun>` action.
//!
//! | Use case | What it decides |
//! |---|---|
//! | [`parse_endpoint`] | Whether the form's server URL is a dialable `host:port` |
//! | [`parse_pairing`] | Whether a scanned QR code is a well-formed `muxr://pair` payload |
//! | [`SaveCredential`] | Gates and stores one server's auth token |
//! | [`UnlockCredential`] | Gates and reads one server's stored auth token |
//!
//! The two parses are plain functions rather than `clean_signals::UseCase`s —
//! that trait is async, and both are pure synchronous parses. The two credential
//! use cases *are* `UseCase`s, since a gated call blocks by construction. Never
//! call [`UseCase::execute`](clean_signals::UseCase::execute) directly from a
//! controller — always drive it through `ControllerCore::run`.

pub mod parse_endpoint;
pub mod parse_pairing;
pub mod save_credential;
pub mod unlock_credential;

pub use save_credential::SaveCredential;
pub use unlock_credential::UnlockCredential;
