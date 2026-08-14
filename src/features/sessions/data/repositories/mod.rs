//! `sessions` repository implementations — one file per concrete
//! `domain::repositories` trait implementation, each owning its own
//! failure-mapping site into [`crate::failure::AppFailure`]. No `frust::`
//! here, same framework-free rule as `domain`, with one sanctioned
//! exception: [`GrpcSessionsRepository`]'s own `Arc<ConnectionController>`
//! field.
//!
//! Two implementations: [`MockSessionsRepository`], the in-memory store
//! headless suites run on, and [`GrpcSessionsRepository`], the real
//! `muxrd`-backed one production uses. `di.rs` chooses between them.

pub mod grpc_sessions_repository;
pub mod mock_sessions_repository;

pub use grpc_sessions_repository::GrpcSessionsRepository;
pub use mock_sessions_repository::MockSessionsRepository;
