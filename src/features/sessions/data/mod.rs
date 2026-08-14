//! `sessions` data layer — implements this feature's `domain` repository
//! trait over a concrete source. No `frust::` types here, same as `domain`,
//! differing only in that it may depend on an external transport crate.
//!
//! The production source is [`repositories::GrpcSessionsRepository`], over
//! `muxrd`'s real gRPC round trip. [`repositories::MockSessionsRepository`]
//! is the in-memory sibling `di::use_mock_sessions_for_tests` opts a test
//! process onto instead — a *real* implementation of the real contract, not
//! a stub: it mutates its own store and awaits a small simulated latency, so
//! everything above it runs the identical code path the real client does.

pub mod repositories;
