//! `settings` domain — entities, repository contracts, use cases.
//! Framework-free: never `frust::`, never a `data`/`presentation` path
//! segment.
//!
//! The `connection` group's facts (endpoint, TLS posture, version) live on
//! [`crate::connection`] instead, deliberately — more than one feature reads
//! them, and a settings-owned copy would give the app two answers to "which
//! server am I on".

pub mod entities;
pub mod repositories;
pub mod use_cases;
