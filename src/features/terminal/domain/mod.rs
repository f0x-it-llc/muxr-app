//! `terminal` domain — entities, repository contract(s), and use cases.
//! Framework-free: a domain file imports only this feature's own `domain`
//! items plus `crate::failure` — never `frust::`, and never a `data`/
//! `presentation` path segment (see `examples/huddle`'s `tests/architecture.rs`
//! for the enforced version of this rule).
//!
//! **One documented widening**: two use cases also name the `sessions`
//! feature's own `domain` contract (its `SessionsRepository` and
//! `RenameRequest`/`Session`), because a session's *name* has exactly one
//! owner and this screen borrows it rather than minting a second store — see
//! [`use_cases::RenameAttachedSession`]. That is a `domain` → sibling
//! `domain` edge only; nothing here touches a sibling's `data` or
//! `presentation`, and the framework-free rule is untouched.

pub mod entities;
pub mod repositories;
pub mod use_cases;
