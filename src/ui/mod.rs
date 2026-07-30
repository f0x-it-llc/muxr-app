//! Shared, cross-feature widget helpers. `examples/huddle`'s `ui/` (toast,
//! sheet, scaffold, fill_box, …) is the shape this module grows into as muxr
//! needs its own app-wide chrome shared by more than one feature.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`backdrop`] | The full-bleed page background both the Connect screen and the splash overlay paint |
//! | [`splash`] | The cold-start splash sequence (see `docs/splash.html`/`docs/muxr-app-icons` for the design reference) |
//!
//! [`splash::sequence`] is the orchestrator `crate::shell` mounts over the
//! navigator on a cold start; everything else under [`splash`] is one of the
//! sequence's independently mountable beats.

pub mod backdrop;
pub mod splash;
