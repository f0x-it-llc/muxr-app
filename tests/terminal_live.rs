//! Headless behavior coverage for the `/terminal` screen's **live demo pane**
//! (task 11's cutover): the "Demo session" overflow-menu entry, the
//! pane-content switch, settings-driven cursor/font, resize-on-delta-only,
//! fullscreen with a live pane, the IME-active-only key bar, and the
//! tap-to-focus-only contract at the root (section 10).
//!
//! Shares `tests/terminal.rs`'s harness shape (the same two-navigator split,
//! the same frame-stepping idiom) rather than importing it — that file's own
//! `Harness` is private to its crate (every `tests/*.rs` file is its own
//! crate root) — see its module docs for the two-navigator rationale this
//! harness reuses verbatim.
//!
//! # Locating the live grid without a widget-level handle
//!
//! [`TerminalGridWidget`](muxr::features::terminal::presentation::term::widget::TerminalGridWidget)
//! paints its own background with a **plain** `fill_rect` (not the rounded
//! fill every `term_box`/`terminal_stage` tile in this app uses), in exactly
//! `GlyphInk::terminal_bg` — the same tone, but a different [`RecScene`]
//! channel, so [`grid_rect`] picks it out of a frame unambiguously: nothing
//! else on this screen plain-fills that colour.
//!
//! # Byte-correctness without a text-capturing paint recorder
//!
//! `frust_scene::GlyphRun` carries shaped glyph ids, not the source string —
//! there is no way to recover printed text from [`RecScene`]. So typed-byte
//! and fixture-replay correctness are asserted the same way
//! `presentation::term`'s own unit tests do: reading the mounted live
//! session's real `vt100::Screen` cells through
//! [`debug_live_probe`](muxr::features::terminal::presentation::pages::debug_live_probe) —
//! the actual emulator the screen's grid renders, reached through the real
//! composition root (menu tap → controller → `Component::build` →
//! `LiveSession` → the feed pump), not a bare widget-level unit test.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, GetUntracked, NavigatorController, PopResult, RwSignal, SizedBox, Stack,
    Theme, any, navigator, provide_context,
};
use frust_core::{RenderRoot, WindowEdgeInsets, WindowInsets};
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::settings::di::expect_preferences_repository;
use muxr::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle as SettingsCursorStyle,
};
use muxr::features::terminal::data::feeds::demo_feed::DemoFeed;
use muxr::features::terminal::domain::feed::TerminalFeed;
use muxr::features::terminal::presentation::pages::{
    debug_controller, debug_live_probe, open_terminal_menu, set_demo_copy_sink, terminal_page,
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
    EPSILON, RecScene, added, center, char_key, largest, ms, node_labels, pointer, pump_frames,
    rounded_in, serial, setup, tap,
};

/// Same oversized surface `tests/terminal.rs` lays out at — the sheet/menu
/// rows this suite's setup pushes through must not fall below the fold.
const SURFACE: Size = Size::new(800.0, 700.0);
const SCRIM_POINT: Point = Point::new(4.0, 4.0);
const PUMP_DEADLINE: Duration = Duration::from_secs(5);
const SESSION_ID: &str = "s-1";

/// The overflow menu's row indices, transcribed from `presentation::pages`
/// (private there; a test that imported them could not catch one changing) —
/// `tests/terminal.rs`'s own `ROW_*` constants document the same reasoning.
const ROW_DEMO: usize = 5;

const EXPAND: &str = "Fullscreen";

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
    nav: NavigatorController<AppState>,
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
        let state = MuxrApp.init();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        let record_only: NavigatorController<AppState> = NavigatorController::new();
        let _router = install_app_router(&record_only, &nav, expect_connection_controller());
        expect_connection_controller().connect(mock_target());
        app_nav().push(&format!("/terminal?session={SESSION_ID}"));

        let mounted = Rc::new(Cell::new(true));
        let logic_nav = nav.clone();
        let logic_mounted = Rc::clone(&mounted);
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            let screen_nav = logic_nav.clone();
            let overlay_nav = logic_nav.clone();
            let screen: AnyView<AppState> = if logic_mounted.get() {
                terminal_page(screen_nav)
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
        root.set_theme(Box::new(Theme::glyph_baseline()));
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

    fn choose_menu_row(&mut self, index: usize) {
        self.nav.pop_with_result(PopResult::of(index));
        self.flush();
    }

    /// Choose the overflow menu's "Demo session" row — the one entry point
    /// into the live pane (see `presentation::pages`' own "Demo entry" note).
    fn enter_demo(&mut self) {
        self.open_menu();
        self.choose_menu_row(ROW_DEMO);
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

    fn await_load(&mut self) {
        // The mock's first load lands quickly; a plain pump is enough (no
        // `node_values` dependency, since the live suite never reads the mock
        // transcript's own content).
        self.pump_for(Duration::from_millis(50));
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

/// The live grid's own plain background fill — see the [module docs](self).
fn grid_rect(scene: &RecScene) -> (Point, Size) {
    let ink = Theme::glyph_baseline()
        .extension::<frust::GlyphInk>()
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

/// The terminal card — the stage's own rounded fill — as `(origin, size,
/// radius)`, exactly `tests/terminal.rs::card`'s helper.
fn card(scene: &RecScene) -> (Point, Size, f64) {
    let ink = Theme::glyph_baseline()
        .extension::<frust::GlyphInk>()
        .copied()
        .expect("the Glyph baseline carries GlyphInk");
    let mut cards = rounded_in(scene, ink.terminal_bg);
    cards.sort_by(|a, b| (b.1.width * b.1.height).total_cmp(&(a.1.width * a.1.height)));
    cards.first().copied().expect("the card paints itself")
}

/// The overflow menu's **panel**, diffed out of the screen painting underneath
/// it: an open `frust::glyph` menu adds exactly one rounded fill to the frame
/// (its drop shadow is a blurred path and its border a stroke, so neither lands
/// in [`RecScene::rounded`]).
fn menu_panel(before: &RecScene, after: &RecScene) -> (Point, Size) {
    let new = added(&before.rounded, &after.rounded);
    assert_eq!(new.len(), 1, "an open menu adds exactly its panel: {new:?}");
    largest(&new).expect("the menu panel painted itself")
}

/// `frust::glyph::menu`'s own row metrics, transcribed rather than imported —
/// they are private to that crate, and (like [`ROW_DEMO`]) a test that could
/// import them could not catch one changing. Rows start [`MENU_PANEL_PAD`] below
/// the panel's top edge, then run one [`MENU_ITEM_ROW_H`] per item and one
/// [`MENU_SEP_TOTAL_H`] per separator.
const MENU_PANEL_PAD: f64 = 6.0;
const MENU_ITEM_ROW_H: f64 = 34.0;
const MENU_SEP_TOTAL_H: f64 = 11.0;

/// Where a tap must land to hit the menu's "Demo session" row — the *pointer*
/// route into the live pane, as opposed to [`Harness::choose_menu_row`]'s
/// result-pop shortcut. `menu_entries` is item/sep/item/item/sep/**item**, so
/// the last row sits three items and two separators down the panel.
fn demo_row_center((origin, size): (Point, Size)) -> Point {
    let row_top = origin.y + MENU_PANEL_PAD + MENU_ITEM_ROW_H * 3.0 + MENU_SEP_TOTAL_H * 2.0;
    Point::new(origin.x + size.width / 2.0, row_top + MENU_ITEM_ROW_H / 2.0)
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

#[test]
fn choosing_demo_session_mounts_the_live_grid_and_replays_the_fixture() {
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

    // `support::long_press` lays out at its own default (800x600) surface —
    // this suite's `SURFACE` is taller, so the press is driven by hand here,
    // at this harness's own sized frames, to keep the grid's geometry (and
    // `point`'s meaning) consistent across the whole gesture.
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

    // Write straight into the shared preferences repository — standing in
    // for the settings screen's own picker save, the same store
    // `crate::features::settings::di::expect_preferences_repository`
    // publishes app-wide.
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
}

/// Review finding M4: `refresh_appearance` used to `set()` both signals
/// unconditionally on every poll tick, waking a tracked rebuild + unpaced
/// frame every poll interval, forever, even while the live pane sat
/// perfectly idle. Pins the fix's read-compare-then-write gate directly:
/// invoking `TerminalController::refresh_appearance` twice in a row is
/// exactly what two real poll ticks do (`LiveSession::enter`'s
/// `use_interval` callback is nothing but
/// `spawn_local(async move { poll.refresh_appearance().await })`), without
/// waiting on that interval's own real-time cadence — this suite has no way
/// to fake the background tokio runtime's timer, and a test that genuinely
/// waited out two production-cadence ticks would be both slow and, at any
/// shortened test-only cadence, testing a cadence nothing ships.
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

    let probe = debug_live_probe().expect("live");
    let windowed = probe
        .feed
        .borrow()
        .last_resize()
        .expect("the very first layout already resized off the fallback grid");

    // Two more steady-state frames: the box hasn't changed, so the feed must
    // not be told about a resize again.
    h.frame();
    h.frame();
    assert_eq!(
        probe.feed.borrow().last_resize(),
        Some(windowed),
        "an unchanged box records nothing new"
    );

    // Fullscreen changes the box substantially — a real delta.
    h.tap_label(EXPAND);
    h.settle_motion(1_000.0);
    let fullscreen = probe
        .feed
        .borrow()
        .last_resize()
        .expect("fullscreen is a real resize");
    assert_ne!(
        fullscreen, windowed,
        "fullscreen's box is a different cell grid"
    );
}

// ---------------------------------------------------------------------------
// 6 — fullscreen keeps the chrome geometry correct with a live pane
// ---------------------------------------------------------------------------

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
}

// ---------------------------------------------------------------------------
// 7 — the key bar: live-mode + IME-active only
// ---------------------------------------------------------------------------

/// A word every key cap announces — enough to prove the bar mounted without
/// pixel-hunting its geometry. `BarKey::name()` (the accessible name), not
/// `BarKey::label()` (the cap's printed text, `"Ctrl"`) — the two
/// deliberately differ (`term::input::BarKey`'s own doc).
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

    h.unmount_screen();
    assert!(
        debug_live_probe().is_none(),
        "the screen's own cleanup withdrew its chrome"
    );
}

// ---------------------------------------------------------------------------
// 9 — pump hygiene: the timer stops once nothing is left to say (task 06)
// ---------------------------------------------------------------------------
//
// These two drive the real `DemoFeed`/`InputSeam`/`run_feed` triple directly
// rather than through the mounted screen: `LiveSession::enter` constructs a
// `DemoFeed::new()`, whose real wall clock takes a genuine 30 seconds to
// drain the fixture — far too slow to wait out in a test — so these use
// `DemoFeed::manual()` instead (`tests/term_widget.rs`'s/`tests/term_input.rs`'s
// own precedent), wiring the exact same seams `LiveSession::enter` wires
// together in production (`InputSeam::wire_pump`), just with the replay's
// clock in the test's own hands.

#[test]
fn run_feed_exits_once_a_drained_feed_has_nothing_left_to_say() {
    let _g = serial();
    let _ambient = setup();

    let feed: Rc<RefCell<DemoFeed>> = Rc::new(RefCell::new(DemoFeed::manual()));
    // Push the manual clock straight past the whole 30s schedule: the
    // fixture is fully drained and nothing has been typed, so the very first
    // wake below should already see a closed feed.
    feed.borrow_mut().set_elapsed(Duration::from_secs(60));
    let feed_dyn: SharedFeed = feed.clone();
    let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
    let generation = RwSignal::new(0u64);
    let cancelled = Arc::new(AtomicBool::new(false));

    // `run_feed` never reaches its own sleep on this path — the very first
    // wake already sees a closed feed and returns — so a plain
    // current-thread runtime drives it to completion with no real
    // wall-clock wait at all. Under the old `!is_drained() || echo` rule
    // (echo defaults on) this call would never return.
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

// ---------------------------------------------------------------------------
// 10 — focus/IME hygiene: nothing summons a keyboard the user did not ask for
// ---------------------------------------------------------------------------
//
// The screen's v2 design is **tap-to-focus only**: the keyboard and the key bar
// are IME-active-only, so mounting the live pane must claim nothing at all and
// only a tap on the grid itself may open a focus/IME session. These three pin
// both halves of that at the root — `RenderRoot::is_focus_active`/`ime_state`
// are literally what a shell reads to decide whether to show a soft keyboard,
// so they are the honest place to assert it (a widget-level `has_focus` would
// not have caught the seeding these tests exist for).

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

/// The other half: tapping the grid *does* open the session — focus claimed and
/// an **active** IME surface published, which is what brings the keyboard (and
/// with it the key bar) up.
#[test]
fn tapping_the_grid_claims_focus_and_publishes_an_active_ime_surface() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.enter_demo();
    let scene = h.frame();

    let (origin, size) = grid_rect(&scene);
    h.tap(center((origin, size)));

    assert_eq!(
        h.session(),
        (true, true),
        "a tap on the grid is the one thing that opens the session"
    );
    assert!(
        h.root.ime_state().expect("a surface is published").active,
        "and the surface it publishes is an active one"
    );
}

/// Entering the demo through a real **pointer** tap on the menu row — the
/// device sequence, rather than [`Harness::choose_menu_row`]'s result-pop
/// shortcut — still leaves no focus/IME session behind once the menu is gone.
///
/// This is the app-side end of a framework defect, filed as a FINDINGS
/// candidate (the register lives outside this repo). `frust::glyph::menu`'s
/// widget claims root focus on **any** pointer `Down` ("a press anywhere claims
/// focus so a subsequent Escape has a chain to travel"), so the Down that picks
/// this row seeds `focus_active = true` with no IME surface behind it, for as
/// long as the menu's exit animation and pop take. Nothing in this app asks for
/// that, there is no seam to opt out of it, and it is the menu's claim — not
/// the live pane's mount — that the frame-gate capture caught. What this app
/// *can* pin is the outcome: the transient claim is released by the pop, no IME
/// surface is ever published, and the pane the row mounted is inert until the
/// user taps it. Green either way, so it keeps holding once the menu stops
/// claiming.
#[test]
fn a_real_menu_row_tap_leaves_no_focus_session_behind() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let before = h.settle();

    let after = h.open_menu();
    let panel = menu_panel(&before, &after);
    h.tap(demo_row_center(panel));
    // Past the menu's own exit staging and the overlay pop that follows it.
    h.flush();
    h.frame();

    assert!(
        debug_live_probe().is_some(),
        "the tap landed on the Demo session row"
    );
    assert_eq!(
        h.session(),
        (false, false),
        "the menu's transient focus claim went away with the menu"
    );
}
