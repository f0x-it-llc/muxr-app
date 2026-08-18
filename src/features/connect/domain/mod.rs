//! `connect` domain — entities, repository contracts, and use cases.
//! Framework-free: never imports `frust::` or a `data`/`presentation` path
//! segment.
//!
//! Deviation: [`entities`] re-exports [`crate::connection::TrustMode`] rather
//! than minting a second trust-mode enum.

pub mod entities;
pub mod repositories;
pub mod use_cases;
