//! `settings` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! - [`pages`] — the `/settings` route entry point and its `Component`.
//! - [`controller`] — the hosted view model: the token list, both
//!   preference groups, and the six mutations.
//! - [`widgets`] — the screen's individual visual elements, arranged (never
//!   resolved) by `pages` alone.
//! - `chrome` — the hand-rolled panel primitive and the switch's
//!   accessibility wrapper; private, only `widgets` paints with it.
//! - [`scramble`] — the token-revoke animation's mechanism.

mod chrome;
pub mod controller;
pub mod pages;
pub mod scramble;
pub mod widgets;
