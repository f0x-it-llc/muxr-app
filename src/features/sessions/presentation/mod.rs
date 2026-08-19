//! `sessions` presentation — the only layer in this feature touching
//! `frust`'s reactive/widget vocabulary.
//!
//! [`pages::sessions_page`] is the `/sessions` route entry; [`controllers`]
//! hosts the view model (session list, the four mutations, the mock-poll
//! gate); [`widgets`] holds the screen's visual pieces, composing
//! `frust::container`/`colored_box`/`divider` directly for the box shapes
//! the Glyph catalog has no counterpart for (card accent, pane pills,
//! `.conn-row` rule, dashed outline) — the feature-local `chrome` primitives
//! that used to stand in are gone. `heartbeat` (private) is the `.conn-row`
//! strip's ping ring + rolling latency readout and the frame ticker driving
//! both.

pub mod controllers;
mod heartbeat;
pub mod pages;
pub(crate) mod widgets;
