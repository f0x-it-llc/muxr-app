//! The `servers` feature slice — the server switcher's model, widgets and view
//! model. Routeless, like [`crate::features::pro`]: it owns a stage the shell
//! raises, not a screen.
//!
//! `{domain, presentation}`, dependencies pointing inward only; [`di`] is the
//! composition-root seam (`crate::MuxrApp::init` calls into it from the shell
//! wiring card). There is no `data` layer: the only state is the app-scoped
//! last-seen handle, plain data in [`di`].
//!
//! `sessions` writes into the last-seen handle through [`di`] and
//! [`domain`] only, never through this slice's presentation.

pub mod di;
pub mod domain;
pub mod presentation;
