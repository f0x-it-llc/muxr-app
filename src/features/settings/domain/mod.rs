//! `settings` domain — entities, the repository contracts, and use cases.
//! Framework-free: a domain file imports only this feature's own `domain`
//! items plus `crate::failure` (and `clean_signals`' `UseCase`/`async_trait`
//! vocabulary, which is framework-agnostic by construction) — never `frust::`,
//! and never a `data`/`presentation` path segment (see `examples/huddle`'s
//! `tests/architecture.rs` for the enforced version of this rule).
//!
//! - [`entities`] — [`AuthToken`](entities::AuthToken) and
//!   [`Preferences`](entities::Preferences), the two aggregates
//!   `docs/redesign.html`'s settings screen renders, each carrying the closed
//!   value sets its picker sheet offers.
//! - [`repositories`] — two contracts, split by who will own them for real:
//!   [`AuthTokensRepository`](repositories::AuthTokensRepository) (server
//!   state, over `muxrd`) and
//!   [`PreferencesRepository`](repositories::PreferencesRepository)
//!   (device-local state, over a storage plugin).
//! - [`use_cases`] — one thin `<verb><noun>` use case per repository method.
//!
//! **What is deliberately *not* here**: the connection facts the `connection`
//! group prints (endpoint, TLS posture, version). Those belong to
//! [`crate::connection`]'s app-scoped controller — more than one feature reads
//! them, and re-minting them behind a settings repository would give the app
//! two answers to "which server am I on".

pub mod entities;
pub mod repositories;
pub mod use_cases;
