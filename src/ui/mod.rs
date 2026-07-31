//! Shared, cross-feature widget helpers. `examples/huddle`'s `ui/` (toast,
//! sheet, scaffold, fill_box, …) is the shape this module grows into as muxr
//! needs its own app-wide chrome shared by more than one feature.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`backdrop`] | The full-bleed page background both the Connect screen and the splash overlay paint |
//! | [`dialogs`] | Confirm / name-input modal helpers over `frust::glyph`'s dialog |
//! | [`sheet`] | The modal bottom-sheet scaffold (a workaround — frust has no sheet primitive) |
//! | [`splash`] | The cold-start splash sequence (see `docs/splash.html`/`docs/muxr-app-icons` for the design reference) |
//! | [`toasts`] | The failure→toast queue handle and the toast layer the shell mounts |
//! | [`topbar`] | The branded `muxr` top bar (brand mark + centered title + one icon slot) |
//!
//! [`splash::sequence`] is the orchestrator `crate::shell` mounts over the
//! navigator on a cold start; everything else under [`splash`] is one of the
//! sequence's independently mountable beats. [`toasts::toast_layer`] is the
//! other thing `crate::shell` mounts — permanently, between the navigator and
//! the splash.
//!
//! [`dialogs`], [`sheet`], [`toasts`] and [`topbar`] are the **presentation
//! substrate** every screen shares: a feature's `presentation` layer calls into
//! them rather than composing its own modal chrome or its own top bar, so "what
//! a muxr dialog looks like" — and "what the muxr bar looks like" — has one
//! answer. Sessions mounts [`topbar`] today; the terminal screen adopts the
//! same bar (with a `⋮` in its icon slot) in Phase 3.

pub mod backdrop;
pub mod dialogs;
pub mod sheet;
pub mod splash;
pub mod toasts;
pub mod topbar;
