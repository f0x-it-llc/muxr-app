//! The `muxr pro` group: a `plan` row naming this install's entitlement, plus
//! — for anything short of Pro — the Upgrade/Restore actions line, in the
//! exact shape `widgets::saved_servers`' own actions row uses. The screen's
//! only two entry points into `features::pro`'s paywall.
//!
//! Both actions call back into [`pages`]'s own shims
//! ([`pages::open_upgrade_sheet`]/[`pages::restore_purchases`]), the
//! screen-state form of the page↔widgets back-edge every other module in this
//! folder already uses — see `docs/FEATURES_ARCHITECTURE.md`'s widgets/
//! paragraph.

use frust::icons::STAR_FILLED;
use frust::{AnyView, ButtonStyle, EdgeInsets, Padding, any, button, divider, inflexible};

use crate::features::pro::domain::entities::Entitlement;
use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::palette::Palette;

use super::rows::{
    ROW_GAP, ROW_PAD_X, ROW_PAD_Y, RowAction, RowMark, ValueTone, column, h_gap, row, settings_row,
    spacer,
};

/// The group's own label, as every other group's is spelled before
/// `text-transform:uppercase` is applied.
pub const PRO_GROUP_LABEL: &str = "muxr pro";
/// The plan row's label.
pub const PLAN_LABEL: &str = "plan";
/// The upgrade action — also how a test finds it.
pub const UPGRADE_LABEL: &str = "Upgrade to Pro";
/// The restore action — also how a test finds it. Deliberately the same text
/// as `features::pro`'s own paywall action: they are two different buttons
/// (this one runs the restore flow directly; the paywall's runs it from
/// inside the sheet), never mounted ambiguously in the same screen state a
/// test taps both from.
pub const RESTORE_LABEL: &str = "Restore purchases";

/// `.set-ic` for the `plan` row — a filled star, this screen's one "premium"
/// tell (no other row uses it).
const MARK_PLAN: RowMark = RowMark::Vector(STAR_FILLED);

/// The `muxr pro` group's rows: the plan row always, and — for anything short
/// of an unlocked install — the actions line underneath it.
pub(in super::super) fn pro_rows(
    entitlement: Entitlement,
    colors: &Palette,
) -> Vec<AnyView<SettingsState>> {
    let tone = match entitlement {
        Entitlement::Pro => ValueTone::Ok,
        Entitlement::Pending => ValueTone::Warn,
        Entitlement::Free => ValueTone::Plain,
    };
    let mut rows = vec![settings_row(
        MARK_PLAN,
        PLAN_LABEL,
        entitlement.label(),
        tone,
        RowAction::Static,
        colors,
    )];
    if !entitlement.is_pro() {
        rows.push(actions_row(colors));
    }
    rows
}

/// The Upgrade/Restore line — `widgets::saved_servers`' own actions-row shape
/// (a trailing pair on a spacer-led row), under its own padding and the
/// group's trailing divider, since [`settings_row`] does not compose this
/// screen's plain rows apart.
fn actions_row(colors: &Palette) -> AnyView<SettingsState> {
    let box_ = column(vec![
        any(Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            row(vec![
                spacer(),
                inflexible(
                    button(UPGRADE_LABEL, |state: &mut SettingsState| {
                        pages::open_upgrade_sheet(state)
                    })
                    .style(ButtonStyle::Primary)
                    .small(),
                ),
                h_gap(ROW_GAP),
                inflexible(
                    button(RESTORE_LABEL, |state: &mut SettingsState| {
                        pages::restore_purchases(state)
                    })
                    .style(ButtonStyle::Ghost)
                    .small(),
                ),
            ]),
        )),
        any(divider(colors.rule)),
    ]);
    any(box_)
}
