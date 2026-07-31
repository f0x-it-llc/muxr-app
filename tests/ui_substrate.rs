//! Headless coverage for the shared presentation substrate (`src/ui/dialogs.rs`,
//! `src/ui/sheet.rs`, `src/ui/toasts.rs`) — no GPU, no window: a `RenderRoot`
//! over a bare navigator, driven through `tests/support`.
//!
//! Every dialog/sheet here is a **pushed navigator page**, so each test follows
//! the same three beats: push it, [`support::settle`] it past the navigator's
//! input-blocking transition, then tap/type.
//!
//! # Why taps are addressed by painted geometry
//!
//! `NavigatorWidget` contributes no `Widget::semantics` and forwards none of
//! its pages', so **nothing inside a pushed page appears in the accessibility
//! tree** — a label-addressed tap can't find a dialog button. The overlay
//! tests therefore locate their targets in the recorded paint output instead
//! (`support`'s [`largest`]/[`smallest`]/[`by_width`]), addressing each by the
//! one geometric property the widget's own layout fixes: the panel is the
//! biggest thing an overlay paints, a dialog's field spans the panel's full
//! content width, and the danger action is the smallest stroked outline. The
//! toast tests, mounting the host without a navigator, use semantics directly
//! (and are the ones that assert message *text*).
//!
//! `State` is a small test-local struct, not `muxr::AppState`: all three
//! helpers are generic over the app state precisely so a screen (or a test)
//! brings its own.

use frust::{AnyView, Component, NavigatorController, NavigatorView, any, button, navigator, text};
use frust_core::{NamedKey, PointerPhase, RenderRoot};
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::failure::AppFailure;
use muxr::ui::dialogs::{confirm_dialog, name_input_dialog};
use muxr::ui::sheet::show_sheet;
use muxr::ui::toasts::{app_toasts, toast_layer};
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    RecScene, by_width, center, char_key, frame, largest, named_key, node_labels, pointer, serial,
    settle, setup, smallest, tap,
};

/// The panel gutter both overlays use, transcribed: `frust::glyph::dialog`'s
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
    /// Mount the navigator and render its first frame.
    fn mount() -> Self {
        let controller: NavigatorController<State> = NavigatorController::new();
        let logic_controller = controller.clone();
        let mut harness = Harness {
            controller,
            root: RenderRoot::new(),
            logic: Box::new(move |_state: &mut State| navigator(&logic_controller, home)),
            state: State::default(),
            tcx: TextContext::new(),
        };
        harness.settle();
        harness
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

    /// Settle, then deliver a pop's result: the navigator flushes a pusher's
    /// `on_result` at the start of the first event pass **after** the pop's
    /// rebuild, so a harmless `Move` is what actually hands the answer to app
    /// state.
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

/// The overlay panel in `scene` — the biggest filled rect it painted.
fn panel(scene: &RecScene) -> (Point, Size) {
    largest(&scene.rounded).expect("an overlay always paints its own panel")
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

    // The danger confirm action paints a transparent fill and an error-colored
    // border, so it is the smallest stroked outline on screen (the panel's own
    // border being the other one).
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
    // Press and release on the scrim: the widget plays its staged exit, then
    // pops with an empty result — which reads as "cancelled", so nothing runs.
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
    // A single-line field submits on Enter, popping the page with its text.
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

    // Typing "enables" the submit: it flips from the ghost style (which paints
    // nothing at all) to the filled primary one, so a new filled rect appears.
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

    // Enter on an untouched (empty) field: nothing submits, nothing closes.
    h.tap(field);
    h.press(NamedKey::Enter);
    h.flush();
    assert!(h.state.names.is_empty(), "an empty name never submits");
    assert_eq!(h.depth(), 2, "…and the dialog stays up");

    // Whitespace only is still empty — and still paints no enabled submit.
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
    show_sheet(&h.controller, || {
        // A filled button, not bare text: it paints a rect (which is what
        // "the body rendered" is observed through here) and its press is the
        // proof that events reach the body at all.
        any(button("Split pane right", |state: &mut State| {
            state.confirmed += 1
        }))
    });
    h.settle()
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
        "the body renders inside the panel: body {body:?}, panel \
         {panel_origin:?}/{panel_size:?}"
    );
    // Bottom-anchored, clamped and centered on this 800px-wide viewport.
    assert_eq!(
        panel_size.width,
        muxr::ui::sheet::MAX_WIDTH,
        "a wide viewport clamps the panel to the 640dp cap"
    );
    assert!(
        (panel_origin.x - (support::W - muxr::ui::sheet::MAX_WIDTH) / 2.0).abs() < 0.5,
        "…and centers it: {panel_origin:?}"
    );
    // The panel is *painted* deliberately past the bottom edge so its bottom
    // corner rounding falls off-screen (see `ui/sheet.rs`'s paint) — its
    // layout rect is flush with it, and the overshoot is exactly one corner
    // radius.
    let painted_bottom = panel_origin.y + panel_size.height;
    assert!(
        painted_bottom > support::H && painted_bottom - support::H < 2.0 * PANEL_PADDING,
        "…anchored to the bottom edge, overshooting it by one corner radius \
         only: {painted_bottom}"
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

    // First: the body's own button fires — which is what proves this test
    // isn't passing vacuously on a navigator that swallowed every event.
    h.tap(center(content_child(&scene)));
    h.flush();
    assert_eq!(h.state.confirmed, 1, "the body's button is live");
    assert_eq!(h.depth(), 2, "pressing the body doesn't close the sheet");

    // Then the regression a plain `Align`-in-`Stack` composition has: a press
    // on the panel's own *background* falling through to the scrim behind it.
    // The drag-handle band at the panel's top edge is panel with no control
    // under it — the body starts below the handle zone.
    let scene = h.settle();
    let (panel_origin, panel_size) = panel(&scene);
    let bare_panel = Point::new(
        panel_origin.x + panel_size.width / 2.0,
        panel_origin.y + 4.0,
    );
    h.tap(bare_panel);
    h.flush();
    assert_eq!(h.state.confirmed, 1, "…and it hit no control");
    assert_eq!(h.depth(), 2, "the sheet stays up");
}

// ---------------------------------------------------------------------------
// failure -> toast
// ---------------------------------------------------------------------------

/// The internal detail an `AppFailure` carries for logs — the string a toast
/// must never show.
const INTERNAL_DETAIL: &str = "tls handshake to 10.0.0.4:50051 timed out";

/// The toast layer alone (the same view `shell::root_view` stacks at index 1)
/// over its own root: this asserts the bridge, not the shell wiring — and it
/// keeps the host out of a navigator, whose missing `semantics` would
/// otherwise hide the toast's own node.
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
            logic: Box::new(|_state: &mut State| toast_layer::<State>()),
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
    // The queue holds the mapped message, not the raw `Display` string either.
    let pending = app_toasts().pending_untracked();
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].contains(INTERNAL_DETAIL));
}

#[test]
fn the_shell_mounts_the_toast_layer_over_the_navigator() {
    let _g = serial();
    let _ambient = setup();

    // The real composition root this time (as `tests/routes.rs` mounts it):
    // a failure raised anywhere in the app must surface through the layer
    // `shell::root_view` stacks at index 1, with no per-screen wiring.
    let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| MuxrApp.build(s);
    let mut tcx = TextContext::new();
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    app_toasts().raise(&AppFailure::Storage("session db is locked".to_string()));
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    let labels = node_labels(&root);
    assert!(
        labels
            .iter()
            .any(|l| l == "Couldn't save your data — please try again."),
        "the app shell mounts the toast layer itself: {labels:?}"
    );
}

#[test]
fn the_toast_layer_is_input_transparent() {
    let _g = serial();
    let _ambient = setup();
    let mut h = ToastHarness::mount();
    app_toasts().show("saved");
    h.frame();

    // The shell stacks this layer *over* the navigator; if the host ever
    // started handling pointer events, every tap on every screen would be
    // swallowed. A press on the toast's own resting spot must go unhandled.
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
