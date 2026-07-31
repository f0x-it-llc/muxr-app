//! `sessions` domain — entities, the repository contract, and use cases.
//! Framework-free: a domain file imports only this feature's own `domain`
//! items plus `crate::failure` (and `clean_signals`' `UseCase`/`async_trait`
//! vocabulary, which is framework-agnostic by construction) — never `frust::`,
//! and never a `data`/`presentation` path segment (see `examples/huddle`'s
//! `tests/architecture.rs` for the enforced version of this rule).
//!
//! - [`entities`] — [`Session`](entities::Session) and the value objects the
//!   `/sessions` cards render.
//! - [`repositories`] — the one
//!   [`SessionsRepository`](repositories::SessionsRepository) contract
//!   (list/create/rename/kill), already async so the real `muxrd` client swaps
//!   in behind it additively.
//! - [`use_cases`] — one thin `<verb><noun>` use case per repository method.

pub mod entities;
pub mod repositories;
pub mod use_cases;
