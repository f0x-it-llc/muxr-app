//! `connect` use cases — one file per `<verb><noun>` action.
//!
//! The two parses are plain functions, not `clean_signals::UseCase`s: that
//! trait is async and both are pure synchronous parses. The two credential use
//! cases are `UseCase`s (a gated call blocks by construction) — never call
//! [`UseCase::execute`](clean_signals::UseCase::execute) directly; always drive
//! it through `ControllerCore::run`.

pub mod parse_endpoint;
pub mod parse_pairing;
pub mod save_credential;
pub mod unlock_credential;

pub use save_credential::SaveCredential;
pub use unlock_credential::UnlockCredential;
