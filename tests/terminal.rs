//! Headless coverage for the `/terminal` screen over the real page builder. The screen mounts
//! under a `Stack` with a mounted overlay navigator, and a record-only navigator carries `Nav` so
//! the pushed `?session=` query reaches the route-meta mirror without building a second screen.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, GetUntracked, NavigatorController, PopResult, Set, SizedBox, Stack, Theme,
    WindowMetrics, any, navigator, provide_context,
};
use frust_core::accesskit::{Node, Role};
use frust_core::{
    FrameTime, InputEvent, NamedKey, PointerPhase, RenderRoot, ScrollDelta, TOUCH_SLOP,
};
use frust_text::TextContext;
use kurbo::{Point, Size};
use peniko::Color;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::sessions::data::repositories::mock_sessions_repository::SEEDED_DEV_ID;
use muxr::features::sessions::di::expect_sessions_repository;
use muxr::features::terminal::data::repositories::mock_terminal_repository::{
    clear_test_agents, clear_verb_log_for_tests, delay_next_list_agents_for_tests,
    fail_next_list_agents_for_tests, seed_agents, verb_log_for_tests,
};
use muxr::features::terminal::domain::entities::{AgentEntry, AgentStatus};
use muxr::features::terminal::presentation::controller::fail_next_tab_list_for_tests;
use muxr::features::terminal::presentation::pages::{
    debug_controller, open_terminal_menu, terminal_page,
};
use muxr::routes::{BarAction, app_nav, install_app_router, route_meta};
use muxr::ui::paint::RADIUS_FULL;
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, by_width, center, char_key, frame, frame_at_sized, largest, ms,
    named_key, node_centers_by_label, node_centers_by_value, node_labels, node_values, pointer,
    pump_frames, rounded_in, same, serial, setup, tap,
};

/// Taller than the default 800×600 so the sheet's own rows never fall below the fold.
const SURFACE: Size = Size::new(800.0, 700.0);
/// A point in the top-left corner: scrim on every overlay this screen pushes.
const SCRIM_POINT: Point = Point::new(4.0, 4.0);
const PANEL_PADDING: f64 = 20.0;
const PUMP_DEADLINE: Duration = Duration::from_secs(5);

const SESSION_ID: &str = SEEDED_DEV_ID;
const SESSION_NAME: &str = "dev";

const DEVBOX_TITLE: &str = "ed@devbox: ~";
const DEVBOX_FIRST_LINE: &str = "ed@devbox:~$ systemctl status muxrd";
const BTOP_TITLE: &str = "ed@workbench: ~";
const LINES_PER_TAB: usize = 4;

/// Transcribed rather than imported, so a test catches either name changing.
const EXPAND: &str = "Fullscreen";
const COLLAPSE: &str = "Exit fullscreen";
const PANES: &str = "Panes & tabs";
const SHEET_TITLE: &str = "panes & tabs";
const CLOSE: &str = "Close";

/// `.term-wrap{padding:14px}` and `.term-card{border-radius:22px}`: what fullscreen zeroes.
const WRAP_PAD: f64 = 14.0;
const CARD_RADIUS: f64 = 22.0;
const WIDE_WRAP_PAD: f64 = 22.0;
const PORTRAIT_TOP_GAP: f64 = WIDE_WRAP_PAD - WRAP_PAD;
const STATUS_ROW_H: f64 = 40.0;
/// `.cur{width:7px;height:13px}`: the caret's box, how a frame is asked whether it is lit.
const CARET: Size = Size::new(7.0, 13.0);

const REST_DEPTH: usize = 1;

const CLOSE_DELAY: Duration = Duration::from_millis(260);

const DETACHED_TOAST: &str = "detached";

const READ_ONLY_TOAST: &str = "read-only session";

/// Every create affordance shows this once the mock answers "unsupported": proof it got there.
const INTERNAL_FAILURE_TOAST: &str = "Something went wrong — please try again.";

/// The add-pane row paints a transparent dashed box, so it is found by its accessible value.
const NEW_PANE_LABEL: &str = "new pane";

const ADD_SPACE_LABEL: &str = "new space";
const ADD_TAB_LABEL: &str = "new tab";
/// A button announces its text as a name, not a run, so it is tapped through `tap_label`.
const NEW_TAB_SUBMIT_LABEL: &str = "Create";

const LUMEN_TITLE: &str = "ed@lumen: ~/api";

const FOOTER_RENAME_LABEL: &str = "Rename session";
const FOOTER_DETACH_LABEL: &str = "Detach session";
const RENAME_DIALOG_TITLE: &str = "Rename session";

const HERDR_SESSION_ID: &str = "herdr:work";

type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

fn mock_target() -> ConnectTarget {
    ConnectTarget {
        host: "mock.example".to_string(),
        port: 7777,
        trust_mode: TrustMode::System,
        label: "Mock rig".to_string(),
        offer_credential: false,
        token: zeroize::Zeroizing::new(String::new()),
        remember: false,
        fingerprint: None,
    }
}

struct Harness {
    nav: NavigatorController<AppState>,
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    mounted: Rc<Cell<bool>>,
}

impl Harness {
    fn mount() -> Self {
        Self::themed(frust_glyph::baseline())
    }

    fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Self {
        support::mount_with_metrics(metrics, Self::mount)
    }

    fn mount_session(session: &str) -> Self {
        Self::themed_session(frust_glyph::baseline(), session)
    }

    fn themed(theme: Theme) -> Self {
        Self::themed_session(theme, SESSION_ID)
    }

    fn themed_session(theme: Theme, session: &str) -> Self {
        let state = MuxrApp.init();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        let record_only: NavigatorController<AppState> = NavigatorController::new();
        let _router = install_app_router(&record_only, &nav, expect_connection_controller());
        // Connected, so the guard lets the push resolve and the recorded query is `/terminal`'s.
        expect_connection_controller().connect(mock_target());
        app_nav().push(&format!("/terminal?session={session}"));

        let mounted = Rc::new(Cell::new(true));
        let logic_nav = nav.clone();
        let logic_mounted = Rc::clone(&mounted);
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            let screen_nav = logic_nav.clone();
            let overlay_nav = logic_nav.clone();
            let screen: AnyView<AppState> = if logic_mounted.get() {
                any(terminal_page(screen_nav))
            } else {
                any(SizedBox::<AppState>(Some(0.0), Some(0.0)))
            };
            any(Stack(vec![
                screen,
                any(navigator(&overlay_nav, || {
                    // Full-bleed and childless, so every tap falls through to the screen below.
                    any(SizedBox::<AppState>(
                        Some(f64::INFINITY),
                        Some(f64::INFINITY),
                    ))
                })),
            ]))
        });
        let mut root = RenderRoot::new();
        root.set_theme(Box::new(theme));
        let mut harness = Harness {
            nav,
            root,
            logic,
            state,
            tcx: TextContext::new(),
            mounted,
        };
        harness.frame();
        harness.await_load();
        harness
    }

    fn frame(&mut self) -> RecScene {
        self.frame_at(0.0)
    }

    fn frame_at(&mut self, at_ms: f64) -> RecScene {
        frame_at_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
            ms(at_ms),
        )
    }

    fn settle(&mut self) -> RecScene {
        support::settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
        )
    }

    /// The navigator flushes `on_result` on the first event pass after the pop's rebuild: a `Move`.
    fn flush(&mut self) {
        self.settle();
        self.settle();
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, SCRIM_POINT));
    }

    fn pump_until_values(&mut self, what: &str, done: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + PUMP_DEADLINE;
        while !done(&node_values(&self.root)) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; on screen: {:?}",
                node_values(&self.root)
            );
            pump_frames(
                &mut self.root,
                &mut self.logic,
                &mut self.state,
                &mut self.tcx,
                1,
            );
        }
    }

    fn pump_until(&mut self, what: &str, done: impl Fn(&Harness) -> bool) {
        let deadline = Instant::now() + PUMP_DEADLINE;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            pump_frames(
                &mut self.root,
                &mut self.logic,
                &mut self.state,
                &mut self.tcx,
                1,
            );
        }
    }

    /// `pump_for` rides out the close delay at the zero clock, then a fixed 300ms carries the
    /// staged exit; a real-time advance would bake in a settled reveal before a caller reads it.
    fn pump_until_closed(&mut self, what: &str, done: impl Fn(&Harness) -> bool) {
        self.pump_for(2 * CLOSE_DELAY);
        self.frame_at(0.0);
        self.frame_at(300.0);
        self.frame_at(300.0);
        assert!(done(self), "timed out waiting for {what}");
    }

    fn await_load(&mut self) {
        self.pump_until_values("the seeded workspace", |values| {
            values.iter().any(|v| v == DEVBOX_TITLE)
        });
    }

    fn tap(&mut self, point: Point) {
        tap(&mut self.root, &mut self.state, point);
    }

    fn tap_label(&mut self, label: &str) {
        let point = node_centers_by_label(&self.root, label)
            .first()
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "nothing labelled {label:?}; labels on screen: {:?}",
                    node_labels(&self.root)
                )
            });
        self.tap(point);
    }

    /// The last match: every tab carries an add-pane row, and the active `devbox` is seeded last.
    fn new_pane_row_point(&self) -> Point {
        node_centers_by_value(&self.root, NEW_PANE_LABEL)
            .last()
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "no node valued {NEW_PANE_LABEL:?}; values on screen: {:?}",
                    node_values(&self.root)
                )
            })
    }

    fn add_space_row_point(&self) -> Point {
        self.value_point(ADD_SPACE_LABEL)
    }

    fn add_tab_row_point(&self) -> Point {
        self.value_point(ADD_TAB_LABEL)
    }

    /// `support::long_press` hardcodes the default surface; this suite's `SURFACE` is taller.
    fn long_press(&mut self, point: Point) {
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Down, point));
        self.frame_at(0.0);
        self.frame_at(support::HELD_MS);
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Up, point));
    }

    fn drag_past_slop_then_release(&mut self, point: Point) {
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Down, point));
        self.frame_at(0.0);
        let dragged = Point::new(point.x, point.y + TOUCH_SLOP + 2.0);
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, dragged));
        self.frame_at(support::HELD_MS);
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Up, dragged));
    }

    fn await_toast(&mut self, message: &str) {
        self.pump_until(&format!("a {message:?} toast"), |_| {
            app_toasts()
                .pending_untracked()
                .iter()
                .any(|m| m == message)
        });
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
        self.nav.depth()
    }

    /// Three frames: the first seeds the driver clock, the second advances it, the third lays out.
    fn settle_motion(&mut self, from_ms: f64) -> RecScene {
        self.frame_at(from_ms);
        self.frame_at(from_ms + 1_000.0);
        self.frame_at(from_ms + 1_010.0);
        self.frame_at(from_ms + 1_020.0)
    }

    fn open_sheet(&mut self) -> (RecScene, RecScene) {
        let before = self.frame();
        open_terminal_menu(&mut self.state);
        let after = self.settle();
        (before, after)
    }

    fn value_point(&self, value: &str) -> Point {
        node_centers_by_value(&self.root, value)
            .first()
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "no node valued {value:?}; values on screen: {:?}",
                    node_values(&self.root)
                )
            })
    }

    fn label_point(&self, label: &str) -> Point {
        node_centers_by_label(&self.root, label)
            .first()
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "no node labelled {label:?}; labels on screen: {:?}",
                    node_labels(&self.root)
                )
            })
    }

    fn tap_footer_rename(&mut self) -> RecScene {
        let point = self.label_point(FOOTER_RENAME_LABEL);
        self.tap(point);
        self.settle()
    }

    fn tap_footer_detach(&mut self) {
        let point = self.label_point(FOOTER_DETACH_LABEL);
        self.tap(point);
        self.flush();
    }

    /// A navigator always keeps one page, so without a floor a stray pop is absorbed invisibly.
    fn push_route_floor(&mut self) -> usize {
        self.nav.push(|| {
            any(SizedBox::<AppState>(
                Some(f64::INFINITY),
                Some(f64::INFINITY),
            ))
        });
        self.settle();
        self.depth()
    }

    /// The delayed close runs on the executor's timer, so this waits on the wall clock.
    fn pump_for(&mut self, span: Duration) {
        let until = Instant::now() + span;
        while Instant::now() < until {
            pump_frames(
                &mut self.root,
                &mut self.logic,
                &mut self.state,
                &mut self.tcx,
                1,
            );
        }
    }

    /// The sheet's one 26px disc: it lives inside the navigator, so it has no semantics node.
    fn close_disc(&mut self) -> Point {
        let scene = self.frame();
        let disc = scene
            .rounded
            .iter()
            .copied()
            .find(|(_, size)| {
                (size.width - 26.0).abs() < EPSILON && (size.height - 26.0).abs() < EPSILON
            })
            .expect("the sheet's 26px close disc");
        center(disc)
    }

    fn unmount_screen(&mut self) {
        self.mounted.set(false);
        self.settle();
    }

    fn values(&self) -> Vec<String> {
        node_values(&self.root)
    }

    fn shows(&self, value: &str) -> bool {
        self.values().iter().any(|v| v == value)
    }

    fn await_repository_name(&mut self, name: &str) {
        let repository = expect_sessions_repository();
        let deadline = Instant::now() + PUMP_DEADLINE;
        while !session_named(&repository, name) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for a session named {name:?}"
            );
            pump_frames(
                &mut self.root,
                &mut self.logic,
                &mut self.state,
                &mut self.tcx,
                1,
            );
        }
    }
}

fn card(scene: &RecScene) -> (Point, Size, f64) {
    let ink = frust_glyph::baseline()
        .extension::<frust_glyph::GlyphInk>()
        .copied()
        .expect("the Glyph baseline carries GlyphInk");
    let mut cards = rounded_in(scene, ink.terminal_bg);
    cards.sort_by(|a, b| (b.1.width * b.1.height).total_cmp(&(a.1.width * a.1.height)));
    cards
        .first()
        .copied()
        .expect("the card paints its own body every frame")
}

fn status_band(scene: &RecScene) -> (Point, Size) {
    let ink = frust_glyph::baseline()
        .extension::<frust_glyph::GlyphInk>()
        .copied()
        .expect("the Glyph baseline carries GlyphInk");
    let bands = rounded_in(scene, ink.terminal_head);
    let bands: Vec<(Point, Size)> = bands
        .into_iter()
        .map(|(origin, size, _)| (origin, size))
        .collect();
    largest(&bands).expect("the status row paints its own band")
}

fn carets(scene: &RecScene) -> usize {
    scene
        .fills
        .iter()
        .filter(|(_, size, _)| {
            (size.width - CARET.width).abs() < EPSILON
                && (size.height - CARET.height).abs() < EPSILON
        })
        .count()
}

fn panel(added: &[(Point, Size)]) -> (Point, Size) {
    largest(added).expect("an overlay always paints its own panel")
}

/// `side_sheet_tree`'s private mid-level tab radius: herdr's nested tab rect.
const TREE_TAB_RADIUS: f64 = 7.0;

fn rounded_fills_at(scene: &RecScene, radius: f64) -> Vec<(Point, Size)> {
    scene
        .rounded_fills
        .iter()
        .filter(|(_, _, r, _)| (*r - radius).abs() < EPSILON)
        .map(|(origin, size, _, _)| (*origin, *size))
        .collect()
}

/// Wide and not transparent: excludes the mark box, the focused dot and the add-pane row.
fn tree_pane_rows(scene: &RecScene) -> Vec<(Point, Size)> {
    let mut candidates: Vec<(Point, Size)> = scene
        .rounded_fills
        .iter()
        .filter(|(_, _, r, color)| {
            (*r - RADIUS_FULL).abs() < EPSILON && *color != Color::TRANSPARENT
        })
        .map(|(origin, size, _, _)| (*origin, *size))
        .collect();
    let widest = candidates
        .iter()
        .map(|(_, size)| size.width)
        .fold(0.0_f64, f64::max);
    candidates.retain(|(_, size)| size.width > widest * 0.5);
    candidates.sort_by(|a, b| a.0.y.total_cmp(&b.0.y));
    candidates
}

/// A focused row's whole fill is the theme's `amber_faint` wash.
fn tree_focused_row(scene: &RecScene) -> Option<usize> {
    let colors = muxr::ui::palette::Palette::from_theme(&frust_glyph::baseline());
    let rows = tree_pane_rows(scene);
    scene
        .rounded_fills
        .iter()
        .filter(|(_, _, r, color)| {
            (*r - RADIUS_FULL).abs() < EPSILON && *color == colors.amber_faint
        })
        .find_map(|(origin, size, _, _)| rows.iter().position(|row| same(row, &(*origin, *size))))
}

fn reduced() -> Theme {
    let mut theme = frust_glyph::baseline();
    theme.motion.reduce_motion = true;
    theme
}

#[test]
fn the_card_prints_the_active_tabs_title_and_every_one_of_its_lines() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::mount();

    assert!(h.shows(DEVBOX_TITLE), "on screen: {:?}", h.values());
    let transcript = h
        .values()
        .into_iter()
        .find(|v| v.contains(DEVBOX_FIRST_LINE))
        .expect("the body publishes its lines");
    assert_eq!(
        transcript.lines().count(),
        LINES_PER_TAB,
        "the mock's four lines, verbatim: {transcript:?}"
    );
    assert!(transcript.contains("Active: active (running) since Jul 04 10:31:23"));
    assert!(transcript.contains("# staggered per-line reveal"));
    assert!(transcript.ends_with("ed@devbox:~$ "), "{transcript:?}");
}

#[test]
fn the_card_paints_glyphs_terminal_ink_and_not_the_mocks_hexes() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let scene = h.settle_motion(0.0);

    let ink = frust_glyph::baseline()
        .extension::<frust_glyph::GlyphInk>()
        .copied()
        .expect("the Glyph baseline carries GlyphInk");
    assert!(
        !rounded_in(&scene, ink.terminal_bg).is_empty(),
        "the card body is `GlyphInk::terminal_bg`"
    );
    assert!(
        !rounded_in(&scene, ink.terminal_head).is_empty(),
        "the status row is `GlyphInk::terminal_head`"
    );
    for hex in [
        Color::from_rgb8(0x1b, 0x18, 0x11),
        Color::from_rgb8(0x22, 0x1e, 0x17),
    ] {
        assert!(
            rounded_in(&scene, hex).is_empty(),
            "the canonical token wins over the mock's {hex:?}"
        );
    }
}

#[test]
fn the_card_sits_in_the_mocks_own_gutter_at_its_own_radius() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (origin, size, radius) = card(&h.frame());
    assert!((origin.x - WRAP_PAD).abs() < EPSILON, "{origin:?}");
    assert!(
        (size.width - (SURFACE.width - 2.0 * WRAP_PAD)).abs() < EPSILON,
        "{size:?}"
    );
    assert!((radius - CARD_RADIUS).abs() < EPSILON, "{radius}");
}

#[test]
fn the_status_row_is_as_short_as_the_expand_button_allows() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (band_origin, band_size) = status_band(&h.frame());
    let (card_origin, card_size, _) = card(&h.frame());
    assert!(
        (band_size.height - STATUS_ROW_H).abs() < EPSILON,
        "the title bar is the button plus its padding, and nothing more: \
         {band_size:?}"
    );
    assert!(
        (band_origin.y - card_origin.y).abs() < EPSILON
            && (band_size.width - card_size.width).abs() < EPSILON,
        "…and it still bands the card's whole top edge: {band_origin:?}/{band_size:?}"
    );
}

#[test]
fn the_padding_band_above_the_expand_button_toggles_fullscreen_too() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // The 28px button sits in a 40px row; a press 2px below the card's top edge must still fire it.
    let (band_origin, _) = status_band(&h.frame());
    let button = node_centers_by_label(&h.root, EXPAND)
        .first()
        .copied()
        .expect("the expand button publishes its own node");
    assert!(!app_nav().bar().fullscreen, "the bar starts lit");

    h.tap(Point::new(button.x, band_origin.y + 2.0));

    assert!(
        app_nav().bar().fullscreen,
        "the row's full height is the button's press target"
    );
}

#[test]
fn compact_fullscreen_adds_a_panes_button_left_of_the_exit_button() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    assert!(
        !node_labels(&h.root).iter().any(|l| l == PANES),
        "no panes control outside fullscreen"
    );
    h.tap_label(EXPAND);
    h.settle_motion(1_000.0);

    let panes = h.label_point(PANES);
    let exit = h.label_point(COLLAPSE);
    assert!(panes.x < exit.x, "{panes:?} sits left of {exit:?}");
    assert!((panes.y - exit.y).abs() < EPSILON, "…in the same row");
}

#[test]
fn the_panes_button_opens_the_side_sheet_and_closing_it_keeps_fullscreen() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.tap_label(EXPAND);
    h.settle_motion(2_000.0);
    assert_eq!(h.depth(), REST_DEPTH, "no sheet before the tap");

    h.tap_label(PANES);
    h.settle_motion(4_000.0);
    assert_eq!(
        h.depth(),
        REST_DEPTH + 1,
        "the panes button mounts the sheet"
    );
    assert!(h.shows(SHEET_TITLE), "{:?}", h.values());
    assert!(
        app_nav().bar().fullscreen,
        "the app bar stays hidden under the sheet"
    );

    let close = h.close_disc();
    h.tap(close);
    h.flush();
    assert_eq!(h.depth(), REST_DEPTH, "the sheet closed");
    assert!(
        app_nav().bar().fullscreen,
        "closing the sheet leaves fullscreen on"
    );
}

#[test]
fn a_tablet_in_fullscreen_has_no_panes_button_and_keeps_its_rail_exit() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);

    h.tap_label(EXPAND);
    h.settle_motion(1_000.0);

    let labels = node_labels(&h.root);
    assert!(!labels.iter().any(|l| l == PANES), "{labels:?}");
    assert!(labels.iter().any(|l| l == EXIT_IMMERSIVE), "{labels:?}");
}

fn runs_at(h: &mut Harness, at_ms: f64) -> usize {
    h.frame_at(at_ms).glyph_runs
}

#[test]
fn the_lines_stagger_in_one_after_another_rather_than_all_at_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // The driver arms on the first paint after the load; each frame is that many ms in.
    let none_yet = runs_at(&mut h, 0.0);
    let first_only = runs_at(&mut h, 150.0);
    let two_in = runs_at(&mut h, 290.0);
    let all_in = runs_at(&mut h, 700.0);
    let settled = runs_at(&mut h, 2_000.0);

    assert!(
        none_yet < first_only,
        "line 0's window opens at 60ms ({none_yet} → {first_only})"
    );
    assert!(
        first_only < two_in,
        "line 1's opens at 200ms ({first_only} → {two_in})"
    );
    assert!(
        two_in < all_in,
        "…and the last at 480ms ({two_in} → {all_in})"
    );
    assert_eq!(all_in, settled, "the reveal settles and stays settled");
}

#[test]
fn reduce_motion_shows_every_line_immediately() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::themed(reduced());

    let first = runs_at(&mut h, 0.0);
    let settled = runs_at(&mut h, 2_000.0);
    assert_eq!(first, settled, "a reduced reveal is not a reveal");
}

#[test]
fn the_caret_blinks_on_the_prompt_line() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    assert_eq!(carets(&h.frame_at(1_200.0)), 1, "lit in the first half");
    assert_eq!(carets(&h.frame_at(1_700.0)), 0, "…and dark in the second");
    assert_eq!(carets(&h.frame_at(2_300.0)), 1, "…and the cycle repeats");
}

#[test]
fn reduce_motion_freezes_the_caret_lit_rather_than_hiding_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::themed(reduced());

    // A caret is a position cue, so reduced motion must not hide it.
    for at in [1_200.0, 1_700.0, 2_300.0] {
        assert_eq!(carets(&h.frame_at(at)), 1, "still lit at {at}ms");
    }
}

#[test]
fn fullscreen_fills_the_screen_swaps_the_mark_and_hides_the_bar() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    assert!(!app_nav().bar().fullscreen, "the bar starts lit");
    h.tap_label(EXPAND);

    let scene = h.settle_motion(1_000.0);
    let (origin, size, radius) = card(&scene);

    assert!(
        origin.x.abs() < EPSILON,
        "the wrap padding is gone: {origin:?}"
    );
    assert!(
        (size.width - SURFACE.width).abs() < EPSILON,
        "the card is the screen: {size:?}"
    );
    assert!(radius < EPSILON, "…and square-cornered: {radius}");
    assert!(
        app_nav().bar().fullscreen,
        "the shell's bar was told to get out of the way"
    );
    assert!(
        h.shows(DEVBOX_TITLE),
        "the pane title is still there, in the same status row"
    );
    assert!(
        node_labels(&h.root).iter().any(|l| l == COLLAPSE),
        "labels: {:?}",
        node_labels(&h.root)
    );
}

#[test]
fn leaving_fullscreen_reverses_every_one_of_those_three() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.tap_label(EXPAND);
    h.settle_motion(1_000.0);
    h.tap_label(COLLAPSE);
    let scene = h.settle_motion(3_000.0);

    let (origin, size, radius) = card(&scene);
    assert!((origin.x - WRAP_PAD).abs() < EPSILON, "{origin:?}");
    assert!((size.width - (SURFACE.width - 2.0 * WRAP_PAD)).abs() < EPSILON);
    assert!((radius - CARD_RADIUS).abs() < EPSILON, "{radius}");
    assert!(!app_nav().bar().fullscreen, "the bar comes back");
    assert!(node_labels(&h.root).iter().any(|l| l == EXPAND));
}

#[test]
fn the_transition_is_animated_rather_than_a_snap() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.tap_label(EXPAND);
    // Partway through the 340ms `slow` transition; the third frame's layout moves the box.
    h.frame_at(0.0);
    h.frame_at(120.0);
    let (origin, _, radius) = card(&h.frame_at(130.0));
    assert!(
        origin.x > 0.0 && origin.x < WRAP_PAD,
        "mid-flight, not snapped: {origin:?}"
    );
    assert!(radius > 0.0 && radius < CARD_RADIUS, "{radius}");
}

#[test]
fn reduce_motion_snaps_both_ends_of_the_transition() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::themed(reduced());

    h.tap_label(EXPAND);
    // Two frames: nothing animates, so one layout pass turns the snapped value into a box.
    h.frame_at(1.0);
    let (origin, size, radius) = card(&h.frame_at(2.0));
    assert!(origin.x.abs() < EPSILON, "already there: {origin:?}");
    assert!((size.width - SURFACE.width).abs() < EPSILON);
    assert!(radius < EPSILON);

    h.tap_label(COLLAPSE);
    h.frame_at(3.0);
    let (origin, _, radius) = card(&h.frame_at(4.0));
    assert!(
        (origin.x - WRAP_PAD).abs() < EPSILON,
        "…and back: {origin:?}"
    );
    assert!((radius - CARD_RADIUS).abs() < EPSILON);
}

#[test]
fn unmounting_in_fullscreen_gives_the_bar_back() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.tap_label(EXPAND);
    h.settle_motion(1_000.0);
    assert!(app_nav().bar().fullscreen);

    // A back-nav straight out of fullscreen must not strand a hidden bar on the route below.
    h.unmount_screen();
    assert!(
        !app_nav().bar().fullscreen,
        "the screen's own cleanup clears the flag"
    );
}

#[test]
fn collapsing_visibly_leaves_the_edge_well_before_half_the_transition() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.tap_label(EXPAND);
    h.settle_motion(1_000.0);
    h.tap_label(COLLAPSE);

    // 80ms into the 340ms stage, where `spatial` overshoots: easing the progress clears the edge.
    h.frame_at(3_000.0);
    h.frame_at(3_080.0);
    let (origin, _, _) = card(&h.frame_at(3_081.0));
    assert!(
        origin.x > WRAP_PAD * 0.25,
        "the card should have visibly left the fullscreen edge by 80ms: {origin:?}"
    );
}

#[test]
fn entering_fullscreen_moves_monotonically_and_stays_in_bounds() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.tap_label(EXPAND);
    h.frame_at(0.0);
    let mut pads = Vec::new();
    for at_ms in [10.0, 40.0, 90.0, 150.0, 250.0, 400.0, 600.0] {
        pads.push(card(&h.frame_at(at_ms)).0.x);
    }
    for pair in pads.windows(2) {
        assert!(
            pair[1] <= pair[0] + EPSILON,
            "the wrap padding should only shrink while entering fullscreen: {pads:?}"
        );
    }
    for &pad in &pads {
        assert!(
            (-EPSILON..=WRAP_PAD + EPSILON).contains(&pad),
            "padding out of its own `[0, {WRAP_PAD}]` range: {pad} in {pads:?}"
        );
    }
}

#[test]
fn interrupting_an_expand_with_a_collapse_never_lets_the_card_climb_back() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.tap_label(EXPAND);
    h.frame_at(0.0);
    h.frame_at(60.0);
    let mid_expand = card(&h.frame_at(61.0)).0.x;
    assert!(
        mid_expand > EPSILON,
        "should still be off the fully-expanded edge: {mid_expand:?}"
    );

    // Seeding the new stage from the painted, already-eased value would overshoot and snap back.
    h.tap_label(COLLAPSE);
    let mut pads = vec![mid_expand];
    for at_ms in [
        1_000.0, 1_010.0, 1_040.0, 1_090.0, 1_150.0, 1_250.0, 1_400.0, 1_600.0,
    ] {
        pads.push(card(&h.frame_at(at_ms)).0.x);
    }
    for pair in pads.windows(2) {
        assert!(
            pair[1] + EPSILON >= pair[0],
            "the wrap padding regressed after the interrupt (fullscreen \
             progress climbed back up rather than only ever collapsing \
             further): {pads:?}"
        );
    }

    let (origin, size, radius) = card(&h.settle_motion(3_000.0));
    assert!((origin.x - WRAP_PAD).abs() < EPSILON, "{origin:?}");
    assert!((size.width - (SURFACE.width - 2.0 * WRAP_PAD)).abs() < EPSILON);
    assert!((radius - CARD_RADIUS).abs() < EPSILON, "{radius}");
}

#[test]
fn the_terminal_route_carries_the_kebab_and_it_opens_the_sheet_directly() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    assert_eq!(
        route_meta("/terminal").and_then(|meta| meta.action),
        Some(BarAction::TerminalMenu),
        "the shell's bar is what mounts the menu button"
    );

    let (before, after) = h.open_sheet();
    assert_eq!(h.depth(), REST_DEPTH + 1, "the sheet is a pushed page");
    let fills = |scene: &RecScene| -> Vec<(Point, Size)> {
        scene.fills.iter().map(|(o, s, _)| (*o, *s)).collect()
    };
    assert!(
        !added(&fills(&before), &fills(&after)).is_empty(),
        "…and it painted its own panel"
    );
}

#[test]
fn opening_the_sheet_shows_the_tree_pre_expanded_on_the_sessions_own_tab() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (_before, after) = h.open_sheet();
    assert_eq!(h.depth(), REST_DEPTH + 1, "the sheet is a pushed page");

    // zellij has no spaces: its tabs promote to the card shape and nothing wears the tab rect.
    assert!(
        rounded_fills_at(&after, TREE_TAB_RADIUS).is_empty(),
        "a zellij session's tabs never wear the nested tab rect"
    );

    assert!(h.shows("ed@devbox zsh"), "devbox's second pane is visible");
}

#[test]
fn a_herdr_session_is_the_only_one_offered_spaces() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    let (_before, after) = h.open_sheet();

    assert!(
        !rounded_fills_at(&after, TREE_TAB_RADIUS).is_empty(),
        "herdr's tabs wear the nested tab rect, unlike a promoted zellij one"
    );
}

#[test]
fn a_tab_row_browses_locally_and_leaves_the_card_and_the_sheet_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.open_sheet();
    // The active tab opens pre-expanded and its header collapses it, so this taps a collapsed one.
    let point = h.value_point("btop");
    h.tap(point);
    h.settle();

    assert!(h.shows("ed@workbench zsh"), "btop's panes are now visible");
    assert!(
        h.shows(DEVBOX_TITLE),
        "the card is the session's, not the browse's"
    );
    assert!(!h.shows(BTOP_TITLE));
    h.pump_for(2 * CLOSE_DELAY);
    assert_eq!(
        h.depth(),
        REST_DEPTH + 1,
        "a browse is not a destination: only a pane row dismisses"
    );
}

#[test]
fn a_pane_row_commits_the_browse_and_then_closes_the_sheet() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.open_sheet();
    h.tap(h.value_point("btop"));
    let browsed = h.settle();
    h.tap(center(tree_pane_rows(&browsed)[0]));

    h.pump_until_values("the btop tab", |values| {
        values.iter().any(|v| v == BTOP_TITLE)
    });
    assert!(h.shows(BTOP_TITLE), "the card followed the pane");
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);
    assert_eq!(h.depth(), REST_DEPTH, "…and the sheet closed behind it");
}

#[test]
fn a_landed_herdr_commit_closes_the_sheet_on_the_documented_beat() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    let (_before, after) = h.open_sheet();
    // On herdr `FocusPane` is the whole move: the server re-points this connection's relay.
    h.tap(center(tree_pane_rows(&after)[1]));
    assert_eq!(
        h.depth(),
        REST_DEPTH + 1,
        "the beat starts with the sheet still up"
    );

    // `pump_until_closed` allows two `CLOSE_DELAY` beats; a commit waiting behind the verb fails.
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);
    assert_eq!(h.depth(), REST_DEPTH, "…and the beat is the documented one");

    let (_, reopened) = h.open_sheet();
    assert_eq!(
        tree_focused_row(&reopened),
        Some(1),
        "the optimistic badge stands, and the layout read behind the reopen \
         confirms it rather than reverting it"
    );
}

#[test]
fn the_focused_badge_follows_a_commit_the_next_time_the_sheet_opens() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (_, opened) = h.open_sheet();
    assert_eq!(
        tree_focused_row(&opened),
        Some(0),
        "the mock seeds the first pane focused"
    );

    h.tap(center(tree_pane_rows(&opened)[1]));
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);

    let (_, reopened) = h.open_sheet();
    assert_eq!(
        tree_focused_row(&reopened),
        Some(1),
        "the badge moved with the commit, without waiting for a reload"
    );
}

#[test]
fn a_pane_commit_replays_the_reveal_from_the_top() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let settled = runs_at(&mut h, 2_000.0);

    h.open_sheet();
    h.tap(h.value_point("btop"));
    let browsed = h.settle();
    h.tap(center(tree_pane_rows(&browsed)[0]));
    h.pump_until_values("the btop tab", |values| {
        values.iter().any(|v| v == BTOP_TITLE)
    });
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);

    let restarted = runs_at(&mut h, 0.0);
    let refilled = runs_at(&mut h, 2_000.0);
    assert!(
        restarted < refilled,
        "the reveal replayed ({restarted} → {refilled})"
    );
    assert_eq!(refilled, settled, "…to the same four lines");
}

#[test]
fn the_sheets_close_button_dismisses_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.open_sheet();
    assert_eq!(h.depth(), REST_DEPTH + 1);
    let scene = h.settle_motion(0.0);
    let close = scene
        .rounded
        .iter()
        .copied()
        .find(|(_, size)| {
            (size.width - 26.0).abs() < EPSILON && (size.height - 26.0).abs() < EPSILON
        })
        .expect("the sheet's 26px close disc");
    h.tap(center(close));
    h.flush();

    assert_eq!(h.depth(), REST_DEPTH, "the sheet closed");
    assert!(!node_labels(&h.root).iter().any(|l| l == CLOSE));
}

#[test]
fn the_scrim_dismisses_the_sheet_too() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.open_sheet();
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), REST_DEPTH);
}

// A pane row leaves the sheet up for `CLOSE_DELAY`; each test fires a second dismissal inside
// it and asserts the sheet closes once and the page under it survives.

#[test]
fn a_second_pane_row_inside_the_close_beat_still_closes_the_sheet_only_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let floor = h.push_route_floor();

    h.open_sheet();
    assert_eq!(h.depth(), floor + 1, "the sheet is up over the route");
    h.tap(h.value_point("btop"));
    let scene = h.settle();
    h.tap(center(tree_pane_rows(&scene)[0]));
    h.tap(h.value_point("lumen-api"));
    let scene = h.settle();
    h.tap(center(tree_pane_rows(&scene)[0]));

    // The commit is single-flight, so the second row is dropped, not raced.
    h.pump_until_values("the btop tab", |values| {
        values.iter().any(|v| v == BTOP_TITLE)
    });
    // `<=`: two stacked closes can land in one rebuild, and the test must report that.
    h.pump_until_closed("the sheet to close", |h| h.depth() <= floor);
    h.pump_for(2 * CLOSE_DELAY);

    assert_eq!(
        h.depth(),
        floor,
        "no second close popped the route under the sheet"
    );
    assert!(
        h.shows(BTOP_TITLE) && !h.shows(LUMEN_TITLE),
        "the dropped row moved neither the card nor the session: {:?}",
        h.values()
    );
}

#[test]
fn a_scrim_dismissal_inside_the_close_beat_leaves_the_route_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let floor = h.push_route_floor();

    let (_before, after) = h.open_sheet();
    h.tap(center(tree_pane_rows(&after)[0]));
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), floor, "the scrim closed it, once");

    h.pump_for(2 * CLOSE_DELAY);
    assert_eq!(
        h.depth(),
        floor,
        "…and the row's late close found the sheet already gone"
    );
}

#[test]
fn the_close_button_inside_the_close_beat_leaves_the_route_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let floor = h.push_route_floor();

    let (_before, after) = h.open_sheet();
    h.tap(center(tree_pane_rows(&after)[0]));
    let close = h.close_disc();
    h.tap(close);
    h.flush();
    assert_eq!(h.depth(), floor, "the close button closed it, once");

    h.pump_for(2 * CLOSE_DELAY);
    assert_eq!(
        h.depth(),
        floor,
        "…and the row's late close found the sheet already gone"
    );
    assert!(
        !node_labels(&h.root).iter().any(|l| l == CLOSE),
        "the sheet is gone, not reopened"
    );
}

#[test]
fn browsing_a_non_selected_space_lists_its_tabs_without_switching_the_connection() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    h.open_sheet();
    // `zelli_mobile` is not the connected space (`forgekit`): expanding it is a read-only peek.
    h.tap(h.value_point("zelli_mobile"));
    h.pump_until("zelli_mobile's own tab to auto-expand", |h| {
        h.shows("ed@zelli zsh")
    });

    let selected: Vec<String> = debug_controller()
        .expect("mounted")
        .spaces
        .get_untracked()
        .into_iter()
        .filter(|space| space.selected)
        .map(|space| space.name)
        .collect();
    assert_eq!(
        selected,
        ["forgekit"],
        "the browse read the space's tabs and moved nothing on the server"
    );
}

#[test]
fn an_empty_non_selected_space_shows_the_hint_row_once_browsed() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    h.open_sheet();
    assert!(
        !h.shows("no tabs yet"),
        "quill is collapsed and unread before the tap"
    );

    h.tap(h.value_point("quill"));
    h.pump_until("quill to be read", |h| h.shows("no tabs yet"));
}

#[test]
fn the_add_tab_row_under_an_empty_browsed_space_switches_onto_it_first() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    app_toasts().clear();

    h.open_sheet();
    let floor = h.depth();

    // `quill` has no tabs, so its add-tab row is the only way onto it.
    h.tap(h.value_point("quill"));
    h.pump_until("quill to be read", |h| h.shows("no tabs yet"));
    h.settle_motion(0.0);

    // The first add-tab row in tree order is quill's; the two behind it are collapsed.
    h.tap(h.add_tab_row_point());
    h.pump_until("the switch to land and the new-tab dialog to open", |h| {
        h.depth() == floor + 1
    });

    let selected: Vec<String> = debug_controller()
        .expect("mounted")
        .spaces
        .get_untracked()
        .into_iter()
        .filter(|space| space.selected)
        .map(|space| space.name)
        .collect();
    assert_eq!(
        selected,
        ["quill"],
        "the create row switched onto the space it sits under, so the tab it \
         is about to name lands there rather than in the space left behind"
    );

    h.tap_label(NEW_TAB_SUBMIT_LABEL);
    h.flush();
    h.await_toast(INTERNAL_FAILURE_TOAST);
}

#[test]
fn unmounting_mid_browse_leaves_the_auto_expansion_nothing_to_panic_on() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    h.open_sheet();
    // Both resume against signals whose owner is gone; a plain read there panics on this pump.
    h.tap(h.value_point("zelli_mobile"));
    h.unmount_screen();
    h.pump_for(Duration::from_millis(600));

    assert!(
        debug_controller().is_none(),
        "the screen really did unmount, so the resumed task really did read a \
         disposed owner"
    );
}

#[test]
fn a_cross_space_commit_switches_the_connection_and_closes_the_sheet_exactly_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    h.open_sheet();
    assert_eq!(h.depth(), REST_DEPTH + 1, "the sheet is up");

    // Waited on through the controller's signal: the pinned-zero paint clock never retracts
    // forgekit's reveal, so its stale rows sit where this browse's rows land.
    h.tap(h.value_point("zelli_mobile"));
    h.pump_until("zelli_mobile's own tab to auto-expand", |_| {
        debug_controller()
            .expect("mounted")
            .expanded_tab
            .get_untracked()
            .as_deref()
            == Some("logs")
    });
    let scene = h.settle_motion(0.0);

    h.tap(center(tree_pane_rows(&scene)[0]));

    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);
    assert_eq!(
        h.depth(),
        REST_DEPTH,
        "one close, and no double pop of the route under it"
    );

    // The tab world and the space list's `selected` flag both move with the landed `switch_space`.
    let controller = debug_controller().expect("mounted");
    assert_eq!(
        controller.active_tab.get_untracked(),
        "logs",
        "the commit's own `switch_space` landed — the connection followed it"
    );
    let selected: Vec<String> = controller
        .spaces
        .get_untracked()
        .into_iter()
        .filter(|space| space.selected)
        .map(|space| space.name)
        .collect();
    assert_eq!(
        selected,
        ["zelli_mobile"],
        "…and the sheet's own space list followed it too"
    );
    let keys: Vec<String> = controller
        .tabs
        .get_untracked()
        .iter()
        .map(|tab| tab.key.clone())
        .collect();
    assert_eq!(keys, ["logs", "worker"], "…onto zelli_mobile's own tabs");
}

#[test]
fn unmounting_mid_commit_leaves_the_switch_round_trip_nothing_to_panic_on() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    h.open_sheet();
    h.tap(h.value_point("zelli_mobile"));
    h.pump_until("zelli_mobile's own tab to auto-expand", |_| {
        debug_controller()
            .expect("mounted")
            .expanded_tab
            .get_untracked()
            .as_deref()
            == Some("logs")
    });
    let scene = h.settle_motion(0.0);

    // The commit parks in its `switch_space` round trip while the screen goes away under it.
    h.tap(center(tree_pane_rows(&scene)[0]));
    pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    h.unmount_screen();
    h.pump_for(Duration::from_millis(600));

    assert!(
        debug_controller().is_none(),
        "the screen really did unmount, so the resumed commit really did read a \
         disposed owner"
    );
}

#[test]
fn a_back_press_dismisses_the_sheet_once_and_resets_the_browse_for_the_next_open() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    let floor = h.push_route_floor();

    h.open_sheet();
    assert_eq!(h.depth(), floor + 1, "the sheet is up over the route");

    h.tap(h.value_point("zelli_mobile"));
    h.pump_until("zelli_mobile's own tab to auto-expand", |h| {
        h.shows("ed@zelli zsh")
    });
    assert!(
        h.shows("2 tabs"),
        "the browse populated zelli_mobile's own tab count: {:?}",
        h.values()
    );

    frust::push_back_press();
    h.settle();

    assert_eq!(
        h.depth(),
        floor,
        "the back press staged-dismissed the sheet — and only the sheet, \
         leaving the route under it alone"
    );

    h.open_sheet();
    assert!(
        !h.shows("2 tabs"),
        "the dismissal reset the browse cache: {:?}",
        h.values()
    );
}

#[test]
fn the_footers_rename_row_round_trips_into_the_sessions_repository() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let before = h.frame();
    h.open_sheet();
    let after = h.tap_footer_rename();
    assert_eq!(h.depth(), REST_DEPTH + 1, "the rename dialog is up");

    let dialog = added(&before.rounded, &after.rounded);
    let content_w = panel(&dialog).1.width - 2.0 * PANEL_PADDING;
    let field = by_width(&dialog, content_w).expect("the name field");
    h.tap(center(field));
    // The dialog seeds the current name, so clear it before typing.
    for _ in 0..SESSION_NAME.len() {
        h.press(NamedKey::Backspace);
    }
    h.type_text("dev-2");
    h.press(NamedKey::Enter);
    h.flush();

    assert_eq!(h.depth(), REST_DEPTH, "a submit closes the dialog");
    h.await_repository_name("dev-2");
}

fn session_named(
    repository: &muxr::features::sessions::di::SharedSessionsRepository,
    name: &str,
) -> bool {
    let listed = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
        .block_on(repository.list_sessions());
    listed
        .map(|sessions| sessions.iter().any(|session| session.name == name))
        .unwrap_or(false)
}

#[test]
fn the_footers_detach_row_pops_the_terminal_route() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    // The record-only navigator never drains, so neither `Nav::bar().meta` nor `depth()` moves;
    // the detach toast, raised with the pop, is the one observable landing.
    h.open_sheet();
    h.tap_footer_detach();
    let deadline = Instant::now() + PUMP_DEADLINE;
    while app_toasts().pending_untracked().is_empty() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the detach"
        );
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    assert_eq!(
        app_toasts().pending_untracked(),
        vec![DETACHED_TOAST.to_string()],
        "the detach landed, popping /terminal"
    );
}

#[test]
fn a_second_detach_while_the_first_is_in_flight_pops_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    // Neither `tap_footer_detach` nor `settle` pumps the task queue: both requests precede landing.
    h.open_sheet();
    h.tap_footer_detach();
    h.open_sheet();
    h.tap_footer_detach();

    let deadline = Instant::now() + PUMP_DEADLINE;
    while app_toasts().pending_untracked().is_empty() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the detach"
        );
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    h.pump_for(2 * CLOSE_DELAY);

    let detached = app_toasts()
        .pending_untracked()
        .into_iter()
        .filter(|message| message == DETACHED_TOAST)
        .count();
    assert_eq!(detached, 1, "one detach, one pop, one toast");
}

/// The mock backend always answers `false`, so no mock connect produces a read-only credential.
fn make_read_only(h: &mut Harness) {
    expect_connection_controller().is_read_only.set(true);
    h.frame();
}

#[test]
fn a_read_only_credential_withdraws_the_trees_three_create_rows() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    make_read_only(&mut h);

    h.open_sheet();
    assert!(
        !h.shows(ADD_SPACE_LABEL) && !h.shows(ADD_TAB_LABEL) && !h.shows(NEW_PANE_LABEL),
        "a read-only session is offered no create row: {:?}",
        h.values()
    );
    h.tap(h.value_point("btop"));
    h.settle();
    assert!(h.shows("ed@workbench zsh"), "the browse moved");
}

#[test]
fn a_read_only_credential_navigates_across_spaces() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    app_toasts().clear();
    make_read_only(&mut h);

    h.open_sheet();
    h.tap(h.value_point("zelli_mobile"));
    // Waited on through the controller's signal, for the stale-reveal reason above.
    h.pump_until("zelli_mobile's own tab to auto-expand", |_| {
        debug_controller()
            .expect("mounted")
            .expanded_tab
            .get_untracked()
            .as_deref()
            == Some("logs")
    });
    // Settled three times: one settle still paints forgekit's pane row under the tap.
    h.settle_motion(0.0);
    h.settle_motion(2_000.0);
    let scene = h.settle_motion(4_000.0);
    h.tap(center(tree_pane_rows(&scene)[0]));
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);

    // `SwitchSpace`, `GoToTab` and `FocusPane` carry no read-only gate on the wire.
    let controller = debug_controller().expect("mounted");
    assert_eq!(
        controller.active_tab.get_untracked(),
        "logs",
        "the commit's `switch_space` landed — the connection followed it"
    );
    let selected: Vec<String> = controller
        .spaces
        .get_untracked()
        .into_iter()
        .filter(|space| space.selected)
        .map(|space| space.name)
        .collect();
    assert_eq!(
        selected,
        ["zelli_mobile"],
        "…and the sheet's own space list followed it"
    );
}

#[test]
fn a_read_only_credential_navigates_across_tabs() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();
    make_read_only(&mut h);

    let (_before, after) = h.open_sheet();

    h.tap(center(tree_pane_rows(&after)[1]));
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);
    assert!(
        !app_toasts()
            .pending_untracked()
            .iter()
            .any(|message| message == READ_ONLY_TOAST),
        "a same-tab commit is not a mutation: {:?}",
        app_toasts().pending_untracked()
    );

    h.open_sheet();
    h.tap(h.value_point("btop"));
    let scene = h.settle();
    h.tap(center(tree_pane_rows(&scene)[0]));

    h.pump_until_values("the btop tab", |values| {
        values.iter().any(|v| v == BTOP_TITLE)
    });
    assert!(h.shows(BTOP_TITLE), "the commit's `go_to_tab` landed");
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);
    assert_eq!(h.depth(), REST_DEPTH, "…and the sheet closed behind it");
}

#[test]
fn a_read_only_credential_navigates_panes_on_herdr() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    app_toasts().clear();
    make_read_only(&mut h);

    let (_before, after) = h.open_sheet();

    h.tap(center(tree_pane_rows(&after)[1]));
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);
    assert_eq!(h.depth(), REST_DEPTH, "the sheet closed");

    let (_, reopened) = h.open_sheet();
    assert_eq!(
        tree_focused_row(&reopened),
        Some(1),
        "the focused pane moved with the commit"
    );
}

#[test]
fn a_read_only_credential_refuses_the_rename_dialog_and_says_why() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();
    make_read_only(&mut h);

    h.open_sheet();
    h.tap_footer_rename();
    h.flush();

    assert_eq!(
        h.depth(),
        REST_DEPTH,
        "the rename dialog never opened over a session that cannot be renamed"
    );
    assert!(
        app_toasts()
            .pending_untracked()
            .iter()
            .any(|message| message == READ_ONLY_TOAST),
        "the row stays on screen and says why: {:?}",
        app_toasts().pending_untracked()
    );
}

#[test]
fn an_orphaned_sheets_footer_presses_are_quiet_no_ops() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    h.open_sheet();
    let floor = h.depth();
    // The sheet outlives the screen that pushed it, and its footer rebuilds whole.
    h.unmount_screen();
    h.tap_footer_rename();
    h.flush();

    assert_eq!(h.depth(), floor, "no dialog over the orphaned sheet");
    assert!(
        !h.shows(RENAME_DIALOG_TITLE),
        "…and nothing asking for a name that has no session left to write it \
         to: {:?}",
        h.values()
    );

    h.tap_footer_detach();
    h.pump_for(2 * CLOSE_DELAY);
    assert!(
        !app_toasts()
            .pending_untracked()
            .iter()
            .any(|message| message == DETACHED_TOAST),
        "no detach went out either: {:?}",
        app_toasts().pending_untracked()
    );
}

#[test]
fn every_affordance_on_this_screen_is_a_vector_and_announces_a_word() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let scene = h.settle_motion(0.0);

    assert!(
        !scene.paths.is_empty(),
        "the `⛶` is a path, not a font glyph"
    );
    let labels = node_labels(&h.root);
    assert!(labels.iter().any(|l| l == EXPAND), "labels: {labels:?}");
    for label in &labels {
        assert!(
            label.chars().all(|c| c.is_ascii() || c == '\u{2026}'),
            "{label:?} looks like a mark rather than a name"
        );
    }
}

#[test]
fn a_frame_of_this_screen_never_paints_a_fallback_glyph_as_chrome() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::mount();

    // `terminal.html` sets these five marks as text; none is in the bundled faces.
    for banned in ["\u{26f6}", "\u{2921}", "\u{25a4}", "\u{23fb}", "\u{22ee}"] {
        assert!(
            !h.values().iter().any(|v| v.contains(banned)),
            "{banned:?} is set as text somewhere: {:?}",
            h.values()
        );
    }
}

#[test]
fn the_screen_reads_the_attached_session_off_the_route_it_was_pushed_with() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // The rename dialog seeds the current name, which the screen only knows from `?session=`.
    let before = h.frame();
    h.open_sheet();
    let after = h.tap_footer_rename();
    assert_eq!(
        h.depth(),
        REST_DEPTH + 1,
        "a screen with no attached session opens no dialog at all"
    );

    let dialog = added(&before.rounded, &after.rounded);
    let content_w = panel(&dialog).1.width - 2.0 * PANEL_PADDING;
    h.tap(center(
        by_width(&dialog, content_w).expect("the name field"),
    ));
    h.press(NamedKey::Backspace);
    h.press(NamedKey::Enter);
    h.flush();

    let trimmed: String = SESSION_NAME[..SESSION_NAME.len() - 1].to_string();
    h.await_repository_name(&trimmed);
}

#[test]
fn a_frame_needs_no_settling_before_the_first_load_lands() {
    let _g = serial();
    let _ambient = setup();
    // The screen paints its card from its first frame, before any repository answers.
    let state = MuxrApp.init();
    let nav: NavigatorController<AppState> = NavigatorController::new();
    let record_only: NavigatorController<AppState> = NavigatorController::new();
    let _router = install_app_router(&record_only, &nav, expect_connection_controller());
    let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
    root.set_theme(Box::new(frust_glyph::baseline()));
    let logic_nav = nav.clone();
    let mut logic: Logic =
        Box::new(move |_state: &mut AppState| AnyView::new(terminal_page(logic_nav.clone())));
    let mut tcx = TextContext::new();
    let mut state = state;

    let scene = frame(&mut root, &mut logic, &mut state, &mut tcx);
    let ink = frust_glyph::baseline()
        .extension::<frust_glyph::GlyphInk>()
        .copied()
        .expect("GlyphInk");
    assert!(
        !rounded_in(&scene, ink.terminal_bg).is_empty(),
        "the card is there from frame one"
    );
    let _ = FrameTime::ZERO;
}

const PHONE_LANDSCAPE_H: f64 = 390.0;

#[test]
fn wide_metrics_grow_the_wrap_padding_but_keep_the_card_full_bleed() {
    let _g = serial();
    let _ambient = setup();
    let mut h =
        Harness::mount_with_metrics(Some(support::metrics_at(SURFACE.width, PHONE_LANDSCAPE_H)));

    let (origin, size, _) = card(&h.frame());
    assert!(
        (origin.x - WIDE_WRAP_PAD).abs() < EPSILON,
        "the wrap padding grew to the wide target: {origin:?}"
    );
    assert!(
        (size.width - (SURFACE.width - 2.0 * WIDE_WRAP_PAD)).abs() < EPSILON,
        "…and the card narrowed by the same amount, not clamped to a fixed \
         column: {size:?}"
    );
    // Mounted without the shell scaffold, the stage starts where the bar ends; the step-up is left.
    assert!(
        (origin.y - PORTRAIT_TOP_GAP).abs() < EPSILON,
        "the same step-up on top, unchanged: {origin:?}"
    );
}

#[test]
fn fullscreen_stays_edge_to_edge_even_with_wide_metrics() {
    let _g = serial();
    let _ambient = setup();
    let mut h =
        Harness::mount_with_metrics(Some(support::metrics_at(SURFACE.width, PHONE_LANDSCAPE_H)));

    h.tap_label(EXPAND);
    // Mid-flight: the wide padding must change without rebuilding the stage, or the card snaps.
    h.frame_at(0.0);
    h.frame_at(120.0);
    let (mid, _, _) = card(&h.frame_at(130.0));
    assert!(
        mid.x > 0.0 && mid.x < WIDE_WRAP_PAD,
        "mid-flight, not snapped: {mid:?}"
    );

    let (origin, size, radius) = card(&h.settle_motion(1_000.0));

    assert!(
        origin.x.abs() < EPSILON,
        "wide metrics never widen fullscreen's own zero pad: {origin:?}"
    );
    assert!(
        (size.width - SURFACE.width).abs() < EPSILON,
        "…still the full screen: {size:?}"
    );
    assert!(radius < EPSILON);
}

#[test]
fn narrow_metrics_leave_the_wrap_padding_unchanged_like_no_metrics_at_all() {
    let _g = serial();
    let _ambient = setup();
    // Narrow published metrics on a full-width paint surface: the branch keys on the published one.
    let mut h = Harness::mount_with_metrics(Some(support::metrics_at(400.0, SURFACE.height)));

    let (origin, size, radius) = card(&h.frame());
    assert!(
        (origin.x - WRAP_PAD).abs() < EPSILON,
        "narrow metrics take the same unwidened branch as no metrics: {origin:?}"
    );
    assert!((size.width - (SURFACE.width - 2.0 * WRAP_PAD)).abs() < EPSILON);
    assert!((radius - CARD_RADIUS).abs() < EPSILON);
}

#[test]
fn narrow_metrics_lift_the_card_off_the_bar_by_the_portrait_gap() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_with_metrics(Some(support::metrics_at(400.0, SURFACE.height)));

    let (origin, _, _) = card(&h.frame());
    assert!(
        (origin.y - PORTRAIT_TOP_GAP).abs() < EPSILON,
        "the card breathes by the portrait step-up: {origin:?}"
    );

    h.tap_label(EXPAND);
    let (origin, _, _) = card(&h.settle_motion(1_000.0));
    assert!(
        origin.y.abs() < EPSILON,
        "fullscreen keeps the card flush against the top edge: {origin:?}"
    );
}

// The create affordances: both rows open a dialog, only the space one keeps a non-blank guard,
// and the sheet stays up however the repository answers.

#[test]
fn the_add_space_row_opens_an_empty_dialog_and_a_typed_label_reaches_the_repository() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    app_toasts().clear();

    let (_before, after) = h.open_sheet();
    let floor = h.depth();

    h.tap(h.add_space_row_point());
    let dialog_frame = h.settle();
    assert_eq!(h.depth(), floor + 1, "the create-space dialog opened");

    // Prefilled empty: an unmodified Enter submits nothing (the dialog's non-blank guard).
    h.press(NamedKey::Enter);
    h.flush();
    assert_eq!(h.depth(), floor + 1, "nothing to submit yet — still open");

    let dialog = added(&after.rounded, &dialog_frame.rounded);
    let content_w = panel(&dialog).1.width - 2.0 * PANEL_PADDING;
    let field = by_width(&dialog, content_w).expect("the name field");
    h.tap(center(field));
    h.type_text("new hq #1");
    h.press(NamedKey::Enter);
    h.flush();

    assert_eq!(
        h.depth(),
        floor,
        "the submit closed the dialog, back onto the sheet"
    );
    h.await_toast(INTERNAL_FAILURE_TOAST);
}

#[test]
fn the_add_tab_row_opens_an_empty_dialog_and_a_typed_name_reaches_the_repository() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    let (_before, after) = h.open_sheet();
    let floor = h.depth();

    h.tap(h.add_tab_row_point());
    let dialog_frame = h.settle();
    assert_eq!(h.depth(), floor + 1, "the new-tab dialog opened");

    let dialog = added(&after.rounded, &dialog_frame.rounded);
    let content_w = panel(&dialog).1.width - 2.0 * PANEL_PADDING;
    let field = by_width(&dialog, content_w).expect("the name field");
    h.tap(center(field));
    h.type_text("build");
    h.press(NamedKey::Enter);
    h.flush();

    assert_eq!(
        h.depth(),
        floor,
        "the submit closed the dialog, back onto the sheet"
    );
    h.await_toast(INTERNAL_FAILURE_TOAST);
}

#[test]
fn a_blank_confirm_on_the_add_tab_dialog_still_submits_unnamed() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    let (_before, after) = h.open_sheet();
    let floor = h.depth();

    h.tap(h.add_tab_row_point());
    let dialog_frame = h.settle();
    assert_eq!(h.depth(), floor + 1, "the new-tab dialog opened");

    // A blank tab name is this create's valid default: the field stays untyped and Enter submits.
    let dialog = added(&after.rounded, &dialog_frame.rounded);
    let content_w = panel(&dialog).1.width - 2.0 * PANEL_PADDING;
    let field = by_width(&dialog, content_w).expect("the name field");
    h.tap(center(field));
    h.press(NamedKey::Enter);
    h.flush();

    assert_eq!(
        h.depth(),
        floor,
        "the blank confirm still closed the dialog, back onto the sheet"
    );
    h.await_toast(INTERNAL_FAILURE_TOAST);
}

#[test]
fn cancelling_the_add_tab_dialog_reaches_the_repository_not_at_all() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    let (_before, after) = h.open_sheet();
    let floor = h.depth();

    h.tap(h.add_tab_row_point());
    let dialog_frame = h.settle();
    assert_eq!(h.depth(), floor + 1, "the new-tab dialog opened");

    // Escape cancels only while the dialog holds focus; a focused field would take it.
    let dialog = added(&after.rounded, &dialog_frame.rounded);
    let (origin, _) = panel(&dialog);
    h.tap(Point::new(origin.x + 10.0, origin.y + 10.0));
    h.press(NamedKey::Escape);
    h.flush();

    assert_eq!(h.depth(), floor, "escape dismissed the dialog");
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "cancel never reached the repository, so nothing failed either: {:?}",
        app_toasts().pending_untracked()
    );
}

#[test]
fn a_tap_on_the_new_pane_row_opens_no_dialog_and_leaves_the_sheet_up() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    h.open_sheet();
    let floor = h.depth();
    let point = h.new_pane_row_point();

    h.tap(point);
    h.settle();

    assert_eq!(h.depth(), floor, "no dialog on a tap either");
    h.await_toast(INTERNAL_FAILURE_TOAST);
    assert_eq!(h.depth(), floor, "…and the sheet stayed up");
}

// The rename gesture's tap region is disjoint from the chip's main one.

#[test]
fn holding_a_space_row_renames_it_without_toggling_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    app_toasts().clear();

    h.open_sheet();
    let floor = h.depth();
    let before = h.frame();

    // `quill` is collapsed, empty and non-selected; press-and-hold opens its rename dialog.
    h.long_press(h.value_point("quill"));
    let after = h.settle();

    assert_eq!(h.depth(), floor + 1, "the rename dialog opened");
    assert!(h.shows("quill"), "prefilled with the space's current name");

    let dialog = added(&before.rounded, &after.rounded);
    let content_w = panel(&dialog).1.width - 2.0 * PANEL_PADDING;
    let field = by_width(&dialog, content_w).expect("the name field");
    h.tap(center(field));
    h.type_text("renamed");
    h.press(NamedKey::Enter);
    h.flush();
    assert_eq!(h.depth(), floor, "the submit closed the dialog");
    h.await_toast(INTERNAL_FAILURE_TOAST);
    h.settle();
    assert!(
        !h.shows("no tabs yet"),
        "the hold never also expanded the empty space: {:?}",
        h.values()
    );
}

#[test]
fn holding_a_tab_row_renames_it_without_toggling_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    h.open_sheet();
    let floor = h.depth();
    let before = h.frame();

    h.long_press(h.value_point("btop"));
    let after = h.settle();

    assert_eq!(h.depth(), floor + 1, "the rename dialog opened");
    assert!(h.shows("btop"), "prefilled with the tab's current label");

    let dialog = added(&before.rounded, &after.rounded);
    let content_w = panel(&dialog).1.width - 2.0 * PANEL_PADDING;
    let field = by_width(&dialog, content_w).expect("the name field");
    h.tap(center(field));
    h.type_text("renamed");
    h.press(NamedKey::Enter);
    h.flush();
    assert_eq!(h.depth(), floor, "the submit closed the dialog");
    h.await_toast(INTERNAL_FAILURE_TOAST);
}

#[test]
fn holding_a_pane_row_renames_it_without_committing_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    let (_, opened) = h.open_sheet();
    let floor = h.depth();
    // The mock seeds the first pane focused, so committing the second is an observable move.
    let point = center(tree_pane_rows(&opened)[1]);
    h.long_press(point);
    let after = h.settle();

    assert_eq!(h.depth(), floor + 1, "the rename dialog opened");
    assert!(h.shows("ed@devbox zsh"), "prefilled with the pane's title");

    let dialog = added(&opened.rounded, &after.rounded);
    let content_w = panel(&dialog).1.width - 2.0 * PANEL_PADDING;
    let field = by_width(&dialog, content_w).expect("the name field");
    h.tap(center(field));
    h.type_text("renamed");
    h.press(NamedKey::Enter);
    h.flush();
    assert_eq!(h.depth(), floor, "the submit closed the dialog");
    h.await_toast(INTERNAL_FAILURE_TOAST);
    h.pump_for(2 * CLOSE_DELAY);
    assert_eq!(
        h.depth(),
        floor,
        "the edit tap never committed the pane it names — the sheet is still up"
    );
    assert_eq!(
        tree_focused_row(&h.frame()),
        Some(0),
        "…and the focused pane never moved"
    );

    let scene = h.settle_motion(0.0);
    h.tap(center(tree_pane_rows(&scene)[1]));
    // `floor - 1`: the sheet is already at `floor`, so this waits for its own pop.
    h.pump_until_closed("the sheet to close", |h| h.depth() == floor - 1);
    assert_eq!(h.depth(), floor - 1, "a plain tap still commits and closes");
}

#[test]
fn a_drag_past_slop_mid_hold_cancels_both_the_rename_and_the_toggle() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    h.open_sheet();
    let floor = h.depth();
    // A landed toggle would show the hint row and a landed hold the dialog; a drag fires neither.
    let point = h.value_point("quill");

    h.drag_past_slop_then_release(point);
    h.settle();

    assert_eq!(
        h.depth(),
        floor,
        "no rename dialog opened over a hold the drag cancelled"
    );
    assert!(
        !h.shows("no tabs yet"),
        "…and the drag never toggled the row open either: {:?}",
        h.values()
    );
}

#[test]
fn a_read_only_credential_withdraws_every_rows_press_and_hold() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    make_read_only(&mut h);

    h.open_sheet();
    let floor = h.depth();
    // A read-only credential gets no `TreeMutations`, so the tree wires no `on_long_press`.
    h.long_press(h.value_point("btop"));
    h.settle();

    assert_eq!(
        h.depth(),
        floor,
        "a read-only session offers no rename affordance anywhere on the sheet"
    );
}

/// The paint surface stays [`SURFACE`]; only the published metrics pick the structure.
const TABLET_FULL: (f64, f64) = (1024.0, 720.0);
const TABLET_COLLAPSED: (f64, f64) = (720.0, 720.0);
/// The zellij mock's header line (four tabs, two panes apiece).
const RAIL_SUMMARY: &str = "4 tabs \u{b7} 8 panes";
const EXIT_IMMERSIVE: &str = "Exit immersive";

fn mount_tablet(session: &str, (width, height): (f64, f64)) -> Harness {
    let metrics = Some(support::metrics_at(width, height));
    support::mount_with_metrics(metrics, || Harness::mount_session(session))
}

fn expanded(
    controller: &muxr::features::terminal::presentation::controller::TerminalController,
) -> (Option<String>, Option<String>) {
    (
        controller.expanded_space.get_untracked(),
        controller.expanded_tab.get_untracked(),
    )
}

#[test]
fn a_tablet_docks_the_rail_resting_on_the_active_tab_without_a_cascade() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);

    let first = h.frame_at(0.0);
    assert!(
        h.shows(SESSION_NAME) && h.shows(RAIL_SUMMARY),
        "{:?}",
        h.values()
    );
    let controller = debug_controller().expect("mounted");
    assert_eq!(expanded(&controller).1.as_deref(), Some("devbox"));
    assert_eq!(
        tree_pane_rows(&first).len(),
        2,
        "devbox's panes are on screen in the first frame, not cascading in"
    );
    assert_eq!(h.depth(), REST_DEPTH, "docked, not raised");
}

#[test]
fn a_tablet_pane_tap_commits_and_the_rail_stays_resting_on_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);

    h.tap(h.value_point("btop"));
    h.settle_motion(0.0);
    h.tap(h.value_point("ed@workbench zsh"));
    h.pump_until_values("the btop tab", |values| {
        values.iter().any(|v| v == BTOP_TITLE)
    });
    h.pump_for(2 * CLOSE_DELAY);

    let controller = debug_controller().expect("mounted");
    assert_eq!(expanded(&controller).1.as_deref(), Some("btop"));
    assert!(h.shows(RAIL_SUMMARY), "the rail is still mounted");
    assert_eq!(h.depth(), REST_DEPTH, "nothing closed, nothing opened");
}

#[test]
fn the_rails_rows_carry_the_float_tag_the_exited_mark_and_the_bell() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    h.frame();

    let labels = node_labels(&h.root);
    assert!(labels.iter().any(|l| l == "float"), "{labels:?}");
    assert!(
        labels.iter().any(|l| l == "bell"),
        "harbor rings: {labels:?}"
    );
    h.tap(h.value_point("btop"));
    h.settle_motion(0.0);
    assert!(node_labels(&h.root).iter().any(|l| l == "exited"));
}

#[test]
fn the_compact_sheet_draws_none_of_the_rails_marks_or_menus() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.open_sheet();

    let labels = node_labels(&h.root);
    assert!(
        !labels
            .iter()
            .any(|l| l == "float" || l == "bell" || l.ends_with(" actions")),
        "{labels:?}"
    );
}

#[test]
fn a_read_only_rail_withdraws_add_rows_and_overflow_buttons_but_still_browses() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(HERDR_SESSION_ID, TABLET_FULL);
    make_read_only(&mut h);
    h.frame();

    assert!(!h.shows(ADD_TAB_LABEL) && !h.shows(NEW_PANE_LABEL) && !h.shows(ADD_SPACE_LABEL));
    assert!(!node_labels(&h.root).iter().any(|l| l.ends_with(" actions")));
    assert!(h.shows("read-only: browse and switch only"));
    h.tap(h.value_point("btop"));
    h.settle_motion(0.0);
    assert!(h.shows("ed@workbench zsh"), "the browse moved");
}

#[test]
fn an_overflow_menus_rename_opens_the_rows_own_dialog() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    h.frame();

    h.tap_label("devbox actions");
    h.settle();
    assert_eq!(h.depth(), REST_DEPTH + 1, "the Glyph menu is up");
    let controller = debug_controller().expect("mounted");
    assert_eq!(
        expanded(&controller).1.as_deref(),
        Some("devbox"),
        "the button's press never also toggled its row"
    );
    h.nav.pop_with_result(PopResult::of(0_usize));
    h.flush();
    h.settle();
    assert!(h.shows("Rename tab"), "{:?}", h.values());
}

/// The kebab menu: Rename, Immersive, Show key bar, a separator, Detach.
const MENU_IMMERSIVE: usize = 1;
const MENU_KEY_BAR: usize = 2;
const MENU_DETACH: usize = 4;

impl Harness {
    fn choose_terminal_menu(&mut self, index: usize) {
        open_terminal_menu(&mut self.state);
        self.settle();
        assert_eq!(self.depth(), REST_DEPTH + 1, "the Glyph menu is up");
        self.nav.pop_with_result(PopResult::of(index));
        self.flush();
        self.settle();
    }
}

#[test]
fn the_tablet_kebab_opens_a_glyph_menu_and_no_sheet() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);

    h.open_sheet();
    assert_eq!(h.depth(), REST_DEPTH + 1, "one page: the menu, not a sheet");
}

#[test]
fn the_tablet_menu_toggles_the_key_bar_signal() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    let controller = debug_controller().expect("mounted");
    assert!(controller.key_bar_visible.get_untracked(), "on by default");

    h.choose_terminal_menu(MENU_KEY_BAR);
    assert!(!controller.key_bar_visible.get_untracked());
    h.choose_terminal_menu(MENU_KEY_BAR);
    assert!(controller.key_bar_visible.get_untracked());
}

#[test]
fn the_tablet_menu_toggles_immersive_mode() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);

    h.choose_terminal_menu(MENU_IMMERSIVE);
    assert!(app_nav().bar().fullscreen);
}

#[test]
fn the_tablet_menus_detach_pops_and_toasts() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    app_toasts().clear();

    h.choose_terminal_menu(MENU_DETACH);
    let deadline = Instant::now() + PUMP_DEADLINE;
    while app_toasts().pending_untracked().is_empty() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the detach"
        );
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    assert_eq!(
        app_toasts().pending_untracked(),
        vec![DETACHED_TOAST.to_string()]
    );
}

#[test]
fn the_tablet_header_carries_the_session_space_and_tab_crumbs() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    h.frame();
    let labels = node_labels(&h.root);
    for crumb in [SESSION_NAME, "devbox"] {
        assert!(labels.iter().any(|l| l == crumb), "{crumb}: {labels:?}");
    }
    let controller = debug_controller().expect("mounted");
    assert_eq!(controller.crumb_labels(), [SESSION_NAME, "devbox"]);

    let mut h = mount_tablet(HERDR_SESSION_ID, TABLET_FULL);
    h.frame();
    let crumbs = debug_controller().expect("mounted").crumb_labels();
    assert!(
        crumbs.ends_with(&["forgekit".to_string(), "devbox".to_string()]),
        "space, tab: {crumbs:?}"
    );
    assert!(
        crumbs
            .iter()
            .all(|name| node_labels(&h.root).contains(name))
    );
}

#[test]
fn the_collapsed_band_hides_the_crumbs() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_COLLAPSED);
    h.frame();
    assert!(!node_labels(&h.root).iter().any(|l| l == SESSION_NAME));
}

#[test]
fn a_read_only_credential_shows_the_chip_in_the_status_row() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(SESSION_ID);
    assert!(!node_labels(&h.root).iter().any(|l| l == "read-only"));
    make_read_only(&mut h);
    assert!(node_labels(&h.root).iter().any(|l| l == "read-only"));
}

#[test]
fn the_collapsed_band_mounts_the_mini_rail_instead() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_COLLAPSED);
    h.frame();

    assert!(!h.shows(RAIL_SUMMARY), "no full rail");
    let labels = node_labels(&h.root);
    assert!(labels.iter().any(|l| l == "ed@devbox zsh"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == EXIT_IMMERSIVE));

    let point = h.label_point("ed@devbox zsh");
    h.tap(point);
    h.pump_until("the mini rail's commit", |_| {
        verb_log_for_tests()
            .iter()
            .any(|verb| verb == "focus_pane:41")
    });
}

#[test]
fn immersive_on_a_tablet_keeps_the_mini_rail_and_its_way_out() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);

    h.tap_label(EXPAND);
    h.settle_motion(1_000.0);
    assert!(
        !h.shows(RAIL_SUMMARY),
        "the full rail folds to the mini rail"
    );
    h.tap_label(EXIT_IMMERSIVE);
    assert!(
        !app_nav().bar().fullscreen,
        "…whose last button leaves immersive"
    );
}

#[test]
fn a_read_only_rail_lands_a_pane_tap_right_behind_a_space_browse() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(HERDR_SESSION_ID, TABLET_FULL);
    make_read_only(&mut h);
    clear_verb_log_for_tests();

    h.tap(h.value_point("zelli_mobile"));
    h.pump_until("zelli_mobile's own tab to auto-expand", |_| {
        debug_controller()
            .expect("mounted")
            .expanded_tab
            .get_untracked()
            .as_deref()
            == Some("logs")
    });
    h.settle_motion(0.0);
    h.settle_motion(2_000.0);
    h.tap(h.value_point("ed@zelli zsh"));
    h.pump_until("the commit", |_| verb_log_for_tests().len() >= 2);
    h.pump_for(2 * CLOSE_DELAY);

    assert_eq!(
        verb_log_for_tests(),
        ["switch_space:zelli_mobile", "focus_pane:11"]
    );
    let controller = debug_controller().expect("mounted");
    assert_eq!(
        expanded(&controller),
        (Some("zelli_mobile".to_string()), Some("logs".to_string()))
    );
    assert_eq!(h.depth(), REST_DEPTH);
}

/// Tab keys are unique per layout only; the mock's poll always reports `devbox` active.
#[test]
fn a_docked_poll_follows_focus_only_while_resting_never_into_a_same_keyed_browse() {
    let _g = serial();
    let _ambient = setup();
    let _h = mount_tablet(HERDR_SESSION_ID, TABLET_FULL);
    let c = debug_controller().expect("mounted");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime");
    let at = |space: &str, tab: &str| (Some(space.to_string()), Some(tab.to_string()));
    let rest_on_btop = || {
        assert!(rt.block_on(c.switch_tab("btop".to_string())));
        c.settle_rest();
        assert_eq!(expanded(&c), at("forgekit", "btop"));
    };
    let poll = || {
        assert!(c.take_layout_poll_tick());
        rt.block_on(c.refresh_layout_from_poll());
    };

    rest_on_btop();
    rt.block_on(c.expand_space("zelli_mobile".to_string()));
    let (space, mut tabs) = c.browsed_space_tabs.get_untracked().expect("browsed");
    tabs[0].key = "btop".to_string();
    c.browsed_space_tabs.set(Some((space, tabs)));
    c.expand_tab("btop".to_string());
    poll();
    assert_eq!(
        expanded(&c),
        at("zelli_mobile", "btop"),
        "a browse is the user's"
    );

    rest_on_btop();
    poll();
    assert_eq!(
        expanded(&c),
        at("forgekit", "devbox"),
        "nothing browsed: it follows"
    );
}

fn current_thread() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
}

/// Re-expanding the selected space puts the rail at rest, so another client's focus is followed.
#[test]
fn a_docked_poll_follows_focus_after_browsing_b_then_re_expanding_a() {
    let _g = serial();
    let _ambient = setup();
    let _h = mount_tablet(HERDR_SESSION_ID, TABLET_FULL);
    let c = debug_controller().expect("mounted");
    let rt = current_thread();
    let at = |space: &str, tab: &str| (Some(space.to_string()), Some(tab.to_string()));
    assert!(rt.block_on(c.switch_tab("btop".to_string())));
    c.settle_rest();

    rt.block_on(c.expand_space("zelli_mobile".to_string()));
    assert!(c.browsed_space_tabs.get_untracked().is_some(), "B browsed");
    rt.block_on(c.expand_space("forgekit".to_string()));
    c.expand_tab("btop".to_string());
    assert_eq!(expanded(&c), at("forgekit", "btop"));

    assert!(c.take_layout_poll_tick());
    rt.block_on(c.refresh_layout_from_poll());
    assert_eq!(
        expanded(&c),
        at("forgekit", "devbox"),
        "back at rest on A: the rail follows the poll's focus"
    );
}

/// A failed space switch, tree commit or grid tap leaves the deep link pending.
#[test]
fn a_claimed_move_whose_rpc_fails_keeps_the_deep_link() {
    let _g = serial();
    let _ambient = setup();
    let _h = Harness::mount_session(HERDR_SESSION_ID);
    let c = debug_controller().expect("mounted");
    let rt = current_thread();
    let active = c.active_tab.get_untracked();
    let kept = |landed: bool, step: &str| {
        assert!(!landed, "{step}: the mock refuses it");
        assert!(
            c.pending_attach_target_for_tests().is_some(),
            "{step}: still pending"
        );
    };

    c.set_attach_target(Some(1), Some(21));
    kept(
        rt.block_on(c.select_space("nowhere".to_string())),
        "space switch",
    );
    kept(rt.block_on(c.commit_pane(active, 999, false)), "commit");
    kept(rt.block_on(c.focus_pane_from_tap(999, false)), "tap");
}

/// The mock refuses the create itself; the controller's unit suite pins the landed case.
#[test]
fn a_space_create_keeps_the_deep_link() {
    let _g = serial();
    let _ambient = setup();
    let _h = Harness::mount_session(HERDR_SESSION_ID);
    let c = debug_controller().expect("mounted");

    c.set_attach_target(Some(1), Some(21));
    current_thread().block_on(c.create_space("scratch".to_string()));
    assert!(c.pending_attach_target_for_tests().is_some());
}

const HOME_CAP: &str = "Home";
const FN_CAP: &str = "Function keys";

impl Harness {
    fn enter_live(&mut self) {
        debug_controller().expect("mounted").enter_live();
        self.flush();
    }

    fn labelled(&self, label: &str) -> bool {
        node_labels(&self.root).iter().any(|l| l == label)
    }
}

#[test]
fn a_live_tablet_shows_the_key_bar_with_no_keyboard_behind_the_menu_toggle() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    h.enter_live();
    assert!(h.labelled(HOME_CAP) && h.labelled("End") && h.labelled(FN_CAP));

    h.choose_terminal_menu(MENU_KEY_BAR);
    assert!(!h.labelled(HOME_CAP) && !h.labelled("Escape"), "hidden");
    h.choose_terminal_menu(MENU_KEY_BAR);
    assert!(h.labelled(HOME_CAP), "and back");
}

#[test]
fn a_collapsed_tablet_drops_home_and_end_from_its_key_bar() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_COLLAPSED);
    h.enter_live();
    assert!(!h.labelled(HOME_CAP) && !h.labelled("End"));
    assert!(h.labelled("Page up") && h.labelled(FN_CAP));
}

#[test]
fn the_tablet_fn_cap_mounts_the_f_key_row() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    h.enter_live();
    assert!(!h.labelled("F1"));

    h.tap_label(FN_CAP);
    h.frame();
    assert!(h.labelled("F1") && h.labelled("F12"));
    let controller = debug_controller().expect("mounted");
    assert!(controller.fn_row_visible.get_untracked());
}

#[test]
fn a_read_only_tablet_mounts_no_key_bar() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    make_read_only(&mut h);
    h.enter_live();
    assert!(!h.labelled(HOME_CAP) && !h.labelled("Escape"));
}

#[test]
fn a_live_phone_keeps_the_ime_only_compact_bar() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(SESSION_ID);
    h.enter_live();
    assert!(!h.labelled("Escape"), "no keyboard, no bar");

    provide_context(support::ime_bottom_inset(300.0));
    h.frame();
    assert!(h.labelled("Escape"), "the keyboard is up");
    assert!(!h.labelled(HOME_CAP), "the Compact tier: no Home cap");
}

#[test]
fn a_mini_rail_tab_cell_is_a_named_button_and_taps_by_its_label() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_COLLAPSED);
    h.frame();
    clear_verb_log_for_tests();

    assert!(node_labels(&h.root).iter().any(|l| l == "lumen-api"));
    h.tap_label("lumen-api");
    h.pump_until("the tab commit", |_| {
        verb_log_for_tests()
            .iter()
            .any(|verb| verb == "go_to_tab:2")
    });
}

#[test]
fn every_kebab_index_decodes_to_the_entry_its_table_declares() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    let controller = debug_controller().expect("mounted");

    open_terminal_menu(&mut h.state);
    h.settle();
    let labels = node_labels(&h.root);
    for entry in [
        "Rename session",
        "Immersive mode",
        "Show key bar",
        "Detach session",
    ] {
        assert!(labels.iter().any(|l| l == entry), "{entry}: {labels:?}");
    }
    h.nav.pop_with_result(PopResult::of(3_usize));
    h.flush();
    h.settle();
    assert_eq!(h.depth(), REST_DEPTH, "the separator picks nothing");
    assert!(controller.key_bar_visible.get_untracked());
    assert!(!app_nav().bar().fullscreen);

    h.choose_terminal_menu(5);
    assert_eq!(h.depth(), REST_DEPTH, "past the table picks nothing");

    h.choose_terminal_menu(0);
    assert_eq!(h.depth(), REST_DEPTH + 1, "entry 0 is the rename dialog");
}

#[test]
fn every_row_menu_index_decodes_to_the_entry_its_table_declares() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    h.frame();

    h.tap_label("devbox actions");
    h.settle();
    let labels = node_labels(&h.root);
    for entry in ["Rename tab", "New pane here"] {
        assert!(labels.iter().any(|l| l == entry), "{entry}: {labels:?}");
    }
    h.nav.pop_with_result(PopResult::of(2_usize));
    h.flush();
    h.settle();
    assert_eq!(h.depth(), REST_DEPTH, "past the table picks nothing");
}

#[test]
fn the_read_only_chip_shows_on_a_tablet_through_the_shared_helper() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    h.frame();
    assert!(!node_labels(&h.root).iter().any(|l| l == "read-only"));
    make_read_only(&mut h);
    assert!(node_labels(&h.root).iter().any(|l| l == "read-only"));
}

/// The mock's agent store outlives every mount, so it is cleared on drop, pass or panic.
struct SeededAgents;

impl Drop for SeededAgents {
    fn drop(&mut self) {
        clear_test_agents();
    }
}

fn seed(agents: Vec<AgentEntry>) -> SeededAgents {
    clear_test_agents();
    seed_agents(agents);
    SeededAgents
}

fn agent(
    name: &str,
    (space, tab_key, tab_id, pane_id): (Option<&str>, &str, u64, u32),
    status: AgentStatus,
    change_seq: u64,
) -> AgentEntry {
    AgentEntry {
        pane_id,
        tab_id,
        tab_key: tab_key.to_string(),
        space_id: space.map(str::to_string),
        space_name: space.map(str::to_string),
        tab_name: tab_key.to_string(),
        kind: "claude".to_string(),
        name: Some(name.to_string()),
        display_kind: None,
        status,
        title: None,
        summary: Some(format!("{name} is busy")),
        cwd: None,
        focused: false,
        change_seq,
    }
}

fn many_agents() -> Vec<AgentEntry> {
    (0..8)
        .map(|i| {
            agent(
                &format!("agent{i}"),
                (None, "devbox", 4, 40 + (i % 2)),
                AgentStatus::Working,
                u64::from(i),
            )
        })
        .collect()
}

fn scroll_views(h: &Harness) -> Vec<(kurbo::Rect, f64)> {
    h.root
        .semantics()
        .nodes
        .iter()
        .filter(|(_, node): &&(_, Node)| node.role() == Role::ScrollView)
        .filter_map(|(_, node)| Some((node.bounds()?, node.scroll_y().unwrap_or(0.0))))
        .map(|(b, y)| (kurbo::Rect::new(b.x0, b.y0, b.x1, b.y1), y))
        .collect()
}

fn scroll_views_between(h: &Harness, x0: f64, x1: f64) -> Vec<(kurbo::Rect, f64)> {
    let mut views: Vec<_> = scroll_views(h)
        .into_iter()
        .filter(|(rect, _)| rect.x0 >= x0 - EPSILON && rect.x1 <= x1 + EPSILON)
        .collect();
    views.sort_by(|a, b| a.0.y0.total_cmp(&b.0.y0));
    views
}

fn wheel(h: &mut Harness, rect: kurbo::Rect) {
    for _ in 0..4 {
        h.root.event(
            &mut h.state,
            &InputEvent::Scroll {
                position: rect.center(),
                delta: ScrollDelta::Lines(0.0, 3.0),
            },
        );
        h.settle();
    }
}

fn halves_scroll_independently(h: &mut Harness, x0: f64, x1: f64) {
    let views = scroll_views_between(h, x0, x1);
    assert_eq!(views.len(), 2, "tree over agents: {views:?}");
    let (upper, lower) = (views[0], views[1]);
    assert!(upper.0.y1 <= lower.0.y0 + EPSILON, "stacked: {views:?}");

    wheel(h, lower.0);
    let moved = scroll_views_between(h, x0, x1);
    assert!(moved[1].1 > lower.1, "the agents half scrolled: {moved:?}");
    assert_eq!(moved[0].1, upper.1, "the tree half stayed put: {moved:?}");

    wheel(h, upper.0);
    let both = scroll_views_between(h, x0, x1);
    assert!(both[0].1 > upper.1, "the tree half scrolled: {both:?}");
    assert_eq!(
        both[1].1, moved[1].1,
        "the agents half stayed put: {both:?}"
    );
}

fn agent_row_labels(h: &Harness) -> Vec<String> {
    node_labels(&h.root)
        .into_iter()
        .filter(|label| label.contains(" (claude): "))
        .collect()
}

fn await_agents(h: &mut Harness, count: usize) {
    h.pump_until("the agents to load", |_| {
        debug_controller()
            .expect("mounted")
            .agents
            .get_untracked()
            .len()
            == count
    });
}

/// `RAIL_WIDTH` (264) plus its hairline.
const RAIL_EDGE: f64 = 265.0;

#[test]
fn with_no_agents_the_sheet_mounts_one_scroll_view() {
    let _g = serial();
    let _ambient = setup();
    clear_test_agents();
    let mut h = Harness::mount();
    let before = scroll_views(&h).len();
    h.open_sheet();
    assert_eq!(scroll_views(&h).len(), before + 1, "the tree alone");
    assert!(agent_row_labels(&h).is_empty() && !h.shows("AI AGENTS \u{b7} 0"));
}

#[test]
fn with_no_agents_the_rail_mounts_one_scroll_view() {
    let _g = serial();
    let _ambient = setup();
    clear_test_agents();
    let h = mount_tablet(SESSION_ID, TABLET_FULL);
    assert_eq!(scroll_views_between(&h, 0.0, RAIL_EDGE).len(), 1);
    assert!(h.shows(RAIL_SUMMARY), "no agent count: {:?}", h.values());
}

#[test]
fn with_agents_the_sheet_splits_into_two_independently_scrolling_halves() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(many_agents());
    let mut h = Harness::mount();
    await_agents(&mut h, 8);
    let before = scroll_views(&h).len();
    h.open_sheet();
    assert_eq!(scroll_views(&h).len(), before + 2, "tree over agents");
    assert!(h.shows("AI AGENTS \u{b7} 8"), "{:?}", h.values());
    halves_scroll_independently(&mut h, SURFACE.width / 2.0, SURFACE.width);
}

#[test]
fn with_agents_the_rail_splits_into_two_independently_scrolling_halves() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(many_agents());
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    h.pump_until("the agents to load", |h| agent_row_labels(h).len() == 8);
    assert!(
        h.shows("4 tabs \u{b7} 8 panes \u{b7} 8 agents"),
        "{:?}",
        h.values()
    );
    halves_scroll_independently(&mut h, 0.0, RAIL_EDGE);
}

#[test]
fn agent_rows_read_blocked_then_done_then_working_then_idle_newest_first() {
    let _g = serial();
    let _ambient = setup();
    let at = |pane| (None, "devbox", 4, pane);
    let _agents = seed(vec![
        agent("idler", at(40), AgentStatus::Idle, 9),
        agent("older", at(41), AgentStatus::Working, 2),
        agent("asker", at(40), AgentStatus::Blocked, 1),
        agent("newer", at(41), AgentStatus::Working, 7),
        agent("closer", at(40), AgentStatus::Done, 3),
    ]);
    let mut h = Harness::mount();
    await_agents(&mut h, 5);
    h.open_sheet();
    let names: Vec<String> = agent_row_labels(&h)
        .iter()
        .map(|label| label.split(' ').next().unwrap_or_default().to_string())
        .collect();
    assert_eq!(names, ["asker", "closer", "newer", "older", "idler"]);
    assert_eq!(
        agent_row_labels(&h)[0],
        "asker (claude): needs you, in devbox \u{203a} p40"
    );
}

fn btop_agent() -> AgentEntry {
    agent(
        "reviewer",
        (Some("forgekit"), "btop", 1, 11),
        AgentStatus::Blocked,
        1,
    )
}

fn park_on_zelli(h: &mut Harness) {
    await_agents(h, 1);
    let c = debug_controller().expect("mounted");
    assert!(current_thread().block_on(c.select_space("zelli_mobile".to_string())));
    h.frame();
    app_toasts().clear();
    clear_verb_log_for_tests();
}

#[test]
fn an_agent_row_in_another_space_jumps_like_a_pane_row_and_closes_the_sheet_once() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(vec![btop_agent()]);
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    park_on_zelli(&mut h);
    let floor = h.push_route_floor();

    open_terminal_menu(&mut h.state);
    h.settle();
    assert_eq!(h.depth(), floor + 1, "the sheet is up");
    h.tap_label(&agent_row_labels(&h)[0]);
    h.pump_until("the jump", |_| verb_log_for_tests().len() >= 3);
    h.pump_until_closed("the sheet to close", |h| h.depth() == floor);

    assert_eq!(
        verb_log_for_tests(),
        ["switch_space:forgekit", "go_to_tab:1", "focus_pane:11"],
        "exactly a pane row's cross-space commit"
    );
    assert_eq!(h.depth(), floor, "one close, the route under it untouched");
}

#[test]
fn an_agent_row_on_the_rail_jumps_and_the_rail_rests_on_the_new_tab() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(vec![btop_agent()]);
    let mut h = mount_tablet(HERDR_SESSION_ID, TABLET_FULL);
    park_on_zelli(&mut h);
    h.pump_until("the agent row", |h| agent_row_labels(h).len() == 1);

    h.tap_label(&agent_row_labels(&h)[0]);
    h.pump_until("the jump", |_| verb_log_for_tests().len() >= 3);
    h.pump_for(2 * CLOSE_DELAY);

    assert_eq!(
        verb_log_for_tests(),
        ["switch_space:forgekit", "go_to_tab:1", "focus_pane:11"]
    );
    let controller = debug_controller().expect("mounted");
    assert_eq!(
        expanded(&controller),
        (Some("forgekit".to_string()), Some("btop".to_string())),
        "resting on where the session now is"
    );
    assert_eq!(h.depth(), REST_DEPTH, "nothing closed, nothing opened");
}

#[test]
fn an_agents_pane_shows_its_tile_and_dot_and_its_tab_the_rollup() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(vec![agent(
        "reviewer",
        (None, "devbox", 4, 40),
        AgentStatus::Blocked,
        1,
    )]);
    let mut h = Harness::mount();
    await_agents(&mut h, 1);
    let (_, scene) = h.open_sheet();

    assert!(
        node_labels(&h.root).iter().any(|l| l == "C"),
        "the pane row's kind tile: {:?}",
        node_labels(&h.root)
    );
    let header = node_centers_by_value(&h.root, "AI AGENTS \u{b7} 1")
        .first()
        .copied()
        .expect("the agents header");
    let warning = muxr::ui::palette::Palette::from_theme(&frust_glyph::baseline()).warning;
    let tree_dots = scene
        .rounded_fills
        .iter()
        .filter(|(origin, size, _, color)| {
            *color == warning && (size.width - 8.0).abs() < EPSILON && origin.y < header.y
        })
        .count();
    assert_eq!(tree_dots, 2, "the pane's status dot and its tab's rollup");
}

#[test]
fn the_rail_header_reads_needs_you_while_an_agent_is_blocked() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(vec![agent(
        "reviewer",
        (None, "devbox", 4, 40),
        AgentStatus::Blocked,
        1,
    )]);
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    h.pump_until("the agents to load", |h| agent_row_labels(h).len() == 1);
    let labels = node_labels(&h.root);
    assert!(labels.iter().any(|l| l == "needs you"), "{labels:?}");
    assert!(!labels.iter().any(|l| l == "running"), "{labels:?}");
}

fn attention_dots(scene: &RecScene) -> usize {
    let warning = muxr::ui::palette::Palette::from_theme(&frust_glyph::baseline()).warning;
    scene
        .rounded_fills
        .iter()
        .filter(|(_, size, _, color)| *color == warning && (size.width - 8.0).abs() < EPSILON)
        .count()
}

#[test]
fn the_immersive_panes_button_carries_the_attention_dot_while_an_agent_is_blocked() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(vec![agent(
        "reviewer",
        (None, "devbox", 4, 40),
        AgentStatus::Blocked,
        1,
    )]);
    let mut h = Harness::mount();
    h.pump_until("the agents to load", |_| {
        !debug_controller()
            .expect("mounted")
            .agents
            .get_untracked()
            .is_empty()
    });
    h.tap_label(EXPAND);
    let scene = h.settle_motion(0.0);
    assert!(node_labels(&h.root).iter().any(|l| l == PANES));
    assert_eq!(attention_dots(&scene), 1, "the dot rides the panes button");
}

#[test]
fn the_immersive_panes_button_has_no_dot_while_nothing_is_blocked() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(vec![agent(
        "reviewer",
        (None, "devbox", 4, 40),
        AgentStatus::Working,
        1,
    )]);
    let mut h = Harness::mount();
    h.pump_until("the agents to load", |_| {
        !debug_controller()
            .expect("mounted")
            .agents
            .get_untracked()
            .is_empty()
    });
    h.tap_label(EXPAND);
    let scene = h.settle_motion(0.0);
    assert!(node_labels(&h.root).iter().any(|l| l == PANES));
    assert_eq!(attention_dots(&scene), 0, "nothing blocked, no dot");
}

/// A blocked agent in a space the mock does not have, so its peek fails.
fn ghost_agent() -> AgentEntry {
    agent(
        "ghost",
        (Some("nowhere"), "btop", 1, 11),
        AgentStatus::Blocked,
        1,
    )
}

#[test]
fn a_cross_space_agent_whose_peek_fails_moves_nothing_and_keeps_the_browse_cache() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(vec![ghost_agent()]);
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    park_on_zelli(&mut h);
    let c = debug_controller().expect("mounted");
    let rt = current_thread();
    let browsed_before = c.browsed_space_tabs.get_untracked();
    let active_before = c.active_tab.get_untracked();
    let agent = c.agents.get_untracked().remove(0);

    assert!(!rt.block_on(c.commit_agent(&agent)));

    assert_eq!(c.browsed_space_tabs.get_untracked(), browsed_before);
    assert_eq!(c.active_tab.get_untracked(), active_before);
    assert!(
        verb_log_for_tests().is_empty(),
        "{:?}",
        verb_log_for_tests()
    );
}

#[test]
fn a_cross_space_agent_whose_pane_commit_fails_leaves_the_browse_cache_as_it_was() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(vec![agent(
        "lost",
        (Some("forgekit"), "btop", 1, 99),
        AgentStatus::Blocked,
        1,
    )]);
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    park_on_zelli(&mut h);
    let c = debug_controller().expect("mounted");
    let rt = current_thread();
    let browsed_before = c.browsed_space_tabs.get_untracked();
    let agent = c.agents.get_untracked().remove(0);

    assert!(!rt.block_on(c.commit_agent(&agent)), "pane 99 is not there");

    assert_eq!(c.browsed_space_tabs.get_untracked(), browsed_before);
}

#[test]
fn an_older_agents_read_landing_last_cannot_overwrite_the_newer_inventory() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(vec![agent(
        "old",
        (None, "devbox", 4, 40),
        AgentStatus::Working,
        1,
    )]);
    let mut h = Harness::mount();
    await_agents(&mut h, 1);
    let c = debug_controller().expect("mounted");
    let rt = current_thread();

    delay_next_list_agents_for_tests(Duration::from_millis(300));
    rt.block_on(async {
        let first = c.reload_tabs();
        let second = async {
            clean_signals::time::sleep(Duration::from_millis(80)).await;
            seed_agents(vec![
                agent("new", (None, "devbox", 4, 40), AgentStatus::Idle, 2),
                agent("newer", (None, "devbox", 4, 41), AgentStatus::Idle, 3),
            ]);
            c.reload_tabs().await;
        };
        tokio::join!(first, second);
    });

    assert_eq!(c.agents.get_untracked().len(), 2, "the newer inventory");
}

#[test]
fn a_failed_agents_read_mutes_the_attention_claims_until_the_next_good_one() {
    let _g = serial();
    let _ambient = setup();
    let _agents = seed(vec![agent(
        "reviewer",
        (None, "devbox", 4, 40),
        AgentStatus::Blocked,
        1,
    )]);
    let mut h = mount_tablet(SESSION_ID, TABLET_FULL);
    h.pump_until("the agents to load", |h| agent_row_labels(h).len() == 1);
    let c = debug_controller().expect("mounted");
    let rt = current_thread();
    assert!(c.has_blocked_agent() && !c.agents_stale.get_untracked());

    fail_next_list_agents_for_tests();
    rt.block_on(c.reload_tabs());
    h.pump_for(Duration::from_millis(100));

    assert!(c.agents_stale.get_untracked());
    assert_eq!(c.agents.get_untracked().len(), 1, "the rows stay published");
    assert!(!c.has_blocked_agent(), "muted while stale");
    let labels = node_labels(&h.root);
    assert!(!labels.iter().any(|l| l == "needs you"), "{labels:?}");

    rt.block_on(c.reload_tabs());
    h.pump_for(Duration::from_millis(100));
    assert!(!c.agents_stale.get_untracked());
    assert!(c.has_blocked_agent());
    assert!(node_labels(&h.root).iter().any(|l| l == "needs you"));
}

#[test]
fn a_failed_tab_reload_on_attach_settle_raises_no_toast_but_a_user_reload_does() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let c = debug_controller().expect("mounted");
    let rt = current_thread();
    h.pump_for(Duration::from_millis(100));
    app_toasts().clear();

    fail_next_tab_list_for_tests();
    rt.block_on(c.reload_tabs_quiet());
    h.pump_for(Duration::from_millis(100));
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "the settle-driven reload is quiet: {:?}",
        app_toasts().pending_untracked()
    );

    fail_next_tab_list_for_tests();
    rt.block_on(c.reload_tabs());
    h.await_toast("Connection problem \u{2014} please try again.");
    app_toasts().clear();
}
