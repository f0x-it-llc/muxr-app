//! `connect` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! The widgets/pages split exists because the cold-start splash assembles this
//! screen piece by piece through [`pages::connect_layout`], the same function
//! the screen uses, so the handoff between the two is a no-op frame. That is
//! also why a piece takes an `Option<&`[`widgets::Live`]`>` rather than reading
//! a controller itself — the splash has none to read (see [`widgets`]' parity
//! contract).
//!
//! [`KeyringUnlock`] is this layer's one page-agnostic export to a sibling:
//! the dock's one-prompt keyring unlock and the saved-row switch, which
//! Settings drives too.

pub mod controller;
pub mod pages;
pub mod widgets;

pub use controller::{KeyringUnlock, KeyringUnlockOutcome};
