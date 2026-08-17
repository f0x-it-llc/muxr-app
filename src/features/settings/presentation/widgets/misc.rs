//! The two rows that don't fit `rows`'/`tokens`'/`saved_servers`' own
//! clusters: the `session poll` group's bell switch, and the screen's
//! closing `Disconnect` action.

use frust::{
    AnyView, ButtonStyle, EdgeInsets, Padding, SizedBox, any, button, flexible, inflexible, text,
};
use frust_glyph::toggle;

use crate::features::settings::presentation::chrome::panel;
use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::rows::{
    MARK_BELL, ROW_GAP, ROW_LINE_GAP, ROW_PAD_X, ROW_PAD_Y, column, h_gap, mark_tile, row,
};

/// `.disconnect-btn{margin:20px 16px 4px}`.
const DISCONNECT_MARGIN_TOP: f64 = spacing::XXL;
const DISCONNECT_MARGIN_BOTTOM: f64 = spacing::TIGHT;
/// The bell row's `.set-value` — copy, not a preference: the switch beside it
/// is the state, and this says what being on *means*.
const BELL_VALUE: &str = "haptic + badge";
/// `.disconnect-btn`'s label.
const DISCONNECT_LABEL: &str = "Disconnect";

/// A complete row with mark tile, flexible label/subtitle column, and the
/// real, themed `frust_glyph::toggle` — structured like [`super::rows::settings_row`]
/// but with the toggle owning its own tap target (no outer
/// `GestureDetector`, the same pointer-capture constraint
/// `widgets::tokens::dialog_read_only_row` documents).
pub(super) fn bell_notifications_row(
    on: bool,
    colors: &Palette,
    on_toggle: impl Fn(&mut SettingsState) + 'static,
) -> AnyView<SettingsState> {
    let theme = active_theme();
    let toggle_view = any(toggle::<SettingsState, _>(
        on,
        move |state: &mut SettingsState, _requested: bool| on_toggle(state),
    )
    .label("bell notifications"));
    let label_column = column(vec![
        any(text("bell notifications").style(inked(&theme.type_scale.label_large, colors.fg))),
        any(SizedBox::<SettingsState>(None, Some(ROW_LINE_GAP))),
        any(text(BELL_VALUE).style(inked(&theme.type_scale.body_small, colors.fg_dim))),
    ]);
    let children = vec![
        inflexible(mark_tile(MARK_BELL, colors)),
        h_gap(ROW_GAP),
        flexible(1, any(label_column)),
        h_gap(ROW_GAP),
        inflexible(toggle_view),
    ];

    let box_ = panel(
        Padding(EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y), row(children)),
        frust::authoring::Color::TRANSPARENT,
        0.0,
    )
    .bottom_rule(colors.rule);

    any(box_)
}

/// The screen's last element: a full-width outlined error-hued action.
/// `ButtonStyle::Danger` *is* the mock's `.disconnect-btn` (transparent
/// fill, error hairline, error ink).
pub(in super::super) fn disconnect_button() -> AnyView<SettingsState> {
    any(Padding(
        EdgeInsets {
            left: ROW_PAD_X,
            top: DISCONNECT_MARGIN_TOP,
            right: ROW_PAD_X,
            bottom: DISCONNECT_MARGIN_BOTTOM,
        },
        button(DISCONNECT_LABEL, |state: &mut SettingsState| {
            pages::open_disconnect_dialog(state)
        })
        .style(ButtonStyle::Danger),
    ))
}
