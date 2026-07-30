//! Feature slices, each a `{domain, data, presentation}` clean-architecture
//! slice (see `examples/huddle`'s `src/README-phase-c.md` for the
//! convention this app follows): [`connect`] (`/`), [`sessions`]
//! (`/sessions`), [`terminal`] (`/terminal`), [`settings`] (`/settings`).
//!
//! Every feature is an empty skeleton today (one placeholder page, no real
//! domain/data logic) — Phase 2 fills each one in.

pub mod connect;
pub mod sessions;
pub mod settings;
pub mod terminal;
