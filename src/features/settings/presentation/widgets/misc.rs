use frust::{
    AnyView, ButtonStyle, EdgeInsets, Padding, SizedBox, View, any, button, divider, flexible,
    inflexible, text,
};
use frust_glyph::toggle;

use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::rows::{
    MARK_BELL, ROW_GAP, ROW_LINE_GAP, ROW_PAD_X, ROW_PAD_Y, centered_row, h_gap, mark_tile,
    stretch_column,
};

/// `.disconnect-btn{margin:20px 16px 4px}`.
const DISCONNECT_MARGIN_TOP: f64 = spacing::XXL;
const DISCONNECT_MARGIN_BOTTOM: f64 = spacing::TIGHT;
const BELL_VALUE: &str = "haptic + badge";
pub(in super::super) const DISCONNECT_COPY: &str = "Locks the keyring on this device and \
                                                    disconnects. You will unlock with Face ID \
                                                    or your fingerprint next time.";
pub(in super::super) const DISCONNECT_LABEL: &str = "Lock & disconnect";

/// The toggle owns its own tap target, so the row has no outer `GestureDetector`.
pub(super) fn bell_notifications_row(
    on: bool,
    colors: &Palette,
    on_toggle: impl Fn(&mut SettingsState) + 'static,
) -> AnyView<SettingsState> // erasure: keep accumulator into Vec<AnyView>
{
    let theme = active_theme();
    let toggle_view =
        toggle::<SettingsState, _>(on, move |state: &mut SettingsState, _requested: bool| {
            on_toggle(state)
        })
        .label("bell notifications");
    let label_column = stretch_column((
        text("bell notifications").style(inked(&theme.type_scale.label_large, colors.fg)),
        SizedBox::<SettingsState>(None, Some(ROW_LINE_GAP)),
        text(BELL_VALUE).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
    ));
    let children = vec![
        inflexible(mark_tile(MARK_BELL, colors)),
        h_gap(ROW_GAP),
        flexible(1, label_column),
        h_gap(ROW_GAP),
        inflexible(toggle_view),
    ];

    let box_ = stretch_column((
        Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            centered_row(children),
        ),
        divider(colors.rule),
    ));

    any(box_)
}

pub(in super::super) fn disconnect_button() -> impl View<SettingsState> {
    Padding(
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
    )
}
