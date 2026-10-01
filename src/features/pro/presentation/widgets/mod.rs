//! `pro`'s widget clusters — one file per cluster, like every other feature's
//! `widgets/`.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`upgrade_sheet`] | The paywall sheet both the Connect server-limit gate and the Settings Upgrade row raise |
//!
//! Unlike `sessions`'/`settings`' page-coupled widgets, this one is
//! page-agnostic by necessity: it has no page of its own to couple to, and two
//! screens raise it. It takes a controller plus an `Rc`-callback context
//! instead (`docs/FEATURES_ARCHITECTURE.md`'s page-agnostic shape).

pub mod upgrade_sheet;

pub use upgrade_sheet::{
    NOT_NOW_LABEL, NOTHING_TO_RESTORE_TOAST, PENDING_TOAST, RESTORE_LABEL, ServerLimitContext,
    UNAVAILABLE_COPY, UPGRADE_SHEET_TITLE, UpgradeContext, forget_and_connect_label,
    show_upgrade_sheet, upgrade_label,
};
