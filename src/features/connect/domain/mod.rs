//! `connect` domain — entities, repository contract(s), and use cases.
//! Framework-free: a domain file imports only this feature's own `domain`
//! items plus `crate::failure` — never `frust::`, and never a `data`/
//! `presentation` path segment (see `examples/huddle`'s `tests/architecture.rs`
//! for the enforced version of this rule).
//!
//! | Module | What it owns |
//! |---|---|
//! | [`entities`] | `SavedServer` — a remembered `muxrd` endpoint |
//! | [`repositories`] | `SavedServersRepository` — the saved-server store contract |
//! | [`use_cases`] | `parse_endpoint` — the form's `host:port` validation |
//!
//! One documented deviation from the import rule above: [`entities`] re-exports
//! [`crate::connection::TrustMode`] rather than minting a second trust-mode
//! enum. See that module's own docs for the reasoning and the tension.

pub mod entities;
pub mod repositories;
pub mod use_cases;
