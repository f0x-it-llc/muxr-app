//! `pro` presentation — the only layer in this feature that touches `frust`'s
//! reactive vocabulary.
//!
//! - [`controller`] — the app-scoped view model: the entitlement, the store's
//!   offer, and the buy/restore flows.
//!
//! No `widgets`/`pages` here: this slice owns no route. The paywall's own UI
//! joins it beside the controller.

pub mod controller;
