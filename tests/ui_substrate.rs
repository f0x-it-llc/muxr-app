//! Headless coverage for the shared presentation substrate (`src/ui/dialogs.rs`, `src/ui/sheet.rs`,
//! `src/ui/side_sheet.rs`, `src/ui/toasts.rs`) — no GPU, no window: a `RenderRoot` over a bare
//! navigator.
//!
//! Every dialog/sheet here is a **pushed navigator page**, so each test follows the same three
//! beats: push it, [`support::settle`] it past the navigator's input-blocking transition, then
//! tap/type.
//!
//! **Why taps are addressed by painted geometry.** `NavigatorWidget` contributes no
//! `Widget::semantics` and forwards none of its pages' — **nothing inside a pushed page appears
//! in the accessibility tree**, so a label-addressed tap can't find a dialog button. The overlay
//! tests locate targets in the recorded paint output instead (`support`'s
//! [`largest`]/[`smallest`]/[`by_width`]), addressing each by the one geometric property its
//! layout fixes: the panel is the biggest paint, a dialog's field spans its full content width,
//! the danger action is the smallest stroked outline. The toast tests mount the host without a
//! navigator and use semantics directly (and are the ones asserting message *text*).
//!
//! `State` is a small test-local struct, not `muxr::AppState`: all three helpers are generic over
//! the app state, each taking nothing but the navigator to push its page onto.

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

/// The panel gutter both overlays use, transcribed: `frust_glyph::dialog`'s
/// `DIALOG_PADDING` and `ui::sheet`'s `PANEL_PAD` are both 20px, so a
/// full-bleed child of either (the name field, the sheet body) is laid out at
/// the panel width minus twice this — the property [`content_child`] addresses
/// it by.
const PANEL_PADDING: f64 = 20.0;

/// A point in the top-left corner — scrim on every overlay here, since both
/// the dialog (centered panel) and the sheet (bottom-anchored panel) leave it
/// uncovered.
const SCRIM_POINT: Point = Point::new(4.0, 4.0);

/// What each test observes: the callbacks the substrate is supposed to fire.
#[derive(Default)]
struct State {
    /// Bumped by [`confirm_dialog`]'s `on_confirm`.
    confirmed: u32,
    /// Every name [`name_input_dialog`] submitted.
    names: Vec<String>,
}

/// The base page every overlay is pushed over — deliberately paints no chrome,
/// so every recorded rect belongs to the overlay under test.
fn home() -> AnyView<State> {
    any(text("muxr"))
}

/// The boxed app-logic closure `RenderRoot::rebuild` drives, for each of the
/// two roots below.
type NavLogic = Box<dyn FnMut(&mut State) -> NavigatorView<State>>;
type ToastLogic = Box<dyn FnMut(&mut State) -> AnyView<State>>;

/// A mounted navigator plus everything a frame needs — the fixture the dialog
/// and sheet behaviors share.
struct Harness {
    controller: NavigatorController<State>,
    root: RenderRoot<State, NavigatorView<State>>,
    logic: NavLogic,
    state: State,
    tcx: TextContext,
}

impl Harness {
    /// Mount the navigator, unthemed, and render its first frame.
    fn mount() -> Self {
        Self::themed(None)
    }

    /// Mount under `theme` — the sheet-staging tests need this since durations, easings, and
    /// `reduce_motion` are all theme reads (an unthemed mount stages on transcribed Glyph
    /// fallbacks instead, what every other test here runs on).
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

    /// One frame painted at an explicit clock — how a staged animation is
    /// caught mid-flight rather than settled.
    fn frame_at(&mut self, at_ms: f64) -> RecScene {
        frame_at(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            ms(at_ms),
        )
    }

    /// Drive a pushed/popped page past its transition (and past a dialog's own
    /// enter/exit staging), returning the last frame's paint record.
    fn settle(&mut self) -> RecScene {
        settle(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
        )
    }

    /// Settle, then deliver a pop's result: the navigator flushes a pusher's `on_result` at the
    /// start of the first event pass **after** the pop's rebuild, so a harmless `Move` is what
    /// actually hands the answer to app state.
    fn flush(&mut self) {
        self.settle();
        self.settle();
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, SCRIM_POINT));
    }

    /// A full tap at `point`.
    fn tap(&mut self, point: Point) {
        tap(&mut self.root, &mut self.state, point);
    }

    /// Type `text` into whatever holds focus, one already-resolved character
    /// event at a time (the shape a platform delivers).
    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.root.event(&mut self.state, &char_key(&c.to_string()));
        }
    }

    /// Press a named key (Enter/Escape) against the focused chain.
    fn press(&mut self, key: NamedKey) {
        self.root.event(&mut self.state, &named_key(key));
    }

    /// The navigator's page-stack depth (1 = only the base page).
    fn depth(&self) -> usize {
        self.controller.depth()
    }
}

/// The overlay panel in `scene` — the biggest surface it filled. Checks both channels: a dialog's
/// panel is a plain rounded rect, the Glyph sheet's is a *path* (only its top corners are
/// rounded, which `fill_rounded_rect` can't express).
fn panel(scene: &RecScene) -> (Point, Size) {
    let mut candidates = scene.rounded.clone();
    candidates.extend(scene.paths.iter().copied());
    largest(&candidates).expect("an overlay always paints its own panel")
}

/// An overlay's full-content-width child — the dialog's name field, the
/// sheet's body (see [`PANEL_PADDING`]). Addressed by width rather than by
/// "the smallest rect", which would find the sheet's own drag handle.
fn content_child(scene: &RecScene) -> (Point, Size) {
    let content_w = panel(scene).1.width - 2.0 * PANEL_PADDING;
    by_width(&scene.rounded, content_w)
        .unwrap_or_else(|| panic!("no full-width child in {:?}", scene.rounded))
}

// ---------------------------------------------------------------------------
// confirm_dialog
// ---------------------------------------------------------------------------

/// Open a confirm dialog over `h` and return the settled frame.
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

    // Danger action: transparent fill + error-colored border, so it's the smallest stroked
    // outline on screen (the panel's own border being the other one).
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
    // Scrim tap: staged exit, then pop with an empty result — reads as "cancelled".
    h.tap(SCRIM_POINT);
    h.flush();

    assert_eq!(
        h.state.confirmed, 0,
        "a dismissal pops with an empty result"
    );
    assert_eq!(h.depth(), 1, "the dialog closed");
}

// ---------------------------------------------------------------------------
// name_input_dialog
// ---------------------------------------------------------------------------

/// Open a name-input dialog over `h` and return the settled frame.
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
    // A single-line field submits on Enter.
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

    // Typing "enables" the submit: ghost style (paints nothing) flips to filled primary.
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

    // Enter on an untouched (empty) field: nothing submits or closes.
    h.tap(field);
    h.press(NamedKey::Enter);
    h.flush();
    assert!(h.state.names.is_empty(), "an empty name never submits");
    assert_eq!(h.depth(), 2, "…and the dialog stays up");

    // Whitespace only is still empty.
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

// ---------------------------------------------------------------------------
// sheet
// ---------------------------------------------------------------------------

/// Open a sheet whose body is one filled (primary) button, and return the
/// settled frame.
fn open_sheet(h: &mut Harness) -> RecScene {
    show_sheet(&h.controller, |_sheet| {
        // A filled button, not bare text: paints a rect ("rendered"), and its press proves
        // events reach the body at all.
        any(button("Split pane right", |state: &mut State| {
            state.confirmed += 1
        }))
    });
    h.settle()
}

/// Open a sheet and hand back the [`SheetHandle`] its body builder was given —
/// the app-side closer every row, button and delayed close goes through.
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

/// A point inside the sheet's own drag-handle strip: the top band of the
/// panel, which carries no control — where a dismiss drag starts.
fn handle_point(scene: &RecScene) -> Point {
    let (origin, size) = panel(scene);
    Point::new(origin.x + size.width / 2.0, origin.y + 10.0)
}

/// How far below its resting place the panel is painting this frame. The Glyph sheet slides by
/// pushing **one** translate around its panel and painting its scrim outside it, so the recorded
/// panel rect never moves and this transform *is* the slide (`support::RecScene`'s docs). Takes
/// the largest y translation rather than "the only one": a page still inside its own push is
/// wrapped in the navigator's transform too (identity here, since the sheet has no page
/// transition).
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
    // Edge to edge: a Glyph sheet's panel spans the viewport.
    assert!(
        (panel_size.width - support::W).abs() < EPSILON && panel_origin.x.abs() < EPSILON,
        "the panel spans the whole width: {panel_origin:?}/{panel_size:?}"
    );
    // Bottom-anchored and flush: only the panel's top corners are rounded.
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

    // First: the body's own button fires — proves this test isn't passing vacuously on a
    // navigator that swallowed every event.
    h.tap(center(content_child(&scene)));
    h.flush();
    assert_eq!(h.state.confirmed, 1, "the body's button is live");
    assert_eq!(h.depth(), 2, "pressing the body doesn't close the sheet");

    // The regression a plain `Align`-in-`Stack` composition has: a press on the panel's own
    // background falling through to the scrim behind it. The drag-handle strip has no control
    // under it, and a press with no travel before release leaves the sheet up.
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
    // Past the half-panel-height dismiss threshold; the release decides, so Down → Move → Up.
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
    // A press on the handle strip claims focus for the sheet itself (no control hit), the chain
    // `Escape` then travels — content holding the deeper focus path would get first crack instead.
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

    // A page under the sheet to be wrongly popped — the overlay/route the sheet went up over.
    h.controller.push(|| any(text("under the sheet")));
    h.settle();
    assert_eq!(h.depth(), 2);

    let sheet = open_sheet_with_handle(&mut h);
    assert_eq!(h.depth(), 3, "the sheet is pushed over it");

    // The defect this gate exists for: a pop lands a rebuild after it's enqueued, so a second
    // closer can still fire for one whole frame.
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

    // The sheet leaves by its own staged exit (a scrim tap), which the app's handle never sees —
    // the completion latch stops a close still in flight from landing on the page underneath.
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 2, "the scrim tap took the sheet");

    assert!(!sheet.close(), "the handle was latched when the page went");
    h.flush();
    assert_eq!(h.depth(), 2, "…so nothing else popped");
}

// ---------------------------------------------------------------------------
// sheet — the scrim fades, only the panel slides
// ---------------------------------------------------------------------------

/// How far into the sheet's entrance the staging probes paint. Comfortably
/// inside the modal enter duration (Glyph's `durations.base`, 220ms) and past
/// its start, so a partial frame is genuinely partial.
const MID_STAGING_MS: f64 = 60.0;

/// The framework's own reduced-motion collapse, transcribed
/// (`frust`'s `nav::transition::REDUCE_MOTION_DURATION`, which the Glyph sheet
/// resolves both of its timings to when a theme asks for reduced motion).
const REDUCE_MOTION_MS: f64 = 120.0;

/// The sheet's scrim in `scene`: every plain fill covering the **whole** surface, where the
/// Glyph sheet puts it every frame regardless of staging. A scrim anywhere else is the defect
/// this section exists for — a whole-page `SlideUp` would translate it with the panel, wiping up
/// the screen instead of fading in place.
fn scrim(scene: &RecScene) -> Vec<Color> {
    fills_at(scene, Point::ZERO, support::surface())
}

/// The scrim's single alpha this frame, panicking if it painted anywhere but
/// full-bleed (or more than once).
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

/// Push a sheet without settling it, and return the harness plus the frame at
/// [`MID_STAGING_MS`] — mid-entrance.
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

    // The panel is still on its way up…
    assert!(
        mid_offset > 0.5,
        "mid-entrance the panel is still below its resting place ({mid_offset})"
    );
    assert!(
        panel_offset(&settled).abs() < EPSILON,
        "…and rests flush once it arrives ({})",
        panel_offset(&settled)
    );
    // …while the scrim is *already* partially dimmed at FULL coverage. The pair is the whole
    // point: under a whole-page `PageTransition::SlideUp` the scrim would be displaced by exactly
    // the same amount as the panel — a black rectangle wiping up the screen.
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

    // The regression, stated as geometry: across the whole entrance the scrim's rect is
    // byte-identical (origin zero, full surface) and only alpha differs. `scrim_alpha` panics
    // unless exactly one full-bleed fill exists, so every probe below is an assertion.
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

    // A scrim tap stages the exit; the pop is deferred until it has played. Two frames: the first
    // *starts* the driver (epochs on its own first advance), the second catches it in flight,
    // comfortably inside the 150ms modal exit.
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

    // It really does go.
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

    // Both channels run the framework's reduced timeline: one collapsed duration, **linear** —
    // the authored `spatial`/`effects` easings are well past linear at this point.
    let progress = (MID_STAGING_MS / REDUCE_MOTION_MS) as f32;
    assert!(
        (scrim_alpha(&mid) - SCRIM_ALPHA * progress).abs() < 0.02,
        "the scrim fades linearly under reduced motion ({} vs {})",
        scrim_alpha(&mid),
        SCRIM_ALPHA * progress
    );

    // …and over by that duration, where the full entrance still has 100ms to run.
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

// ---------------------------------------------------------------------------
// side_sheet
// ---------------------------------------------------------------------------

/// Open a side sheet whose body is one filled (primary) button and whose footer is plain content,
/// and return the settled frame.
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

/// Open a side sheet and hand back the [`SheetHandle`] its body builder was given, plus a counter
/// bumped once per `on_dismiss` firing — [`show_side_sheet`]'s own extra seam over
/// [`show_sheet`]'s.
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

    // The body's own button — the biggest rounded fill on screen (the header's
    // own close chip is a small fixed circle).
    let body = largest(&scene.rounded).expect("the body's button painted a filled box");
    h.tap(center(body));
    h.flush();
    assert_eq!(h.state.confirmed, 1, "the body is live");
    assert_eq!(h.depth(), 2, "pressing the body doesn't close the sheet");

    // The panel is pinned to the trailing edge, so the leading corner is
    // always clear of it — the same scrim point the bottom sheet uses.
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

    // Settling further frames must not re-fire it.
    h.settle();
    assert_eq!(dismissed.get(), 1);
}

#[test]
fn a_duplicate_close_pops_the_side_sheet_once_and_leaves_the_page_under_it_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // A page under the side sheet to be wrongly popped.
    h.controller.push(|| any(text("under the side sheet")));
    h.settle();
    assert_eq!(h.depth(), 2);

    let (sheet, dismissed) = open_side_sheet_with_handle(&mut h);
    assert_eq!(h.depth(), 3, "the side sheet is pushed over it");

    // The defect this gate exists for: a pop lands a rebuild after it's enqueued, so a second
    // closer can still fire for one whole frame.
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

    // The side sheet leaves by its own staged exit (a scrim tap), which the app's handle never
    // sees — the completion latch stops a close still in flight from landing on the page
    // underneath.
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

    // Two close_after calls close together: the first arms a longer delay, the
    // second — a fresh row tap inside the same close beat — re-times it to
    // its own, shorter one. Only the second may land.
    let first = sheet.clone();
    spawn_local(async move {
        first.close_after(Duration::from_millis(400)).await;
    });
    let second = sheet.clone();
    spawn_local(async move {
        second.close_after(Duration::from_millis(20)).await;
    });

    // Ride out the real async timer behind `close_after` first — a plain pump loop at the zero
    // paint clock, so nothing here nudges the sheet's own staged-exit animation early (mirrors
    // `tests/terminal.rs`'s `Harness::pump_for`).
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    // …then let the staged exit itself play out and land the pop.
    h.flush();

    assert_eq!(dismissed.get(), 1, "exactly one close landed");
    assert_eq!(h.depth(), 1, "…and it popped the side sheet");
}

// ---------------------------------------------------------------------------
// failure -> toast
// ---------------------------------------------------------------------------

/// The internal detail an `AppFailure` carries for logs — the string a toast
/// must never show.
const INTERNAL_DETAIL: &str = "tls handshake to 10.0.0.4:50051 timed out";

/// The toast layer alone (the view `MuxrApp::build` stacks over the root overlay host) over its
/// own root: asserts the bridge, not the app wiring, and keeps the host out of a navigator whose
/// covered pages would otherwise hide the toast's own node.
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
    // The queue holds the mapped message too, not the raw `Display` string.
    let pending = app_toasts().pending_untracked();
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].contains(INTERNAL_DETAIL));
}

#[test]
fn the_app_root_mounts_the_toast_layer_over_the_overlay_host() {
    let _g = serial();
    let _ambient = setup();

    // The real composition root this time: a failure raised anywhere in the app must surface
    // through the layer `MuxrApp::build` stacks over the overlay host, with no per-screen wiring.
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

/// A sheet body's one control, and the toast raised while that sheet is up.
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

    // The app's own path: a sheet is a page pushed onto the root overlay host.
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

    // An open overlay drops the host's whole root page from the tree, so a toast still listed is
    // mounted outside the host; listed after the sheet's control, it is stacked above the sheet.
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

    // The app root stacks this layer *over* everything; if the host ever handled pointer events,
    // every tap on every screen and sheet would be swallowed.
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

// ---- ui::agent_marks ----

/// Mounts one agent-marks view, then reads back its accessible labels.
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
