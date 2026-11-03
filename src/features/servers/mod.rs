//! The `servers` feature slice — the server switcher's model, widgets and view
//! model. Routeless, like [`crate::features::pro`]: it owns a stage the shell
//! raises, not a screen.
//!
//! `{domain, presentation}`, dependencies pointing inward only; [`di`] is the
//! composition-root seam (`crate::MuxrApp::init` calls into it). There is no `data` layer: the only state is the app-scoped
//! last-seen handle, plain data in [`di`].
//!
//! `sessions` depends on this slice in two places: it writes into the
//! last-seen handle through [`di`] and [`domain`], and it imports
//! `presentation::controller::ServersSwitcherController` for the switcher FAB
//! and the poll pause while the stage is open. Nothing else of this slice's
//! presentation is reached from `sessions`.

pub mod di;
pub mod domain;
pub mod presentation;
