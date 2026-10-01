//! The Tablet detail pane (`docs/tablet.html`'s `.s-detail`): the selected
//! session's header — name, status/backend chips, the this-device and bell
//! chips, then rename, kill and the primary attach — over a grid of Glyph
//! cards: overview stats, the active tab's pane tiles, and the full-width
//! tabs → panes card whose pane pills carry a tab/pane target (honoured by
//! the terminal once Phase 4's attach-into-pane card lands; until then it
//! attaches at the session's server focus).
//!
//! Every count reads the session's `LayoutSummary`. No selection, a list
//! still loading, a failed first load, an empty list and a resurrectible
//! session each draw their own state; under a read-only credential rename and
//! kill turn ghost and refuse with a toast, attach never does.

use frust::authoring::text::TextOverflow;
use frust::icons::PANE_MARK;
use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, FlexChild,
    FlexView, GestureDetector, Padding, SizedBox, any, button, container, divider, flexible, icon,
    inflexible, scroll_view, text,
};
use frust_glyph::{BadgeVariant, badge, empty_state, glyph_card, skeleton, stat_card, tag};

use crate::features::sessions::domain::entities::{
    PaneSummary, Session, SessionBackend, TabSummary, ThirdMetric,
};
use crate::ui::paint::{HAIRLINE, RADIUS_FULL};
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

use super::super::pages::{self, SessionsState};
use super::session_card::{bell_mark, status_variant};
use super::{LoadPhase, active_theme, column, h_gap, row};

/// `.d-body{padding:20px;gap:14px}`.
const BODY_PAD: f64 = spacing::XXL;
const GRID_GAP: f64 = spacing::LG;
/// `.d-top{padding:8px 20px;gap:10px}`.
const HEAD_PAD_X: f64 = spacing::XXL;
const HEAD_PAD_Y: f64 = spacing::SM;
const HEAD_GAP: f64 = spacing::MD;
/// Between chips, and between the header's action buttons.
const CHIP_GAP: f64 = spacing::SM;
/// Between the overview's stat cards.
const STAT_GAP: f64 = spacing::SM;
/// `.lay{height:112px}` — the active tab's tile band, a widget dimension.
const TILES_HEIGHT: f64 = 112.0;
/// `.lay{gap:5px}` / `.lp{padding:7px 9px}`, on the shared scale.
const TILE_GAP: f64 = spacing::TIGHT;
const TILE_PAD: f64 = spacing::SM;
/// `.tree-tab-h{padding:9px 11px}` / `.tree-panes{gap:6px}`.
const TAB_PAD_X: f64 = spacing::MD;
const TAB_PAD_Y: f64 = spacing::SM;
const PILL_GAP: f64 = spacing::XS;
/// `.pp{padding:6px 12px 6px 6px}`.
const PILL_PAD_X: f64 = spacing::MD;
const PILL_PAD_Y: f64 = spacing::XS;
/// The pane mark leading a pill.
const PANE_ICON: f64 = 14.0;
/// Pane pills per row: Flex has no wrap, so the focused tab's pills fill
/// fixed-width rows instead.
const PILLS_PER_ROW_FULL: usize = 3;
const PILLS_PER_ROW_COLLAPSED: usize = 2;
/// The loading state's skeleton heights (`docs/tablet.html`'s `.skel` rows).
const SKELETON_TITLE: f64 = 18.0;
const SKELETON_STATS: f64 = 44.0;
const SKELETON_ROW: f64 = 38.0;

const SELECT_TITLE: &str = "Select a session";
const SELECT_DESC: &str = "Its layout and tabs show here. Attach from the header.";
const EMPTY_TITLE: &str = "No sessions";
const EMPTY_DESC: &str = "Create one on the left to start working on this server.";
const SEP: &str = " \u{b7} ";
const RESURRECT_TITLE: &str = "saved, not running";
const RESURRECT_DESC: &str = "This session is kept by the server but has no live panes. \
                              Attaching resurrects it with its saved tabs; the layout preview \
                              appears once it runs.";

/// The pane beside the Tablet list column. See the [module docs](self).
pub(in crate::features::sessions::presentation) fn detail_pane(
    sessions: &[Session],
    selected: Option<&str>,
    phase: LoadPhase,
    read_only: bool,
    full: bool,
    colors: &Palette,
) -> AnyView<SessionsState> {
    let body = match pane_state(phase, sessions.len(), selected_index(sessions, selected)) {
        PaneState::Loading => loading_body(full),
        PaneState::Empty => hint(EMPTY_TITLE, EMPTY_DESC),
        PaneState::Selected(index) => {
            return selected_detail(&sessions[index], read_only, full, colors);
        }
        PaneState::NoSelection => hint(SELECT_TITLE, SELECT_DESC),
    };
    any(container(body).fill(colors.base))
}

/// What the pane shows, before any widget is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaneState {
    Loading,
    Empty,
    /// The selected session's detail, at its index in the list.
    Selected(usize),
    NoSelection,
}

/// Which state the pane draws: the load phase first, then an empty list,
/// then whether the selection (an index into the list) still resolves. A
/// failed first load behaves like a loaded one — its list is just empty
/// until a refresh lands.
fn pane_state(phase: LoadPhase, session_count: usize, selected: Option<usize>) -> PaneState {
    match phase {
        LoadPhase::Loading => PaneState::Loading,
        LoadPhase::Loaded if session_count == 0 => PaneState::Empty,
        LoadPhase::Failed | LoadPhase::Loaded => {
            selected.map_or(PaneState::NoSelection, PaneState::Selected)
        }
    }
}

fn selected_index(sessions: &[Session], selected: Option<&str>) -> Option<usize> {
    selected.and_then(|id| sessions.iter().position(|s| s.id == id))
}

/// Which cards a selected session's body carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyKind {
    /// Not running: the resurrect card above the overview.
    Resurrect,
    /// Running but no tab detail reported: the overview alone.
    OverviewOnly,
    /// Overview beside the active tab, over the tabs tree.
    Full,
}

fn body_kind(live: bool, has_tabs: bool) -> BodyKind {
    if !live {
        BodyKind::Resurrect
    } else if !has_tabs {
        BodyKind::OverviewOnly
    } else {
        BodyKind::Full
    }
}

/// How the active tab's panes tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TileLayout {
    None,
    One,
    Two,
    /// A 2fr tile beside a column of the rest.
    Stacked,
}

fn tile_layout(pane_count: usize) -> TileLayout {
    match pane_count {
        0 => TileLayout::None,
        1 => TileLayout::One,
        2 => TileLayout::Two,
        _ => TileLayout::Stacked,
    }
}

/// Pane pills per row on the full or collapsed band.
fn pills_per_row(full: bool) -> usize {
    if full {
        PILLS_PER_ROW_FULL
    } else {
        PILLS_PER_ROW_COLLAPSED
    }
}

/// A centered Glyph empty state.
fn hint(title: &str, desc: &str) -> AnyView<SessionsState> {
    any(Align(
        Alignment::CENTER,
        empty_state(title, desc).glyph("\u{25e7}"),
    ))
}

/// The list's first load in flight: skeleton cards in the detail's grid.
fn loading_body(full: bool) -> AnyView<SessionsState> {
    let stretch = f64::INFINITY;
    let overview = glyph_card()
        .title(skeleton(stretch, SKELETON_TITLE))
        .desc(skeleton(stretch, SKELETON_STATS));
    let layout = glyph_card()
        .title(skeleton(stretch, SKELETON_TITLE))
        .desc(skeleton(stretch, TILES_HEIGHT));
    let rows = (0..3)
        .map(|_| any(skeleton(stretch, SKELETON_ROW)))
        .collect();
    let tree = glyph_card().desc(column(rows, spacing::SM));
    let cards = vec![grid(vec![any(overview), any(layout)], full), any(tree)];
    body_scroller(cards)
}

/// Header over the scrolling body for `session`.
fn selected_detail(
    session: &Session,
    read_only: bool,
    full: bool,
    colors: &Palette,
) -> AnyView<SessionsState> {
    let overview = overview_card(session, colors);
    let kind = body_kind(
        session.status.is_live(),
        !session.layout.tabs_detail.is_empty(),
    );
    let cards = match kind {
        BodyKind::Resurrect => {
            vec![resurrect_card(session, colors), grid(vec![overview], full)]
        }
        BodyKind::OverviewOnly => vec![grid(vec![overview], full)],
        BodyKind::Full => vec![
            grid(vec![overview, active_tab_card(session, colors)], full),
            tree_card(session, full, colors),
        ],
    };
    any(container(
        FlexView::new(
            Axis::Vertical,
            vec![
                inflexible(header(session, read_only, colors)),
                inflexible(divider(colors.rule)),
                flexible(1, body_scroller(cards)),
            ],
        )
        .cross_axis(CrossAxisAlignment::Stretch),
    )
    .fill(colors.base))
}

fn body_scroller(cards: Vec<AnyView<SessionsState>>) -> AnyView<SessionsState> {
    any(scroll_view(Padding(
        EdgeInsets::all(BODY_PAD),
        column(cards, GRID_GAP),
    )))
}

/// Two cards to a row on the full band, one on the collapsed band.
fn grid(cards: Vec<AnyView<SessionsState>>, full: bool) -> AnyView<SessionsState> {
    if !full {
        return any(column(cards, GRID_GAP));
    }
    let mut rows = Vec::new();
    let mut cards = cards.into_iter();
    while let Some(first) = cards.next() {
        let second = cards
            .next()
            .unwrap_or_else(|| any(SizedBox::<SessionsState>(None, None)));
        rows.push(any(FlexView::new(
            Axis::Horizontal,
            vec![flexible(1, first), h_gap(GRID_GAP), flexible(1, second)],
        )
        .cross_axis(CrossAxisAlignment::Start)));
    }
    any(column(rows, GRID_GAP))
}

/// `.d-top`: the name and chips, then the actions trailing on their own row.
fn header(session: &Session, read_only: bool, colors: &Palette) -> AnyView<SessionsState> {
    let theme = active_theme();
    let mut chips = vec![
        flexible(
            1,
            text(session.name.clone())
                .style(inked(&theme.type_scale.title_medium, colors.fg))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ),
        h_gap(CHIP_GAP),
        inflexible(badge(
            session.status.label(),
            status_variant(session.status),
        )),
        h_gap(CHIP_GAP),
        inflexible(badge(session.backend.label(), BadgeVariant::Neutral)),
    ];
    if session.is_current {
        chips.push(h_gap(CHIP_GAP));
        chips.push(inflexible(badge(
            "attached from this device",
            BadgeVariant::Accent,
        )));
    }
    if session.has_bell {
        chips.push(h_gap(CHIP_GAP));
        chips.push(inflexible(badge("bell", BadgeVariant::Warning)));
    }
    let actions = row(vec![
        flexible(1, SizedBox::<SessionsState>(None, None)),
        inflexible(mutation_button(
            "rename",
            ButtonStyle::Secondary,
            read_only,
            session,
            pages::open_rename,
        )),
        h_gap(CHIP_GAP),
        inflexible(mutation_button(
            "kill",
            ButtonStyle::Danger,
            read_only,
            session,
            pages::open_kill,
        )),
        h_gap(CHIP_GAP),
        inflexible(attach_button(session)),
    ]);
    any(container(Padding(
        EdgeInsets::symmetric(HEAD_PAD_X, HEAD_PAD_Y),
        column(vec![any(row(chips)), any(actions)], HEAD_GAP),
    ))
    .fill(colors.surface))
}

/// Rename or kill: `style` normally, ghost under read-only, where the page's
/// shim refuses it with the read-only toast.
fn mutation_button(
    label: &'static str,
    style: ButtonStyle,
    read_only: bool,
    session: &Session,
    press: fn(&mut SessionsState, &str),
) -> AnyView<SessionsState> {
    let id = session.id.clone();
    let style = if read_only { ButtonStyle::Ghost } else { style };
    any(button(label, move |state: &mut SessionsState| press(state, &id)).style(style))
}

/// The primary `→ attach`, worded for a resurrect when nothing is running.
fn attach_button(session: &Session) -> AnyView<SessionsState> {
    let label = if session.status.is_live() {
        "\u{2192} attach"
    } else {
        "\u{2192} attach to resurrect"
    };
    let id = session.id.clone();
    any(button(label, move |state: &mut SessionsState| {
        pages::attach(state, &id)
    }))
}

/// A card's dim lowercase heading (`.card-title`).
fn card_title(title: impl Into<String>, colors: &Palette) -> AnyView<SessionsState> {
    let theme = active_theme();
    any(text(title.into()).style(inked(&theme.type_scale.label_small, colors.fg_dim)))
}

/// Tabs, panes and the backend's third metric, then the age on its own row
/// (the age wording is the widest value).
fn overview_card(session: &Session, colors: &Palette) -> AnyView<SessionsState> {
    let layout = &session.layout;
    let (third_label, third_value) = third_stat(layout.third);
    let counts = FlexView::new(
        Axis::Horizontal,
        vec![
            flexible(1, stat_card("tabs", layout.tabs.to_string())),
            h_gap(STAT_GAP),
            flexible(1, stat_card("panes", layout.panes.to_string())),
            h_gap(STAT_GAP),
            flexible(1, stat_card(third_label, third_value.to_string())),
        ],
    )
    .cross_axis(CrossAxisAlignment::Stretch);
    let stats = column(
        vec![any(counts), any(stat_card("age", session.age_label()))],
        STAT_GAP,
    );
    any(glyph_card()
        .title(card_title("overview", colors))
        .desc(stats))
}

/// The third metric as a stat's label and value.
fn third_stat(third: ThirdMetric) -> (&'static str, u32) {
    match third {
        ThirdMetric::Clients(n) => ("clients", n),
        ThirdMetric::Spaces(n) => ("spaces", n),
        ThirdMetric::TabsSaved(n) => ("tabs saved", n),
    }
}

/// `.resurrect`: what a saved session is, and the attach that revives it.
fn resurrect_card(session: &Session, colors: &Palette) -> AnyView<SessionsState> {
    let theme = active_theme();
    let desc = column(
        vec![
            any(text(RESURRECT_DESC).style(inked(&theme.type_scale.body_small, colors.fg_muted))),
            any(row(vec![
                inflexible(attach_button(session)),
                flexible(1, SizedBox::<SessionsState>(None, None)),
            ])),
        ],
        spacing::BASE,
    );
    any(glyph_card()
        .title(card_title(RESURRECT_TITLE, colors))
        .desc(desc))
}

/// `space · <space> · ` on herdr when the layout names its space.
fn space_prefix(session: &Session) -> Option<String> {
    match (session.backend, &session.layout.space) {
        (SessionBackend::Herdr, Some(space)) => Some(space.clone()),
        _ => None,
    }
}

/// The focused tab's panes as tiles: one full, two equal, three or more as
/// a 2fr tile beside a stacked column.
fn active_tab_card(session: &Session, colors: &Palette) -> AnyView<SessionsState> {
    let tab = session.layout.focused_tab();
    let tab_name = tab.map_or("\u{2014}", |tab| tab.name.as_str());
    let title = match space_prefix(session) {
        Some(space) => format!("space{SEP}{space}{SEP}tab{SEP}{tab_name}"),
        None => format!("active tab{SEP}{tab_name}"),
    };
    let panes = tab.map_or(&[][..], |tab| tab.panes.as_slice());
    let tiles: AnyView<SessionsState> = match (tile_layout(panes.len()), panes) {
        (TileLayout::None, _) => card_title("no panes reported", colors),
        (TileLayout::One, [only]) => tile(only, colors),
        (TileLayout::Two, [a, b]) => any(tile_row(vec![
            flexible(1, tile(a, colors)),
            flexible(1, tile(b, colors)),
        ])),
        (_, [first, rest @ ..]) => {
            let stacked = rest.iter().map(|pane| flexible(1, tile(pane, colors)));
            let stack = FlexView::new(Axis::Vertical, spaced(stacked.collect(), false))
                .cross_axis(CrossAxisAlignment::Stretch);
            any(tile_row(vec![
                flexible(2, tile(first, colors)),
                flexible(1, stack),
            ]))
        }
        (_, []) => card_title("no panes reported", colors),
    };
    any(glyph_card()
        .title(card_title(title, colors))
        .desc(SizedBox::<SessionsState>(None, Some(TILES_HEIGHT)).child(tiles)))
}

fn tile_row(children: Vec<FlexChild<SessionsState>>) -> FlexView<SessionsState> {
    FlexView::new(Axis::Horizontal, spaced(children, true)).cross_axis(CrossAxisAlignment::Stretch)
}

/// `children` with a [`TILE_GAP`] spacer between neighbours along the axis.
fn spaced(children: Vec<FlexChild<SessionsState>>, across: bool) -> Vec<FlexChild<SessionsState>> {
    let mut spaced = Vec::with_capacity(children.len() * 2);
    for (index, child) in children.into_iter().enumerate() {
        if index > 0 {
            spaced.push(if across {
                h_gap(TILE_GAP)
            } else {
                inflexible(SizedBox::<SessionsState>(None, Some(TILE_GAP)))
            });
        }
        spaced.push(child);
    }
    spaced
}

/// `.lp`: one pane tile, amber when focused.
fn tile(pane: &PaneSummary, colors: &Palette) -> AnyView<SessionsState> {
    let theme = active_theme();
    let (fill, border, ink) = if pane.focused {
        (colors.amber_faint, colors.amber_border, colors.amber)
    } else {
        (colors.raised, colors.rule, colors.fg_muted)
    };
    let title = text(pane.title.clone())
        .style(inked(&theme.type_scale.label_small, ink))
        .max_lines(1)
        .overflow(TextOverflow::Ellipsis);
    any(container(Padding(
        EdgeInsets::all(TILE_PAD),
        Align(Alignment::TOP_LEFT, title),
    ))
    .fill(fill)
    .border(border, HAIRLINE)
    .radius(theme.shape.extra_small))
}

/// Every tab as a rectangle, the focused one open on its pane pills.
fn tree_card(session: &Session, full: bool, colors: &Palette) -> AnyView<SessionsState> {
    let title = match space_prefix(session) {
        Some(space) => format!("tabs \u{2192} panes{SEP}space {space}"),
        None => "tabs \u{2192} panes".to_string(),
    };
    let tabs = session
        .layout
        .tabs_detail
        .iter()
        .map(|tab| tab_rect(&session.id, tab, full, colors))
        .collect();
    any(glyph_card()
        .title(card_title(title, colors))
        .desc(column(tabs, spacing::SM)))
}

/// `.tree-tab`: index badge, name, one chip, and the focused tab's pills.
fn tab_rect(id: &str, tab: &TabSummary, full: bool, colors: &Palette) -> AnyView<SessionsState> {
    let theme = active_theme();
    let position = (tab.index + 1).to_string();
    let index_variant = if tab.focused {
        BadgeVariant::Accent
    } else {
        BadgeVariant::Neutral
    };
    let mut head = vec![
        inflexible(badge(position, index_variant)),
        h_gap(CHIP_GAP),
        flexible(
            1,
            text(tab.name.clone())
                .style(inked(&theme.type_scale.label_large, colors.fg))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ),
    ];
    if tab.has_bell && !tab.focused {
        head.push(inflexible(bell_mark(colors)));
        head.push(h_gap(CHIP_GAP));
    }
    head.push(inflexible(tab_chip(tab)));
    let mut parts = vec![any(Padding(
        EdgeInsets::symmetric(TAB_PAD_X, TAB_PAD_Y),
        row(head),
    ))];
    if tab.focused && !tab.panes.is_empty() {
        parts.push(any(divider(colors.rule)));
        parts.push(any(container(Padding(
            EdgeInsets::symmetric(TAB_PAD_X, TAB_PAD_Y),
            pane_pills(id, tab, full, colors),
        ))
        .fill(colors.base)));
    }
    any(container(column(parts, 0.0))
        .fill(colors.surface)
        .border(colors.outline, HAIRLINE)
        .radius(theme.shape.small))
}

/// `focused tab` | `bell` | `N panes`.
fn tab_chip(tab: &TabSummary) -> AnyView<SessionsState> {
    let chip = if tab.focused {
        badge("focused tab", BadgeVariant::Accent)
    } else if tab.has_bell {
        badge("bell", BadgeVariant::Warning)
    } else {
        badge(panes_count(tab.panes.len()), BadgeVariant::Neutral)
    };
    any(chip)
}

fn panes_count(count: usize) -> String {
    if count == 1 {
        "1 pane".to_string()
    } else {
        format!("{count} panes")
    }
}

/// The tab's pills in equal-width rows, the last row padded with blanks.
fn pane_pills(id: &str, tab: &TabSummary, full: bool, colors: &Palette) -> AnyView<SessionsState> {
    let per_row = pills_per_row(full);
    let rows = tab
        .panes
        .chunks(per_row)
        .map(|chunk| {
            let mut cells: Vec<FlexChild<SessionsState>> = Vec::with_capacity(per_row * 2);
            for slot in 0..per_row {
                if slot > 0 {
                    cells.push(h_gap(PILL_GAP));
                }
                cells.push(match chunk.get(slot) {
                    Some(pane) => flexible(1, pane_pill(id, tab.index, pane, colors)),
                    None => flexible(1, SizedBox::<SessionsState>(None, None)),
                });
            }
            any(row(cells))
        })
        .collect();
    any(column(rows, PILL_GAP))
}

/// `.pp`: pane mark, title, cwd, `float` — a tap attaches with the pane's
/// `(tab, id)` target (see the module docs for who honours it).
fn pane_pill(id: &str, tab: u32, pane: &PaneSummary, colors: &Palette) -> AnyView<SessionsState> {
    let theme = active_theme();
    let (fill, border, ink) = if pane.focused {
        (colors.amber_faint, colors.amber_border, colors.amber)
    } else {
        (colors.surface, colors.outline, colors.fg_muted)
    };
    let mut parts = vec![
        inflexible(icon(PANE_MARK).size(PANE_ICON).color(ink)),
        h_gap(PILL_GAP),
        flexible(
            2,
            text(pane.title.clone())
                .style(inked(&theme.type_scale.label_small, ink))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ),
    ];
    if let Some(cwd) = &pane.cwd {
        parts.push(h_gap(PILL_GAP));
        parts.push(flexible(
            1,
            text(cwd.clone())
                .style(inked(&theme.type_scale.label_small, colors.fg_dim))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ));
    }
    if pane.floating {
        parts.push(h_gap(PILL_GAP));
        parts.push(inflexible(tag::<SessionsState>("float")));
    }
    let pill = container(Padding(
        EdgeInsets::symmetric(PILL_PAD_X, PILL_PAD_Y),
        row(parts),
    ))
    .fill(fill)
    .border(border, HAIRLINE)
    .radius(RADIUS_FULL);
    let id = id.to_string();
    let pane_id = pane.id;
    any(GestureDetector(pill)
        .on_tap(move |state: &mut SessionsState| pages::attach_pane(state, &id, tab, pane_id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tabs_pane_count_reads_in_the_singular_for_one() {
        assert_eq!(panes_count(1), "1 pane");
        assert_eq!(panes_count(3), "3 panes");
    }

    #[test]
    fn every_third_metric_splits_into_a_label_and_its_count() {
        assert_eq!(third_stat(ThirdMetric::Clients(2)), ("clients", 2));
        assert_eq!(third_stat(ThirdMetric::Spaces(5)), ("spaces", 5));
        assert_eq!(third_stat(ThirdMetric::TabsSaved(0)), ("tabs saved", 0));
    }

    #[test]
    fn the_pane_state_follows_phase_then_emptiness_then_selection() {
        assert_eq!(pane_state(LoadPhase::Loading, 0, None), PaneState::Loading);
        assert_eq!(
            pane_state(LoadPhase::Loading, 3, Some(1)),
            PaneState::Loading,
            "loading wins over a stale selection"
        );
        assert_eq!(pane_state(LoadPhase::Loaded, 0, None), PaneState::Empty);
        assert_eq!(
            pane_state(LoadPhase::Loaded, 3, None),
            PaneState::NoSelection
        );
        assert_eq!(
            pane_state(LoadPhase::Loaded, 3, Some(2)),
            PaneState::Selected(2)
        );
    }

    #[test]
    fn a_failed_first_load_asks_for_a_selection_rather_than_claiming_emptiness() {
        assert_eq!(
            pane_state(LoadPhase::Failed, 0, None),
            PaneState::NoSelection
        );
        assert_eq!(
            pane_state(LoadPhase::Failed, 2, Some(0)),
            PaneState::Selected(0)
        );
    }

    #[test]
    fn the_body_carries_the_cards_its_session_state_earns() {
        assert_eq!(body_kind(false, true), BodyKind::Resurrect);
        assert_eq!(body_kind(false, false), BodyKind::Resurrect);
        assert_eq!(body_kind(true, false), BodyKind::OverviewOnly);
        assert_eq!(body_kind(true, true), BodyKind::Full);
    }

    #[test]
    fn panes_tile_one_two_or_a_stack_by_count() {
        assert_eq!(tile_layout(0), TileLayout::None);
        assert_eq!(tile_layout(1), TileLayout::One);
        assert_eq!(tile_layout(2), TileLayout::Two);
        assert_eq!(tile_layout(3), TileLayout::Stacked);
        assert_eq!(tile_layout(9), TileLayout::Stacked);
    }

    #[test]
    fn the_full_band_fits_one_more_pill_per_row() {
        assert_eq!(pills_per_row(true), PILLS_PER_ROW_FULL);
        assert_eq!(pills_per_row(false), PILLS_PER_ROW_COLLAPSED);
    }
}
