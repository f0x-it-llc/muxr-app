use frust::AnyView;
use frust::icons::{REFRESH, STAR_FILLED};

use crate::features::pro::domain::entities::Entitlement;
use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::palette::Palette;

use super::rows::{RowAction, RowMark, ValueTone, button_row, settings_row};

pub const PRO_GROUP_LABEL: &str = "muxr pro";
pub const PLAN_LABEL: &str = "plan";
const RESTORE_LABEL: &str = "Restore purchases";
const RESTORE_SUBTITLE: &str = "for a purchase made on another device";
const RESTORING_SUBTITLE: &str = "restoring\u{2026}";
const RESTORE_BUTTON_LABEL: &str = "restore";

const MARK_PLAN: RowMark = RowMark::Vector(STAR_FILLED);

const MARK_RESTORE: RowMark = RowMark::Vector(REFRESH);

pub(in super::super) fn pro_rows(
    entitlement: Entitlement,
    restoring: bool,
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
            if restoring {
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
