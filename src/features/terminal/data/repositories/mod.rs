//! Each repository owns its own failure mapping into [`crate::failure::AppFailure`].

pub mod grpc_terminal_repository;
pub mod mock_terminal_repository;

pub use grpc_terminal_repository::GrpcTerminalRepository;
pub use mock_terminal_repository::MockTerminalRepository;
