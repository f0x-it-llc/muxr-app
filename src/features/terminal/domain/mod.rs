//! `terminal` domain — entities, repository contract(s), and use cases.
//! Framework-free: a domain file imports only this feature's own `domain`
//! items plus `crate::failure` — never `frust::`, and never a `data`/
//! `presentation` path segment (see `examples/huddle`'s `tests/architecture.rs`
//! for the enforced version of this rule).
//!
//! **Two documented widenings**: two use cases also name the `sessions`
//! feature's own `domain` contract (its `SessionsRepository` and
//! `RenameRequest`/`Session`), because a session's *name* has exactly one
//! owner and this screen borrows it rather than minting a second store — see
//! [`use_cases::RenameAttachedSession`]. And `presentation::controller`'s live
//! pane reuses `settings`' own `CursorStyle`/`PreferencesRepository`
//! (re-exported as [`entities::SettingsCursorStyle`]/
//! [`repositories::PreferencesRepository`]) rather than minting a second
//! cursor-shape enum or a second handle onto the same store. Both are
//! `domain` → sibling `domain` edges only; nothing here touches a sibling's
//! `data` or `presentation`, and the framework-free rule is untouched.
//!
//! [`repositories::SessionsRepository`], [`entities::RenameRequest`],
//! [`entities::SettingsCursorStyle`] and [`repositories::PreferencesRepository`]
//! are all *re-exported* from those modules rather than re-minted, mirroring
//! `connect`'s own `TrustMode` re-export. `presentation::controller` needs
//! each of them too (to type its constructor, its rename call and its
//! cursor-style mapping), and reaching past this `domain` into a sibling
//! `domain` directly would put a sibling `domain` name in `presentation` —
//! exactly the edge the layering table forbids. Importing them through
//! `terminal::domain` instead keeps every widening to the edges already
//! sanctioned above. `presentation::pages` also resolves
//! `settings::di::expect_preferences_repository()` directly (the same shape
//! it already uses for `sessions::di::expect_sessions_repository()`) — a
//! separate, `di.rs`-only widening outside the layer table entirely, see
//! `docs/ARCHITECTURE.md`'s Feature Slices section.

pub mod entities;
pub mod feed;
pub mod repositories;
pub mod use_cases;
