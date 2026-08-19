//! `settings` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! - [`pages`] — the `/settings` route entry point and its `Component`.
//! - [`controller`] — the hosted view model: the token list, both
//!   preference groups, and the six mutations.
//! - [`widgets`] — the screen's individual visual elements, arranged (never
//!   resolved) by `pages` alone. Its own `rows` module carries the one
//!   decorated-fill primitive `frust::container` doesn't quite reach (a
//!   paint-only bleed, and a zero-radius fill kept out of every
//!   rounded-rect probe) — the former `chrome` module's whole reason to
//!   exist, dissolved onto `frust::container`/`frust::divider` everywhere
//!   else.
//! - [`scramble`] — the token-revoke animation's mechanism.

pub mod controller;
pub mod pages;
pub mod scramble;
pub mod widgets;
