//! Repository implementations. The two real stores use `frust::spawn_blocking`
//! to keep platform calls off the UI thread; the credential store requires it.
//! [`MockCredentialService`] runs the real vault algorithms over an in-memory map.

mod json;
pub mod mock_credential_service;
pub mod mock_saved_servers;
pub mod prefs_saved_servers;
pub mod secure_credential_service;

pub use mock_credential_service::MockCredentialService;
pub use mock_saved_servers::MockSavedServersRepository;
pub use prefs_saved_servers::PrefsSavedServersRepository;
pub use secure_credential_service::SecureCredentialStore;
