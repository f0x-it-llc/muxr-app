//! `settings` repository implementations — one file per concrete
//! `domain::repositories` trait implementation, each owning its own
//! failure-mapping site into [`crate::failure::AppFailure`]. Framework-free
//! like `domain`, with one documented exception: [`PrefsRepository`]'s two
//! savers reach `frust::spawn_blocking` to persist off the UI thread.
//!
//! Four: [`GrpcAuthTokensRepository`]/[`PrefsRepository`] are the real,
//! production pair; [`MockAuthTokensRepository`]/[`MockPreferencesRepository`]
//! are in-memory, test-only. `di.rs` chooses per repository, independently.

pub mod grpc_auth_tokens_repository;
pub mod mock_auth_tokens_repository;
pub mod mock_preferences_repository;
pub mod prefs_repository;

pub use grpc_auth_tokens_repository::GrpcAuthTokensRepository;
pub use mock_auth_tokens_repository::MockAuthTokensRepository;
pub use mock_preferences_repository::MockPreferencesRepository;
pub use prefs_repository::PrefsRepository;
