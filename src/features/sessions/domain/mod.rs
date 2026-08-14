//! `sessions` domain — entities, the repository contract, and use cases.
//! Framework-free: never `frust::`, never a `data`/`presentation` path
//! segment (see `examples/huddle`'s `tests/architecture.rs` for the
//! enforced version of this rule).
//!
//! - [`entities`] — [`Session`](entities::Session) and the value objects the
//!   `/sessions` cards render.
//! - [`repositories`] — the one
//!   [`SessionsRepository`](repositories::SessionsRepository) contract
//!   (list/create/rename/kill).
//! - [`use_cases`] — one thin `<verb><noun>` use case per repository method.

pub mod entities;
pub mod repositories;
pub mod use_cases;
