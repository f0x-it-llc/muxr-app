//! `settings` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary. [`pages::settings_page`] is the
//! placeholder `crate::routes` mounts at `/settings` today; a controller
//! (view model) joins this module once Phase 2 gives the feature real domain
//! logic to drive.

pub mod pages;
