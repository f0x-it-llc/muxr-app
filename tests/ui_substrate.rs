//! Headless coverage for the shared presentation substrate (dialogs, sheets, side sheets, toasts) over a bare navigator.
//! Dialogs and sheets are pushed pages: push, [`support::settle`] past the input-blocking transition, then tap or type.
//! Pushed pages have no semantics, so taps are addressed by painted geometry; toast tests use semantics.

use frust::{
    AnyView, Component, NavigatorController, NavigatorView, Theme, any, button, navigator, row,
    spawn_local, text,
};
use frust_core::{NamedKey, PointerPhase, RenderRoot};
use frust_text::TextContext;
use kurbo::{Point, Size};
use peniko::Color;

use muxr::agents::AgentSummary;
use muxr::failure::AppFailure;
use muxr::ui::agent_marks::{count_pills, kind_tile, kind_tiles};
use muxr::ui::dialogs::{confirm_dialog, name_input_dialog};
use muxr::ui::sheet::{SCRIM_ALPHA, SheetHandle, show_sheet};
use muxr::ui::side_sheet::show_side_sheet;
use muxr::ui::toasts::{app_toasts, toast_layer};
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, alpha_of, by_width, center, char_key, fills_at, frame, frame_at, largest,
    ms, named_key, node_center_by_label, node_labels, pointer, pump_frames, serial, settle, setup,
    smallest, tap,
};

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The panel gutter dialogs and sheets share (20px): a full-bleed child is panel width minus twice this.
const PANEL_PADDING: f64 = 20.0;

const SCRIM_POINT: Point = Point::new(4.0, 4.0);

#[derive(Default)]
struct State {
    confirmed: u32,
    names: Vec<String>,
}

fn home() -> AnyView<State> {
    any(text("muxr"))
}

type NavLogic = Box<dyn FnMut(&mut State) -> NavigatorView<State>>;
type ToastLogic = Box<dyn FnMut(&mut State) -> AnyView<State>>;

struct Harness {
    controller: NavigatorController<State>,
    root: RenderRoot<State, NavigatorView<State>>,
    logic: NavLogic,
    state: State,
    tcx: TextContext,
}

impl Harness {
    fn mount() -> Self {
        Self::themed(None)
    }

    fn themed(theme: Option<Theme>) -> Self {
        let controller: NavigatorController<State> = NavigatorController::new();
        let logic_controller = controller.clone();
        let mut root: RenderRoot<State, NavigatorView<State>> = RenderRoot::new();
        if let Some(theme) = theme {
            root.set_theme(Box::new(theme));
        }
        let mut harness = Harness {
            controller,
            root,
            logic: Box::new(move |_state: &mut State| navigator(&logic_controller, home)),
            state: State::default(),
            tcx: TextContext::new(),
        };
        harness.settle();
        harness
    }

    fn frame_at(&mut self, at_ms: f64) -> RecScene {
        frame_at(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            ms(at_ms),
        )
    }

    fn settle(&mut self) -> RecScene {
        settle(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
        )
    }

    /// Settle, then a harmless `Move`: the navigator delivers a pop's result at the first event pass after the rebuild.
    fn flush(&mut self) {
        self.settle();
        self.settle();
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, SCRIM_POINT));
    }

    fn tap(&mut self, point: Point) {
        tap(&mut self.root, &mut self.state, point);
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.root.event(&mut self.state, &char_key(&c.to_string()));
        }
    }

    fn press(&mut self, key: NamedKey) {
        self.root.event(&mut self.state, &named_key(key));
    }

    fn depth(&self) -> usize {
        self.controller.depth()
    }
}

fn panel(scene: &RecScene) -> (Point, Size) {
    let mut candidates = scene.rounded.clone();
    candidates.extend(scene.paths.iter().copied());
    largest(&candidates).expect("an overlay always paints its own panel")
}

/// An overlay's full-content-width child, addressed by width because "the smallest rect" would find the drag handle.
fn content_child(scene: &RecScene) -> (Point, Size) {
    let content_w = panel(scene).1.width - 2.0 * PANEL_PADDING;
    by_width(&scene.rounded, content_w)
        .unwrap_or_else(|| panic!("no full-width child in {:?}", scene.rounded))
}

fn open_confirm(h: &mut Harness) -> RecScene {
    confirm_dialog(
        &h.controller,
        "Kill session?",
        "Any attached client is disconnected immediately.",
        "Kill",
        |state: &mut State| state.confirmed += 1,
    );
    h.settle()
}

#[test]
fn confirm_dialog_fires_on_confirm_and_closes() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let scene = open_confirm(&mut h);
    assert_eq!(h.depth(), 2, "the dialog is pushed as its own page");

    let confirm = smallest(&scene.strokes).expect("the danger action strokes its border");
    assert!(
        confirm.1.width < panel(&scene).1.width,
        "the action sits inside the panel: {confirm:?}"
    );
    h.tap(center(confirm));
    h.flush();

    assert_eq!(h.state.confirmed, 1, "the confirm action ran on_confirm");
    assert_eq!(h.depth(), 1, "…and the dialog closed behind it");
}

#[test]
fn confirm_dialog_scrim_tap_closes_without_running_the_action() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    open_confirm(&mut h);
    h.tap(SCRIM_POINT);
    h.flush();

    assert_eq!(
        h.state.confirmed, 0,
        "a dismissal pops with an empty result"
    );
    assert_eq!(h.depth(), 1, "the dialog closed");
}

fn open_name_input(h: &mut Harness) -> RecScene {
    name_input_dialog(
        &h.controller,
        "New session",
        "",
        |state: &mut State, name: String| state.names.push(name),
    );
    h.settle()
}

#[test]
fn name_input_dialog_submits_the_typed_text() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let scene = open_name_input(&mut h);
    h.tap(center(content_child(&scene)));
    h.type_text("dev");
    h.press(NamedKey::Enter);
    h.flush();

    assert_eq!(h.state.names, vec!["dev".to_string()]);
    assert_eq!(h.depth(), 1, "a submit closes the dialog");
}

#[test]
fn name_input_dialog_submits_from_its_button_too() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let scene = open_name_input(&mut h);
    let filled_while_empty = scene.rounded.len();
    h.tap(center(content_child(&scene)));
    h.type_text("dev");

    let scene = h.settle();
    assert!(
        scene.rounded.len() > filled_while_empty,
        "the submit button only paints a filled chrome once it is enabled: \
         {} vs {filled_while_empty}",
        scene.rounded.len()
    );
    let submit = smallest(&scene.rounded).expect("the enabled submit paints a filled rect");
    h.tap(center(submit));
    h.flush();

    assert_eq!(h.state.names, vec!["dev".to_string()]);
    assert_eq!(h.depth(), 1);
}

#[test]
fn name_input_dialog_blocks_an_empty_submit() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let scene = open_name_input(&mut h);
    let field = center(content_child(&scene));

    h.tap(field);
    h.press(NamedKey::Enter);
    h.flush();
    assert!(h.state.names.is_empty(), "an empty name never submits");
    assert_eq!(h.depth(), 2, "…and the dialog stays up");

    h.tap(field);
    h.type_text("   ");
    let scene = h.settle();
    assert_eq!(
        scene.rounded.len(),
        3,
        "panel + field frame + field fill, and no enabled submit: {:?}",
        scene.rounded
    );
    h.press(NamedKey::Enter);
    h.flush();
    assert!(h.state.names.is_empty(), "a blank name is not a name");
    assert_eq!(h.depth(), 2);
}

fn open_sheet(h: &mut Harness) -> RecScene {
    show_sheet(&h.controller, |_sheet| {
        any(button("Split pane right", |state: &mut State| {
            state.confirmed += 1
        }))
    });
    h.settle()
}

fn open_sheet_with_handle(h: &mut Harness) -> SheetHandle {
    let captured: Rc<RefCell<Option<SheetHandle>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&captured);
    show_sheet(&h.controller, move |sheet| {
        *sink.borrow_mut() = Some(sheet.clone());
        any(text("panes & tabs"))
    });
    h.settle();
    let handle = captured.borrow().clone();
    handle.expect("the body builder ran, so it was handed a handle")
}

fn handle_point(scene: &RecScene) -> Point {
    let (origin, size) = panel(scene);
    Point::new(origin.x + size.width / 2.0, origin.y + 10.0)
}

/// How far below its resting place the panel paints: the sheet slides by one translate while the scrim stays put.
fn panel_offset(scene: &RecScene) -> f64 {
    scene
        .transforms
        .iter()
        .map(|transform| transform.as_coeffs()[5])
        .fold(
            0.0,
            |widest, y| if y.abs() > widest.abs() { y } else { widest },
        )
}

#[test]
fn sheet_opens_renders_its_body_and_dismisses_on_a_scrim_tap() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let scene = open_sheet(&mut h);
    assert_eq!(h.depth(), 2, "the sheet is pushed as its own page");

    let (panel_origin, panel_size) = panel(&scene);
    let body = content_child(&scene);
    assert!(
        body.0.y > panel_origin.y && body.1.width < panel_size.width,
        "the body renders inside the panel, inset by its gutter: body {body:?}, \
         panel {panel_origin:?}/{panel_size:?}"
    );
    assert!(
        (panel_size.width - support::W).abs() < EPSILON && panel_origin.x.abs() < EPSILON,
        "the panel spans the whole width: {panel_origin:?}/{panel_size:?}"
    );
    assert!(
        (panel_origin.y + panel_size.height - support::H).abs() < EPSILON,
        "…anchored to the bottom edge: {panel_origin:?}/{panel_size:?}"
    );

    h.tap(SCRIM_POINT);
    h.flush();

    assert_eq!(h.depth(), 1, "a scrim tap dismisses the sheet");
    assert!(
        h.settle().rounded.is_empty(),
        "…and the sheet's chrome is gone with it"
    );
}

#[test]
fn a_tap_on_the_sheet_panel_never_dismisses_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let scene = open_sheet(&mut h);

    h.tap(center(content_child(&scene)));
    h.flush();
    assert_eq!(h.state.confirmed, 1, "the body's button is live");
    assert_eq!(h.depth(), 2, "pressing the body doesn't close the sheet");

    // A press on the panel's own background must not fall through to the scrim behind it.
    let scene = h.settle();
    h.tap(handle_point(&scene));
    h.flush();
    assert_eq!(h.state.confirmed, 1, "…and it hit no control");
    assert_eq!(h.depth(), 2, "the sheet stays up");
}

#[test]
fn a_drag_down_from_the_handle_dismisses_the_sheet() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let scene = open_sheet(&mut h);
    let (_, panel_size) = panel(&scene);
    let start = handle_point(&scene);
    // Past the half-panel-height dismiss threshold; the release decides, so Down, Move, Up.
    let end = Point::new(start.x, start.y + panel_size.height * 0.75);

    h.root
        .event(&mut h.state, &pointer(PointerPhase::Down, start));
    h.root
        .event(&mut h.state, &pointer(PointerPhase::Move, end));
    h.root.event(&mut h.state, &pointer(PointerPhase::Up, end));
    h.flush();

    assert_eq!(h.depth(), 1, "a past-threshold handle drag dismisses");
    assert_eq!(
        h.state.confirmed, 0,
        "…without ever pressing the body it dragged over"
    );
}

#[test]
fn a_short_drag_from_the_handle_leaves_the_sheet_up() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let scene = open_sheet(&mut h);
    let start = handle_point(&scene);
    let end = Point::new(start.x, start.y + 8.0);

    h.root
        .event(&mut h.state, &pointer(PointerPhase::Down, start));
    h.root
        .event(&mut h.state, &pointer(PointerPhase::Move, end));
    h.root.event(&mut h.state, &pointer(PointerPhase::Up, end));
    h.flush();

    assert_eq!(
        h.depth(),
        2,
        "a short drag settles back rather than dismissing"
    );
}

#[test]
fn escape_dismisses_the_sheet() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let scene = open_sheet(&mut h);
    h.tap(handle_point(&scene));
    h.press(NamedKey::Escape);
    h.flush();

    assert_eq!(h.depth(), 1, "Escape dismisses the sheet");
}

#[test]
fn a_duplicate_close_pops_the_sheet_once_and_leaves_the_page_under_it_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.controller.push(|| any(text("under the sheet")));
    h.settle();
    assert_eq!(h.depth(), 2);

    let sheet = open_sheet_with_handle(&mut h);
    assert_eq!(h.depth(), 3, "the sheet is pushed over it");

    // A pop lands a rebuild after it is enqueued, so a second closer can fire for one whole frame.
    assert!(sheet.close(), "the first close is the one that closes it");
    assert!(!sheet.close(), "…and the second is a no-op");
    h.flush();

    assert_eq!(h.depth(), 2, "exactly one page popped — the sheet's own");
}

#[test]
fn a_close_after_the_sheet_is_already_gone_pops_nothing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.controller.push(|| any(text("under the sheet")));
    h.settle();
    let sheet = open_sheet_with_handle(&mut h);
    assert_eq!(h.depth(), 3);

    // A staged exit never reaches the app's handle; the completion latch stops a close in flight from landing on the page underneath.
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 2, "the scrim tap took the sheet");

    assert!(!sheet.close(), "the handle was latched when the page went");
    h.flush();
    assert_eq!(h.depth(), 2, "…so nothing else popped");
}

/// Inside the modal enter duration (220ms) and past its start, so a partial frame is genuinely partial.
const MID_STAGING_MS: f64 = 60.0;

const REDUCE_MOTION_MS: f64 = 120.0;

/// The sheet's scrim: every plain fill covering the whole surface, painted there regardless of staging.
fn scrim(scene: &RecScene) -> Vec<Color> {
    fills_at(scene, Point::ZERO, support::surface())
}

fn scrim_alpha(scene: &RecScene) -> f32 {
    let found = scrim(scene);
    assert_eq!(
        found.len(),
        1,
        "the sheet paints exactly one full-bleed scrim; all fills were {:?}",
        scene
            .fills
            .iter()
            .map(|(o, s, c)| (o.x, o.y, s.width, s.height, alpha_of(*c)))
            .collect::<Vec<_>>()
    );
    alpha_of(found[0])
}

fn mid_entrance(theme: Theme) -> (Harness, RecScene) {
    let mut h = Harness::themed(Some(theme));
    show_sheet(&h.controller, |_sheet| {
        any(button("Split pane right", |state: &mut State| {
            state.confirmed += 1
        }))
    });
    h.frame_at(0.0);
    let mid = h.frame_at(MID_STAGING_MS);
    (h, mid)
}

#[test]
fn the_panel_rises_while_the_scrim_fades_in_place() {
    let _g = serial();
    let _ambient = setup();

    let (mut h, mid) = mid_entrance(frust_glyph::baseline());
    let mid_offset = panel_offset(&mid);
    let mid_scrim = scrim_alpha(&mid);

    let settled = h.settle();

    assert!(
        mid_offset > 0.5,
        "mid-entrance the panel is still below its resting place ({mid_offset})"
    );
    assert!(
        panel_offset(&settled).abs() < EPSILON,
        "…and rests flush once it arrives ({})",
        panel_offset(&settled)
    );
    assert!(
        mid_scrim > 0.0 && mid_scrim < SCRIM_ALPHA - 1e-6,
        "mid-entrance the scrim is partly faded in ({mid_scrim} of \
         {SCRIM_ALPHA})"
    );
    assert!(
        (scrim_alpha(&settled) - SCRIM_ALPHA).abs() < 1e-6,
        "…and settles at the modal's own alpha"
    );
    assert_eq!(h.depth(), 2, "the sheet is up throughout");
}

#[test]
fn the_scrims_rect_never_moves() {
    let _g = serial();
    let _ambient = setup();

    // Across the entrance the scrim's rect is identical and only alpha differs.
    let mut h = Harness::themed(Some(frust_glyph::baseline()));
    show_sheet(&h.controller, |_sheet| any(text("pair a host")));
    h.frame_at(0.0);

    let mut previous = 0.0_f32;
    for at in [10.0, 40.0, 90.0, 150.0, 210.0] {
        let alpha = scrim_alpha(&h.frame_at(at));
        assert!(
            alpha >= previous,
            "the entrance fade never runs backwards ({alpha} after {previous})"
        );
        previous = alpha;
    }
}

#[test]
fn a_dismissal_slides_the_panel_back_down_while_the_scrim_fades_out() {
    let _g = serial();
    let _ambient = setup();

    let mut h = Harness::themed(Some(frust_glyph::baseline()));
    show_sheet(&h.controller, |_sheet| any(text("pair a host")));
    let settled = h.settle();
    assert!(panel_offset(&settled).abs() < EPSILON);
    assert_eq!(h.depth(), 2);

    // Two frames: the first starts the driver, the second catches it in flight inside the 150ms exit.
    h.tap(SCRIM_POINT);
    h.frame_at(support::SETTLE_MS);
    let mid = h.frame_at(support::SETTLE_MS + 60.0);
    assert_eq!(h.depth(), 2, "the sheet is still up while it leaves");

    assert!(
        panel_offset(&mid) > 0.5,
        "mid-exit the panel is on its way back down ({})",
        panel_offset(&mid)
    );
    let mid_scrim = scrim_alpha(&mid);
    assert!(
        mid_scrim > 0.0 && mid_scrim < SCRIM_ALPHA - 1e-6,
        "…while the scrim fades out with it ({mid_scrim})"
    );

    h.flush();
    assert_eq!(h.depth(), 1, "the exit popped the page when it finished");
}

#[test]
fn reduce_motion_collapses_the_sheet_to_the_frameworks_linear_staging() {
    let _g = serial();
    let _ambient = setup();

    let mut theme = frust_glyph::baseline();
    theme.motion.reduce_motion = true;
    let (mut h, mid) = mid_entrance(theme);

    // Both channels run the framework's reduced timeline: one collapsed duration, linear.
    let progress = (MID_STAGING_MS / REDUCE_MOTION_MS) as f32;
    assert!(
        (scrim_alpha(&mid) - SCRIM_ALPHA * progress).abs() < 0.02,
        "the scrim fades linearly under reduced motion ({} vs {})",
        scrim_alpha(&mid),
        SCRIM_ALPHA * progress
    );

    let collapsed = h.frame_at(REDUCE_MOTION_MS + 5.0);
    assert!(
        panel_offset(&collapsed).abs() < EPSILON,
        "the panel has arrived ({})",
        panel_offset(&collapsed)
    );
    assert!(
        (scrim_alpha(&collapsed) - SCRIM_ALPHA).abs() < 1e-6,
        "…and the scrim is fully dimmed"
    );
}

fn open_side_sheet(h: &mut Harness) -> RecScene {
    show_side_sheet(
        &h.controller,
        "panes & tabs",
        |_sheet| {
            any(button("Split pane right", |state: &mut State| {
                state.confirmed += 1
            }))
        },
        |_sheet| any(text("hold a row to rename")),
        || {},
    );
    h.settle()
}

fn open_side_sheet_with_handle(h: &mut Harness) -> (SheetHandle, Rc<Cell<usize>>) {
    let captured: Rc<RefCell<Option<SheetHandle>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&captured);
    let dismissed = Rc::new(Cell::new(0usize));
    let counted = Rc::clone(&dismissed);
    show_side_sheet(
        &h.controller,
        "panes & tabs",
        move |sheet| {
            *sink.borrow_mut() = Some(sheet.clone());
            any(text("panes & tabs"))
        },
        |_sheet| any(text("hold a row to rename")),
        move || counted.set(counted.get() + 1),
    );
    h.settle();
    let handle = captured
        .borrow()
        .clone()
        .expect("the body builder ran, so it was handed a handle");
    (handle, dismissed)
}

#[test]
fn side_sheet_opens_renders_its_body_and_dismisses_on_a_scrim_tap() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let scene = open_side_sheet(&mut h);
    assert_eq!(h.depth(), 2, "the side sheet is pushed as its own page");

    let body = largest(&scene.rounded).expect("the body's button painted a filled box");
    h.tap(center(body));
    h.flush();
    assert_eq!(h.state.confirmed, 1, "the body is live");
    assert_eq!(h.depth(), 2, "pressing the body doesn't close the sheet");

    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 1, "a scrim tap dismisses the side sheet too");
}

#[test]
fn a_dismissal_fires_on_dismiss_exactly_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (_sheet, dismissed) = open_side_sheet_with_handle(&mut h);
    assert_eq!(dismissed.get(), 0, "nothing has dismissed it yet");

    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 1, "the scrim closed it");
    assert_eq!(dismissed.get(), 1, "on_dismiss ran exactly once");

    h.settle();
    assert_eq!(dismissed.get(), 1);
}

#[test]
fn a_duplicate_close_pops_the_side_sheet_once_and_leaves_the_page_under_it_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.controller.push(|| any(text("under the side sheet")));
    h.settle();
    assert_eq!(h.depth(), 2);

    let (sheet, dismissed) = open_side_sheet_with_handle(&mut h);
    assert_eq!(h.depth(), 3, "the side sheet is pushed over it");

    // A pop lands a rebuild after it is enqueued, so a second closer can fire for one whole frame.
    assert!(sheet.close(), "the first close is the one that closes it");
    assert!(!sheet.close(), "…and the second is a no-op");
    h.flush();

    assert_eq!(
        h.depth(),
        2,
        "exactly one page popped — the side sheet's own"
    );
    assert_eq!(dismissed.get(), 1, "on_dismiss still ran exactly once");
}

#[test]
fn a_close_after_the_side_sheet_is_already_gone_pops_nothing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.controller.push(|| any(text("under the side sheet")));
    h.settle();
    let (sheet, dismissed) = open_side_sheet_with_handle(&mut h);
    assert_eq!(h.depth(), 3);

    // A staged exit never reaches the app's handle; the completion latch stops a close in flight from landing on the page underneath.
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 2, "the scrim tap took the side sheet");
    assert_eq!(dismissed.get(), 1);

    assert!(!sheet.close(), "the handle was latched when the page went");
    h.flush();
    assert_eq!(h.depth(), 2, "…so nothing else popped");
    assert_eq!(
        dismissed.get(),
        1,
        "…and on_dismiss never fires a second time"
    );
}

#[test]
fn a_later_close_after_call_re_times_the_pending_close_rather_than_stacking_a_second() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (sheet, dismissed) = open_side_sheet_with_handle(&mut h);
    assert_eq!(h.depth(), 2);

    let first = sheet.clone();
    spawn_local(async move {
        first.close_after(Duration::from_millis(400)).await;
    });
    let second = sheet.clone();
    spawn_local(async move {
        second.close_after(Duration::from_millis(20)).await;
    });

    // Ride out the real async timer at the zero paint clock so the staged exit is not nudged early.
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    h.flush();

    assert_eq!(dismissed.get(), 1, "exactly one close landed");
    assert_eq!(h.depth(), 1, "…and it popped the side sheet");
}

const INTERNAL_DETAIL: &str = "tls handshake to 10.0.0.4:50051 timed out";

struct ToastHarness {
    root: RenderRoot<State, AnyView<State>>,
    logic: ToastLogic,
    state: State,
    tcx: TextContext,
}

impl ToastHarness {
    fn mount() -> Self {
        let mut harness = ToastHarness {
            root: RenderRoot::new(),
            logic: Box::new(|_state: &mut State| AnyView::new(toast_layer::<State>())),
            state: State::default(),
            tcx: TextContext::new(),
        };
        harness.frame();
        harness
    }

    fn frame(&mut self) -> RecScene {
        frame(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
        )
    }
}

#[test]
fn a_raised_failure_surfaces_its_user_message_and_never_its_internals() {
    let _g = serial();
    let _ambient = setup();
    let mut h = ToastHarness::mount();

    app_toasts().raise(&AppFailure::Network(INTERNAL_DETAIL.to_string()));
    h.frame();

    let labels = node_labels(&h.root);
    assert!(
        labels
            .iter()
            .any(|l| l == "Connection problem — please try again."),
        "the toast shows AppFailure's own user message: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.contains(INTERNAL_DETAIL)),
        "the internal detail is for logs only, never the screen: {labels:?}"
    );
    let pending = app_toasts().pending_untracked();
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].contains(INTERNAL_DETAIL));
}

#[test]
fn the_app_root_mounts_the_toast_layer_over_the_overlay_host() {
    let _g = serial();
    let _ambient = setup();

    let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| AnyView::new(MuxrApp.build(s));
    let mut tcx = TextContext::new();
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    app_toasts().raise(&AppFailure::Storage("session db is locked".to_string()));
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    let labels = node_labels(&root);
    assert!(
        labels
            .iter()
            .any(|l| l == "Couldn't save your data — please try again."),
        "the app root mounts the toast layer itself: {labels:?}"
    );
}

const SHEET_ACTION: &str = "Restore purchases";
const SHEET_TOAST: &str = "Nothing to restore";

#[test]
fn a_toast_surfaces_over_an_open_sheet_and_leaves_it_tappable() {
    let _g = serial();
    let _ambient = setup();

    let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| AnyView::new(MuxrApp.build(s));
    let mut tcx = TextContext::new();
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    let pressed = Rc::new(Cell::new(0_u32));
    let sink = Rc::clone(&pressed);
    show_sheet(&state.overlays, move |_sheet| {
        let sink = Rc::clone(&sink);
        any(button(SHEET_ACTION, move |_state: &mut AppState| {
            sink.set(sink.get() + 1);
        }))
    });
    settle(&mut root, &mut logic, &mut state, &mut tcx);

    app_toasts().show(SHEET_TOAST);
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    let labels = node_labels(&root);
    let action = labels.iter().position(|l| l == SHEET_ACTION);
    let toast = labels.iter().position(|l| l == SHEET_TOAST);
    assert!(action.is_some(), "the sheet is the routed page: {labels:?}");
    assert!(
        toast > action,
        "the toast surfaces over the open sheet: {labels:?}"
    );

    let target =
        node_center_by_label(&root, SHEET_ACTION).expect("the sheet's control is laid out");
    tap(&mut root, &mut state, target);
    assert_eq!(
        pressed.get(),
        1,
        "the toast layer over the sheet swallows no tap"
    );
}

#[test]
fn the_toast_layer_is_input_transparent() {
    let _g = serial();
    let _ambient = setup();
    let mut h = ToastHarness::mount();
    app_toasts().show("saved");
    h.frame();

    // The app root stacks this layer over everything; if the host handled pointer events every tap would be swallowed.
    let outcome = h.root.event(
        &mut h.state,
        &pointer(PointerPhase::Down, Point::new(support::W / 2.0, 560.0)),
    );
    assert!(
        !outcome.handled,
        "the toast host must let every pointer event through to the screen \
         underneath it"
    );
}

fn agent_mark_labels(build: impl Fn() -> AnyView<State> + 'static) -> Vec<String> {
    let mut root: RenderRoot<State, AnyView<State>> = RenderRoot::new();
    let mut state = State::default();
    let mut logic = move |_: &mut State| build();
    let mut tcx = TextContext::new();
    frame(&mut root, &mut logic, &mut state, &mut tcx);
    node_labels(&root)
}

#[test]
fn count_pills_label_each_nonzero_status_in_priority_order() {
    let _g = serial();
    let _ambient = setup();
    let summary = AgentSummary {
        total: 5,
        blocked: 1,
        done: 0,
        working: 3,
        idle: 1,
        ..AgentSummary::default()
    };
    let labels = agent_mark_labels(move || {
        let mut strip = row::<State>();
        for pill in count_pills(&summary) {
            strip = strip.child(pill);
        }
        any(strip)
    });
    let pills: Vec<&String> = labels
        .iter()
        .filter(|l| l.contains(['◉', '✓', '●', '○']))
        .collect();
    assert_eq!(pills, ["◉ 1", "● 3", "○ 1"], "all labels: {labels:?}");
}

#[test]
fn kind_tiles_show_monograms_then_a_plus_n_tile() {
    let _g = serial();
    let _ambient = setup();
    let kinds: Vec<String> = ["claude", "codex", "gemini", "pi", "mystery"]
        .map(String::from)
        .to_vec();
    let labels = agent_mark_labels(move || any(kind_tiles::<State>(&kinds, 3)));
    for expected in ["C", "X", "G", "+2"] {
        assert!(
            labels.iter().any(|l| l == expected),
            "{expected} in {labels:?}"
        );
    }
    assert!(!labels.iter().any(|l| l == "π"), "past the cap: {labels:?}");
}

#[test]
fn kind_tile_falls_back_to_a_question_mark_for_an_unknown_kind() {
    let _g = serial();
    let _ambient = setup();
    let labels = agent_mark_labels(|| any(kind_tile::<State>("mystery", 22.0)));
    assert!(labels.iter().any(|l| l == "?"), "{labels:?}");
    let labels = agent_mark_labels(|| any(kind_tile::<State>("pi", 22.0)));
    assert!(labels.iter().any(|l| l == "π"), "{labels:?}");
}
