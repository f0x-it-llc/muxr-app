//! `sessions` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! - [`pages`] — [`pages::sessions_page`], the `/sessions` route entry point,
//!   and the [`SessionsScreen`](pages::SessionsScreen) `Component` behind it
//!   (the canonical screen shape `docs/ARCHITECTURE.md`'s "Building a Screen"
//!   section mandates).
//! - [`controllers`] — [`controllers::SessionsController`], the hosted view
//!   model: the session list, the four mutations, and the mock poll's gate.
//! - [`widgets`] — the screen's individual visual elements, transcribed from
//!   `docs/redesign.html`'s sessions block, arranged by `pages` alone.
//! - `chrome` — the screen's hand-rolled panel/swatch/dashed-box primitives,
//!   for the boxes the Glyph catalog has no counterpart for. Private: only
//!   `widgets` and `heartbeat` paint with them.
//! - `heartbeat` — the `.conn-row` strip's connection heartbeat: the ping
//!   ring's geometry and clock, the rolling latency readout, and the frame
//!   ticker that drives both. Private: the screen mounts it, `widgets`
//!   renders the pose it computes.

mod chrome;
pub mod controllers;
mod heartbeat;
pub mod pages;
pub(crate) mod widgets;
