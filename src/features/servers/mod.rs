//! The `servers` feature slice: the server switcher's model, widgets and view model. Routeless; it owns a stage the shell raises.
//! `{domain, presentation}` with no `data` layer; [`di`] is the composition-root seam.
//! `sessions` writes the last-seen handle through [`di`] and imports `ServersSwitcherController` for the switcher FAB and poll pause.

pub mod di;
pub mod domain;
pub mod presentation;
