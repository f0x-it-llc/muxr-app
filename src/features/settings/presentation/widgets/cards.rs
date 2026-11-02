//! The Tablet card grid: each settings group as a Glyph card, packed into two
//! independent columns on a full-width window (a short card never holds its
//! neighbour's row open), closed by the Lock & disconnect card.

use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, FlexChild,
    FlexView, Padding, SizedBox, View, any, button, flexible, inflexible, row, safe_area,
    scroll_view, text,
};
use frust_glyph::glyph_card;

use crate::features::settings::presentation::pages::{SettingsState, open_disconnect_dialog};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::misc::{DISCONNECT_COPY, DISCONNECT_LABEL};
use super::rows::{SCROLL_PAD_BOTTOM, SCROLL_PAD_TOP, group_label};

/// The card grid's content max width on Tablet, centred in the window.
const CARD_GRID_MAX_WIDTH: f64 = 920.0;
/// The gap between cards, horizontally and vertically.
const CARD_GAP: f64 = spacing::LG;

/// One group as a Glyph card: the group label is the title, the rows the body.
fn group_card(
    label: &str,
    rows: Vec<AnyView<SettingsState>>,
    colors: &Palette,
) -> impl View<SettingsState> {
    glyph_card::<SettingsState>()
        .title(group_label(label, colors))
        .desc(
            FlexView::new(Axis::Vertical, rows.into_iter().map(inflexible).collect())
                .cross_axis(CrossAxisAlignment::Stretch),
        )
}

/// The closing full-width card: the label, the confirmation copy as the value
/// and the danger button trailing.
fn disconnect_card(colors: &Palette) -> impl View<SettingsState> {
    let theme = active_theme();
    let row = row()
        .flex(
            1,
            text(DISCONNECT_COPY).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        )
        .child(SizedBox::<SettingsState>(Some(CARD_GAP), None))
        .child(
            button(DISCONNECT_LABEL, |state: &mut SettingsState| {
                open_disconnect_dialog(state)
            })
            .style(ButtonStyle::Danger),
        )
        .cross_axis(CrossAxisAlignment::Center);
    glyph_card::<SettingsState>()
        .title(group_label("lock & disconnect", colors))
        .desc(Padding(EdgeInsets::symmetric(spacing::XL, 0.0), row))
}

/// `children` with a [`CARD_GAP`] spacer between each neighbour, running
/// along `axis`; `flex` is each child's flex factor (`None` = inflexible).
fn interleave_gaps<V: View<SettingsState>>(
    children: Vec<V>,
    axis: Axis,
    flex: Option<u32>,
) -> Vec<FlexChild<SettingsState>> {
    let mut out = Vec::with_capacity(children.len() * 2);
    for (index, child) in children.into_iter().enumerate() {
        if index > 0 {
            out.push(inflexible(match axis {
                Axis::Horizontal => SizedBox::<SettingsState>(Some(CARD_GAP), None),
                Axis::Vertical => SizedBox::<SettingsState>(None, Some(CARD_GAP)),
            }));
        }
        out.push(match flex {
            Some(factor) => flexible(factor, child),
            None => inflexible(child),
        });
    }
    out
}

/// The Tablet body: the cards packed into two equal columns when `full` —
/// even indices left, odd right, each column stacking its own cards at
/// [`CARD_GAP`] — one column otherwise, then the Disconnect card, centred at
/// [`CARD_GRID_MAX_WIDTH`].
pub(in super::super) fn tablet_scroller(
    groups: Vec<(&'static str, Vec<AnyView<SettingsState>>)>,
    full: bool,
    colors: &Palette,
) -> impl View<SettingsState> + use<> {
    let cards: Vec<AnyView<SettingsState>> = groups // erasure: keep accumulator into Vec<AnyView>
        .into_iter()
        .map(|(label, rows)| any(group_card(label, rows, colors)))
        .collect();
    // erasure: keep accumulator into Vec<AnyView>
    let mut lines: Vec<AnyView<SettingsState>> = if full {
        let (left, right): (Vec<_>, Vec<_>) = cards
            .into_iter()
            .enumerate()
            .partition(|(index, _)| index % 2 == 0);
        let column = |cards: Vec<(usize, AnyView<SettingsState>)>| {
            let cards = cards.into_iter().map(|(_, card)| card).collect();
            FlexView::new(Axis::Vertical, interleave_gaps(cards, Axis::Vertical, None))
                .cross_axis(CrossAxisAlignment::Stretch)
        };
        vec![any(FlexView::new(
            Axis::Horizontal,
            interleave_gaps(vec![column(left), column(right)], Axis::Horizontal, Some(1)),
        )
        .cross_axis(CrossAxisAlignment::Start))]
    } else {
        cards
    };
    lines.push(any(disconnect_card(colors)));

    let grid = Padding(
        EdgeInsets {
            left: spacing::XL,
            top: SCROLL_PAD_TOP,
            right: spacing::XL,
            bottom: SCROLL_PAD_BOTTOM,
        },
        FlexView::new(Axis::Vertical, interleave_gaps(lines, Axis::Vertical, None))
            .cross_axis(CrossAxisAlignment::Stretch),
    );
    Align(
        Alignment::CENTER,
        SizedBox::<SettingsState>(Some(CARD_GRID_MAX_WIDTH), None)
            .child(safe_area(scroll_view(grid)).top(false)),
    )
}
