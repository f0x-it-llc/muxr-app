//! `settings` repository implementations — one file per concrete
//! `domain::repositories` trait implementation, each owning its own
//! failure-mapping site into [`crate::failure::AppFailure`]. No `frust::`
//! signals/widgets here — same framework-free rule as `domain` (see
//! `crate::features::settings::domain`'s module doc) — with one narrow,
//! documented exception: [`PrefsRepository`]'s two savers reach
//! `frust::spawn_blocking` to persist off the UI thread (see that module's
//! own docs for why).
//!
//! Three today: [`MockAuthTokensRepository`] (the in-memory token store, no
//! real `muxrd` client yet), [`MockPreferencesRepository`] (the
//! pre-persistence in-memory preference record, kept for headless tests —
//! see `di`'s own docs), and [`PrefsRepository`] (the real,
//! `frust-shared-preferences`-backed preference store `di.rs` wires in for a
//! real app run). `di.rs` is the one place that chooses.

pub mod mock_auth_tokens_repository;
pub mod mock_preferences_repository;
pub mod prefs_repository;

pub use mock_auth_tokens_repository::MockAuthTokensRepository;
pub use mock_preferences_repository::MockPreferencesRepository;
pub use prefs_repository::PrefsRepository;
