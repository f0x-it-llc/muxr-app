//! The `sessions` feature slice — muxr's `/sessions` route: session list with
//! create/rename/kill, the server-info/latency strip, and the attach path into
//! `/terminal`. Credential-offer UI lives on Connect, not here (see
//! [`crate::connection`]).
//!
//! `{domain, data, presentation}` clean-architecture split, dependencies
//! inward only (`docs/FEATURES_ARCHITECTURE.md`). [`di`] is the
//! composition-root seam `crate::MuxrApp::init` calls; it publishes
//! `GrpcSessionsRepository` in production, `MockSessionsRepository` for
//! headless tests.

pub mod data;
pub mod di;
pub mod domain;
pub mod presentation;
