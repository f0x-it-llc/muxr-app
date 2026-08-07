//! `connect` use cases — one file per `<verb><noun>` action (see
//! `examples/huddle`'s `domain/use_cases/` convention).
//!
//! | Use case | What it decides |
//! |---|---|
//! | [`parse_endpoint`] | Whether the form's server URL is a dialable `host:port` |
//! | [`parse_pairing`] | Whether a scanned QR code is a well-formed `muxr://pair` payload |
//! | [`SaveCredential`] | Gates and stores one server's auth token |
//! | [`UnlockCredential`] | Gates and reads one server's stored auth token |
//!
//! [`parse_endpoint`](parse_endpoint::parse_endpoint) and
//! [`parse_pairing`](parse_pairing::parse_pairing_uri) are plain functions
//! rather than `clean_signals::UseCase`s — see `parse_endpoint`'s own module
//! docs for why (that trait is async; both are pure synchronous parses). The
//! two credential use cases *are* `UseCase`s: both are asynchronous by
//! construction (a gated call blocks, see
//! [`repositories::CredentialService`](super::repositories::CredentialService)),
//! and the controller drives them through `ControllerCore::run` like every
//! other awaited call in this app.
//!
//! Never call [`UseCase::execute`](clean_signals::UseCase::execute) directly
//! from a controller — always drive it through `ControllerCore::run` (huddle's
//! rule, restated).

pub mod parse_endpoint;
pub mod parse_pairing;
pub mod save_credential;
pub mod unlock_credential;

pub use save_credential::SaveCredential;
pub use unlock_credential::UnlockCredential;
