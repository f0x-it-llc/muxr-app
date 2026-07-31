//! `sessions` repository implementations — one file per concrete
//! `domain::repositories` trait implementation, each owning its own
//! failure-mapping site into [`crate::failure::AppFailure`]. No `frust::`
//! signals/widgets here — same framework-free rule as `domain` (see
//! `crate::features::sessions::domain`'s module doc).
//!
//! One implementation today: [`MockSessionsRepository`], the in-memory store
//! the mock era runs on. The real `muxrd` gRPC client joins it here as a
//! sibling file, and `di.rs` is the one line that chooses between them.

pub mod mock_sessions_repository;

pub use mock_sessions_repository::MockSessionsRepository;
