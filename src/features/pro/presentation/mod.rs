//! The only layer touching `frust`; no pages, the paywall sheet is pushed by whichever screen needs it.

pub mod controller;
pub mod widgets;

pub use widgets::{
    NOT_NOW_LABEL, NOTHING_TO_RESTORE_TOAST, PENDING_TOAST, RESTORE_LABEL, ServerLimitContext,
    UNAVAILABLE_COPY, UPGRADE_SHEET_TITLE, UpgradeContext, forget_and_connect_label,
    show_upgrade_sheet, upgrade_label,
};
