//! The `muxr pro` group: a single `plan` row naming this install's
//! entitlement. The screen's one entry point into `features::pro`'s
//! paywall — short of Pro, tapping the whole row opens the sheet; Restore
//! lives only on the sheet itself.

use frust::AnyView;
use frust::icons::STAR_FILLED;

use crate::features::pro::domain::entities::Entitlement;
use crate::features::settings::presentation::pages::SettingsState;
use crate::ui::palette::Palette;

use super::rows::{RowAction, RowMark, ValueTone, settings_row};

/// The group's own label, as every other group's is spelled before
/// `text-transform:uppercase` is applied.
pub const PRO_GROUP_LABEL: &str = "muxr pro";
/// The plan row's label.
pub const PLAN_LABEL: &str = "plan";

/// `.set-ic` for the `plan` row — a filled star, this screen's one "premium"
/// tell (no other row uses it).
const MARK_PLAN: RowMark = RowMark::Vector(STAR_FILLED);

/// The `muxr pro` group's rows: the plan row alone. An entitled install has
/// nothing to buy or restore — no chevron, no gesture; anything short of Pro
/// opens the paywall sheet on a tap.
pub(in super::super) fn pro_rows(
    entitlement: Entitlement,
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
    vec![settings_row(
        MARK_PLAN,
        PLAN_LABEL,
        entitlement.label(),
        tone,
        action,
        colors,
    )]
}
