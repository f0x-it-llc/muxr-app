//! `settings` repository implementations — one file per concrete
//! `domain::repositories` trait implementation, each owning its own
//! failure-mapping site into [`crate::failure::AppFailure`]. No `frust::`
//! signals/widgets here — same framework-free rule as `domain` (see
//! `crate::features::settings::domain`'s module doc).
//!
//! Two today, one per contract: [`MockAuthTokensRepository`] (the in-memory
//! token store) and [`MockPreferencesRepository`] (the in-memory preference
//! record). They are replaced independently — the real `muxrd` gRPC client
//! joins the first as a sibling file, a secure-storage/shared-preferences
//! plugin joins the second — and `di.rs` is the one place that chooses.

pub mod mock_auth_tokens_repository;
pub mod mock_preferences_repository;

pub use mock_auth_tokens_repository::MockAuthTokensRepository;
pub use mock_preferences_repository::MockPreferencesRepository;
