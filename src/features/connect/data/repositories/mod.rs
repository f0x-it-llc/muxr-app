//! `connect` repository implementations — one file per concrete
//! `domain::repositories` trait implementation, each owning its own
//! failure-mapping site into [`crate::failure::AppFailure`]. No `frust::`
//! signals/widgets here, with one exception: the two real stores reach
//! `frust::spawn_blocking` to keep their platform calls off the UI thread — for
//! the credential store that is a hard safety rule, not a nicety.
//!
//! [`json`] is not a repository: it's the hand-rolled encoder/parser the
//! saved-server store's payload needs, scoped to this module.

mod json;
pub mod mock_credential_service;
pub mod mock_saved_servers;
pub mod prefs_saved_servers;
pub mod secure_credential_service;

pub use mock_credential_service::MockCredentialService;
pub use mock_saved_servers::MockSavedServersRepository;
pub use prefs_saved_servers::PrefsSavedServersRepository;
pub use secure_credential_service::SecureCredentialStore;
