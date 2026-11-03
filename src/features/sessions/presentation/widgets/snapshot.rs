//! The Sessions screen as a picture: [`sessions_snapshot`] re-renders a
//! server's last [`PageSnapshot`] with the very widgets the live page is built
//! from (the bar, the `.conn-row` strip, [`session_card`], the create row, the
//! Tablet master-detail), wrapped in [`Inert`] so nothing in it reacts. The
//! server switcher's peer cards use it, composed by the app-level shell, so a
//! peer reads like the live card beside it — recents, without a screenshot API.
//!
//! Page-agnostic: it needs no `SessionsState` value, no controller, no poll and
//! no loading rain. The widgets are typed over the page's state, but a view
//! builds without a state value; only events need one, and [`Inert`] never
//! forwards any, so a tap on the picture reaches the card, never a session.
//!
//! The picture assumes the loaded phase (a snapshot is only filed by a
//! successful list), the heartbeat at rest on the handshake latency, and no
//! selection on the Tablet class.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, EventCtx, EventResult, InputEvent, LayoutCtx,
    PaintCtx, PaintScene, SemanticsCtx, Size, Widget, build_child, rebuild_child, teardown_child,
};
use frust::{
    Align, Alignment, AnyView, Axis, CrossAxisAlignment, EdgeInsets, Either, FlexView, Padding,
    SizedBox, View, any, flexible, icons, inflexible, safe_area, scroll_view,
};
use frust_glyph::{TitleDirection, TitlePosition, app_bar};

use crate::connection::ServerInfo;
use crate::features::servers::domain::entities::PageSnapshot;
use crate::responsive::Responsive;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;
use crate::ui::topbar::{bar_icon, brand_leading};

use super::super::heartbeat::HeartbeatVisual;
use super::super::pages::SessionsState;
use super::{
    CardMode, LoadPhase, detail_pane, gapped_column, host_of, master_detail, new_session_row,
    page_tone, server_strip, session_card, tablet_list, tablet_list_pieces,
};

/// The bar title of a snapshot with no handshake facts.
const FALLBACK_TITLE: &str = "muxrd";
/// The bar's Settings affordance name (the live bar's).
const SETTINGS_LABEL: &str = "Settings";
/// The Compact list's padding and gap — the live page's.
const LIST_PAD_X: f64 = spacing::XL;
const LIST_PAD_Y: f64 = spacing::LG;
const LIST_GAP: f64 = spacing::BASE;
/// The live page's wide-window clamp.
const CLAMP_WIDTH: f64 = 640.0;

/// How many snapshots this process has built: the observable behind "a closed
/// stage constructs no peer faces" (see `controllers::snapshots_built_for_tests`).
static BUILT: AtomicUsize = AtomicUsize::new(0);

/// The running count of [`sessions_snapshot`] calls.
pub(crate) fn built() -> usize {
    BUILT.load(Ordering::Relaxed)
}

/// `page` as the inert Sessions screen under the shell's bar, laid out for
/// `rule`'s window class. A view over any outer state: nothing in it touches
/// the host's state.
pub fn sessions_snapshot<S: 'static>(page: &PageSnapshot, rule: Responsive) -> impl View<S> {
    BUILT.fetch_add(1, Ordering::Relaxed);
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    let title = page
        .info
        .as_ref()
        .map_or(FALLBACK_TITLE.to_string(), |info| {
            host_of(&info.endpoint).to_string()
        });
    let bar = app_bar::<SessionsState>(title)
        .title_direction(TitleDirection::Forward)
        .title_position(TitlePosition::Center)
        .leading(brand_leading())
        .actions(vec![bar_icon(
            icons::SETTINGS,
            SETTINGS_LABEL,
            |_: &mut SessionsState| {},
        )]);
    let body = body(page, rule, &colors);
    let screen = FlexView::new(Axis::Vertical, vec![inflexible(bar), flexible(1, body)])
        .cross_axis(CrossAxisAlignment::Stretch);
    Inert {
        child: screen,
        state: PhantomData,
    }
}

/// The page's body: the strip over the list (Compact), or the master-detail
/// row with nothing selected (Tablet).
fn body<'a>(
    page: &'a PageSnapshot,
    rule: Responsive,
    colors: &'a Palette,
) -> impl View<SessionsState> + use<'a> {
    let strip = server_strip(
        page.info.as_ref(),
        at_rest(page.info.as_ref()),
        page.read_only,
        colors,
    );
    if rule.is_tablet() {
        let full = rule.is_full();
        let pieces = tablet_list_pieces(
            &page.sessions,
            LoadPhase::Loaded,
            page.read_only,
            None,
            full,
            colors,
        );
        let list = scroll_view(tablet_list(pieces));
        let footer = Some(new_session_row(colors, page.read_only));
        let detail = detail_pane(
            &page.sessions,
            &[],
            None,
            LoadPhase::Loaded,
            page.read_only,
            full,
            colors,
        );
        let body = master_detail(strip, list, footer, detail, full, colors);
        return Either::Left(page_tone(body, colors));
    }
    let mut pieces: Vec<AnyView<SessionsState>> = page
        .sessions
        .iter()
        .map(|session| any(session_card(session, colors, CardMode::Compact)))
        .collect();
    pieces.push(any(new_session_row(colors, page.read_only)));
    let list = Padding(
        EdgeInsets::symmetric(LIST_PAD_X, LIST_PAD_Y),
        gapped_column(pieces, LIST_GAP),
    );
    let band = safe_area(scroll_view(list)).top(false);
    let band = if rule.wide_clamp {
        Either::Left(Align(
            Alignment::CENTER,
            SizedBox::<SessionsState>(Some(CLAMP_WIDTH), None).child(band),
        ))
    } else {
        Either::Right(band)
    };
    let screen = FlexView::new(Axis::Vertical, vec![inflexible(strip), flexible(1, band)])
        .cross_axis(CrossAxisAlignment::Stretch);
    Either::Right(page_tone(screen, colors))
}

/// The heartbeat as a settled strip draws it: no ring, no roll, the handshake
/// latency.
fn at_rest(info: Option<&ServerInfo>) -> HeartbeatVisual {
    HeartbeatVisual {
        ring: None,
        latency_ms: info.map_or(0, |info| info.latency_ms),
        rolling: false,
    }
}

/// Lays out, paints and describes `child`, and answers no input: the picture
/// of a screen that is not the live one. `state` ties the view to the host
/// state it is mounted under; the child's own state type never meets a value.
struct Inert<V, S> {
    child: V,
    state: PhantomData<fn(&mut S)>,
}

struct InertWidget {
    child: ChildPod,
}

impl<V: View<SessionsState>, S: 'static> View<S> for Inert<V, S> {
    type Element = InertWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> InertWidget {
        InertWidget {
            child: build_child(&self.child, ctx),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut InertWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        rebuild_child(&prev.child, &self.child, &mut element.child, ctx)
    }

    fn teardown(&self, element: &mut InertWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for InertWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        self.child.layout_child(ctx, bc)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, _ctx: &mut EventCtx, _event: &InputEvent) -> EventResult {
        EventResult::Ignored
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::sessions::domain::entities::{
        LayoutSummary, Session, SessionBackend, SessionStatus, ThirdMetric,
    };
    use frust_core::{
        InputEvent, PointerButton, PointerEvent, PointerPhase, RenderRoot, WindowInsets,
        WindowMetrics,
    };
    use kurbo::{Point, Size};

    fn session(name: &str) -> Session {
        Session {
            id: name.to_string(),
            name: name.to_string(),
            status: SessionStatus::Running,
            backend: SessionBackend::Zellij,
            age_secs: 0,
            has_bell: false,
            is_current: false,
            layout: LayoutSummary {
                tabs: 1,
                panes: 1,
                third: ThirdMetric::Clients(0),
                preview: Vec::new(),
                tabs_detail: Vec::new(),
                space: None,
            },
            agents: None,
        }
    }

    fn page(names: &[&str]) -> PageSnapshot {
        PageSnapshot {
            sessions: names.iter().map(|name| session(name)).collect(),
            info: None,
            read_only: false,
        }
    }

    fn rule_at(width: f64, height: f64) -> Responsive {
        Responsive::of(Some(WindowMetrics::new(
            Size::new(width, height),
            1.0,
            WindowInsets::default(),
        )))
    }

    /// The snapshot mounted under a host state of `()`, laid out at `size`.
    fn mounted(page: &PageSnapshot, rule: Responsive, size: Size) -> RenderRoot<(), AnyView<()>> {
        let mut root: RenderRoot<(), AnyView<()>> = RenderRoot::new();
        let mut logic = |_: &mut ()| AnyView::new(sessions_snapshot::<()>(page, rule));
        root.rebuild(&mut logic, &mut ());
        let mut tcx = frust_text::TextContext::new();
        let tcx_any: &mut dyn std::any::Any = &mut tcx;
        root.layout_with_text(size, tcx_any);
        root
    }

    fn values(root: &RenderRoot<(), AnyView<()>>) -> Vec<String> {
        root.semantics()
            .nodes
            .iter()
            .filter_map(|(_, node)| node.value().map(str::to_string))
            .collect()
    }

    #[test]
    fn a_compact_snapshot_prints_the_sessions_names_and_the_create_row() {
        let root = mounted(
            &page(&["alpha", "beta"]),
            rule_at(390.0, 844.0),
            Size::new(390.0, 844.0),
        );
        let values = values(&root);
        for wanted in ["alpha", "beta", "new session"] {
            assert!(values.iter().any(|v| v == wanted), "{wanted}: {values:?}");
        }
    }

    #[test]
    fn a_tablet_snapshot_prints_the_list_column_beside_the_unselected_detail() {
        let root = mounted(
            &page(&["alpha"]),
            rule_at(1024.0, 720.0),
            Size::new(1024.0, 720.0),
        );
        let values = values(&root);
        assert!(values.iter().any(|v| v == "alpha"), "{values:?}");
    }

    #[test]
    fn a_tap_on_the_snapshot_is_ignored_wherever_it_lands() {
        let size = Size::new(390.0, 844.0);
        let mut root = mounted(&page(&["alpha"]), rule_at(390.0, 844.0), size);
        let mut tap = |x: f64, y: f64| {
            let mut redraw = false;
            for phase in [PointerPhase::Down, PointerPhase::Up] {
                let event = InputEvent::Pointer(PointerEvent {
                    phase,
                    position: Point::new(x, y),
                    button: PointerButton::Primary,
                });
                redraw |= root.event(&mut (), &event).needs_redraw;
            }
            redraw
        };
        // Across the bar, the first card and the create row's bands.
        for y in (20..size.height as i32).step_by(40) {
            assert!(!tap(200.0, f64::from(y)), "a tap at y={y} reached a widget");
            assert!(!tap(370.0, f64::from(y)), "a tap at y={y} reached a widget");
        }
    }
}
