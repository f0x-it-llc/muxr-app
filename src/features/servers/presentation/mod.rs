//! `servers` presentation — the only layer in this slice that touches
//! `frust`'s reactive vocabulary.
//!
//! - [`controller`] — the app-scoped switcher view model.
//! - [`widgets`] — the card faces, label row and action bar, page-agnostic
//!   because the shell host (not this slice) composes the stage.
//!
//! No `pages` here: the slice owns no route.

pub mod controller;
pub mod widgets;
