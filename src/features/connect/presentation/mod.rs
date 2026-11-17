//! Pieces take an `Option<&`[`widgets::Live`]`>`: the cold-start splash assembles them
//! through the same layout functions with no controller to read.

pub mod controller;
pub mod pages;
pub mod widgets;

pub use controller::{KeyringUnlock, KeyringUnlockOutcome, SAVED_LIST_UNAVAILABLE_MESSAGE};
pub use widgets::getting_started::open_getting_started;
