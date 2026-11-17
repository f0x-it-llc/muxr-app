//! The Tablet card grid: two independent columns, so a short card never holds its
//! neighbour's row open.

use frust::{
    Align, Alignment, AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexChild, FlexView, Padding,
    SizedBox, View, any, flexible, inflexible, safe_area, scroll_view,
};
use frust_glyph::glyph_card;

use crate::features::settings::presentation::pages::SettingsState;
use crate::ui::palette::Palette;
use crate::ui::spacing;

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

fn card_children<S: 'static, V: View<S>>(children: Vec<V>, flex: Option<u32>) -> Vec<FlexChild<S>> {
    children
        .into_iter()
        .map(|child| match flex {
            Some(factor) => flexible(factor, child),
            None => inflexible(child),
        })
        .collect()
}

/// The server screen's cards. The Full band puts `identity`, `access` and `this_server` in the
/// left column and the `tokens` card in the right; the Collapsed band stacks them in that order.
pub(in super::super) struct ServerCards {
    pub identity: AnyView<SettingsState>,
    pub access: Vec<AnyView<SettingsState>>,
    pub this_server: Vec<AnyView<SettingsState>>,
    pub tokens: Vec<AnyView<SettingsState>>,
}

pub(in super::super) fn server_tablet_scroller(
    cards: ServerCards,
    full: bool,
    colors: &Palette,
) -> impl View<SettingsState> + use<> {
    let ServerCards {
        identity,
        access,
        this_server,
        tokens,
    } = cards;
    let access = any(group_card("access", access, colors));
    let this_server = any(group_card("this server", this_server, colors));
    let tokens = any(group_card("auth tokens", tokens, colors));
    let lines: Vec<AnyView<SettingsState>> = if full {
        let column = |cards: Vec<AnyView<SettingsState>>| {
            FlexView::new(Axis::Vertical, card_children(cards, None))
                .gap(CARD_GAP)
                .cross_axis(CrossAxisAlignment::Stretch)
        };
        vec![any(FlexView::new(
            Axis::Horizontal,
            card_children(
                vec![
                    column(vec![identity, access, this_server]),
                    column(vec![tokens]),
                ],
                Some(1),
            ),
        )
        .gap(CARD_GAP)
        .cross_axis(CrossAxisAlignment::Start))]
    } else {
        vec![identity, access, this_server, tokens]
    };
    grid_shell(lines)
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
    grid_shell(lines)
}

fn grid_shell<S: 'static>(lines: Vec<AnyView<S>>) -> impl View<S> + use<S> {
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
