//! The Tablet's docked rail and its 56px mini rail, transcribed from the `tablet.html` mock.
//! Composition only: the tree arrives built and every press leaves through a callback.
//! `frust::button` takes a text label only, so the footer's `detach` carries no leading mark.

use std::rc::Rc;

use frust::authoring::text::{FontStyle, TextOverflow};
use frust::authoring::{
    Action, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, EventCtx, EventResult, InputEvent,
    LayoutCtx, PaintCtx, PaintScene, Point, Role, SemanticsCtx, Size, View, Widget, build_child,
    rebuild_child, route_event_single, teardown_child,
};
use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, FlexView,
    GestureDetector, IconData, Padding, SizedBox, any, button, column, container, flexible, icons,
    inflexible, row, scroll_view, text,
};
use frust_glyph::{BadgeVariant, badge, tooltip};

use crate::features::terminal::domain::entities::{PaneEntry, Space, TabEntry};
use crate::features::terminal::domain::feed::LinkStatus;
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;
use crate::ui::topbar::icon_button;

use super::agents_section::tree_split;
use super::side_sheet_tree::SPACE_DIAMOND;
use super::util::{centered_row, h_gap, read_only_badge, sized, v_gap};

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
const EXIT_IMMERSIVE: &str = "Exit immersive";

pub(crate) struct RailHead<'a> {
    pub name: &'a str,
    pub summary: String,
    pub link: Option<LinkStatus>,
    pub read_only: bool,
    pub needs_you: bool,
}

/// `N spaces · M panes` on herdr, `N tabs · M panes` on zellij, over the tabs this client read.
pub(crate) fn rail_summary(spaces: Option<usize>, tabs: &[TabEntry], agents: usize) -> String {
    let panes: usize = tabs.iter().map(|tab| tab.panes.len()).sum();
    let (count, unit) = match spaces {
        Some(spaces) => (spaces, "space"),
        None => (tabs.len(), "tab"),
    };
    let summary = format!("{} \u{b7} {}", plural(count, unit), plural(panes, "pane"));
    if agents == 0 {
        return summary;
    }
    format!("{summary} \u{b7} {}", plural(agents, "agent"))
}

fn plural(count: usize, unit: &str) -> String {
    let suffix = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{suffix}")
}

/// `running` with no stream to report, `needs you` while an agent waits; a link problem still wins.
fn status_chip(link: Option<LinkStatus>, needs_you: bool) -> (&'static str, BadgeVariant) {
    match link {
        None | Some(LinkStatus::Connected) if needs_you => ("needs you", BadgeVariant::Warning),
        None | Some(LinkStatus::Connected) => ("running", BadgeVariant::Success),
        Some(LinkStatus::Idle | LinkStatus::Reconnecting) => ("connecting", BadgeVariant::Warning),
        Some(LinkStatus::Disconnected) => ("offline", BadgeVariant::Error),
        Some(LinkStatus::Closed) => ("exited", BadgeVariant::Neutral),
    }
}

/// `top` clears the status-bar inset, which the app bar does not.
pub(crate) fn docked_rail<
    State: 'static,
    Tree: View<State>,
    Agents: View<State>,
    Detach: Fn(&mut State) + 'static,
>(
    head: &RailHead<'_>,
    tree: Tree,
    agents: Option<Agents>,
    top: f64,
    on_detach: Detach,
) -> impl View<State> + use<State, Tree, Agents, Detach> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);

    let (status, variant) = status_chip(head.link, head.needs_you);
    let mut chips = vec![
        flexible(
            1,
            column()
                .child(
                    text(head.name.to_string())
                        .style(sized(&theme.type_scale.label_large, NAME_SIZE, colors.fg))
                        .max_lines(1)
                        .overflow(TextOverflow::Ellipsis),
                )
                .child(text(head.summary.clone()).style(sized(
                    &theme.type_scale.label_small,
                    SUMMARY_SIZE,
                    colors.fg_dim,
                )))
                .cross_axis(CrossAxisAlignment::Start),
        ),
        h_gap(spacing::SM),
        inflexible(badge(status, variant)),
    ];
    if head.read_only {
        chips.push(h_gap(spacing::TIGHT));
        chips.push(inflexible(read_only_badge()));
    }

    let mut tip_style = sized(&theme.type_scale.body_small, TIP_SIZE, colors.fg_dim);
    tip_style.style = FontStyle::Italic;
    let tip = if head.read_only { TIP_READ_ONLY } else { TIP };
    let footer = column()
        .child(text(tip).style(tip_style))
        .child(v_gap(spacing::XS))
        .child(button(DETACH_LABEL, on_detach).style(ButtonStyle::Secondary))
        .cross_axis(CrossAxisAlignment::Stretch);

    let body = column()
        .child(SizedBox::<State>(None, Some(top)))
        .child(Padding(HEAD_PAD, centered_row(chips)))
        .child(rule(None, Some(HAIRLINE), &colors))
        .flex(1, tree_split(Padding(TREE_PAD, tree), agents))
        .child(rule(None, Some(HAIRLINE), &colors))
        .child(Padding(FOOT_PAD, footer))
        .cross_axis(CrossAxisAlignment::Stretch);
    panel(RAIL_WIDTH, body, &colors)
}

pub(crate) struct MiniData<'a> {
    pub spaces: &'a [Space],
    pub tabs: &'a [TabEntry],
    pub active_tab: &'a str,
    pub fullscreen: bool,
    pub top: f64,
}

pub(crate) type PressSpace<State> = Rc<dyn Fn(&mut State, &str)>;
pub(crate) type PressTab<State> = Rc<dyn Fn(&mut State, &TabEntry)>;
pub(crate) type PressPane<State> = Rc<dyn Fn(&mut State, &str, &PaneEntry)>;

pub(crate) struct MiniActions<State: 'static> {
    pub on_space: PressSpace<State>,
    pub on_tab: PressTab<State>,
    pub on_pane: PressPane<State>,
    pub on_exit_immersive: Rc<dyn Fn(&mut State)>,
}

pub(crate) fn mini_rail<State: 'static>(
    data: &MiniData<'_>,
    actions: &MiniActions<State>,
) -> impl View<State> + use<State> {
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
        any(tooltip(target, label.to_string())) // erasure: keep accumulator into Vec<AnyView>
    };

    let mut items: Vec<AnyView<State>> = Vec::new(); // erasure: keep accumulator into Vec<AnyView>
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
        items.push(any(rule(Some(MINI_SEP_WIDTH), Some(HAIRLINE), &colors)));
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
        let cell = labelled_button(tab.label.clone(), cell);
        items.push(any(tooltip(cell, tab.label.clone())));
    }
    if let Some(tab) = data.tabs.iter().find(|tab| tab.key == data.active_tab) {
        items.push(any(rule(Some(MINI_SEP_WIDTH), Some(HAIRLINE), &colors)));
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
        scroll_view(Padding(EdgeInsets::symmetric(0.0, spacing::MD), body)),
        &colors,
    )
}

fn panel<State: 'static, Body: View<State>>(
    width: f64,
    body: Body,
    colors: &Palette,
) -> impl View<State> + use<State, Body> {
    let column = container(SizedBox::<State>(Some(width), None).child(body)).fill(colors.surface);
    row()
        .child(column)
        .child(rule(Some(HAIRLINE), None, colors))
        .cross_axis(CrossAxisAlignment::Stretch)
}

fn rule<State: 'static>(
    width: Option<f64>,
    height: Option<f64>,
    colors: &Palette,
) -> impl View<State> {
    container(SizedBox::<State>(width, height)).fill(colors.outline)
}

/// A bare [`GestureDetector`] is invisible to a screen reader; the inner badge shows only an index.
pub(crate) fn labelled_button<V>(label: String, child: V) -> LabelledButtonView<V> {
    LabelledButtonView { label, child }
}

pub(crate) struct LabelledButtonView<V> {
    label: String,
    child: V,
}

pub(crate) struct LabelledButtonWidget {
    label: String,
    child: ChildPod,
}

impl<State: 'static, V: View<State>> View<State> for LabelledButtonView<V> {
    type Element = LabelledButtonWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> LabelledButtonWidget {
        LabelledButtonWidget {
            label: self.label.clone(),
            child: build_child(&self.child, ctx),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut LabelledButtonWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.label != self.label {
            element.label = self.label.clone();
            // Semantics are recomputed off the paint pass.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut LabelledButtonWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for LabelledButtonWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_node(Role::Button, |node| {
            node.set_label(self.label.as_str());
            node.add_action(Action::Click);
        });
    }
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
                    agent: None,
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
            floating_panes_visible: false,
            fullscreen_active: false,
        }
    }

    #[test]
    fn the_summary_counts_spaces_on_herdr_and_tabs_on_zellij() {
        let tabs = vec![tab("a", 2), tab("b", 1)];
        assert_eq!(rail_summary(Some(3), &tabs, 0), "3 spaces \u{b7} 3 panes");
        assert_eq!(rail_summary(None, &tabs, 0), "2 tabs \u{b7} 3 panes");
        assert_eq!(
            rail_summary(Some(1), &tabs[1..], 0),
            "1 space \u{b7} 1 pane"
        );
    }

    #[test]
    fn the_summary_counts_agents_only_when_there_are_any() {
        let tabs = vec![tab("a", 2), tab("b", 1)];
        assert_eq!(
            rail_summary(None, &tabs, 4),
            "2 tabs \u{b7} 3 panes \u{b7} 4 agents"
        );
        assert_eq!(
            rail_summary(Some(1), &tabs, 1),
            "1 space \u{b7} 3 panes \u{b7} 1 agent"
        );
    }

    #[test]
    fn a_blocked_agent_turns_running_into_needs_you_but_never_hides_a_link_problem() {
        assert_eq!(
            status_chip(None, true),
            ("needs you", BadgeVariant::Warning)
        );
        assert_eq!(
            status_chip(Some(LinkStatus::Connected), true).0,
            "needs you"
        );
        assert_eq!(
            status_chip(Some(LinkStatus::Disconnected), true).0,
            "offline"
        );
    }

    #[test]
    fn the_status_chip_reads_the_link_and_a_streamless_session_as_running() {
        assert_eq!(status_chip(None, false).0, "running");
        assert_eq!(status_chip(Some(LinkStatus::Connected), false).0, "running");
        assert_eq!(
            status_chip(Some(LinkStatus::Reconnecting), false).1,
            BadgeVariant::Warning
        );
        assert_eq!(
            status_chip(Some(LinkStatus::Disconnected), false).1,
            BadgeVariant::Error
        );
    }
}
