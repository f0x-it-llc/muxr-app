//! Headless coverage for the `/terminal` screen's live pane: content switch, settings-driven
//! cursor and font, delta-only resize, fullscreen, the IME-gated key bar, tap-to-focus, the link
//! strip, layout refresh, deep links, the agents stream and pinch.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::task::Poll;
use std::time::{Duration, Instant};

use frust::input::TOUCH_SLOP;
use frust::{
    AnyView, Component, GetUntracked, NavigatorController, RwSignal, Set, SizedBox, Stack, any,
    navigator, provide_context,
};
use frust_core::accesskit::Role;
use frust_core::{RenderRoot, WindowEdgeInsets, WindowInsets};
// A test-only peek at the system-UI slot; `Cargo.toml` says why it is not reached via `frust::`.
use frust_shell_common::{SystemUiMode, current_system_ui_mode};
use frust_text::TextContext;
use kurbo::{Point, Size, Vec2};

use muxr::connection::{CAP_AGENTS_STREAM, ConnectTarget, TrustMode, expect_connection_controller};
use muxr::failure::AppFailure;
use muxr::features::sessions::di::expect_sessions_repository;
use muxr::features::settings::di::expect_preferences_repository;
use muxr::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle as SettingsCursorStyle,
};
use muxr::features::terminal::data::feeds::demo_feed::DemoFeed;
use muxr::features::terminal::data::repositories::mock_terminal_repository::{
    TEST_FLOATING_PANE_ID, add_test_tab, agent_watch_open_for_tests, agent_watch_opens_for_tests,
    agent_watch_stops_for_tests, clear_test_agents, clear_test_tabs, clear_verb_log_for_tests,
    delay_next_list_agents_for_tests, drop_agent_stream_for_tests, end_agent_stream_for_tests,
    fail_next_list_agents_for_tests, open_next_agent_watch_down_for_tests,
    push_agent_event_for_tests, seed_agents, seed_test_floating_overlay,
    seed_test_union_fullscreen, verb_log_for_tests,
};
use muxr::features::terminal::di::{SharedTerminalRepository, expect_terminal_repository};
use muxr::features::terminal::domain::entities::{
    AgentEntry, AgentEventEntry, AgentStatus, Space, TabEntry, TermLine,
};
use muxr::features::terminal::domain::feed::{LinkStatus, TerminalFeed};
use muxr::features::terminal::domain::repositories::{TerminalRepository, WatchEnd};
use muxr::features::terminal::presentation::controller::{
    AttachTarget, TerminalController, advance_agents_clock_for_tests,
};
use muxr::features::terminal::presentation::pages::{
    debug_controller, debug_grid_hooks, debug_live_probe, open_terminal_menu, set_demo_copy_sink,
    terminal_page,
};
use muxr::features::terminal::presentation::term::CursorStyle as GridCursorStyle;
use muxr::features::terminal::presentation::term::chip::{CHIP_H, CHIP_W};
use muxr::features::terminal::presentation::term::{
    Emission, InputSeam, SharedEmulator, SharedFeed, TerminalEmulator, pump_wake, run_feed,
};
use muxr::features::terminal::presentation::term::{PinchIntent, PinchSink};
use muxr::routes::{app_nav, install_app_router};
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, center, char_key, ms, node_labels, pointer, pump_frames, rounded_in, serial,
    setup, tap,
};

/// Tall enough that the sheet and menu rows the setup taps stay above the fold.
const SURFACE: Size = Size::new(800.0, 700.0);
const SCRIM_POINT: Point = Point::new(4.0, 4.0);
const PUMP_DEADLINE: Duration = Duration::from_secs(5);
const SESSION_ID: &str = "s-1";
/// `SHEET_CLOSE_DELAY`, private in `presentation::pages`: how long a commit leaves the sheet up.
const CLOSE_DELAY: Duration = Duration::from_millis(260);

const SHEET_LABEL: &str = "panes & tabs";

const EXPAND: &str = "Fullscreen";
const COLLAPSE: &str = "Exit fullscreen";

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
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    mounted: Rc<Cell<bool>>,
}

impl Harness {
    fn mount() -> Self {
        Self::mount_session(SESSION_ID)
    }

    /// A herdr session id prefix is the one thing the sheet's spaces section is gated on.
    fn mount_session(session: &str) -> Self {
        Self::mount_at(&format!("/terminal?session={session}"))
    }

    fn mount_at(location: &str) -> Self {
        let state = MuxrApp.init();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        let record_only: NavigatorController<AppState> = NavigatorController::new();
        let _router = install_app_router(&record_only, &nav, expect_connection_controller());
        expect_connection_controller().connect(mock_target());
        app_nav().push(location);

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
                    any(SizedBox::<AppState>(
                        Some(f64::INFINITY),
                        Some(f64::INFINITY),
                    ))
                })),
            ]))
        });
        let mut root = RenderRoot::new();
        root.set_theme(Box::new(frust_glyph::baseline()));
        let mut harness = Harness {
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
        support::frame_at_sized(
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

    fn settle_motion(&mut self, from_ms: f64) -> RecScene {
        self.frame_at(from_ms);
        self.frame_at(from_ms + 1_000.0);
        self.frame_at(from_ms + 1_010.0);
        self.frame_at(from_ms + 1_020.0)
    }

    fn flush(&mut self) {
        self.settle();
        self.settle();
        self.root.event(
            &mut self.state,
            &pointer(frust_core::PointerPhase::Move, SCRIM_POINT),
        );
    }

    fn tap(&mut self, point: Point) {
        tap(&mut self.root, &mut self.state, point);
    }

    fn tap_label(&mut self, label: &str) {
        let point = support::node_centers_by_label(&self.root, label)
            .first()
            .copied()
            .unwrap_or_else(|| panic!("nothing labelled {label:?}"));
        self.tap(point);
    }

    fn open_menu(&mut self) -> RecScene {
        open_terminal_menu(&mut self.state);
        self.settle()
    }

    /// `enter_live` only flips the flag; the settle builds the live session under the screen.
    fn enter_demo(&mut self) {
        debug_controller()
            .expect("a terminal screen is mounted")
            .enter_live();
        self.flush();
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

    fn pump_until_closed(&mut self, what: &str, done: impl Fn(&Harness) -> bool) {
        self.pump_for(2 * CLOSE_DELAY);
        self.frame_at(0.0);
        self.frame_at(300.0);
        self.frame_at(300.0);
        assert!(done(self), "timed out waiting for {what}");
    }

    fn await_load(&mut self) {
        self.pump_for(Duration::from_millis(50));
    }

    fn one_frame(&mut self) {
        pump_frames(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            1,
        );
    }

    fn unmount_screen(&mut self) {
        self.mounted.set(false);
        self.settle();
    }

    fn session(&self) -> (bool, bool) {
        (self.root.is_focus_active(), self.root.ime_state().is_some())
    }
}

fn demo_feed() -> Rc<RefCell<DemoFeed>> {
    debug_live_probe()
        .expect("a live pane is mounted")
        .feed
        .expect("the demo entry mounts the fixture replay")
}

/// The grid's own plain `terminal_bg` fill, a channel no rounded tile on this screen uses.
fn grid_rect(scene: &RecScene) -> (Point, Size) {
    let ink = frust_glyph::baseline()
        .extension::<frust_glyph::GlyphInk>()
        .copied()
        .expect("the Glyph baseline carries GlyphInk");
    scene
        .fills
        .iter()
        .copied()
        .find(|(_, _, color)| *color == ink.terminal_bg)
        .map(|(origin, size, _)| (origin, size))
        .expect("the live grid paints its own plain background fill")
}

/// Approximate: the widget leaves under one cell of its box unused, and callers aim mid-pane.
fn cell_point(scene: &RecScene, col: u16, row: u16) -> Point {
    let (origin, size) = grid_rect(scene);
    let (rows, cols) = debug_live_probe()
        .expect("a live pane is mounted")
        .emulator
        .borrow()
        .screen()
        .size();
    assert!(
        col < cols && row < rows,
        "cell ({col},{row}) is outside the rendered {cols}x{rows} grid"
    );
    let cell_w = size.width / f64::from(cols);
    let cell_h = size.height / f64::from(rows);
    Point::new(
        origin.x + cell_w * (f64::from(col) + 0.5),
        origin.y + cell_h * (f64::from(row) + 0.5),
    )
}

/// Inside the mock's focused pane, the left half of an 80x24 grid; `SIBLING_CELL` is the right.
const FOCUSED_CELL: (u16, u16) = (5, 5);
const SIBLING_CELL: (u16, u16) = (45, 5);

fn focused_pane_id() -> Option<u32> {
    debug_controller()?
        .active_tab_entry()?
        .panes
        .iter()
        .find(|pane| pane.focused)
        .map(|pane| pane.id)
}

/// Down, one Move past `TOUCH_SLOP`, Up: the grid can only read it as a scroll.
fn drag_scroll(h: &mut Harness, from: Point) {
    let to = from + Vec2::new(0.0, TOUCH_SLOP * 4.0);
    h.root
        .event(&mut h.state, &pointer(frust_core::PointerPhase::Down, from));
    h.root
        .event(&mut h.state, &pointer(frust_core::PointerPhase::Move, to));
    h.root
        .event(&mut h.state, &pointer(frust_core::PointerPhase::Up, to));
}

fn card(scene: &RecScene) -> (Point, Size, f64) {
    let ink = frust_glyph::baseline()
        .extension::<frust_glyph::GlyphInk>()
        .copied()
        .expect("the Glyph baseline carries GlyphInk");
    let mut cards = rounded_in(scene, ink.terminal_bg);
    cards.sort_by(|a, b| (b.1.width * b.1.height).total_cmp(&(a.1.width * a.1.height)));
    cards.first().copied().expect("the card paints itself")
}

fn cell_text(
    probe: &muxr::features::terminal::presentation::pages::LiveProbe,
    row: u16,
    col: u16,
) -> String {
    probe
        .emulator
        .borrow()
        .screen()
        .cell(row, col)
        .map(|c| c.contents().to_string())
        .unwrap_or_default()
}

#[test]
fn the_mock_stays_the_default_and_carries_no_live_probe() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::mount();
    assert!(
        debug_live_probe().is_none(),
        "no demo session has been entered yet"
    );
    let _ = h;
}

#[test]
fn entering_the_live_pane_mounts_the_grid_and_replays_the_fixture() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    h.enter_demo();
    let scene = h.frame();

    assert!(
        debug_controller().expect("mounted").live.get_untracked(),
        "the controller's flag flipped"
    );
    let probe = debug_live_probe().expect("a live session is mounted");
    let _ = grid_rect(&scene);

    h.pump_until("the fixture's first line", |_| {
        let probe = debug_live_probe().expect("still mounted");
        let (rows, cols) = probe.emulator.borrow().screen().size();
        (0..rows).any(|row| {
            let mut line = String::new();
            for col in 0..cols {
                line.push_str(&cell_text(&probe, row, col));
            }
            line.contains("INFO") && line.contains("[00000]")
        })
    });
    let _ = probe;
}

#[test]
fn a_typed_character_echoes_at_the_cursor_byte_correctly() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    let scene = h.frame();

    let (origin, size) = grid_rect(&scene);
    h.tap(center((origin, size)));

    let probe = debug_live_probe().expect("live");
    let (row, col) = probe.emulator.borrow().screen().cursor_position();

    h.root.event(&mut h.state, &char_key("q"));
    h.pump_until("the echoed character", |_| {
        cell_text(&probe, row, col) == "q"
    });

    assert_eq!(cell_text(&probe, row, col), "q");
}

#[test]
fn a_long_press_selects_and_copy_reaches_the_injected_sink() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let captured: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let sink_captured = Rc::clone(&captured);
    set_demo_copy_sink(Some(Rc::new(move |text: &str| {
        sink_captured.borrow_mut().push(text.to_string());
        true
    })));

    h.enter_demo();
    h.pump_until("some fixture content to select", |_| {
        let probe = debug_live_probe().expect("still mounted");
        probe.emulator.borrow().screen().contents() != ""
    });
    let scene = h.frame();
    let (origin, size) = grid_rect(&scene);
    let (rows, cols) = {
        let probe = debug_live_probe().expect("live");
        probe.emulator.borrow().screen().size()
    };
    let cell_w = size.width / f64::from(cols);
    let cell_h = size.height / f64::from(rows);
    let point = Point::new(origin.x + cell_w * 24.0, origin.y + cell_h * 0.5);

    // `support::long_press` lays out at 800x600; driven by hand to keep this surface's geometry.
    h.root.event(
        &mut h.state,
        &pointer(frust_core::PointerPhase::Down, point),
    );
    h.frame_at(0.0);
    h.frame_at(support::HELD_MS);
    h.root
        .event(&mut h.state, &pointer(frust_core::PointerPhase::Up, point));
    let after = h.frame();

    let chip = after
        .rounded
        .iter()
        .copied()
        .find(|(_, size)| {
            (size.width - CHIP_W).abs() < EPSILON && (size.height - CHIP_H).abs() < EPSILON
        })
        .expect("the copy chip painted its own pill");

    let copy_point = Point::new(chip.0.x + 20.0, chip.0.y + chip.1.height / 2.0);
    h.tap(copy_point);

    assert_eq!(captured.borrow().len(), 1, "copy fired exactly once");
    assert!(
        !captured.borrow()[0].is_empty(),
        "the injected sink received the selected text"
    );

    set_demo_copy_sink(None);
}

#[test]
fn a_settings_change_reaches_the_live_panes_widget_inputs() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    h.frame();

    let controller = debug_controller().expect("mounted");
    assert_eq!(
        controller.cursor_style.get_untracked(),
        GridCursorStyle::Block
    );

    // Stands in for the settings screen's picker save, into the same shared store.
    let repository = expect_preferences_repository();
    let block_on = |fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>| {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    };
    block_on(Box::pin(async move {
        let _ = repository
            .save_appearance(AppearancePrefs {
                font_family: "Space Mono".to_string(),
                font_size_px: 14,
                cursor_style: SettingsCursorStyle::Underline,
            })
            .await;
    }));

    h.pump_until("the polled appearance refresh", |_| {
        debug_controller()
            .expect("mounted")
            .cursor_style
            .get_untracked()
            == GridCursorStyle::Underline
    });

    assert_eq!(
        debug_controller()
            .expect("mounted")
            .font_size
            .get_untracked(),
        14.0
    );
    // "Space Mono" is not bundled, so the poll resolves it to the shipped default stack.
    assert_eq!(
        debug_controller()
            .expect("mounted")
            .font_family
            .get_untracked(),
        muxr::fonts::default_terminal_family()
    );
}

/// Two direct `refresh_appearance` calls are two poll ticks; an idle tick must write no signal.
#[test]
fn an_idle_poll_with_unchanged_prefs_writes_nothing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    h.frame();

    let controller = debug_controller().expect("mounted");
    let block_on = |fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>| {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    };

    // A non-default appearance first, or the first read would match the defaults and prove nothing.
    let repository = expect_preferences_repository();
    block_on(Box::pin(async move {
        let _ = repository
            .save_appearance(AppearancePrefs {
                font_family: "Space Mono".to_string(),
                font_size_px: 14,
                cursor_style: SettingsCursorStyle::Underline,
            })
            .await;
    }));

    let first = Arc::clone(&controller);
    block_on(Box::pin(async move { first.refresh_appearance().await }));
    assert_eq!(
        controller.cursor_style.get_untracked(),
        GridCursorStyle::Underline
    );
    assert_eq!(controller.font_size.get_untracked(), 14.0);
    let resolved_family = controller.font_family.get_untracked();
    assert_eq!(
        resolved_family,
        muxr::fonts::default_terminal_family(),
        "\"Space Mono\" isn't bundled, so this resolves to the default stack too"
    );
    let after_first = controller.appearance_writes();
    assert!(
        after_first >= 1,
        "a real appearance change must write something"
    );

    let second = Arc::clone(&controller);
    block_on(Box::pin(async move { second.refresh_appearance().await }));
    let third = Arc::clone(&controller);
    block_on(Box::pin(async move { third.refresh_appearance().await }));

    assert_eq!(
        controller.font_family.get_untracked(),
        resolved_family,
        "font_family rides the same idle-poll gate"
    );
    assert_eq!(
        controller.appearance_writes(),
        after_first,
        "two identical poll cycles must write nothing further"
    );
}

#[test]
fn resize_reaches_the_feed_only_on_an_actual_cell_grid_delta() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    h.settle();

    let feed = demo_feed();
    let windowed = feed
        .borrow()
        .last_resize()
        .expect("the very first layout already resized off the fallback grid");

    h.frame();
    h.frame();
    assert_eq!(
        feed.borrow().last_resize(),
        Some(windowed),
        "an unchanged box records nothing new"
    );

    h.tap_label(EXPAND);
    h.settle_motion(1_000.0);
    let fullscreen = feed
        .borrow()
        .last_resize()
        .expect("fullscreen is a real resize");
    assert_ne!(
        fullscreen, windowed,
        "fullscreen's box is a different cell grid"
    );
}

#[test]
fn fullscreen_with_the_live_pane_still_fills_the_screen() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    h.frame();

    h.tap_label(EXPAND);
    let scene = h.settle_motion(1_000.0);
    let (origin, size, radius) = card(&scene);

    assert!(origin.x.abs() < EPSILON, "{origin:?}");
    assert!((size.width - SURFACE.width).abs() < EPSILON, "{size:?}");
    assert!(radius < EPSILON, "{radius}");
    let _ = grid_rect(&scene);
    // The system-UI write is synchronous with the press, so it is observable before any settle.
    assert_eq!(
        current_system_ui_mode().1,
        SystemUiMode::ImmersiveSticky,
        "expand hides the OS bars too"
    );

    h.tap_label(COLLAPSE);
    h.settle_motion(3_000.0);
    assert_eq!(
        current_system_ui_mode().1,
        SystemUiMode::EdgeToEdge,
        "collapse restores the OS bars"
    );
}

/// `BarKey::name()`, the accessible name, which deliberately differs from the printed label.
const A_KEY_CAP_LABEL: &str = "Control";

fn keyboard_insets(bottom: f64) -> WindowInsets {
    WindowInsets::new(
        WindowEdgeInsets::default(),
        WindowEdgeInsets::new(0.0, 0.0, 0.0, bottom),
    )
}

#[test]
fn the_key_bar_never_appears_in_mock_mode_even_with_the_keyboard_up() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    provide_context(keyboard_insets(300.0));
    h.frame();
    assert!(
        !node_labels(&h.root).iter().any(|l| l == A_KEY_CAP_LABEL),
        "the mock stays bar-free"
    );
}

#[test]
fn the_key_bar_mounts_only_while_live_and_the_keyboard_is_up() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();

    provide_context(keyboard_insets(0.0));
    h.frame();
    assert!(
        !node_labels(&h.root).iter().any(|l| l == A_KEY_CAP_LABEL),
        "no keyboard, no bar"
    );

    provide_context(keyboard_insets(300.0));
    h.frame();
    assert!(
        node_labels(&h.root).iter().any(|l| l == A_KEY_CAP_LABEL),
        "the keyboard is up: the bar mounts"
    );

    provide_context(keyboard_insets(0.0));
    h.frame();
    assert!(
        !node_labels(&h.root).iter().any(|l| l == A_KEY_CAP_LABEL),
        "the keyboard went away: the bar collapses again"
    );
}

// The link strip is driven through `TerminalController::link`, the signal a live feed writes.

/// Transcribed from `presentation::pages`, where they are private.
const LINK_RECONNECTING: &str = "reconnecting\u{2026}";
const LINK_DISCONNECTED: &str = "disconnected";
const LINK_CLOSED: &str = "session ended";
const LINK_RETRY: &str = "Retry";
const LINK_LEAVE: &str = "Back to sessions";

fn publish_link(h: &mut Harness, status: Option<LinkStatus>) {
    debug_controller()
        .expect("a terminal screen is mounted")
        .link
        .set(status);
    h.frame();
}

#[test]
fn a_healthy_or_absent_link_says_nothing_at_all() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();

    for status in [None, Some(LinkStatus::Idle), Some(LinkStatus::Connected)] {
        publish_link(&mut h, status);
        let values = support::node_values(&h.root);
        assert!(
            !values.iter().any(|value| value == LINK_RECONNECTING
                || value == LINK_DISCONNECTED
                || value == LINK_CLOSED),
            "{status:?} is not worth a band across the card: {values:?}"
        );
        assert!(
            !node_labels(&h.root)
                .iter()
                .any(|label| label == LINK_RETRY || label == LINK_LEAVE),
            "{status:?} offers nothing to press"
        );
    }
}

#[test]
fn a_reconnecting_link_says_so_and_offers_nothing_to_interrupt_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    publish_link(&mut h, Some(LinkStatus::Reconnecting));

    let values = support::node_values(&h.root);
    assert!(
        values.iter().any(|value| value == LINK_RECONNECTING),
        "{values:?}"
    );
    assert!(
        !node_labels(&h.root)
            .iter()
            .any(|label| label == LINK_RETRY || label == LINK_LEAVE),
        "a session coming back on its own has nothing to press"
    );
}

#[test]
fn a_disconnected_link_offers_the_retry_that_re_arms_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    publish_link(&mut h, Some(LinkStatus::Disconnected));

    assert!(
        support::node_values(&h.root)
            .iter()
            .any(|value| value == LINK_DISCONNECTED)
    );
    assert!(node_labels(&h.root).iter().any(|label| label == LINK_RETRY));

    // The demo replay has no stream to re-arm: the press must reach the session and not panic.
    h.tap_label(LINK_RETRY);
    h.frame();
    assert!(
        debug_live_probe().is_some(),
        "the live pane survived the press"
    );
}

#[test]
fn a_closed_link_offers_the_only_thing_left_the_way_back_to_the_list() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    publish_link(&mut h, Some(LinkStatus::Closed));

    assert!(
        support::node_values(&h.root)
            .iter()
            .any(|value| value == LINK_CLOSED)
    );
    assert!(node_labels(&h.root).iter().any(|label| label == LINK_LEAVE));
    assert!(
        !node_labels(&h.root).iter().any(|label| label == LINK_RETRY),
        "a session the server ended has nothing to retry"
    );

    // `Nav::pop()` only enqueues, so where it lands is unobservable here; the tap must not panic.
    h.tap_label(LINK_LEAVE);
    h.flush();
}

#[test]
fn the_demo_entry_stays_on_the_fixture_replay_and_reports_no_link() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    h.frame();

    assert!(
        debug_live_probe()
            .expect("a live pane is mounted")
            .feed
            .is_some(),
        "the demo row mounts the checked-in replay whatever is connected"
    );
    assert_eq!(
        debug_controller()
            .expect("a terminal screen is mounted")
            .link
            .get_untracked(),
        None,
        "a replay has no link to report"
    );
}

#[test]
fn unmounting_drops_the_published_chrome_and_the_live_probe() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    h.frame();
    assert!(debug_live_probe().is_some());

    // Unmounting while fullscreen must not strand hidden OS bars on the screen that comes back.
    h.tap_label(EXPAND);
    h.settle_motion(1_000.0);
    assert_eq!(current_system_ui_mode().1, SystemUiMode::ImmersiveSticky);

    h.unmount_screen();
    assert!(
        debug_live_probe().is_none(),
        "the screen's own cleanup withdrew its chrome"
    );
    assert_eq!(
        current_system_ui_mode().1,
        SystemUiMode::EdgeToEdge,
        "the screen's own cleanup restored the OS bars too"
    );
}

// These drive `DemoFeed::manual()` directly: the mounted screen's real feed takes 30s to drain.

#[test]
fn run_feed_exits_once_a_drained_feed_has_nothing_left_to_say() {
    let _g = serial();
    let _ambient = setup();

    let feed: Rc<RefCell<DemoFeed>> = Rc::new(RefCell::new(DemoFeed::manual()));
    feed.borrow_mut().set_elapsed(Duration::from_secs(60));
    let feed_dyn: SharedFeed = feed.clone();
    let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
    let generation = RwSignal::new(0u64);
    let cancelled = Arc::new(AtomicBool::new(false));

    // The first wake already sees a closed feed, so a current-thread runtime finishes with no wait.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a current-thread runtime");
    rt.block_on(run_feed(feed_dyn, emulator, generation, cancelled));

    assert!(
        feed.borrow().is_drained(),
        "the one wake drained the whole fixture before the loop exited"
    );
    assert!(
        !feed.borrow().is_open(),
        "and nothing was queued, so the loop had every reason to stop"
    );
}

#[test]
fn a_keystroke_after_drain_echoes_synchronously_in_the_same_dispatch_call() {
    let _g = serial();
    let _ambient = setup();

    let feed: Rc<RefCell<DemoFeed>> = Rc::new(RefCell::new(DemoFeed::manual()));
    feed.borrow_mut().set_elapsed(Duration::from_secs(60));
    let feed_dyn: SharedFeed = feed.clone();

    {
        let scratch: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
        let scratch_generation = RwSignal::new(0u64);
        pump_wake(&feed_dyn, &scratch, scratch_generation);
    }
    assert!(
        !feed.borrow().is_open(),
        "nothing left to replay or echo yet"
    );

    let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
    let generation = RwSignal::new(0u64);
    let mut seam = InputSeam::new(feed_dyn.clone());
    seam.wire_pump(Rc::clone(&emulator), generation);

    // No `pump_wake` here: the echo must reach the emulator inside `dispatch` alone.
    seam.dispatch(&Emission {
        bytes: b"q".to_vec(),
        ..Default::default()
    });

    assert_eq!(
        emulator
            .borrow()
            .screen()
            .cell(0, 0)
            .map(|c| c.contents().to_string()),
        Some("q".to_string()),
        "the echo landed in the shared emulator in the same dispatch call"
    );
    assert!(
        generation.get_untracked() > 0,
        "the synchronous wake published a new generation"
    );
}

// Tap-to-focus only: mounting claims nothing, a tap on the session's pane raises the keyboard,
// a tap on another pane only moves focus, and a drag-scroll raises nothing.

#[test]
fn mounting_the_live_pane_claims_no_focus_and_publishes_no_ime_surface() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    assert_eq!(
        h.session(),
        (false, false),
        "the mock screen holds no focus/IME session to begin with"
    );
    let before = h.root.focus_ime_generation();

    h.enter_demo();
    h.frame();
    // Past the first feed wakes and the first appearance poll: a late claim is just as wrong.
    h.pump_for(Duration::from_millis(50));

    assert!(
        debug_live_probe().is_some(),
        "the live pane really did mount"
    );
    assert_eq!(
        h.session(),
        (false, false),
        "mounting the grid must not claim focus or publish an IME surface"
    );
    assert_eq!(
        h.root.focus_ime_generation(),
        before,
        "and must not move the focus/IME edge a shell wakes on"
    );
}

#[test]
fn tapping_the_grid_claims_focus_and_publishes_an_active_ime_surface() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    // The hit test runs against the loaded tab world, so wait for it to land.
    await_workspace(&mut h);
    let scene = h.frame();
    let focused = focused_pane_id().expect("the mock seeds one focused pane per tab");

    h.tap(cell_point(&scene, FOCUSED_CELL.0, FOCUSED_CELL.1));

    assert_eq!(
        h.session(),
        (true, true),
        "a tap on the focused pane is the one thing that opens the session"
    );
    assert_eq!(
        focused_pane_id(),
        Some(focused),
        "…and it moved no focus: the pane it landed on was already the one"
    );
    assert!(
        h.root.ime_state().expect("a surface is published").active,
        "and the surface it publishes is an active one"
    );
}

#[test]
fn a_tap_on_an_unfocused_pane_focuses_it_and_summons_no_keyboard() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    await_workspace(&mut h);
    let scene = h.frame();
    let before = focused_pane_id().expect("the mock seeds one focused pane per tab");

    h.tap(cell_point(&scene, SIBLING_CELL.0, SIBLING_CELL.1));

    assert_eq!(
        h.session(),
        (true, false),
        "the tap claims focus, exactly as every Down does, and publishes nothing"
    );
    h.pump_until("the FocusPane to land", |_| {
        focused_pane_id().is_some_and(|id| id != before)
    });
    // The focused-paint republish is gated on engagement, not on focus.
    h.frame();
    assert_eq!(
        h.session(),
        (true, false),
        "no later paint resurrects a surface the tap declined to publish"
    );
}

#[test]
fn a_hidden_full_size_floating_pane_is_never_targeted_by_a_tap_or_a_spread() {
    let _g = serial();
    let _ambient = setup();
    seed_test_floating_overlay(Some(false));
    let mut h = Harness::mount();
    h.enter_demo();
    await_workspace(&mut h);
    let scene = h.frame();
    let hooks = debug_grid_hooks().expect("a live session is mounted");
    let left = focused_pane_id().expect("the mock seeds one focused pane per tab");
    let right = left + 1;
    clear_verb_log_for_tests();

    let on_content_tap = hooks.on_content_tap.expect("zellij wires the tap hook");
    on_content_tap(u32::from(SIBLING_CELL.0), u32::from(SIBLING_CELL.1));
    h.pump_for(Duration::from_millis(100));
    let log = verb_log_for_tests();
    assert!(
        log.contains(&format!("focus_pane:{right}")),
        "the tap focuses the tiled pane under it, got {log:?}"
    );
    assert!(!log.contains(&format!("focus_pane:{TEST_FLOATING_PANE_ID}")));

    let on_pinch = hooks.on_pinch.expect("zellij wires the pinch hook");
    on_pinch(
        muxr::features::terminal::presentation::term::PinchIntent::EnterFullscreen,
        cell_point(&scene, FOCUSED_CELL.0, FOCUSED_CELL.1),
    );
    h.pump_for(Duration::from_millis(100));
    let log = verb_log_for_tests();
    assert!(
        log.iter()
            .any(|verb| verb.starts_with("toggle_pane_fullscreen:")
                && !verb.contains(&format!(":{TEST_FLOATING_PANE_ID}:"))),
        "the spread toggles a tiled pane, got {log:?}"
    );
    assert!(
        !log.iter()
            .any(|verb| verb.contains(&format!(":{TEST_FLOATING_PANE_ID}:")))
    );
    assert!(
        !log.iter()
            .any(|verb| verb.contains(&format!(":{TEST_FLOATING_PANE_ID}"))),
        "the hidden layer's pane never appears in the verb log, got {log:?}"
    );
    h.unmount_screen();

    seed_test_floating_overlay(Some(true));
    let mut h = Harness::mount();
    h.enter_demo();
    await_workspace(&mut h);
    h.frame();
    clear_verb_log_for_tests();
    let on_content_tap = debug_grid_hooks()
        .and_then(|hooks| hooks.on_content_tap)
        .expect("zellij wires the tap hook");
    on_content_tap(u32::from(FOCUSED_CELL.0), u32::from(FOCUSED_CELL.1));
    h.pump_for(Duration::from_millis(100));
    assert!(
        verb_log_for_tests().contains(&format!("focus_pane:{TEST_FLOATING_PANE_ID}")),
        "with the layer visible the floating pane takes the tap"
    );
    seed_test_floating_overlay(None);
}

/// zellij flags every client-active pane `is_fullscreen` while one pane is fullscreen.
#[test]
fn a_converge_exits_the_focused_fullscreen_pane_not_a_union_flagged_sibling() {
    let _g = serial();
    let _ambient = setup();
    seed_test_floating_overlay(Some(false));
    let mut h = Harness::mount();
    h.enter_demo();
    await_workspace(&mut h);
    let scene = h.frame();
    let hooks = debug_grid_hooks().expect("a live session is mounted");
    let first = focused_pane_id().expect("the mock seeds one focused pane per tab");
    let second = first + 1;

    let on_content_tap = hooks.on_content_tap.expect("zellij wires the tap hook");
    on_content_tap(u32::from(SIBLING_CELL.0), u32::from(SIBLING_CELL.1));
    h.pump_for(Duration::from_millis(100));
    assert_eq!(focused_pane_id(), Some(second));

    seed_test_union_fullscreen(&[first]);
    let on_pinch = hooks.on_pinch.expect("zellij wires the pinch hook");
    on_pinch(
        PinchIntent::EnterFullscreen,
        cell_point(&scene, SIBLING_CELL.0, SIBLING_CELL.1),
    );
    h.pump_for(Duration::from_millis(100));
    let tab = debug_controller()
        .and_then(|c| c.active_tab_entry())
        .expect("an active tab");
    assert!(tab.panes.iter().any(|p| p.id == first && p.is_fullscreen));
    clear_verb_log_for_tests();

    on_pinch(
        PinchIntent::ExitFullscreen,
        cell_point(&scene, FOCUSED_CELL.0, FOCUSED_CELL.1),
    );
    h.pump_for(Duration::from_millis(100));
    let log = verb_log_for_tests();
    assert_eq!(
        log.last(),
        Some(&format!("toggle_pane_fullscreen:{second}:false")),
        "got {log:?}"
    );
    assert!(!log.contains(&format!("toggle_pane_fullscreen:{first}:false")));
    seed_test_union_fullscreen(&[]);
    seed_test_floating_overlay(None);
}

#[test]
fn a_second_tap_on_the_newly_focused_pane_raises_the_keyboard() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    await_workspace(&mut h);
    let scene = h.frame();
    let before = focused_pane_id().expect("the mock seeds one focused pane per tab");

    let at = cell_point(&scene, SIBLING_CELL.0, SIBLING_CELL.1);
    h.tap(at);
    h.pump_until("the FocusPane to land", |_| {
        focused_pane_id().is_some_and(|id| id != before)
    });
    assert_eq!(h.session(), (true, false), "the first tap raised nothing");

    h.tap(at);
    assert_eq!(
        h.session(),
        (true, true),
        "the second tap on the pane the session is now on raises the keyboard"
    );
    assert!(h.root.ime_state().expect("a surface is published").active);
}

#[test]
fn a_drag_scroll_publishes_no_ime_surface() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    let scene = h.frame();

    let start = cell_point(&scene, FOCUSED_CELL.0, FOCUSED_CELL.1);
    drag_scroll(&mut h, start);

    assert_eq!(
        h.session(),
        (true, false),
        "a scroll claims focus like any Down and publishes no surface"
    );
    h.frame();
    assert_eq!(
        h.session(),
        (true, false),
        "…and no paint after it puts one up either"
    );
}

/// herdr wires no tap hook; the scroll runs first so the tap's own claim cannot mask it.
#[test]
fn herdr_keeps_tap_to_keyboard_while_its_scroll_raises_nothing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session("herdr:work");
    h.enter_demo();
    let scene = h.frame();
    assert!(
        debug_grid_hooks()
            .expect("a live session is mounted")
            .on_content_tap
            .is_none(),
        "herdr wires no tap hook — that absence is what this pins"
    );

    let at = cell_point(&scene, FOCUSED_CELL.0, FOCUSED_CELL.1);
    drag_scroll(&mut h, at);
    h.frame();
    assert_eq!(
        h.session(),
        (true, false),
        "a scroll never raises a keyboard on herdr either — not in the event \
         pass, and not on the paint after it"
    );

    h.tap(at);
    assert_eq!(
        h.session(),
        (true, true),
        "…but a tap does, with nothing to ask about it"
    );
    assert!(h.root.ime_state().expect("a surface is published").active);
}

/// `side_sheet_tree`'s private mid-level tab radius: herdr's nested tab rect.
const TREE_TAB_RADIUS: f64 = 7.0;

/// Wide enough to be a row, not the pill mark box or focused dot drawn at the same radius.
fn tree_pane_rows(scene: &RecScene) -> Vec<(Point, Size)> {
    let mut candidates: Vec<(Point, Size)> = scene
        .rounded_fills
        .iter()
        .filter(|(_, _, r, color)| {
            (*r - muxr::ui::paint::RADIUS_FULL).abs() < EPSILON
                && *color != peniko::Color::TRANSPARENT
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

/// Waits for the first workspace load: a sheet opened before the tab world lands lists nothing.
fn open_sheet(h: &mut Harness) {
    h.pump_until("the workspace to load", |_| {
        debug_controller().is_some_and(|c| c.loaded.get_untracked())
    });
    h.settle();
    h.open_menu();
}

#[test]
fn a_pane_commit_moves_the_session_without_disturbing_the_live_pane() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    let emulator = Rc::clone(&debug_live_probe().expect("a live pane is mounted").emulator);
    let feed = demo_feed();

    open_sheet(&mut h);
    let rows = tree_pane_rows(&h.frame());
    assert_eq!(rows.len(), 2, "devbox's own panes, pre-expanded: {rows:?}");
    h.tap(center(rows[1]));
    h.pump_until_closed("the sheet to close", |h| {
        !support::node_values(&h.root)
            .iter()
            .any(|v| v == SHEET_LABEL)
    });

    let probe = debug_live_probe().expect("the live pane survived the commit");
    assert!(
        Rc::ptr_eq(&probe.emulator, &emulator),
        "the very same emulator, so the grid was never rebuilt"
    );
    assert!(
        Rc::ptr_eq(&demo_feed(), &feed),
        "…and the very same feed, so nothing re-attached behind the sheet"
    );
    assert_eq!(
        h.session(),
        (false, false),
        "committing from the sheet summons no keyboard"
    );
}

#[test]
fn the_trees_nested_tab_rect_follows_the_attached_sessions_own_backend() {
    let _g = serial();
    let _ambient = setup();

    let mut zellij = Harness::mount();
    open_sheet(&mut zellij);
    let scene = zellij.frame();
    assert!(
        scene
            .rounded_fills
            .iter()
            .filter(|(_, _, r, _)| (*r - TREE_TAB_RADIUS).abs() < EPSILON)
            .count()
            == 0,
        "zellij has no spaces, so its tabs are promoted to the card shape \
         instead of the nested tab rect"
    );
    drop(zellij);

    let mut herdr = Harness::mount_session("herdr:work");
    open_sheet(&mut herdr);
    let scene = herdr.frame();
    assert!(
        scene
            .rounded_fills
            .iter()
            .any(|(_, _, r, _)| (*r - TREE_TAB_RADIUS).abs() < EPSILON),
        "herdr has spaces, so its tabs wear the nested tab rect"
    );
}

#[test]
fn the_grid_gesture_hooks_wire_tap_only_off_herdr_and_wheel_always() {
    let _g = serial();
    let _ambient = setup();

    let mut zellij = Harness::mount_session("zellij:dev");
    zellij.enter_demo();
    let hooks = debug_grid_hooks().expect("a live session is mounted");
    assert!(
        hooks.on_content_tap.is_some(),
        "not herdr, so the grid's content-tap hook is wired"
    );
    assert!(
        hooks.on_wheel.is_some(),
        "the wheel hook is always wired once a live session exists"
    );
    // A real unmount disposes the background tasks; `on_cleanup` never fires from a bare `Drop`.
    zellij.unmount_screen();

    let mut herdr = Harness::mount_session("herdr:work");
    herdr.enter_demo();
    let hooks = debug_grid_hooks().expect("a live session is mounted");
    assert!(
        hooks.on_content_tap.is_none(),
        "herdr's single-pane stream has nothing a tap could focus beyond \
         the claim its own Down already makes"
    );
    assert!(hooks.on_wheel.is_some(), "scroll still works on herdr");
}

#[test]
fn invoking_the_wired_hooks_never_panics_on_a_hit_a_miss_or_either_wheel_route() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session("zellij:dev");
    h.enter_demo();
    let hooks = debug_grid_hooks().expect("a live session is mounted");
    let on_content_tap = hooks.on_content_tap.expect("zellij wires the tap hook");
    let on_wheel = hooks.on_wheel.expect("the wheel hook is always wired");

    // A no-hit tap: the mock's seeded panes tile (0,0)-(80,24) between them.
    assert!(
        on_content_tap(500, 500),
        "a tap that resolves to nothing raises the keyboard"
    );
    h.pump_for(Duration::from_millis(50));

    assert!(
        on_content_tap(u32::from(FOCUSED_CELL.0), u32::from(FOCUSED_CELL.1)),
        "a tap on the focused pane raises the keyboard"
    );
    h.pump_for(Duration::from_millis(50));

    assert!(
        !on_content_tap(u32::from(SIBLING_CELL.0), u32::from(SIBLING_CELL.1)),
        "a tap that moves the focus raises no keyboard"
    );
    h.pump_for(Duration::from_millis(50));

    // Capability present: the mouse-frame route; absent below: the `ScrollPane` fallback.
    on_wheel(true, 5, 5);
    on_wheel(false, 5, 5);
    h.pump_for(Duration::from_millis(50));

    let cc = expect_connection_controller();
    let mut info = cc.server_info.get_untracked().expect("connected");
    info.capabilities.clear();
    cc.server_info.set(Some(info));
    on_wheel(true, 5, 5);
    h.pump_for(Duration::from_millis(50));

    h.frame();
}

// The wire has no layout-changed signal, so each trigger must ask again. `add_test_tab` is
// process-global: every test clears it on both sides, serialized.

/// Appended past the mock's four seeded tabs.
const SCRATCH_TAB: &str = "scratch";
/// The tree prints `TabEntry::label` directly, not the index-prefixed `pill_label`.
const SCRATCH_PILL: &str = SCRATCH_TAB;

/// The background runtime's timer cannot be faked, so a poll tick is invoked directly.
fn drive(fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
        .block_on(fut);
}

fn await_workspace(h: &mut Harness) {
    h.pump_until("the workspace to load", |_| {
        debug_controller().is_some_and(|c| c.loaded.get_untracked())
    });
}

fn published_tab(key: &str) -> bool {
    debug_controller()
        .expect("a terminal screen is mounted")
        .tabs
        .get_untracked()
        .iter()
        .any(|tab| tab.key == key)
}

#[test]
fn a_layout_poll_tick_picks_up_a_tab_opened_elsewhere_and_an_idle_one_writes_nothing() {
    let _g = serial();
    let _ambient = setup();
    clear_test_tabs();
    let mut h = Harness::mount();
    await_workspace(&mut h);
    let controller = debug_controller().expect("a terminal screen is mounted");
    assert!(!published_tab(SCRATCH_TAB), "nothing has opened one yet");

    add_test_tab(SCRATCH_TAB);
    assert!(controller.take_layout_poll_tick(), "the poll is armed");
    let tick = Arc::clone(&controller);
    drive(Box::pin(
        async move { tick.refresh_layout_from_poll().await },
    ));

    assert!(published_tab(SCRATCH_TAB), "the tick picked it up");
    h.frame();

    // The poll runs for the life of the mount: an idle tick that woke a rebuild would wake forever.
    let idle = controller.layout_writes();
    assert!(controller.take_layout_poll_tick());
    let tick = Arc::clone(&controller);
    drive(Box::pin(
        async move { tick.refresh_layout_from_poll().await },
    ));
    assert_eq!(
        controller.layout_writes(),
        idle,
        "an unchanged answer wakes no rebuild"
    );

    clear_test_tabs();
}

#[test]
fn opening_the_sheet_re_reads_the_layout_without_blocking_the_open_on_it() {
    let _g = serial();
    let _ambient = setup();
    clear_test_tabs();
    let mut h = Harness::mount();
    await_workspace(&mut h);

    add_test_tab(SCRATCH_TAB);
    open_sheet(&mut h);

    assert!(
        support::node_values(&h.root)
            .iter()
            .any(|v| v == SHEET_LABEL),
        "the sheet is up on the state it already had — the open is never \
         blocked on the round trip behind it"
    );
    h.pump_until("the sheet to list the tab opened elsewhere", |h| {
        support::node_values(&h.root)
            .iter()
            .any(|v| v == SCRATCH_PILL)
    });

    clear_test_tabs();
}

#[test]
fn the_first_content_tap_that_resolves_to_nothing_re_reads_the_layout_once() {
    let _g = serial();
    let _ambient = setup();
    clear_test_tabs();
    let mut h = Harness::mount_session("zellij:dev");
    h.enter_demo();
    await_workspace(&mut h);
    let on_content_tap = debug_grid_hooks()
        .expect("a live session is mounted")
        .on_content_tap
        .expect("zellij wires the tap hook");

    // Taps hit-test the last layout answer, so the first miss re-reads a stale one.
    add_test_tab(SCRATCH_TAB);
    assert!(
        on_content_tap(500, 500),
        "a miss still answers 'raise' — the backstop is about the layout, not \
         the keyboard"
    );
    h.pump_until("the backstop reload", |_| published_tab(SCRATCH_TAB));

    clear_test_tabs();
    add_test_tab("scratch-two");
    assert!(on_content_tap(500, 500));
    h.pump_for(Duration::from_millis(200));
    assert!(
        !published_tab("scratch-two"),
        "the backstop is a one-shot, not a reload on every miss"
    );

    clear_test_tabs();
}

#[test]
fn the_sheet_body_composes_a_scroll_container() {
    // Headless has no `WindowMetrics`, so this pins structure, a `Role::ScrollView` node, not size.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session("zellij:dev");
    open_sheet(&mut h);
    assert!(
        support::node_roles(&h.root).contains(&Role::ScrollView),
        "the sheet body has no scroll container: {:?}",
        support::node_roles(&h.root)
    );
}

fn focused_pane_of(key: &str) -> Option<u32> {
    let tabs = debug_controller().expect("mounted").tabs.get_untracked();
    let tab = tabs.into_iter().find(|tab| tab.key == key)?;
    tab.panes
        .iter()
        .find(|pane| pane.focused)
        .map(|pane| pane.id)
}

#[test]
fn a_deep_link_lands_on_its_tab_and_pane_once() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    // Position 2 is harbor (tab id 3); pane 31 is its unfocused sibling.
    let mut h = Harness::mount_at("/terminal?session=s-1&tab=2&pane=31");
    h.pump_until("the target", |_| verb_log_for_tests().len() >= 2);
    h.pump_for(Duration::from_millis(100));

    assert_eq!(verb_log_for_tests(), ["go_to_tab:3", "focus_pane:31"]);
    let c = debug_controller().expect("mounted");
    assert_eq!(c.active_tab.get_untracked(), "harbor");
    assert_eq!(focused_pane_of("harbor"), Some(31));

    let again = Arc::clone(&c);
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
        .block_on(async move {
            again.reload_tabs().await;
            let _ = again.mirror_link(LinkStatus::Connected, Some("c-2".to_string()));
            again.apply_attach_target().await;
        });
    h.pump_for(Duration::from_millis(100));
    assert_eq!(
        verb_log_for_tests(),
        ["go_to_tab:3", "focus_pane:31"],
        "no re-apply"
    );
}

#[test]
fn a_deep_link_naming_no_such_tab_or_pane_sends_nothing() {
    let _g = serial();
    let _ambient = setup();
    for location in [
        "/terminal?session=s-1&tab=9&pane=31",
        "/terminal?session=s-1&tab=2&pane=99",
        "/terminal?session=s-1&tab=x&pane=y",
    ] {
        clear_verb_log_for_tests();
        let mut h = Harness::mount_at(location);
        // A fixed settle: no mock-idle signal exists, and a negative can only wait out the window.
        h.pump_for(Duration::from_millis(150));
        assert!(
            verb_log_for_tests().is_empty(),
            "{location}: {:?}",
            verb_log_for_tests()
        );
        drop(h);
    }
}

// herdr spaces: `forgekit` selected, `zelli_mobile` foreign with `logs` (101) and `worker` (102).
/// The mock seeds transcripts only for the selected space's tabs, so this answers every one empty.
struct QuietLines(SharedTerminalRepository);

#[clean_signals::async_trait]
impl TerminalRepository for QuietLines {
    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure> {
        self.0.list_spaces().await
    }
    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
        self.0.list_tabs().await
    }
    async fn list_tabs_in_space(&self, space_id: &str) -> Result<Vec<TabEntry>, AppFailure> {
        self.0.list_tabs_in_space(space_id).await
    }
    async fn tab_lines(&self, _key: String) -> Result<Vec<TermLine>, AppFailure> {
        Ok(Vec::new())
    }
    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure> {
        self.0.switch_space(space_id).await
    }
    async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure> {
        self.0.go_to_tab(tab_id).await
    }
    async fn detach_session(&self, id: String) -> Result<(), AppFailure> {
        self.0.detach_session(id).await
    }
    async fn focus_pane(&self, pane_id: u32, is_plugin: bool) -> Result<(), AppFailure> {
        self.0.focus_pane(pane_id, is_plugin).await
    }
}

fn live_foreign_link(rt: &tokio::runtime::Runtime) -> Arc<TerminalController> {
    let _state = MuxrApp.init();
    let c = Arc::new(TerminalController::new(
        Arc::new(QuietLines(expect_terminal_repository())),
        expect_sessions_repository(),
        expect_preferences_repository(),
        Some("herdr:work".to_string()),
    ));
    c.set_attach_target_in_space(Some("zelli_mobile".to_string()), Some(1), Some(21));
    c.attach_live();
    rt.block_on(c.load());
    assert!(c.mirror_link(LinkStatus::Connected, Some("c-1".to_string())));
    c
}

#[test]
fn a_deep_link_into_a_foreign_space_switches_space_then_tab_then_pane() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    let rt = runtime();
    let c = live_foreign_link(&rt);
    rt.block_on(async {
        c.reload_tabs().await;
        c.apply_attach_target().await;
    });

    assert_eq!(
        verb_log_for_tests(),
        [
            "switch_space:zelli_mobile",
            "go_to_tab:102",
            "focus_pane:21"
        ]
    );
    assert_eq!(c.active_tab.get_untracked(), "worker");
    assert_eq!(c.pending_attach_target_for_tests(), None, "applied once");
    assert_eq!(
        c.resume_slot().target().space_id.as_deref(),
        Some("zelli_mobile"),
        "a reconnect resumes in the linked space"
    );
}

#[test]
fn a_foreign_space_link_naming_a_missing_tab_or_pane_moves_nothing() {
    let _g = serial();
    let _ambient = setup();
    for location in [
        "/terminal?session=herdr:work&space=zelli_mobile&tab=9&pane=21",
        "/terminal?session=herdr:work&space=zelli_mobile&tab=1&pane=99",
        "/terminal?session=herdr:work&space=nowhere&tab=1&pane=21",
    ] {
        clear_verb_log_for_tests();
        let mut h = Harness::mount_at(location);
        h.pump_for(Duration::from_millis(150));
        assert!(
            verb_log_for_tests().is_empty(),
            "{location}: {:?}",
            verb_log_for_tests()
        );
        drop(h);
    }
}

#[test]
fn a_link_naming_the_selected_space_behaves_as_one_without() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    // `forgekit` is the selected space; position 2 is harbor (tab id 3).
    let mut h = Harness::mount_at("/terminal?session=herdr:work&space=forgekit&tab=2&pane=31");
    h.pump_until("the target", |_| verb_log_for_tests().len() >= 2);
    h.pump_for(Duration::from_millis(100));

    assert_eq!(verb_log_for_tests(), ["go_to_tab:3", "focus_pane:31"]);
}

#[test]
fn a_user_move_supersedes_a_foreign_space_link() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    let rt = runtime();
    let c = live_foreign_link(&rt);
    assert!(c.pending_attach_target_for_tests().is_some());

    rt.block_on(async {
        assert!(c.commit_pane("btop".to_string(), 11, false).await);
    });
    assert_eq!(c.pending_attach_target_for_tests(), None);
    rt.block_on(async {
        c.reload_tabs().await;
        c.apply_attach_target().await;
    });
    assert_eq!(verb_log_for_tests(), ["go_to_tab:1", "focus_pane:11"]);
}

struct NoSelectedSpace(QuietLines);

#[clean_signals::async_trait]
impl TerminalRepository for NoSelectedSpace {
    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure> {
        let spaces = self.0.list_spaces().await?;
        Ok(spaces
            .into_iter()
            .map(|space| Space {
                selected: false,
                ..space
            })
            .collect())
    }
    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
        self.0.list_tabs().await
    }
    async fn list_tabs_in_space(&self, space_id: &str) -> Result<Vec<TabEntry>, AppFailure> {
        self.0.list_tabs_in_space(space_id).await
    }
    async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure> {
        self.0.tab_lines(key).await
    }
    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure> {
        self.0.switch_space(space_id).await
    }
    async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure> {
        self.0.go_to_tab(tab_id).await
    }
    async fn detach_session(&self, id: String) -> Result<(), AppFailure> {
        self.0.detach_session(id).await
    }
    async fn focus_pane(&self, pane_id: u32, is_plugin: bool) -> Result<(), AppFailure> {
        self.0.focus_pane(pane_id, is_plugin).await
    }
}

#[test]
fn a_space_link_with_no_selected_space_peeks_the_space_and_switches_into_it() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    let rt = runtime();
    let _state = MuxrApp.init();
    let c = Arc::new(TerminalController::new(
        Arc::new(NoSelectedSpace(QuietLines(expect_terminal_repository()))),
        expect_sessions_repository(),
        expect_preferences_repository(),
        Some("herdr:work".to_string()),
    ));
    c.set_attach_target_in_space(Some("zelli_mobile".to_string()), Some(1), Some(21));
    c.attach_live();
    rt.block_on(c.load());
    assert!(c.mirror_link(LinkStatus::Connected, Some("c-1".to_string())));
    assert!(c.spaces.get_untracked().iter().all(|space| !space.selected));

    rt.block_on(async {
        c.reload_tabs().await;
        c.apply_attach_target().await;
    });

    assert_eq!(
        verb_log_for_tests(),
        [
            "switch_space:zelli_mobile",
            "go_to_tab:102",
            "focus_pane:21"
        ],
        "resolved against the peeked space, never the current tab world"
    );
    assert_eq!(c.pending_attach_target_for_tests(), None);
}

#[test]
fn a_space_link_on_a_backend_without_spaces_moves_nothing() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    // Position 2 and pane 31 resolve in the mock's tab world: without the space the link lands.
    let location = format!("/terminal?session={SESSION_ID}&space=forgekit&tab=2&pane=31");
    let mut h = Harness::mount_at(&location);
    await_workspace(&mut h);
    h.pump_for(Duration::from_millis(150));

    assert!(
        verb_log_for_tests().is_empty(),
        "{:?}",
        verb_log_for_tests()
    );
}

/// Boxed so a test can open a round trip, leave it parked, and land it later.
type Step<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
}

fn start(rt: &tokio::runtime::Runtime, step: &mut Step<'_>) {
    rt.block_on(std::future::poll_fn(|cx| {
        assert!(
            step.as_mut().poll(cx).is_pending(),
            "parked on a round trip"
        );
        Poll::Ready(())
    }));
}

fn live_deep_link() -> Arc<TerminalController> {
    let _state = MuxrApp.init();
    live_deep_link_over(expect_terminal_repository())
}

fn live_deep_link_over(repository: SharedTerminalRepository) -> Arc<TerminalController> {
    let c = Arc::new(TerminalController::new(
        repository,
        expect_sessions_repository(),
        expect_preferences_repository(),
        Some(SESSION_ID.to_string()),
    ));
    c.set_attach_target(Some(2), Some(31));
    c.attach_live();
    c
}

#[test]
fn a_load_fenced_out_behind_the_settle_leaves_the_deep_link_to_the_settled_reload() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    let c = live_deep_link();
    let rt = runtime();

    let mut load: Step = Box::pin(c.load());
    start(&rt, &mut load);
    assert!(c.mirror_link(LinkStatus::Connected, Some("c-1".to_string())));
    let mut settled: Step = Box::pin(async {
        c.reload_tabs().await;
        c.apply_attach_target().await;
    });
    start(&rt, &mut settled);
    rt.block_on(load);
    assert!(
        verb_log_for_tests().is_empty(),
        "the load's tabs were fenced out"
    );
    assert!(c.tabs.get_untracked().is_empty(), "…and never published");

    rt.block_on(settled);
    assert_eq!(verb_log_for_tests(), ["go_to_tab:3", "focus_pane:31"]);
    assert_eq!(c.active_tab.get_untracked(), "harbor");
}

#[test]
fn a_handshake_that_never_settles_neither_applies_nor_drops_the_deep_link() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    let c = live_deep_link();

    runtime().block_on(async {
        c.load().await;
        c.reload_tabs().await;
        c.apply_attach_target().await;
    });
    assert!(
        verb_log_for_tests().is_empty(),
        "{:?}",
        verb_log_for_tests()
    );
    assert_eq!(
        c.pending_attach_target_for_tests(),
        Some(AttachTarget {
            space_id: None,
            tab_position: Some(2),
            pane_id: Some(31),
        }),
        "still held for a settle that may yet come"
    );
}

/// A move the user made after mount wins once it lands; while in flight it only defers the link.
#[test]
fn a_user_move_in_flight_at_the_settle_supersedes_the_deep_link() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    let c = live_deep_link();
    let rt = runtime();
    rt.block_on(c.load());

    let mut tap: Step = Box::pin(async {
        assert!(c.commit_pane("btop".to_string(), 11, false).await);
    });
    start(&rt, &mut tap);
    assert!(
        c.pending_attach_target_for_tests().is_some(),
        "kept while the move is in flight"
    );
    assert!(c.mirror_link(LinkStatus::Connected, Some("c-1".to_string())));
    rt.block_on(async {
        c.reload_tabs().await;
        c.apply_attach_target().await;
    });
    rt.block_on(tap);
    assert_eq!(
        c.pending_attach_target_for_tests(),
        None,
        "dropped once the move landed"
    );

    assert_eq!(verb_log_for_tests(), ["go_to_tab:1", "focus_pane:11"]);
    assert_eq!(c.active_tab.get_untracked(), "btop");
}

#[test]
fn a_grid_tap_in_flight_at_the_settle_defers_the_deep_link() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    let c = live_deep_link();
    let rt = runtime();
    rt.block_on(c.load());
    let pane = 11;

    let mut tap: Step = Box::pin(async {
        assert!(c.focus_pane_from_tap(pane, false).await);
    });
    start(&rt, &mut tap);
    assert!(c.mirror_link(LinkStatus::Connected, Some("c-1".to_string())));
    rt.block_on(async {
        c.reload_tabs().await;
        c.apply_attach_target().await;
    });
    assert!(
        c.pending_attach_target_for_tests().is_some(),
        "kept while the tap is in flight"
    );
    assert!(
        verb_log_for_tests().is_empty(),
        "no verb has landed, the link's included: {:?}",
        verb_log_for_tests()
    );

    rt.block_on(tap);
    assert_eq!(c.pending_attach_target_for_tests(), None, "the landed tap");
    assert_eq!(verb_log_for_tests(), ["focus_pane:11"]);
}

/// The mock answers `new_tab` as unsupported; this create parks on a timer, as the wire's does.
struct SlowCreate(SharedTerminalRepository);

#[clean_signals::async_trait]
impl TerminalRepository for SlowCreate {
    async fn list_spaces(&self) -> Result<Vec<Space>, AppFailure> {
        self.0.list_spaces().await
    }
    async fn list_tabs(&self) -> Result<Vec<TabEntry>, AppFailure> {
        self.0.list_tabs().await
    }
    async fn tab_lines(&self, key: String) -> Result<Vec<TermLine>, AppFailure> {
        self.0.tab_lines(key).await
    }
    async fn switch_space(&self, space_id: String) -> Result<(), AppFailure> {
        self.0.switch_space(space_id).await
    }
    async fn go_to_tab(&self, tab_id: u64) -> Result<(), AppFailure> {
        self.0.go_to_tab(tab_id).await
    }
    async fn detach_session(&self, id: String) -> Result<(), AppFailure> {
        self.0.detach_session(id).await
    }
    async fn focus_pane(&self, pane_id: u32, is_plugin: bool) -> Result<(), AppFailure> {
        self.0.focus_pane(pane_id, is_plugin).await
    }
    async fn new_tab(&self, _name: String) -> Result<(), AppFailure> {
        clean_signals::time::sleep(Duration::from_millis(20)).await;
        Ok(())
    }
}

#[test]
fn a_new_tab_in_flight_at_the_settle_defers_the_deep_link() {
    let _g = serial();
    let _ambient = setup();
    clear_verb_log_for_tests();
    let _state = MuxrApp.init();
    let c = live_deep_link_over(Arc::new(SlowCreate(expect_terminal_repository())));
    let rt = runtime();
    rt.block_on(c.load());

    let mut create: Step = Box::pin(async {
        c.new_tab("x".to_string()).await;
    });
    start(&rt, &mut create);
    assert!(c.mirror_link(LinkStatus::Connected, Some("c-1".to_string())));
    rt.block_on(async {
        c.reload_tabs().await;
        c.apply_attach_target().await;
    });
    assert!(
        verb_log_for_tests().is_empty(),
        "no deep-link verbs while the create is in flight: {:?}",
        verb_log_for_tests()
    );
    assert!(c.pending_attach_target_for_tests().is_some());

    rt.block_on(create);
    assert_eq!(
        c.pending_attach_target_for_tests(),
        None,
        "the landed create"
    );
}

// The mock connection advertises no `agents-stream`, so each streaming test switches it on first.

struct AgentsReset;

impl Drop for AgentsReset {
    fn drop(&mut self) {
        clear_test_agents();
    }
}

fn streamed_agent(pane_id: u32, status: AgentStatus, change_seq: u64) -> AgentEntry {
    AgentEntry {
        pane_id,
        tab_id: 4,
        tab_key: "devbox".to_string(),
        space_id: None,
        space_name: None,
        tab_name: "devbox".to_string(),
        kind: "claude".to_string(),
        name: Some(format!("agent{pane_id}")),
        display_kind: None,
        status,
        title: None,
        summary: None,
        cwd: None,
        focused: false,
        change_seq,
    }
}

fn agent_panes() -> Vec<u32> {
    debug_controller()
        .expect("a terminal screen is mounted")
        .agents
        .get_untracked()
        .iter()
        .map(|agent| agent.pane_id)
        .collect()
}

fn mount_watching(agents: Vec<AgentEntry>, streaming: bool) -> (Harness, AgentsReset) {
    clear_test_agents();
    seed_agents(agents.clone());
    let reset = AgentsReset;
    let mut h = Harness::mount();
    await_workspace(&mut h);
    if streaming {
        let cc = expect_connection_controller();
        let mut info = cc.server_info.get_untracked().expect("connected");
        info.capabilities.insert(CAP_AGENTS_STREAM.to_string());
        cc.server_info.set(Some(info));
    }
    h.enter_demo();
    h.pump_until("the agents to load", |_| {
        agent_panes().len() == agents.len()
    });
    // Let the mount's own relist land too, so the safety relist's window starts from it.
    h.pump_for(Duration::from_millis(100));
    (h, reset)
}

fn poll_tick(controller: &Arc<TerminalController>) {
    assert!(controller.take_layout_poll_tick(), "the poll is armed");
    let tick = Arc::clone(controller);
    drive(Box::pin(
        async move { tick.refresh_layout_from_poll().await },
    ));
}

fn working_and_idle() -> Vec<AgentEntry> {
    vec![
        streamed_agent(40, AgentStatus::Working, 3),
        streamed_agent(41, AgentStatus::Idle, 2),
    ]
}

#[test]
fn a_pushed_blocked_change_re_sorts_its_row_to_the_top_within_one_frame_without_a_poll() {
    let _g = serial();
    let _ambient = setup();
    let opens = agent_watch_opens_for_tests();
    let (mut h, _reset) = mount_watching(working_and_idle(), true);
    assert_eq!(
        agent_watch_opens_for_tests(),
        opens + 1,
        "the stream opened"
    );
    assert_eq!(agent_panes(), [40, 41]);

    let blocked = Box::new(streamed_agent(41, AgentStatus::Blocked, 9));
    assert!(push_agent_event_for_tests(AgentEventEntry::Changed(
        blocked
    )));
    h.one_frame();

    assert_eq!(agent_panes(), [41, 40], "the blocked row leads");
    let controller = debug_controller().expect("mounted");
    assert_eq!(
        controller.agents.get_untracked()[0].status,
        AgentStatus::Blocked
    );
    assert!(controller.has_blocked_agent());
}

#[test]
fn a_pushed_removal_drops_its_row() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, _reset) = mount_watching(working_and_idle(), true);

    assert!(push_agent_event_for_tests(AgentEventEntry::Removed(40)));
    h.one_frame();

    assert_eq!(agent_panes(), [41]);
}

#[test]
fn a_pushed_snapshot_replaces_the_whole_list() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, _reset) = mount_watching(working_and_idle(), true);

    let fresh = vec![streamed_agent(50, AgentStatus::Done, 1)];
    assert!(push_agent_event_for_tests(AgentEventEntry::Snapshot(fresh)));
    h.one_frame();

    assert_eq!(agent_panes(), [50]);
}

#[test]
fn without_the_capability_no_stream_opens_and_the_poll_still_refreshes() {
    let _g = serial();
    let _ambient = setup();
    let opens = agent_watch_opens_for_tests();
    let (_h, _reset) = mount_watching(working_and_idle(), false);
    assert_eq!(agent_watch_opens_for_tests(), opens, "nothing to watch");

    let mut agents = working_and_idle();
    agents.push(streamed_agent(42, AgentStatus::Blocked, 5));
    seed_agents(agents);
    poll_tick(&debug_controller().expect("mounted"));

    assert_eq!(agent_panes(), [42, 40, 41], "the poll re-read them");
}

#[test]
fn unmounting_the_screen_stops_the_agents_stream() {
    let _g = serial();
    let _ambient = setup();
    let stops = agent_watch_stops_for_tests();
    let (mut h, _reset) = mount_watching(working_and_idle(), true);
    assert!(agent_watch_open_for_tests());

    h.unmount_screen();

    assert_eq!(
        agent_watch_stops_for_tests(),
        stops + 1,
        "the handle dropped"
    );
    assert!(!agent_watch_open_for_tests());
    assert!(!push_agent_event_for_tests(AgentEventEntry::Removed(40)));
}

#[test]
fn a_lost_stream_falls_back_to_the_poll_until_the_stream_delivers_again() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, _reset) = mount_watching(working_and_idle(), true);
    let controller = debug_controller().expect("mounted");

    drop_agent_stream_for_tests();
    fail_next_list_agents_for_tests();
    poll_tick(&controller);
    assert!(controller.agents_stale.get_untracked());

    let blocked = Box::new(streamed_agent(41, AgentStatus::Blocked, 9));
    assert!(push_agent_event_for_tests(AgentEventEntry::Changed(
        blocked
    )));
    h.one_frame();
    assert!(!controller.agents_stale.get_untracked());
    assert!(controller.has_blocked_agent());

    seed_agents(vec![streamed_agent(60, AgentStatus::Idle, 1)]);
    poll_tick(&controller);
    assert_eq!(agent_panes(), [41, 40], "no re-list under a live stream");

    drop_agent_stream_for_tests();
    poll_tick(&controller);
    assert_eq!(agent_panes(), [60]);
    assert!(!controller.agents_stale.get_untracked());
}

/// The window the controller waits between two opens of a subscription (`AGENT_WATCH_RETRY`).
const WATCH_RETRY: Duration = Duration::from_secs(30);

fn blocked_and_idle() -> Vec<AgentEntry> {
    vec![
        streamed_agent(40, AgentStatus::Blocked, 3),
        streamed_agent(41, AgentStatus::Idle, 2),
    ]
}

#[test]
fn an_ended_watch_is_dropped_on_the_next_tick_without_ever_raising_the_stale_flag() {
    let _g = serial();
    let _ambient = setup();
    let (_h, _reset) = mount_watching(blocked_and_idle(), true);
    let controller = debug_controller().expect("mounted");
    let (raises, stops) = (
        controller.agents_stale_raises(),
        agent_watch_stops_for_tests(),
    );
    assert!(controller.has_blocked_agent());

    end_agent_stream_for_tests(WatchEnd::EndedClean);
    for _ in 0..3 {
        poll_tick(&controller);
        assert!(controller.has_blocked_agent(), "the badge holds");
    }

    assert_eq!(
        controller.agents_stale_raises(),
        raises,
        "never stale, not even mid-tick"
    );
    assert_eq!(
        agent_watch_stops_for_tests(),
        stops + 1,
        "the ended handle dropped"
    );
}

#[test]
fn a_zellij_shaped_watch_leaves_the_badge_stable_and_the_poll_running() {
    let _g = serial();
    let _ambient = setup();
    let (_h, _reset) = mount_watching(Vec::new(), true);
    let controller = debug_controller().expect("mounted");
    let raises = controller.agents_stale_raises();

    end_agent_stream_for_tests(WatchEnd::EndedClean);
    poll_tick(&controller);
    poll_tick(&controller);
    assert!(!controller.has_blocked_agent());
    assert_eq!(controller.agents_stale_raises(), raises);

    seed_agents(vec![streamed_agent(42, AgentStatus::Blocked, 1)]);
    poll_tick(&controller);
    assert_eq!(agent_panes(), [42], "the poll keeps reading them");
    assert_eq!(controller.agents_stale_raises(), raises);
}

#[test]
fn a_cleanly_ended_watch_reopens_once_the_retry_window_passes_while_the_stream_is_advertised() {
    let _g = serial();
    let _ambient = setup();
    let opens = agent_watch_opens_for_tests();
    let (mut h, _reset) = mount_watching(working_and_idle(), true);
    let controller = debug_controller().expect("mounted");

    end_agent_stream_for_tests(WatchEnd::EndedClean);
    poll_tick(&controller);
    assert_eq!(
        agent_watch_opens_for_tests(),
        opens + 1,
        "not inside the window"
    );
    assert!(!agent_watch_open_for_tests());

    advance_agents_clock_for_tests(WATCH_RETRY);
    poll_tick(&controller);
    assert_eq!(agent_watch_opens_for_tests(), opens + 2, "reopened");
    assert!(agent_watch_open_for_tests());
    poll_tick(&controller);
    assert_eq!(agent_watch_opens_for_tests(), opens + 2, "once");

    let blocked = Box::new(streamed_agent(41, AgentStatus::Blocked, 9));
    assert!(push_agent_event_for_tests(AgentEventEntry::Changed(
        blocked
    )));
    h.pump_for(Duration::from_millis(50));
    assert_eq!(agent_panes(), [41, 40]);
}

#[test]
fn an_unauthenticated_watch_never_reopens_for_the_mount() {
    let _g = serial();
    let _ambient = setup();
    let opens = agent_watch_opens_for_tests();
    let (_h, _reset) = mount_watching(working_and_idle(), true);
    let controller = debug_controller().expect("mounted");

    end_agent_stream_for_tests(WatchEnd::EndedUnauthenticated);
    poll_tick(&controller);
    advance_agents_clock_for_tests(WATCH_RETRY * 2);
    poll_tick(&controller);
    poll_tick(&controller);

    assert_eq!(agent_watch_opens_for_tests(), opens + 1);
    assert!(!agent_watch_open_for_tests());
}

#[test]
fn a_watch_not_yet_live_is_not_stale_but_a_live_one_going_down_is_once() {
    let _g = serial();
    let _ambient = setup();
    open_next_agent_watch_down_for_tests();
    let (mut h, _reset) = mount_watching(working_and_idle(), true);
    let controller = debug_controller().expect("mounted");
    let raises = controller.agents_stale_raises();

    poll_tick(&controller);
    poll_tick(&controller);
    assert_eq!(
        controller.agents_stale_raises(),
        raises,
        "still opening, not down"
    );

    let blocked = Box::new(streamed_agent(41, AgentStatus::Blocked, 9));
    assert!(push_agent_event_for_tests(AgentEventEntry::Changed(
        blocked
    )));
    h.one_frame();
    poll_tick(&controller);
    drop_agent_stream_for_tests();
    poll_tick(&controller);
    poll_tick(&controller);
    assert_eq!(
        controller.agents_stale_raises(),
        raises,
        "the drop's relist succeeded: not even an instant of stale"
    );
    assert!(
        !controller.agents_stale.get_untracked(),
        "the poll's read confirmed them"
    );
}

#[test]
fn a_live_stream_dropping_with_a_working_relist_never_reads_stale_but_a_failed_relist_does() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, _reset) = mount_watching(blocked_and_idle(), true);
    let controller = debug_controller().expect("mounted");
    assert!(controller.has_blocked_agent());
    poll_tick(&controller);
    let raises = controller.agents_stale_raises();

    drop_agent_stream_for_tests();
    assert!(controller.take_layout_poll_tick(), "the poll is armed");
    delay_next_list_agents_for_tests(Duration::from_millis(40));
    let tick = Arc::clone(&controller);
    let mut tick = Box::pin(async move { tick.refresh_layout_from_poll().await });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime");
    let mut sampled = 0;
    runtime.block_on(async {
        loop {
            let finished =
                std::future::poll_fn(|cx| Poll::Ready(tick.as_mut().poll(cx).is_ready())).await;
            assert!(!controller.agents_stale.get_untracked(), "never stale");
            if finished {
                break;
            }
            sampled += 1;
            h.one_frame();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
    assert!(sampled > 1, "the relist was in flight across frames");
    assert_eq!(controller.agents_stale_raises(), raises, "no flicker");
    assert!(controller.has_blocked_agent());

    let (mut h, _reset) = mount_watching(working_and_idle(), true);
    let controller = debug_controller().expect("mounted");
    let raises = controller.agents_stale_raises();
    drop_agent_stream_for_tests();
    fail_next_list_agents_for_tests();
    poll_tick(&controller);
    assert_eq!(controller.agents_stale_raises(), raises + 1);
    assert!(controller.agents_stale.get_untracked());
    h.one_frame();
}

fn named_agent(pane_id: u32, name: &str, change_seq: u64) -> AgentEntry {
    AgentEntry {
        name: Some(name.to_string()),
        ..streamed_agent(pane_id, AgentStatus::Working, change_seq)
    }
}

fn agent_names() -> Vec<(u32, String)> {
    let mut rows: Vec<(u32, String)> = debug_controller()
        .expect("mounted")
        .agents
        .get_untracked()
        .iter()
        .map(|row| (row.pane_id, row.name.clone().unwrap_or_default()))
        .collect();
    rows.sort();
    rows
}

#[test]
fn a_poll_only_relist_replaces_rows_after_a_restart_reassigned_the_pane_ids() {
    let _g = serial();
    let _ambient = setup();
    let (_h, _reset) = mount_watching(vec![named_agent(1, "A", 40), named_agent(2, "B", 5)], false);
    let controller = debug_controller().expect("mounted");
    assert_eq!(agent_names(), [(1, "A".to_string()), (2, "B".to_string())]);

    seed_agents(vec![named_agent(1, "B", 5), named_agent(2, "A", 40)]);
    poll_tick(&controller);

    assert_eq!(
        agent_names(),
        [(1, "B".to_string()), (2, "A".to_string())],
        "the relist's rows outright: no duplicate A, B present"
    );
}

/// Grid-local focal px: the left pane (cols 0-40 of the mock's 80), the right one, a gutter.
const PINCH_LEFT: Point = Point::new(50.0, 5.0);
const PINCH_RIGHT: Point = Point::new(450.0, 5.0);
const PINCH_GUTTER: Point = Point::new(790.0, 5.0);

fn pane_fullscreen_flags() -> Vec<(u32, bool)> {
    debug_controller()
        .expect("mounted")
        .active_tab_entry()
        .expect("a loaded tab")
        .panes
        .iter()
        .map(|pane| (pane.id, pane.is_fullscreen))
        .collect()
}

fn pinch_hook(h: &mut Harness) -> PinchSink {
    h.enter_demo();
    await_workspace(h);
    debug_grid_hooks()
        .expect("a live session is mounted")
        .on_pinch
        .expect("a live zellij session wires the pinch hook")
}

#[test]
fn pinch_hook_is_wired_for_live_zellij_only() {
    let _g = serial();
    let _ambient = setup();

    let mut zellij = Harness::mount_session("zellij:dev");
    zellij.enter_demo();
    assert!(debug_grid_hooks().expect("live").on_pinch.is_some());
    zellij.unmount_screen();

    let mut herdr = Harness::mount_session("herdr:work");
    herdr.enter_demo();
    assert!(
        debug_grid_hooks().expect("live").on_pinch.is_none(),
        "herdr has one pane to fullscreen"
    );
    herdr.unmount_screen();

    let mut read_only = Harness::mount_session("zellij:dev");
    expect_connection_controller().is_read_only.set(true);
    read_only.enter_demo();
    assert!(
        debug_grid_hooks().expect("live").on_pinch.is_none(),
        "a read-only credential sends nothing"
    );
}

#[test]
fn pinch_spread_fullscreens_the_tiled_pane_never_the_seeded_floating_one_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session("zellij:dev");
    let on_pinch = pinch_hook(&mut h);
    h.settle();

    clear_verb_log_for_tests();
    on_pinch(PinchIntent::EnterFullscreen, PINCH_RIGHT);
    h.pump_until("the fullscreen verb", |_| {
        verb_log_for_tests().last().map(String::as_str) == Some("toggle_pane_fullscreen:40:false")
    });
    h.pump_until("the reload", |_| {
        pane_fullscreen_flags() == [(40, true), (41, false)]
    });

    h.pump_for(Duration::from_millis(150));
    let logged = verb_log_for_tests().len();
    on_pinch(PinchIntent::EnterFullscreen, PINCH_LEFT);
    h.pump_for(Duration::from_millis(150));
    assert_eq!(
        verb_log_for_tests().len(),
        logged,
        "already fullscreen: no verb"
    );
}

#[test]
fn pinch_spread_in_a_gutter_fullscreens_the_focused_pane() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session("zellij:dev");
    let on_pinch = pinch_hook(&mut h);
    h.settle();

    clear_verb_log_for_tests();
    on_pinch(PinchIntent::EnterFullscreen, PINCH_GUTTER);
    h.pump_until("the fullscreen verb", |_| {
        verb_log_for_tests().last().map(String::as_str) == Some("toggle_pane_fullscreen:40:false")
    });
}

#[test]
fn pinch_converge_restores_the_fullscreen_pane_and_is_a_no_op_without_one() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session("zellij:dev");
    let on_pinch = pinch_hook(&mut h);
    h.settle();

    clear_verb_log_for_tests();
    on_pinch(PinchIntent::ExitFullscreen, PINCH_LEFT);
    h.pump_for(Duration::from_millis(150));
    assert!(
        verb_log_for_tests().is_empty(),
        "nothing fullscreen: no verb"
    );

    on_pinch(PinchIntent::EnterFullscreen, PINCH_RIGHT);
    h.pump_until("fullscreen", |_| {
        pane_fullscreen_flags() == [(40, true), (41, false)]
    });

    // Let the first toggle's single-flight claim drop behind its reload.
    h.pump_for(Duration::from_millis(150));
    clear_verb_log_for_tests();
    on_pinch(PinchIntent::ExitFullscreen, PINCH_LEFT);
    h.pump_until("the restore verb", |_| {
        verb_log_for_tests().last().map(String::as_str) == Some("toggle_pane_fullscreen:40:false")
    });
    h.pump_until("restored", |_| {
        pane_fullscreen_flags() == [(40, false), (41, false)]
    });
}
