//! Headless behavior coverage for the `/terminal` screen's **live pane**: the pane-content
//! switch, settings-driven cursor/font, resize-on-delta-only, fullscreen with a live pane, the
//! IME-active-only key bar, and the tap-to-focus-only contract at the root — including which taps
//! may raise a keyboard and which may not.
//!
//! The fixture replay is entered through the narrowest seam that already existed —
//! `pages::debug_controller` plus `TerminalController::enter_live` — so nothing about the
//! composition under it changes (see [`Harness::enter_demo`]). Shares `tests/terminal.rs`'s
//! harness shape rather than importing it: every `tests/*.rs` file is its own crate root.
//!
//! The grid is **located by paint**: it fills its background with a plain `fill_rect` in
//! `GlyphInk::terminal_bg`, a different [`RecScene`] channel from every rounded tile on this
//! screen. Byte-correctness is asserted off the real emulator instead of the scene, since
//! `GlyphRun` carries shaped glyph ids rather than the source string:
//! [`debug_live_probe`](muxr::features::terminal::presentation::pages::debug_live_probe) reaches
//! the actual `vt100::Screen` the mounted screen renders.
//!
//! The agents stream closes the file: scripted through the mock repository's watch seam, with the
//! server's `agents-stream` capability switched on per test.

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
// Test-only peek at the OS system-UI slot `frust::set_system_ui_mode` writes
// — see `Cargo.toml`'s dev-dependency comment for why this is
// `frust-shell-common` directly rather than `frust::`.
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
    add_test_tab, agent_watch_open_for_tests, agent_watch_opens_for_tests,
    agent_watch_stops_for_tests, clear_test_agents, clear_test_tabs, clear_verb_log_for_tests,
    drop_agent_stream_for_tests, end_agent_stream_for_tests, fail_next_list_agents_for_tests,
    open_next_agent_watch_down_for_tests, push_agent_event_for_tests, seed_agents,
    verb_log_for_tests,
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
use muxr::routes::{app_nav, install_app_router};
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, center, char_key, ms, node_labels, pointer, pump_frames, rounded_in, serial,
    setup, tap,
};

/// Same oversized surface `tests/terminal.rs` lays out at — the sheet/menu
/// rows this suite's setup pushes through must not fall below the fold.
const SURFACE: Size = Size::new(800.0, 700.0);
const SCRIM_POINT: Point = Point::new(4.0, 4.0);
const PUMP_DEADLINE: Duration = Duration::from_secs(5);
const SESSION_ID: &str = "s-1";
/// `presentation::pages`' own `SHEET_CLOSE_DELAY`, transcribed (it is private
/// there) — how long a pane commit leaves the sheet up so the selection visibly
/// moves first.
const CLOSE_DELAY: Duration = Duration::from_millis(260);

/// The side sheet's header label — how section 10 proves the open landed.
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

/// The screen, an overlay navigator over it, and everything a frame needs —
/// `tests/terminal.rs::Harness`'s shape, trimmed to what this suite uses.
struct Harness {
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    mounted: Rc<Cell<bool>>,
}

impl Harness {
    /// Mount the screen, attached to [`SESSION_ID`], with the first mock load
    /// already landed and no demo session entered yet.
    fn mount() -> Self {
        Self::mount_session(SESSION_ID)
    }

    /// [`mount`](Self::mount), attached to `session` — how this suite reaches
    /// a **herdr** session, whose id prefix is the one thing the sheet's
    /// spaces section is gated on.
    fn mount_session(session: &str) -> Self {
        Self::mount_at(&format!("/terminal?session={session}"))
    }

    /// [`mount`](Self::mount) at the full `location`, query included.
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

    /// Mount the checked-in fixture replay in the card, through the mounted
    /// screen's own controller — the seam the [module docs](self) describe.
    /// `enter_live` only flips the controller's flag; the settle that follows is
    /// the `build` that observes it and constructs the live session under the
    /// screen's own reactive owner.
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

    /// Wait for a sheet closed through `SheetHandle::close_after` to actually
    /// pop — `tests/terminal.rs`'s own `pump_until_closed`, transcribed here for
    /// this suite's one caller. See that method's doc for why this is two fixed
    /// beats rather than one real-time-tracked loop.
    fn pump_until_closed(&mut self, what: &str, done: impl Fn(&Harness) -> bool) {
        self.pump_for(2 * CLOSE_DELAY);
        self.frame_at(0.0);
        self.frame_at(300.0);
        self.frame_at(300.0);
        assert!(done(self), "timed out waiting for {what}");
    }

    fn await_load(&mut self) {
        // The mock's first load lands quickly; a plain pump is enough (no
        // `node_values` dependency, since the live suite never reads the mock
        // transcript's own content).
        self.pump_for(Duration::from_millis(50));
    }

    /// Exactly one frame: the UI-thread task queue once, then a rebuild.
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

    /// The root's focus/IME session as `(focus_active, publishes_a_surface)` —
    /// the exact pair a platform shell reads to decide whether a soft keyboard
    /// should be on screen (`RenderRoot::is_focus_active`/`ime_state`).
    fn session(&self) -> (bool, bool) {
        (self.root.is_focus_active(), self.root.ime_state().is_some())
    }
}

/// The mounted session's demo feed. `LiveProbe::feed` is an `Option` because the
/// same probe also fronts a live **`muxrd`** session, whose feed is a real stream
/// with no replay cursor; every test here drives the demo entry, which has one.
fn demo_feed() -> Rc<RefCell<DemoFeed>> {
    debug_live_probe()
        .expect("a live pane is mounted")
        .feed
        .expect("the demo entry mounts the fixture replay")
}

/// The live grid's own plain background fill — see the [module docs](self).
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

/// The pixel a tap must land on to name grid cell `(col, row)` — the grid's own painted box
/// divided by the emulator's cell grid.
///
/// **Approximate by construction**, and safely so: the widget takes at most one cell less than
/// the box it painted per axis, an error under half a cell at the columns anything here aims at,
/// and every caller targets the middle of a pane rect dozens of cells across.
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

/// A cell well inside the mock's **focused** pane — its seed is the left half
/// of a classic 80x24 grid (`mock_terminal_repository::seed_panes`).
const FOCUSED_CELL: (u16, u16) = (5, 5);
/// …and one well inside its unfocused sibling, the right half.
const SIBLING_CELL: (u16, u16) = (45, 5);

/// The id of the SERVER-ACTIVE tab's focused pane, as the mounted screen's own
/// tab world holds it — how a test reads whether a tap moved the focus.
fn focused_pane_id() -> Option<u32> {
    debug_controller()?
        .active_tab_entry()?
        .panes
        .iter()
        .find(|pane| pane.focused)
        .map(|pane| pane.id)
}

/// One finger drag from `from`, far enough past the framework's own [`TOUCH_SLOP`]
/// that the grid's touch state machine can only read it as a **scroll**: Down, one
/// Move that crosses the slop, then Up where the Move left off.
fn drag_scroll(h: &mut Harness, from: Point) {
    let to = from + Vec2::new(0.0, TOUCH_SLOP * 4.0);
    h.root
        .event(&mut h.state, &pointer(frust_core::PointerPhase::Down, from));
    h.root
        .event(&mut h.state, &pointer(frust_core::PointerPhase::Move, to));
    h.root
        .event(&mut h.state, &pointer(frust_core::PointerPhase::Up, to));
}

/// The terminal card — the stage's own rounded fill — as `(origin, size,
/// radius)`, exactly `tests/terminal.rs::card`'s helper.
fn card(scene: &RecScene) -> (Point, Size, f64) {
    let ink = frust_glyph::baseline()
        .extension::<frust_glyph::GlyphInk>()
        .copied()
        .expect("the Glyph baseline carries GlyphInk");
    let mut cards = rounded_in(scene, ink.terminal_bg);
    cards.sort_by(|a, b| (b.1.width * b.1.height).total_cmp(&(a.1.width * a.1.height)));
    cards.first().copied().expect("the card paints itself")
}

/// The text a `vt100::Screen` cell holds, or an empty string for a blank one.
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

// ---------------------------------------------------------------------------
// 1 — mounting the live surface
// ---------------------------------------------------------------------------

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

/// The seam's whole route, end to end: the controller call the removed menu row
/// used to make still mounts the real grid over the checked-in fixture and
/// replays it — nothing between the flag and the pump was menu-shaped.
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
    // The grid painted its own background — proof the pane-content switch
    // actually rendered `term::terminal_surface`, not just flipped a flag.
    let _ = grid_rect(&scene);

    // The fixture's own first line (`DemoFeed`'s own test transcribes the
    // same two substrings) has to have reached the emulator by now.
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

// ---------------------------------------------------------------------------
// 2 — typed bytes echo, byte-correct, through the real IME seam
// ---------------------------------------------------------------------------

#[test]
fn a_typed_character_echoes_at_the_cursor_byte_correctly() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    let scene = h.frame();

    // Claim focus on the grid — the same tap-to-focus path a real keyboard
    // taps through.
    let (origin, size) = grid_rect(&scene);
    h.tap(center((origin, size)));

    let probe = debug_live_probe().expect("live");
    let (row, col) = probe.emulator.borrow().screen().cursor_position();

    // A raw key event, exactly the shape a platform delivers — echoed back
    // through `DemoFeed::send_input` on the pump's very next wake.
    h.root.event(&mut h.state, &char_key("q"));
    h.pump_until("the echoed character", |_| {
        cell_text(&probe, row, col) == "q"
    });

    assert_eq!(cell_text(&probe, row, col), "q");
}

// ---------------------------------------------------------------------------
// 3 — selection + copy, with an injected sink
// ---------------------------------------------------------------------------

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
    // The emulator starts blank — give the fixture replay a chance to paint
    // some words to select before long-pressing on top of it.
    h.pump_until("some fixture content to select", |_| {
        let probe = debug_live_probe().expect("still mounted");
        probe.emulator.borrow().screen().contents() != ""
    });
    let scene = h.frame();
    let (origin, size) = grid_rect(&scene);
    // Row 0 is the only row with content this soon after entering — a point
    // a few cells into it (comfortably past the loupe's own edge clamp on
    // every side) lands on an actual word to select.
    let (rows, cols) = {
        let probe = debug_live_probe().expect("live");
        probe.emulator.borrow().screen().size()
    };
    let cell_w = size.width / f64::from(cols);
    let cell_h = size.height / f64::from(rows);
    let point = Point::new(origin.x + cell_w * 24.0, origin.y + cell_h * 0.5);

    // `support::long_press` lays out at its own default (800x600) surface — this
    // suite's `SURFACE` is taller, so the press is driven by hand here, at this
    // harness's own sized frames, to keep the grid's geometry consistent.
    h.root.event(
        &mut h.state,
        &pointer(frust_core::PointerPhase::Down, point),
    );
    h.frame_at(0.0);
    h.frame_at(support::HELD_MS);
    h.root
        .event(&mut h.state, &pointer(frust_core::PointerPhase::Up, point));
    let after = h.frame();

    // The Copy chip appeared: a rounded fill sized exactly `CHIP_W x CHIP_H`.
    let chip = after
        .rounded
        .iter()
        .copied()
        .find(|(_, size)| {
            (size.width - CHIP_W).abs() < EPSILON && (size.height - CHIP_H).abs() < EPSILON
        })
        .expect("the copy chip painted its own pill");

    // Tap its left (Copy) half.
    let copy_point = Point::new(chip.0.x + 20.0, chip.0.y + chip.1.height / 2.0);
    h.tap(copy_point);

    assert_eq!(captured.borrow().len(), 1, "copy fired exactly once");
    assert!(
        !captured.borrow()[0].is_empty(),
        "the injected sink received the selected text"
    );

    set_demo_copy_sink(None);
}

// ---------------------------------------------------------------------------
// 4 — settings reflect into the live pane
// ---------------------------------------------------------------------------

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

    // Write straight into the shared preferences repository — standing in for
    // the settings screen's own picker save, the same store
    // `crate::features::settings::di::expect_preferences_repository` publishes.
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

    // The live pane's own poll (`LiveSession::enter`'s `use_interval`) picks
    // it up within one beat — no debounce to wait past, just its own cadence.
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
    // "Space Mono" isn't a bundled face (`crate::fonts::installed_families`),
    // so the real poll must resolve it to the shipped default stack rather
    // than shape with a family fontique has never heard of.
    assert_eq!(
        debug_controller()
            .expect("mounted")
            .font_family
            .get_untracked(),
        muxr::fonts::default_terminal_family()
    );
}

/// `refresh_appearance` must not `set()` its signals unconditionally on every poll tick, waking a
/// tracked rebuild + unpaced frame forever while the pane sits idle. Invoking it twice in a row
/// is exactly what two real poll ticks do, without waiting on the real-time cadence (nothing here
/// can fake the background runtime's timer, and a shortened test-only cadence would test a
/// cadence nothing ships).
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

    // Establish a real, non-default appearance first — otherwise the very
    // first read would already match the seeded defaults and write nothing,
    // proving nothing about the *gate* this test exists to pin.
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

    // First poll cycle: the store really changed since construction, so this
    // tick must write both signals.
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

    // Two more identical poll cycles: nothing changed in the shared store
    // since the first read landed, so read-compare-then-write must write
    // neither signal on either tick — the idle-poll case M4 flagged.
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

// ---------------------------------------------------------------------------
// 5 — resize propagates on a real cell-grid delta only
// ---------------------------------------------------------------------------

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

    // Two more steady-state frames: the box hasn't changed, so the feed must
    // not be told about a resize again.
    h.frame();
    h.frame();
    assert_eq!(
        feed.borrow().last_resize(),
        Some(windowed),
        "an unchanged box records nothing new"
    );

    // Fullscreen changes the box substantially — a real delta.
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

// 6 — fullscreen keeps the chrome geometry correct with a live pane, and drives
//     the OS system UI mode alongside the app's own chrome.

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
    // …and the live grid is still there, still painting.
    let _ = grid_rect(&scene);
    // The OS status/nav bars mirror the card's own request. The write is
    // synchronous with the press (`Actions::toggle_fullscreen`), not build-time,
    // so it is already observable before any of the settle above ran.
    assert_eq!(
        current_system_ui_mode().1,
        SystemUiMode::ImmersiveSticky,
        "expand hides the OS bars too"
    );

    // Collapsing reverses it — `pages::restore_system_ui`'s other call site
    // besides `init`'s `on_cleanup`.
    h.tap_label(COLLAPSE);
    h.settle_motion(3_000.0);
    assert_eq!(
        current_system_ui_mode().1,
        SystemUiMode::EdgeToEdge,
        "collapse restores the OS bars"
    );
}

// ---------------------------------------------------------------------------
// 7 — the key bar: live-mode + IME-active only
// ---------------------------------------------------------------------------

/// A word every key cap announces — enough to prove the bar mounted without
/// pixel-hunting its geometry. `BarKey::name()` (the accessible name), not
/// `BarKey::label()` (the cap's printed text) — the two deliberately differ.
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

// The link strip. It renders off one mirrored signal (`TerminalController::link`), the seam a
// live `GrpcTerminalFeed` writes through on every repaint-pump wake. Driving that signal directly
// is the whole surface under test here: what the *feed* publishes is pinned by that type's own
// fixture-backed tests, and standing a TLS server up inside a headless UI suite would test the
// transport twice and the screen not at all.

/// The strip's three messages and two affordances (transcribed from `presentation::pages`,
/// private there).
const LINK_RECONNECTING: &str = "reconnecting\u{2026}";
const LINK_DISCONNECTED: &str = "disconnected";
const LINK_CLOSED: &str = "session ended";
const LINK_RETRY: &str = "Retry";
const LINK_LEAVE: &str = "Back to sessions";

/// Publish `status` as the mounted screen's link state and rebuild.
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

    // The demo replay has no stream to re-arm, so this is the press path
    // itself: it must reach the session and do nothing, rather than panic on a
    // feed with no backoff.
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

    // `LINK_LEAVE`'s press is one line, `state.actions.app_nav.pop()`
    // (`presentation::widgets::link_strip`) — the same `Nav::pop()` every
    // bar `‹` in the app calls, which `tests/navigation_motion.rs` already
    // proves pops an in-shell route end to end. What this harness *cannot*
    // observe is where that pop landed: `Nav::pop()`/`Router::pop()` only
    // **enqueue** an op on the outer `NavigatorController`
    // (`crates/frust-widgets`'s nav module — "a queued-but-undrained op is
    // not in it"), and this suite's `record_only` controller is never built
    // as a live `navigator(...)` (mounting one would build a *second*
    // terminal screen inside it, fighting the one under test — the module
    // docs' whole reason for the split) — so `Nav::bar().meta` reads `None`
    // whether or not the press fired at all. The tap itself is still the
    // assertion that matters here: it must not panic reaching for a session
    // the demo replay never had.
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

// ---------------------------------------------------------------------------
// 8 — teardown
// ---------------------------------------------------------------------------

#[test]
fn unmounting_drops_the_published_chrome_and_the_live_probe() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    h.frame();
    assert!(debug_live_probe().is_some());

    // Unmounting *while fullscreen* is the regression this exists for: a
    // back-nav straight out of fullscreen must not strand hidden OS bars on
    // whatever screen comes back into view (`pages::restore_system_ui`'s doc).
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

// Pump hygiene: the timer stops once nothing is left to say. These two drive the real
// `DemoFeed`/`InputSeam`/`run_feed` triple directly rather than through the mounted screen:
// `LiveSession::enter`'s real `DemoFeed::new()` takes a genuine 30s wall clock to drain the
// fixture, so these use `DemoFeed::manual()` instead, wiring the same seams with the clock in
// test hands.

#[test]
fn run_feed_exits_once_a_drained_feed_has_nothing_left_to_say() {
    let _g = serial();
    let _ambient = setup();

    let feed: Rc<RefCell<DemoFeed>> = Rc::new(RefCell::new(DemoFeed::manual()));
    // Push the manual clock past the whole 30s schedule: fully drained, nothing typed, so the
    // very first wake below should already see a closed feed.
    feed.borrow_mut().set_elapsed(Duration::from_secs(60));
    let feed_dyn: SharedFeed = feed.clone();
    let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
    let generation = RwSignal::new(0u64);
    let cancelled = Arc::new(AtomicBool::new(false));

    // `run_feed` never reaches its own sleep on this path — the very first wake
    // already sees a closed feed and returns — so a plain current-thread runtime
    // drives it to completion with no real wall-clock wait. Under the old
    // `!is_drained() || echo` rule (echo defaults on) this would never return.
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

    // Drain the replay itself out of the way first, into a throwaway
    // emulator — this test is about the keystroke's own echo, not the
    // fixture's own content.
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

    // No `pump_wake` call from the test itself between here and the
    // assertion below — the echo must reach the emulator inside `dispatch`
    // alone, exactly the case the old timer-only design kept alive for.
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

// Focus/IME hygiene: nothing summons a keyboard the user didn't ask for. The screen's v2 design
// is **tap-to-focus only**: keyboard and key bar are IME-active-only, so mounting the live pane
// claims nothing, and only a tap on the grid itself may open a focus/IME session.
// `RenderRoot::is_focus_active`/`ime_state` are what a shell reads to decide whether to show a
// soft keyboard, so they're the honest place to assert it.
//
// "A tap" is where the two-stage pane semantics live: a tap on the pane the session is already
// on raises the keyboard, a tap on a *different* pane only moves focus there, and a drag-scroll
// raises nothing. herdr's single-pane stream wires no tap hook, keeping the unconditional
// tap → keyboard. The focus *claim* is invariant across all of them.

/// A freshly mounted live pane claims **nothing** — no root focus, no published
/// IME surface, not even a focus/IME generation edge for a shell to wake on.
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
    // Past the first feed wakes and the appearance poll's first tick: a claim
    // that arrived a few frames late would be just as wrong as one on mount.
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

/// The other half: tapping the grid **on the pane the session is already on** opens
/// the session — focus claimed and an **active** IME surface published, which is
/// what brings the keyboard (and with it the key bar) up. Aimed at [`FOCUSED_CELL`]
/// rather than the grid's centre: only a tap on the *focused* pane is the typing
/// gesture this pins.
#[test]
fn tapping_the_grid_claims_focus_and_publishes_an_active_ime_surface() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    // The hit test runs against the loaded tab world, so the tap only *means*
    // "on the focused pane" once that world has landed.
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

/// A tap on a **different** pane is a pointing gesture: it claims focus (every
/// `Down` does), fires `FocusPane` so zellij's own border moves, and publishes no
/// IME surface at all — no keyboard covering the pane the user just asked to see.
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
    // …and the frames the landing woke must not raise one behind its back:
    // the focused-paint republish is gated on engagement, not on focus.
    h.frame();
    assert_eq!(
        h.session(),
        (true, false),
        "no later paint resurrects a surface the tap declined to publish"
    );
}

/// …and the second tap on that same pane — now the focused one, thanks to the
/// optimistic move the first tap's `FocusPane` made — *is* the typing gesture,
/// so it raises the keyboard. Two taps, no extra state: exactly the spec.
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

/// A drag-scroll raises nothing, wherever it starts. The gesture the terminal
/// is used with most is the one that must never cover it with a keyboard.
#[test]
fn a_drag_scroll_publishes_no_ime_surface() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    let scene = h.frame();

    // Starts on the focused pane — the one cell a *tap* would raise a keyboard
    // from, so nothing about the destination is doing the work here.
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

/// herdr wires no tap hook at all (its single-pane stream has no second pane to
/// point at), so the grid answers for itself: a tap raises the keyboard, and a
/// scroll still does not. The scroll runs first, so the tap's own claim cannot be
/// what the scroll is measured against.
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

// The "panes & tabs" side sheet over a live pane. The sheet's own tree-browse-then-commit
// behaviour is pinned in `tests/terminal.rs`; what only *this* suite can say is what happens to
// the **live pane** underneath it: a commit must move the session without tearing the grid, its
// seams, or its feed down, and the tree's nested tab rect must follow the attached session's
// backend (herdr only).

/// `side_sheet_tree`'s own mid-level tab radius (private there, transcribed the
/// same way `tests/terminal.rs` does) — herdr's nested tab **rect**, never
/// worn by a promoted zellij tab.
const TREE_TAB_RADIUS: f64 = 7.0;

/// The tree's pane rows in `scene`, top to bottom — every
/// [`muxr::ui::paint::RADIUS_FULL`] rect wide enough to be a row rather than
/// the pill-shaped mark box or focused dot each row also carries at the same
/// radius (`tests/terminal.rs`'s identical `tree_pane_rows` idiom).
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

/// Open the sheet through the real app-bar entry point — the menu button now
/// opens it directly. Waits for the first workspace load first: this suite's
/// own [`Harness::await_load`] only pumps long enough for the live pane's
/// needs, and a sheet opened before the tab world lands has nothing to list.
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
    // The second pane of the tab the session is already on — a `FocusPane`
    // with no tab move behind it.
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

// The live grid's gesture hooks: tap → FocusPane (zellij only) and
// wheel → mouse frames / ScrollPane fallback (always).

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
    // A real unmount (not merely dropping the harness), so the feed pump and
    // appearance-poll background tasks this session spun up are disposed before
    // a second screen mounts over the same reactive runtime — an `on_cleanup`
    // never fires from a bare `Drop`.
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

    // A no-hit tap (the mock's seeded panes tile (0,0)-(80,24) between them) —
    // no RPC, no crash (`resolve_tap_action`'s own "stale/no-hit tap" case),
    // and the keyboard verdict a tap with nothing under it gets.
    assert!(
        on_content_tap(500, 500),
        "a tap that resolves to nothing raises the keyboard"
    );
    h.pump_for(Duration::from_millis(50));

    // A hit on the pane the session is already on: no RPC either, same
    // verdict — this is the typing gesture.
    assert!(
        on_content_tap(u32::from(FOCUSED_CELL.0), u32::from(FOCUSED_CELL.1)),
        "a tap on the focused pane raises the keyboard"
    );
    h.pump_for(Duration::from_millis(50));

    // A hit on the *sibling* — the pointing gesture: it exercises the real
    // `focus_pane_from_tap` round trip end to end, and raises nothing.
    assert!(
        !on_content_tap(u32::from(SIBLING_CELL.0), u32::from(SIBLING_CELL.1)),
        "a tap that moves the focus raises no keyboard"
    );
    h.pump_for(Duration::from_millis(50));

    // Capability present (the mock connection backend's own default,
    // `connection::MockConnectionBackend`) — the mouse-frame route, against
    // a demo feed whose `send_mouse` is `TerminalFeed`'s own no-op default.
    on_wheel(true, 5, 5);
    on_wheel(false, 5, 5);
    h.pump_for(Duration::from_millis(50));

    // Capability absent — the `ScrollPane` RPC fallback, against a mock
    // repository that answers "unsupported" (no override) and is silently
    // dropped (see `TerminalController::scroll_pane_from_wheel`'s own doc).
    let cc = expect_connection_controller();
    let mut info = cc.server_info.get_untracked().expect("connected");
    info.capabilities.clear();
    cc.server_info.set(Some(info));
    on_wheel(true, 5, 5);
    h.pump_for(Duration::from_millis(50));

    // Nothing above panicked, and the screen is still alive.
    h.frame();
}

// Layout refresh: the poll, the sheet's own re-read, and the tap-miss backstop. The wire has no
// layout-changed signal, so a tab opened from *elsewhere* only reaches this screen because it
// asked again. What these pin is the asking: each trigger fires, the answer lands, and an idle
// one writes nothing. `MockTerminalRepository::add_test_tab` is the tab world moving behind the
// screen's back; it's process-global, so every test below clears it on both sides (serialized).
//
// The **device** races these close aren't reproducible headlessly (need a real `AttachTerminal`
// stream and a real relay).

/// The tab a test opens "from elsewhere", and the pill the sheet draws for it —
/// `TabEntry::pill_label`'s `<index> <label>`, appended past the mock's four seeded tabs.
const SCRATCH_TAB: &str = "scratch";
/// The tree's own tab-row label for it — just [`SCRATCH_TAB`] itself
/// (`side_sheet_tree`'s disclosure row prints `TabEntry::label` directly, not
/// the flat sheet's index-prefixed `pill_label`).
const SCRATCH_PILL: &str = SCRATCH_TAB;

/// Drive one controller call to completion — the same local-runtime shape section 4
/// uses, and for the same reason (this suite cannot fake the background runtime's
/// timer, so a poll tick is invoked directly rather than waited out).
fn drive(fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
        .block_on(fut);
}

/// Pump until the mounted screen's first workspace load has landed.
fn await_workspace(h: &mut Harness) {
    h.pump_until("the workspace to load", |_| {
        debug_controller().is_some_and(|c| c.loaded.get_untracked())
    });
}

/// Whether the mounted screen's published tab world carries `key`.
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

    // A tab opened from somewhere else entirely: no user action here, and
    // nothing on the wire announces it.
    add_test_tab(SCRATCH_TAB);
    assert!(controller.take_layout_poll_tick(), "the poll is armed");
    let tick = Arc::clone(&controller);
    drive(Box::pin(
        async move { tick.refresh_layout_from_poll().await },
    ));

    assert!(published_tab(SCRATCH_TAB), "the tick picked it up");
    h.frame();

    // …and the next tick, with the tab world exactly where that one left it,
    // writes nothing at all — the poll runs for the life of the mount, so an
    // idle tick that woke a rebuild would wake one forever.
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

    // Opened from elsewhere while this screen sat there — the sheet is the
    // one surface that lists tabs, so opening it is what has to notice.
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

    // The backstop's whole point: taps are hit-tested against the last layout
    // answer, so a screen whose answer is stale/empty has no working tap at all
    // — the first miss re-reads it rather than leaving the user to find the sheet.
    add_test_tab(SCRATCH_TAB);
    assert!(
        on_content_tap(500, 500),
        "a miss still answers 'raise' — the backstop is about the layout, not \
         the keyboard"
    );
    h.pump_until("the backstop reload", |_| published_tab(SCRATCH_TAB));

    // …once per mount. A second miss is the plain no-op it always was.
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

// ---------------------------------------------------------------------------
// The sheet body composes a scroll container.
// ---------------------------------------------------------------------------

#[test]
fn the_sheet_body_composes_a_scroll_container() {
    // Structure-level pin: no screenshot/pixel assert (visual sizing is a device
    // gate — headless has no `WindowMetrics`, so the scroll region's own cap
    // never engages here and it always sizes to its content exactly as the plain
    // column it replaced did). What this pins is that the sheet's scrollable
    // sections really are wrapped in a `frust::scroll_view`, which — unlike a
    // label or a colour — publishes its own unambiguous `Role::ScrollView` node,
    // the same role-based locator `tests/navigation_motion.rs` uses.
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

// ---------------------------------------------------------------------------
// Attach into a pane: the `tab`/`pane` deep-link params.
// ---------------------------------------------------------------------------

/// The focused pane id of the tab keyed `key`, off the mounted controller.
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
    // Position 2 is gohora (tab id 3); pane 31 is its unfocused sibling.
    let mut h = Harness::mount_at("/terminal?session=s-1&tab=2&pane=31");
    h.pump_until("the target", |_| verb_log_for_tests().len() >= 2);
    h.pump_for(Duration::from_millis(100));

    assert_eq!(verb_log_for_tests(), ["go_to_tab:3", "focus_pane:31"]);
    let c = debug_controller().expect("mounted");
    assert_eq!(c.active_tab.get_untracked(), "gohora");
    assert_eq!(focused_pane_of("gohora"), Some(31));

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
        // A fixed settle: the harness has no "mock idle" signal to pump until, and a
        // negative can only wait out the window a wrong apply would land in.
        h.pump_for(Duration::from_millis(150));
        assert!(
            verb_log_for_tests().is_empty(),
            "{location}: {:?}",
            verb_log_for_tests()
        );
        drop(h);
    }
}

// A link naming a space other than the selected one: the space switches first, then the
// tab, then the pane — on a herdr session, whose spaces the mock seeds (`forgekit`
// selected, `zelli_mobile` foreign: tabs `logs` (id 101) and `worker` (id 102)).
/// The mock seeds transcripts for the selected space's tabs only, so a switch onto a
/// `zelli_mobile` tab other than its active one would fail at the transcript read. This
/// answers every transcript empty and delegates the rest.
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

/// A live herdr controller over [`QuietLines`] aimed at `zelli_mobile`'s `worker` tab, pane 21,
/// loaded and handshaken but not yet applied.
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
    // `forgekit` is the selected space; position 2 is gohora (tab id 3).
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

/// [`QuietLines`] over a space list in which no space is selected — a herdr layout whose
/// selection has not been read (or was lost).
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

/// One controller call, boxed so a test can open it, leave it parked on its round
/// trip, and land it later — the ordering a race needs, with no clock in it.
type Step<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
}

/// Poll `step` once on `rt`: far enough to open its round trip, never to land it.
fn start(rt: &tokio::runtime::Runtime, step: &mut Step<'_>) {
    rt.block_on(std::future::poll_fn(|cx| {
        assert!(
            step.as_mut().poll(cx).is_pending(),
            "parked on a round trip"
        );
        Poll::Ready(())
    }));
}

/// A **live** controller for `?session=s-1&tab=2&pane=31` (gohora, tab id 3), over the
/// composition root's own repositories but mounted nowhere, so the test alone decides
/// when each layout fetch opens and lands.
fn live_deep_link() -> Arc<TerminalController> {
    let _state = MuxrApp.init();
    live_deep_link_over(expect_terminal_repository())
}

/// [`live_deep_link`] over `repository` (the context must already be initialised).
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
    assert_eq!(c.active_tab.get_untracked(), "gohora");
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

/// The supersede rule ([`AttachTarget`]'s doc): a move the user made after mount
/// wins, so the link is dropped once that move lands; while it is in flight its
/// claim only defers the link.
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

/// A grid tap takes no claim, yet while its verb is in flight the settled link
/// must neither fire nor drop; only the landed tap supersedes it.
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

/// The mock answers `new_tab` as unsupported, so this wraps it with a create that
/// parks on a timer, as the wire's does.
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

/// `new_tab` holds only the create claim; its in-flight verb still defers the link,
/// and the landed create then supersedes it.
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

// The agents stream. The mock repository opens a watch with a snapshot of its seeded inventory and
// then delivers only what a test pushes; the mock connection advertises no `agents-stream`, so
// each test that streams switches it on before the live pane mounts.

/// Clears the seeded agents when a test ends, however it ends.
struct AgentsReset;

impl Drop for AgentsReset {
    fn drop(&mut self) {
        clear_test_agents();
    }
}

/// A `claude` agent in pane `pane_id` of the zellij mock's `devbox` tab.
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

/// The mounted screen's agent rows, by pane id, in published order.
fn agent_panes() -> Vec<u32> {
    debug_controller()
        .expect("a terminal screen is mounted")
        .agents
        .get_untracked()
        .iter()
        .map(|agent| agent.pane_id)
        .collect()
}

/// Mount with `agents` seeded, advertise the stream when `streaming`, enter the live pane, and
/// wait for the rows.
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
    // The rows may be the stream's snapshot: let the mount's own relist land too, so the
    // safety relist's window starts from it.
    h.pump_for(Duration::from_millis(100));
    (h, reset)
}

/// Run one layout poll tick to completion.
fn poll_tick(controller: &Arc<TerminalController>) {
    assert!(controller.take_layout_poll_tick(), "the poll is armed");
    let tick = Arc::clone(controller);
    drive(Box::pin(
        async move { tick.refresh_layout_from_poll().await },
    ));
}

/// The two rows most stream tests start from: a working agent above an idle one.
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

    // Down, and the fallback read fails: the list is unconfirmed.
    drop_agent_stream_for_tests();
    fail_next_list_agents_for_tests();
    poll_tick(&controller);
    assert!(controller.agents_stale.get_untracked());

    // The stream delivers again: confirmed, and live.
    let blocked = Box::new(streamed_agent(41, AgentStatus::Blocked, 9));
    assert!(push_agent_event_for_tests(AgentEventEntry::Changed(
        blocked
    )));
    h.one_frame();
    assert!(!controller.agents_stale.get_untracked());
    assert!(controller.has_blocked_agent());

    // Live and freshly listed, a tick leaves the agents to the stream.
    seed_agents(vec![streamed_agent(60, AgentStatus::Idle, 1)]);
    poll_tick(&controller);
    assert_eq!(agent_panes(), [41, 40], "no re-list under a live stream");

    // Down again, a tick re-reads them.
    drop_agent_stream_for_tests();
    poll_tick(&controller);
    assert_eq!(agent_panes(), [60]);
    assert!(!controller.agents_stale.get_untracked());
}

// The subscription's lifecycle: stale only on a live-to-down transition, an ended watch dropped on
// the next tick, and reopened at most once per retry window unless the credential was refused.

/// The window the controller waits between two opens of a subscription (`AGENT_WATCH_RETRY`).
const WATCH_RETRY: Duration = Duration::from_secs(30);

/// A blocked agent above an idle one, so the attention badge has something to say.
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

    // An empty snapshot (the mount's), then a clean end: the server has no agents to stream.
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

    // The reopened stream delivers like the first.
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
        raises + 1,
        "one transition, one raise"
    );
    assert!(
        !controller.agents_stale.get_untracked(),
        "the poll's read confirmed them"
    );
}
