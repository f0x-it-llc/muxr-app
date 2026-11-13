//! Feature slices, each a `{domain, data, presentation}` clean-architecture
//! slice (see `examples/huddle`'s `src/README-phase-c.md` for the
//! convention this app follows): [`connect`] (`/`), [`sessions`]
//! (`/sessions`), [`terminal`] (`/terminal`), [`settings`] (`/settings`).
//!
//! Every feature is an empty skeleton today (one placeholder page, no real
//! domain/data logic) — Phase 2 fills each one in.
//!
//! [`pro`] and [`servers`] are the routeless slices: `pro` owns the in-app
//! purchase and the entitlement every other feature gates on, `servers` the
//! server switcher's model and widgets — neither is a screen.

pub mod connect;
pub mod pro;
pub mod servers;
pub mod sessions;
pub mod settings;
pub mod terminal;
