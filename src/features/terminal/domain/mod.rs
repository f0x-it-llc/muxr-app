//! `terminal` domain — entities, repository contract(s), and use cases.
//! Framework-free: a domain file imports only this feature's own `domain`
//! items plus `crate::failure` — never `frust::`, and never a `data`/
//! `presentation` path segment (see `examples/huddle`'s `tests/architecture.rs`
//! for the enforced version of this rule). Empty today — Phase 2 fills it in
//! as the feature's real terminal I/O logic lands.

pub mod entities;
pub mod repositories;
pub mod use_cases;
