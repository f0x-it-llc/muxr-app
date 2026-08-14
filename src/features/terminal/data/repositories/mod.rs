//! `terminal` repository implementations — one file per concrete
//! `domain::repositories` impl, each owning its own failure-mapping site into
//! [`crate::failure::AppFailure`]: [`MockTerminalRepository`], the in-memory
//! store, and [`GrpcTerminalRepository`], the real `muxrd` client.
//! `crate::features::terminal::di` picks which one is published.

pub mod grpc_terminal_repository;
pub mod mock_terminal_repository;

pub use grpc_terminal_repository::GrpcTerminalRepository;
pub use mock_terminal_repository::MockTerminalRepository;
