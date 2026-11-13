//! Page-agnostic: this slice has no page, and two screens raise the sheet.

pub mod upgrade_sheet;

pub use upgrade_sheet::{
    NOT_NOW_LABEL, NOTHING_TO_RESTORE_TOAST, PENDING_TOAST, RESTORE_LABEL, ServerLimitContext,
    UNAVAILABLE_COPY, UPGRADE_SHEET_TITLE, UpgradeContext, forget_and_connect_label,
    show_upgrade_sheet, upgrade_label,
};
