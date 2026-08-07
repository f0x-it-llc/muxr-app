//! `terminal` repository implementations — one file per concrete
//! `domain::repositories` trait implementation, each owning its own
//! failure-mapping site into [`crate::failure::AppFailure`]. No `frust::`
//! signals/widgets here — same framework-free rule as `domain` (see
//! `crate::features::terminal::domain`'s module doc).
//!
//! Two: [`MockTerminalRepository`], the in-memory store seeded from
//! `docs/terminal.html`, and [`GrpcTerminalRepository`], the real `muxrd`
//! client. `crate::features::terminal::di` picks which one is published.

pub mod grpc_terminal_repository;
pub mod mock_terminal_repository;

pub use grpc_terminal_repository::GrpcTerminalRepository;
pub use mock_terminal_repository::MockTerminalRepository;
