//! `sessions` domain — [`entities`], the
//! [`repositories::SessionsRepository`] contract (list/create/rename/kill),
//! and one thin `<verb><noun>` use case per method. Framework-free: never
//! `frust::`, never a `data`/`presentation` import (enforced by
//! `examples/huddle`'s `tests/architecture.rs`).

pub mod entities;
pub mod repositories;
pub mod use_cases;
