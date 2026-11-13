//! Real and mock repository implementations; `di.rs` picks each independently.

pub mod grpc_auth_tokens_repository;
pub mod mock_auth_tokens_repository;
pub mod mock_preferences_repository;
pub mod prefs_repository;

pub use grpc_auth_tokens_repository::GrpcAuthTokensRepository;
pub use mock_auth_tokens_repository::MockAuthTokensRepository;
pub use mock_preferences_repository::MockPreferencesRepository;
pub use prefs_repository::PrefsRepository;
