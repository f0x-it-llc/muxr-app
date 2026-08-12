//! `settings` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! - [`pages`] — [`pages::settings_page`], the `/settings` route entry point,
//!   and the [`SettingsScreen`](pages::SettingsScreen) `Component` behind it
//!   (the canonical screen shape `docs/ARCHITECTURE.md`'s "Building a Screen"
//!   section mandates). It also owns the four picker sheets' resolution and
//!   persistence, since those cross the component boundary onto the app's
//!   navigator.
//! - [`controller`] — [`controller::SettingsController`], the hosted view
//!   model: the token list, both preference groups, and the six mutations.
//! - [`pieces`] — the screen's individual visual elements, transcribed from
//!   `docs/redesign.html`'s settings block and picker sheet. `pub(super)`
//!   throughout: arranged by `pages` alone.
//! - `chrome` — the screen's hand-rolled panel primitive and the switch's
//!   accessibility wrapper, for the boxes the Glyph catalog has no counterpart
//!   for. Private: only `pieces` paints with them.
//! - [`scramble`] — the token-revoke animation's mechanism: its clock (with a
//!   headless-test override), the transcribed noise source, the per-token
//!   timeline state, and the two custom widgets the collapse needs.

mod chrome;
pub mod controller;
pub mod pages;
pub mod pieces;
pub mod scramble;
