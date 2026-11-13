//! The two parses are plain functions, not `UseCase`s: both are pure and synchronous.
//! Drive the credential use cases through `ControllerCore::run`, never `execute` directly.

pub mod parse_endpoint;
pub mod parse_pairing;
pub mod save_credential;
pub mod unlock_credential;

pub use save_credential::SaveCredential;
pub use unlock_credential::UnlockCredential;
