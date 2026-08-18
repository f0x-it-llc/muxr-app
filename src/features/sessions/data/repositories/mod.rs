//! Two `SessionsRepository` implementations, each owning its own
//! failure-mapping into [`crate::failure::AppFailure`]:
//! [`GrpcSessionsRepository`] (the real `muxrd` client — one sanctioned
//! `Arc<ConnectionController>` field breaks the framework-free rule) and
//! [`MockSessionsRepository`] (in-memory, for headless tests). `di.rs`
//! chooses between them.

pub mod grpc_sessions_repository;
pub mod mock_sessions_repository;

pub use grpc_sessions_repository::GrpcSessionsRepository;
pub use mock_sessions_repository::MockSessionsRepository;
