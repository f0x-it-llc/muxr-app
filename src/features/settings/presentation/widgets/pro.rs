//! The `muxr pro` group: a single `plan` row naming this install's
//! entitlement. The screen's one entry point into `features::pro`'s
//! paywall — short of Pro, tapping the whole row opens the sheet — followed,
//! short of Pro, by a `Restore purchases` row running the sheet's own restore.

use frust::AnyView;
use frust::icons::{REFRESH, STAR_FILLED};

use crate::features::pro::domain::entities::Entitlement;
use crate::features::pro::presentation::RESTORE_LABEL;
use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::palette::Palette;

use super::rows::{RowAction, RowMark, ValueTone, button_row, settings_row};

/// The group's own label, as every other group's is spelled before
/// `text-transform:uppercase` is applied.
pub const PRO_GROUP_LABEL: &str = "muxr pro";
/// The plan row's label.
pub const PLAN_LABEL: &str = "plan";
/// The restore row's subtitle, and what it reads while a restore is in flight.
const RESTORE_SUBTITLE: &str = "for a purchase made on another device";
const RESTORING_SUBTITLE: &str = "restoring\u{2026}";
/// The restore row's trailing button.
const RESTORE_BUTTON_LABEL: &str = "restore";

/// `.set-ic` for the `plan` row — a filled star, this screen's one "premium"
/// tell (no other row uses it).
const MARK_PLAN: RowMark = RowMark::Vector(STAR_FILLED);

/// `.set-ic` for the restore row.
const MARK_RESTORE: RowMark = RowMark::Vector(REFRESH);

/// The `muxr pro` group's rows: the plan row, then — short of Pro — the
/// restore row. An entitled install has nothing to buy or restore: no
/// chevron, no gesture; anything short of Pro opens the paywall sheet on a
/// tap. `busy` is the controller's single-flight flag.
pub(in super::super) fn pro_rows(
    entitlement: Entitlement,
    busy: bool,
    colors: &Palette,
) -> Vec<AnyView<SettingsState>> {
    let tone = match entitlement {
        Entitlement::Pro => ValueTone::Ok,
        Entitlement::Pending => ValueTone::Warn,
        Entitlement::Free => ValueTone::Plain,
    };
    let action = if entitlement.is_pro() {
        RowAction::Static
    } else {
        RowAction::UpgradeSheet
    };
    let mut rows = vec![settings_row(
        MARK_PLAN,
        PLAN_LABEL,
        entitlement.label(),
        tone,
        action,
        colors,
    )];
    if !entitlement.is_pro() {
        rows.push(button_row(
            MARK_RESTORE,
            RESTORE_LABEL,
            if busy {
                RESTORING_SUBTITLE
            } else {
                RESTORE_SUBTITLE
            },
            RESTORE_BUTTON_LABEL,
            pages::restore_purchases,
            colors,
        ));
    }
    rows
}
