//! Framework-free entities, repository contracts and use cases.
//!
//! Connection facts (endpoint, TLS, version) live on [`crate::connection`] because several features read them.

pub mod entities;
pub mod repositories;
pub mod use_cases;
