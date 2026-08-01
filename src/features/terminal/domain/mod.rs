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
//!
//! [`repositories::SessionsRepository`] and [`entities::RenameRequest`] are
//! *re-exported* from those two modules rather than re-minted, mirroring
//! `connect`'s own `TrustMode` re-export. `presentation::controller` needs
//! both types too (to type its constructor and its rename call), and reaching
//! past this `domain` into `sessions::domain` directly would put a sibling
//! `domain` name in `presentation` — exactly the edge the layering table
//! forbids. Importing them through `terminal::domain` instead keeps the
//! widening to the one edge already sanctioned above.

pub mod entities;
pub mod repositories;
pub mod use_cases;
