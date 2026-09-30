//! The Tablet's docked rail (`docs/tablet.html` frame 03): the "panes & tabs"
//! tree docked beside the card instead of raised in a sheet, and its 56px mini
//! rail for the collapsed band and immersive mode.
//!
//! **Composition only.** The tree arrives already built
//! ([`super::side_sheet_tree::tree_body`] in its docked mode) and every press
//! leaves through a caller-supplied callback, so this module owns layout and
//! tone, never the controller.
//!
//! Known gap, as on Connect's in-flow QR button: `frust::button` takes a text
//! label only, so the footer's `detach` carries no leading logout mark.

use std::rc::Rc;

use frust::authoring::Size;
use frust::authoring::text::{FontStyle, TextOverflow};
use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, FlexView,
    GestureDetector, IconData, Padding, SizedBox, any, button, container, flexible, icons,
    inflexible, scroll_view, text,
};
use frust_glyph::{BadgeVariant, badge, tooltip};

use crate::features::terminal::data::feeds::grpc_feed::FeedStatus;
use crate::features::terminal::domain::entities::{PaneEntry, Space, TabEntry};
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;
use crate::ui::topbar::icon_button;

use super::side_sheet_tree::SPACE_DIAMOND;
use super::util::{h_gap, row, sized, v_gap};

/// `.rail{width:264px}`.
pub(crate) const RAIL_WIDTH: f64 = 264.0;
/// `.term-screen.fs .rail{width:56px}` — the mini rail.
pub(crate) const MINI_RAIL_WIDTH: f64 = 56.0;
/// `.mini-btn{width:44px;height:44px}` and its 15px mark.
const MINI_BUTTON: f64 = 44.0;
const MINI_MARK: f64 = 15.0;
/// `.mini-sep{width:20px}`.
const MINI_SEP_WIDTH: f64 = 20.0;

/// `.rail-head .n{font-size:12.5px}`, its `small` line, and `.rail-foot .tip`.
const NAME_SIZE: f32 = 12.5;
const SUMMARY_SIZE: f32 = 9.5;
const TIP_SIZE: f32 = 9.5;

/// `.rail-head{padding:12px 14px 10px}`.
const HEAD_PAD: EdgeInsets = EdgeInsets {
    left: spacing::LG,
    top: spacing::BASE,
    right: spacing::LG,
    bottom: spacing::MD,
};
/// `.rail-tree{padding:12px 12px 8px}`.
const TREE_PAD: EdgeInsets = EdgeInsets {
    left: spacing::BASE,
    top: spacing::BASE,
    right: spacing::BASE,
    bottom: spacing::SM,
};
/// `.rail-foot{padding:8px 12px 10px}`.
const FOOT_PAD: EdgeInsets = EdgeInsets {
    left: spacing::BASE,
    top: spacing::SM,
    right: spacing::BASE,
    bottom: spacing::MD,
};

const TIP: &str = "hold a row to rename, or use its \u{22ef} menu";
const TIP_READ_ONLY: &str = "read-only: browse and switch only";
const DETACH_LABEL: &str = "detach";
const READ_ONLY_CHIP: &str = "read-only";
const EXIT_IMMERSIVE: &str = "Exit immersive";

/// What the rail's header prints.
pub(crate) struct RailHead<'a> {
    pub name: &'a str,
    /// `"3 spaces · 8 panes"` — see [`rail_summary`].
    pub summary: String,
    pub link: Option<FeedStatus>,
    pub read_only: bool,
}

/// The header's small line: `N spaces · M panes` on herdr, `N tabs · M panes`
/// on zellij, counting the panes of the tabs this client has read.
pub(crate) fn rail_summary(spaces: Option<usize>, tabs: &[TabEntry]) -> String {
    let panes: usize = tabs.iter().map(|tab| tab.panes.len()).sum();
    let (count, unit) = match spaces {
        Some(spaces) => (spaces, "space"),
        None => (tabs.len(), "tab"),
    };
    format!("{} \u{b7} {}", plural(count, unit), plural(panes, "pane"))
}

fn plural(count: usize, unit: &str) -> String {
    let suffix = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{suffix}")
}

/// The status chip: the attached stream's link, or `running` for a session
/// with no stream to report (the mock transcript).
fn status_chip(link: Option<FeedStatus>) -> (&'static str, BadgeVariant) {
    match link {
        None | Some(FeedStatus::Connected) => ("running", BadgeVariant::Success),
        Some(FeedStatus::Idle | FeedStatus::Reconnecting) => ("connecting", BadgeVariant::Warning),
        Some(FeedStatus::Disconnected) => ("offline", BadgeVariant::Error),
        Some(FeedStatus::Closed) => ("exited", BadgeVariant::Neutral),
    }
}

/// The full rail: header, the scrolling `tree`, and the footer's tip and one
/// `detach` button. `top` clears a status-bar inset the app bar no longer does.
pub(crate) fn docked_rail<State: 'static>(
    head: &RailHead<'_>,
    tree: AnyView<State>,
    top: f64,
    on_detach: impl Fn(&mut State) + 'static,
) -> AnyView<State> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);

    let (status, variant) = status_chip(head.link);
    let mut chips = vec![
        flexible(
            1,
            FlexView::new(
                Axis::Vertical,
                vec![
                    inflexible(
                        text(head.name.to_string())
                            .style(sized(&theme.type_scale.label_large, NAME_SIZE, colors.fg))
                            .max_lines(1)
                            .overflow(TextOverflow::Ellipsis),
                    ),
                    inflexible(text(head.summary.clone()).style(sized(
                        &theme.type_scale.label_small,
                        SUMMARY_SIZE,
                        colors.fg_dim,
                    ))),
                ],
            )
            .cross_axis(CrossAxisAlignment::Start),
        ),
        h_gap(spacing::SM),
        inflexible(badge(status, variant)),
    ];
    if head.read_only {
        chips.push(h_gap(spacing::TIGHT));
        chips.push(inflexible(badge(READ_ONLY_CHIP, BadgeVariant::Info)));
    }

    let mut tip_style = sized(&theme.type_scale.body_small, TIP_SIZE, colors.fg_dim);
    tip_style.style = FontStyle::Italic;
    let tip = if head.read_only { TIP_READ_ONLY } else { TIP };
    let footer = FlexView::new(
        Axis::Vertical,
        vec![
            inflexible(text(tip).style(tip_style)),
            inflexible(v_gap(spacing::XS)),
            inflexible(button(DETACH_LABEL, on_detach).style(ButtonStyle::Secondary)),
        ],
    )
    .cross_axis(CrossAxisAlignment::Stretch);

    let body = FlexView::new(
        Axis::Vertical,
        vec![
            inflexible(SizedBox::<State>(None, Some(top))),
            inflexible(Padding(HEAD_PAD, row(chips))),
            inflexible(rule(None, Some(HAIRLINE), &colors)),
            flexible(1, scroll_view(Padding(TREE_PAD, tree))),
            inflexible(rule(None, Some(HAIRLINE), &colors)),
            inflexible(Padding(FOOT_PAD, footer)),
        ],
    )
    .cross_axis(CrossAxisAlignment::Stretch);
    panel(RAIL_WIDTH, any(body), &colors)
}

/// What the mini rail draws: every space, the selected space's tabs, and the
/// active tab's panes.
pub(crate) struct MiniData<'a> {
    pub spaces: &'a [Space],
    pub tabs: &'a [TabEntry],
    pub active_tab: &'a str,
    pub fullscreen: bool,
    pub top: f64,
}

/// A mini-rail space press: its id.
pub(crate) type PressSpace<State> = Rc<dyn Fn(&mut State, &str)>;
/// A mini-rail tab press.
pub(crate) type PressTab<State> = Rc<dyn Fn(&mut State, &TabEntry)>;
/// A mini-rail pane press: its tab's key, and the pane.
pub(crate) type PressPane<State> = Rc<dyn Fn(&mut State, &str, &PaneEntry)>;

/// Every mini-rail press.
pub(crate) struct MiniActions<State: 'static> {
    pub on_space: PressSpace<State>,
    pub on_tab: PressTab<State>,
    pub on_pane: PressPane<State>,
    pub on_exit_immersive: Rc<dyn Fn(&mut State)>,
}

/// The 56px mini rail: 44px targets, each with its name as a tooltip, in
/// separated runs of spaces, tabs (index badges) and panes, then — immersive
/// only — the way back out.
pub(crate) fn mini_rail<State: 'static>(
    data: &MiniData<'_>,
    actions: &MiniActions<State>,
) -> AnyView<State> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    let mark = |source: IconData, label: &str, active: bool, press: Rc<dyn Fn(&mut State)>| {
        let (fill, ink) = if active {
            (colors.amber_faint, colors.amber)
        } else {
            (colors.base, colors.fg_dim)
        };
        let target = icon_button(source, label, move |state: &mut State| press(state))
            .fill(fill)
            .ink(ink)
            .icon_size(MINI_MARK)
            .box_size(Size::new(MINI_BUTTON, MINI_BUTTON))
            .radius(theme.shape.medium);
        any(tooltip(target, label.to_string()))
    };

    let mut items: Vec<AnyView<State>> = Vec::new();
    for space in data.spaces {
        let open = Rc::clone(&actions.on_space);
        let id = space.id.clone();
        let press = Rc::new(move |state: &mut State| open(state, &id));
        items.push(mark(
            SPACE_DIAMOND.into(),
            &space.name,
            space.selected,
            press,
        ));
    }
    if !data.spaces.is_empty() {
        items.push(rule(Some(MINI_SEP_WIDTH), Some(HAIRLINE), &colors));
    }
    for tab in data.tabs {
        let active = tab.key == data.active_tab;
        let variant = if active {
            BadgeVariant::Accent
        } else {
            BadgeVariant::Neutral
        };
        let open = Rc::clone(&actions.on_tab);
        let target = tab.clone();
        let cell = GestureDetector(
            SizedBox::<State>(Some(MINI_BUTTON), Some(MINI_BUTTON)).child(Align(
                Alignment::CENTER,
                badge(tab.index.to_string(), variant),
            )),
        )
        .on_tap(move |state: &mut State| open(state, &target));
        items.push(any(tooltip(cell, tab.label.clone())));
    }
    if let Some(tab) = data.tabs.iter().find(|tab| tab.key == data.active_tab) {
        items.push(rule(Some(MINI_SEP_WIDTH), Some(HAIRLINE), &colors));
        for pane in &tab.panes {
            let open = Rc::clone(&actions.on_pane);
            let (key, target) = (tab.key.clone(), pane.clone());
            let press = Rc::new(move |state: &mut State| open(state, &key, &target));
            items.push(mark(
                icons::PANE_MARK.into(),
                &pane.title,
                pane.focused,
                press,
            ));
        }
    }
    if data.fullscreen {
        let exit = Rc::clone(&actions.on_exit_immersive);
        let press = Rc::new(move |state: &mut State| exit(state));
        items.push(mark(
            icons::FULLSCREEN_EXIT.into(),
            EXIT_IMMERSIVE,
            false,
            press,
        ));
    }

    let mut column: Vec<_> = vec![inflexible(SizedBox::<State>(None, Some(data.top)))];
    for item in items {
        column.push(inflexible(v_gap(spacing::XS)));
        column.push(inflexible(item));
    }
    let body = FlexView::new(Axis::Vertical, column).cross_axis(CrossAxisAlignment::Center);
    panel(
        MINI_RAIL_WIDTH,
        any(scroll_view(Padding(
            EdgeInsets::symmetric(0.0, spacing::MD),
            body,
        ))),
        &colors,
    )
}

/// The rail's column at `width` on the surface tone, with its hairline right edge.
fn panel<State: 'static>(width: f64, body: AnyView<State>, colors: &Palette) -> AnyView<State> {
    let column = container(SizedBox::<State>(Some(width), None).child(body)).fill(colors.surface);
    any(FlexView::new(
        Axis::Horizontal,
        vec![
            inflexible(column),
            inflexible(rule(Some(HAIRLINE), None, colors)),
        ],
    )
    .cross_axis(CrossAxisAlignment::Stretch))
}

/// A hairline rule: `width`×`height`, the unset one stretched by its parent.
fn rule<State: 'static>(
    width: Option<f64>,
    height: Option<f64>,
    colors: &Palette,
) -> AnyView<State> {
    any(container(SizedBox::<State>(width, height)).fill(colors.outline))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(key: &str, panes: usize) -> TabEntry {
        TabEntry {
            index: 1,
            tab_id: 1,
            key: key.to_string(),
            label: key.to_string(),
            pane_title: String::new(),
            pane_name: String::new(),
            pane_path: String::new(),
            panes: (0..panes)
                .map(|id| PaneEntry {
                    id: id as u32,
                    title: String::new(),
                    cwd: String::new(),
                    focused: false,
                    floating: false,
                    exited: false,
                    is_plugin: false,
                    is_fullscreen: false,
                    x: 0,
                    y: 0,
                    rows: 1,
                    cols: 1,
                })
                .collect(),
            active: false,
            has_bell: false,
        }
    }

    #[test]
    fn the_summary_counts_spaces_on_herdr_and_tabs_on_zellij() {
        let tabs = vec![tab("a", 2), tab("b", 1)];
        assert_eq!(rail_summary(Some(3), &tabs), "3 spaces \u{b7} 3 panes");
        assert_eq!(rail_summary(None, &tabs), "2 tabs \u{b7} 3 panes");
        assert_eq!(rail_summary(Some(1), &tabs[1..]), "1 space \u{b7} 1 pane");
    }

    #[test]
    fn the_status_chip_reads_the_link_and_a_streamless_session_as_running() {
        assert_eq!(status_chip(None).0, "running");
        assert_eq!(status_chip(Some(FeedStatus::Connected)).0, "running");
        assert_eq!(
            status_chip(Some(FeedStatus::Reconnecting)).1,
            BadgeVariant::Warning
        );
        assert_eq!(
            status_chip(Some(FeedStatus::Disconnected)).1,
            BadgeVariant::Error
        );
    }
}
