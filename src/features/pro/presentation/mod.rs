//! `pro` presentation — the only layer in this feature that touches `frust`'s
//! reactive vocabulary.
//!
//! - [`controller`] — the app-scoped view model: the entitlement, the store's
//!   offer, and the buy/restore flows.
//! - [`widgets`] — the paywall sheet, page-agnostic because two screens raise
//!   it and this slice owns no route of its own.
//!
//! No `pages` here: there is no `/pro` route. The sheet is pushed onto the
//! root overlay host by whichever screen needs it.

pub mod controller;
pub mod widgets;

pub use widgets::{
    NOT_NOW_LABEL, NOTHING_TO_RESTORE_TOAST, PENDING_TOAST, RESTORE_LABEL, ServerLimitContext,
    UNAVAILABLE_COPY, UPGRADE_SHEET_TITLE, UpgradeContext, forget_and_connect_label,
    show_upgrade_sheet, upgrade_label,
};
