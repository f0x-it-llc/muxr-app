//! `sessions` data layer — implements `domain`'s repository trait over a
//! concrete source; framework-free except the external transport crate.
//! [`repositories::GrpcSessionsRepository`] is the production `muxrd` gRPC
//! client; [`repositories::MockSessionsRepository`] is a real in-memory
//! implementation of the same contract, not a stub, used by headless tests.

pub mod repositories;
