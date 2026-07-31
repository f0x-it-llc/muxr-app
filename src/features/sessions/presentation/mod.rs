//! `sessions` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! - [`pages`] — [`pages::sessions_page`], the `/sessions` route entry point,
//!   and the [`SessionsScreen`](pages::SessionsScreen) `Component` behind it
//!   (the canonical screen shape `docs/ARCHITECTURE.md`'s "Building a Screen"
//!   section mandates).
//! - [`controllers`] — [`controllers::SessionsController`], the hosted view
//!   model: the session list, the four mutations, and the mock poll's gate.
//! - `pieces` — the screen's individual visual elements, transcribed from
//!   `docs/redesign.html`'s sessions/picker blocks. Private: only `pages`
//!   arranges them.

pub mod controllers;
pub mod pages;
mod pieces;
