//! `connect` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`controller`] | `ConnectController` — the form's view model (clean-signals) |
//! | [`pieces`] | The eight Connect-screen pieces, each independently mountable |
//! | [`pages`] | The `/` route's page: the `ConnectScreen` `Component` + the shared layout |
//! | [`chrome`] | The two hand-rolled paint primitives the pieces compose from |
//!
//! The pieces/pages split exists because the cold-start splash assembles this
//! screen piece by piece: [`crate::ui::splash::sequence`] takes
//! [`pieces::connect_pieces`] and lays them out through
//! [`pages::connect_layout`], the same function the screen uses, so the handoff
//! between the two is a no-op frame. That is also why a piece takes an
//! `Option<&`[`pieces::Live`]`>` rather than reading a controller itself: the
//! splash has none to read.
//!
//! Per `docs/ARCHITECTURE.md`'s "Building a Screen" charter, the screen is a
//! per-screen `Component` hosting [`controller::ConnectController`] through
//! `clean_signals_frust::use_controller`; every observed value lives on that
//! controller's signals, and the component's own retained state holds only the
//! non-reactive handles.

pub mod chrome;
pub mod controller;
pub mod pages;
pub mod pieces;
