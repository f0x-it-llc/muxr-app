//! `terminal` domain — entities, repository contracts and use cases.
//! Framework-free: a domain file imports only this feature's own `domain` items
//! plus `crate::failure` — never `frust::`, never a `data`/`presentation` path.
//!
//! **Two documented widenings**, both `domain` → sibling `domain` only: the
//! rename use case names `sessions`' own contract (a session's name has one
//! owner), and the live pane reuses `settings`' `CursorStyle`/
//! `PreferencesRepository`. Each is *re-exported* here rather than re-minted,
//! so `presentation` names only `terminal::domain` — reaching into a sibling
//! `domain` directly is the edge the layering table forbids.

pub mod entities;
pub mod feed;
pub mod repositories;
pub mod use_cases;
