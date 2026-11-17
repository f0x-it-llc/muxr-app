//! The Tablet card grid: two independent columns, so a short card never holds its
//! neighbour's row open.

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

const CARD_GRID_MAX_WIDTH: f64 = 920.0;
const CARD_GAP: f64 = spacing::LG;

/// Which Full-band column a group's card sits in; the Collapsed band keeps the given order.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum CardColumn {
    Left,
    Right,
}

fn group_card<S: 'static>(
    label: &str,
    rows: Vec<AnyView<S>>,
    colors: &Palette,
) -> impl View<S> + use<S> {
    glyph_card::<S>().title(group_label(label, colors)).desc(
        FlexView::new(Axis::Vertical, rows.into_iter().map(inflexible).collect())
            .cross_axis(CrossAxisAlignment::Stretch),
    )
}

fn disconnect_card(colors: &Palette) -> impl View<SettingsState> {
    let theme = active_theme();
    let row = row()
        .flex(
            1,
            text(DISCONNECT_COPY).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        )
        .child(
            button(DISCONNECT_LABEL, |state: &mut SettingsState| {
                open_disconnect_dialog(state)
            })
            .style(ButtonStyle::Danger),
        )
        .gap(CARD_GAP)
        .cross_axis(CrossAxisAlignment::Center);
    glyph_card::<SettingsState>()
        .title(group_label("lock & disconnect", colors))
        .desc(Padding(EdgeInsets::symmetric(spacing::XL, 0.0), row))
}

fn card_children<S: 'static, V: View<S>>(children: Vec<V>, flex: Option<u32>) -> Vec<FlexChild<S>> {
    children
        .into_iter()
        .map(|child| match flex {
            Some(factor) => flexible(factor, child),
            None => inflexible(child),
        })
        .collect()
}

/// The server screen alternates its groups between the columns and closes on Lock & disconnect.
pub(in super::super) fn tablet_scroller(
    groups: Vec<(&'static str, Vec<AnyView<SettingsState>>)>,
    full: bool,
    colors: &Palette,
) -> impl View<SettingsState> + use<> {
    let placed = groups
        .into_iter()
        .enumerate()
        .map(|(index, (label, rows))| {
            let column = if index % 2 == 0 {
                CardColumn::Left
            } else {
                CardColumn::Right
            };
            (column, label, rows)
        })
        .collect();
    let tail = any(disconnect_card(colors)); // erasure: keep accumulator into Vec<AnyView>
    card_grid(placed, full, Some(tail), colors)
}

pub(in super::super) fn card_grid<S: 'static>(
    groups: Vec<(CardColumn, &'static str, Vec<AnyView<S>>)>,
    full: bool,
    tail: Option<AnyView<S>>,
    colors: &Palette,
) -> impl View<S> + use<S> {
    // erasure: keep accumulator into Vec<AnyView>
    let mut lines: Vec<AnyView<S>> = if full {
        let (left, right): (Vec<_>, Vec<_>) = groups
            .into_iter()
            .partition(|(column, _, _)| *column == CardColumn::Left);
        let column = |groups: Vec<(CardColumn, &'static str, Vec<AnyView<S>>)>| {
            let cards = groups
                .into_iter()
                .map(|(_, label, rows)| group_card(label, rows, colors))
                .collect();
            FlexView::new(Axis::Vertical, card_children(cards, None))
                .gap(CARD_GAP)
                .cross_axis(CrossAxisAlignment::Stretch)
        };
        vec![any(FlexView::new(
            Axis::Horizontal,
            card_children(vec![column(left), column(right)], Some(1)),
        )
        .gap(CARD_GAP)
        .cross_axis(CrossAxisAlignment::Start))]
    } else {
        groups
            .into_iter()
            .map(|(_, label, rows)| any(group_card(label, rows, colors)))
            .collect()
    };
    lines.extend(tail);

    let grid = Padding(
        EdgeInsets {
            left: spacing::XL,
            top: SCROLL_PAD_TOP,
            right: spacing::XL,
            bottom: SCROLL_PAD_BOTTOM,
        },
        FlexView::new(Axis::Vertical, card_children(lines, None))
            .gap(CARD_GAP)
            .cross_axis(CrossAxisAlignment::Stretch),
    );
    Align(
        Alignment::CENTER,
        SizedBox::<S>(Some(CARD_GRID_MAX_WIDTH), None)
            .child(safe_area(scroll_view(grid)).top(false)),
    )
}
