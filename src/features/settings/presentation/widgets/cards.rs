//! The Tablet card grid: each settings group as a Glyph card, paired into two
//! columns on a full-width window, closed by the Disconnect card.

use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, FlexChild,
    FlexView, Padding, SizedBox, any, button, flexible, inflexible, safe_area, scroll_view, text,
};
use frust_glyph::glyph_card;

use crate::features::settings::presentation::pages::{
    DISCONNECT_COPY, DISCONNECT_LABEL, SettingsState, open_disconnect_dialog,
};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

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
) -> AnyView<SettingsState> {
    any(glyph_card::<SettingsState>()
        .title(group_label(label, colors))
        .desc(
            FlexView::new(Axis::Vertical, rows.into_iter().map(inflexible).collect())
                .cross_axis(CrossAxisAlignment::Stretch),
        ))
}

/// The closing full-width card: the label, the confirmation copy as the value
/// and the danger button trailing.
fn disconnect_card(colors: &Palette) -> AnyView<SettingsState> {
    let theme = active_theme();
    let row = FlexView::new(
        Axis::Horizontal,
        vec![
            flexible(
                1,
                text(DISCONNECT_COPY).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
            ),
            inflexible(SizedBox::<SettingsState>(Some(CARD_GAP), None)),
            inflexible(
                button(DISCONNECT_LABEL, |state: &mut SettingsState| {
                    open_disconnect_dialog(state)
                })
                .style(ButtonStyle::Danger),
            ),
        ],
    )
    .cross_axis(CrossAxisAlignment::Center);
    any(glyph_card::<SettingsState>()
        .title(group_label("disconnect", colors))
        .desc(Padding(EdgeInsets::symmetric(spacing::XL, 0.0), row)))
}

/// `children` with a [`CARD_GAP`] spacer between each neighbour, running
/// along `axis`; `flex` is each child's flex factor (`None` = inflexible).
fn interleave_gaps(
    children: Vec<AnyView<SettingsState>>,
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

/// The Tablet body: the cards in two equal columns when `full`, one column
/// otherwise, then the Disconnect card, centred at [`CARD_GRID_MAX_WIDTH`].
pub(in super::super) fn tablet_scroller(
    groups: Vec<(&'static str, Vec<AnyView<SettingsState>>)>,
    full: bool,
    colors: &Palette,
) -> AnyView<SettingsState> {
    let cards: Vec<AnyView<SettingsState>> = groups
        .into_iter()
        .map(|(label, rows)| group_card(label, rows, colors))
        .collect();
    let mut lines: Vec<AnyView<SettingsState>> = if full {
        let mut cards = cards.into_iter().peekable();
        let mut pairs = Vec::new();
        while cards.peek().is_some() {
            let pair: Vec<_> = cards.by_ref().take(2).collect();
            pairs.push(any(FlexView::new(
                Axis::Horizontal,
                interleave_gaps(pair, Axis::Horizontal, Some(1)),
            )
            .cross_axis(CrossAxisAlignment::Start)));
        }
        pairs
    } else {
        cards
    };
    lines.push(disconnect_card(colors));

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
    any(Align(
        Alignment::CENTER,
        SizedBox::<SettingsState>(Some(CARD_GRID_MAX_WIDTH), None)
            .child(safe_area(scroll_view(grid)).top(false)),
    ))
}
