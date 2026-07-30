//! `connect` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`pieces`] | The eight Connect-screen pieces, each independently mountable |
//! | [`pages`] | The `/` route's page: the pieces arranged per `docs/splash.html`'s `.ui-layer` |
//! | [`chrome`] | The two hand-rolled paint primitives the pieces compose from |
//!
//! The split exists because the cold-start splash assembles this screen piece
//! by piece: [`crate::ui::splash::sequence`] takes [`pieces::connect_pieces`]
//! and lays them out through [`pages::connect_layout`], the same function the
//! route uses, so the handoff between the two is a no-op frame.
//!
//! A controller (view model) joins this module once Phase 2 gives the feature
//! real domain logic to drive; the pieces are static presentation until then.

pub mod chrome;
pub mod pages;
pub mod pieces;
