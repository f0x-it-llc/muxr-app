//! Headless behavior coverage for the `/terminal` screen
//! (`src/features/terminal/**`) — no GPU, no window: a `RenderRoot` over the
//! real page builder, driven through `tests/support`.
//!
//! # The harness, and its two navigators
//!
//! Like `tests/sessions.rs`, [`Harness`] mounts the screen as the **bottom
//! layer of a `Stack`** with a navigator over it, because `NavigatorWidget`
//! contributes no `Widget::semantics` and forwards none of its pages' — a
//! screen mounted *inside* one is invisible to the accessibility tree. Beside
//! it sits a second, deliberately **unmounted** navigator, and that one is what
//! `muxr::routes::install_app_router` is pointed at:
//!
//! - the **mounted** navigator stands in for the app's root overlay host
//!   (`muxr::AppState::overlays`), which is what the screen pushes its overlays
//!   through (the overflow menu, the sheet, the rename dialog) since FINDINGS
//!   #44 — so each is exercised end to end and located, like every pushed page,
//!   by painted geometry. In production that host wraps the whole app rather
//!   than sitting beside the screen; what it is *above* is asserted in
//!   `tests/navigation_motion.rs`, which mounts the real shell;
//! - the **record-only** navigator carries the app's one `Nav` handle. Pushing
//!   `/terminal?session=s-1` through it records that query in the route-meta
//!   mirror — which is the only way this screen can learn its session id
//!   (`Nav::param`; frust's own route builder never sees a query) — while the
//!   page op it enqueues is never drained, because nothing mounts that
//!   navigator. Without the split, that push would build a *second* terminal
//!   screen inside the mounted navigator and the two would fight over the app
//!   bar's published chrome.
//!
//! That same mirror is what makes two otherwise invisible facts assertable:
//! `Nav::bar().fullscreen` is exactly "the shell's bar has been told to get out
//! of the way", and `Nav::bar().meta` going `None` is exactly "the terminal
//! route was popped off the app's stack".
//!
//! # Selecting a menu row without pixel-hunting one
//!
//! `frust::glyph::show_glyph_menu` paints its rows internally (no child pods)
//! and lives inside the navigator, so a row is reachable neither by semantics
//! nor by a rect of its own. Its **contract**, though, is public and exact:
//! a selection pops the menu page with `PopResult::of(index)` into the built
//! entry list. So [`Harness::choose_menu_row`] pops with that result, which
//! runs the very `on_result` the widget would have run. What the entry list
//! *is* (three rows and one separator, in order) is pinned by
//! `features::terminal::presentation::pages`' own unit tests, which can name
//! the constants directly.
//!
//! # Frame stepping
//!
//! The card's reveal, its caret and the fullscreen transition all advance off
//! `PaintCtx::frame_time`, so they are stepped with [`support::frame_at`]
//! rather than waited on: a paint at `t` *is* the animation at `t`.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, NavigatorController, PopResult, SizedBox, Stack, Theme, WindowMetrics, any,
    navigator, provide_context,
};
use frust_core::{FrameTime, NamedKey, PointerPhase, RenderRoot, WindowInsets};
use frust_text::TextContext;
use kurbo::{Point, Size};
use peniko::Color;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::sessions::di::expect_sessions_repository;
use muxr::features::terminal::presentation::pages::{open_terminal_menu, terminal_page};
use muxr::routes::{BarAction, app_nav, install_app_router, route_meta};
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, by_width, center, char_key, frame, frame_at_sized, largest, ms,
    named_key, node_centers_by_label, node_labels, node_values, pointer, pump_frames, rounded_in,
    serial, setup, tap,
};

/// The surface every frame in this suite lays out at. Wider and taller than
/// the default 800×600 so the sheet's own rows never fall below the fold.
const SURFACE: Size = Size::new(800.0, 700.0);
/// A point in the top-left corner — scrim on every overlay this screen pushes.
const SCRIM_POINT: Point = Point::new(4.0, 4.0);
/// The panel gutter `glyph::dialog`/`ui::sheet` share, transcribed.
const PANEL_PADDING: f64 = 20.0;
/// How long a pumped wait may run before the test fails rather than hangs.
const PUMP_DEADLINE: Duration = Duration::from_secs(5);

/// The session the suite attaches to — the sessions mock's first seeded one.
const SESSION_ID: &str = "s-1";
const SESSION_NAME: &str = "dev";

/// `docs/terminal.html`'s `TABS`, as the screen must print them.
const DEVBOX_TITLE: &str = "ed@devbox: ~";
const DEVBOX_FIRST_LINE: &str = "ed@devbox:~$ systemctl status muxrd";
const BTOP_TITLE: &str = "ed@precision: ~";
/// How many lines each tab carries (and so how many the reveal staggers).
const LINES_PER_TAB: usize = 4;

/// The expand button's two accessible names, transcribed from
/// `presentation::pieces` (the implementation's constants; a test that imported
/// them could not catch one of them changing).
const EXPAND: &str = "Fullscreen";
const COLLAPSE: &str = "Exit fullscreen";
/// …and the sheet's close button's.
const CLOSE: &str = "Close";

/// `.term-wrap{padding:14px}` and `.term-card{border-radius:22px}`,
/// transcribed — the two numbers fullscreen animates to zero.
const WRAP_PAD: f64 = 14.0;
const CARD_RADIUS: f64 = 22.0;
/// The wrap padding's wide-window target past
/// `crate::responsive::WIDE_BREAKPOINT_PX` (FINDINGS #21's width-clamp pass),
/// transcribed (private to `presentation::pages`) — the card itself stays
/// full-bleed at any width; only the space around it grows from
/// [`WRAP_PAD`]'s 14px to this.
const WIDE_WRAP_PAD: f64 = 22.0;
/// `.cur{width:7px;height:13px}` — the caret's own box, which is how a frame
/// is asked whether the caret is lit.
const CARET: Size = Size::new(7.0, 13.0);

/// Menu row indices into the built entry list (separator included) — the
/// contract `show_glyph_menu` pops with. Named in
/// `presentation::pages`; transcribed here for the same reason as the labels.
const ROW_PANES: usize = 0;
const ROW_RENAME: usize = 2;
const ROW_DETACH: usize = 3;

/// The mounted navigator's depth with no overlay up: its own (empty, full-bleed)
/// initial page.
const REST_DEPTH: usize = 1;

/// `presentation::pages`' own `SHEET_CLOSE_DELAY`, transcribed (it is private
/// there, and a test that imported it could not catch it changing): how long a
/// tab pill leaves the sheet up so the selection visibly moves first.
const CLOSE_DELAY: Duration = Duration::from_millis(260);

/// What a landed detach says, transcribed from `presentation::pages`.
const DETACHED_TOAST: &str = "detached";

/// The second tab's pane title (`docs/terminal.html`'s `TABS[1]`) — what the
/// card prints once the *second* of two rapid pill taps has landed.
const MINEFIT_TITLE: &str = "ed@minefit: ~/api";

/// The app-logic closure `RenderRoot::rebuild` drives.
type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

/// A mock connect target — the guard bounces `/terminal` without one.
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

/// A [`WindowMetrics`] whose size is `size` — see `Harness::mount_with_metrics`.
fn metrics_at(size: Size) -> WindowMetrics {
    WindowMetrics::new(size, 1.0, WindowInsets::default())
}

/// The screen, an overlay navigator over it, and everything a frame needs.
struct Harness {
    /// The **mounted** navigator: the screen's overlays land here.
    nav: NavigatorController<AppState>,
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    /// Whether the screen is still mounted — flipping this to `false`
    /// unmounts it, which is how [`Harness::unmount_screen`] stands in for a
    /// back-nav off the route.
    mounted: Rc<Cell<bool>>,
}

impl Harness {
    /// Mount the screen (see the [module docs](self) for the shape), attached
    /// to [`SESSION_ID`] and with the first load already landed.
    fn mount() -> Self {
        Self::themed(Theme::glyph_baseline())
    }

    /// [`Harness::mount`], first publishing `metrics` into context when
    /// `Some` — the shape `muxr`'s pages read via
    /// `use_context::<WindowMetrics>()` for the responsive width-clamp pass
    /// (FINDINGS #21). `None` leaves context unset, exactly the state every
    /// other test in this suite mounts under (no shell has published yet),
    /// so this is purely additive.
    fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Self {
        if let Some(metrics) = metrics {
            provide_context(metrics);
        }
        Self::mount()
    }

    /// [`mount`](Self::mount) under an explicit theme — how the suite drives
    /// `reduce_motion`.
    fn themed(theme: Theme) -> Self {
        // The real composition root: it publishes the app-scoped connection
        // controller and calls every feature's `di::provide` — including this
        // feature's repository and the sessions one it renames through.
        let state = MuxrApp.init();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        // Never mounted — see the module docs.
        let record_only: NavigatorController<AppState> = NavigatorController::new();
        let _router = install_app_router(&record_only, &nav, expect_connection_controller());
        // Connected, so the session guard lets the push below resolve (and so
        // the recorded query survives — a bounced push records `/`'s, not
        // `/terminal`'s).
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
                    // Full-bleed and childless: it paints nothing and handles
                    // nothing, so every tap falls through to the screen below.
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

    /// One frame at the zero clock.
    fn frame(&mut self) -> RecScene {
        self.frame_at(0.0)
    }

    /// One frame painted at `at_ms` — how every animation here is stepped.
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

    /// Drive a pushed/popped overlay past the navigator's input-blocking
    /// transition, returning the last frame's paint record.
    fn settle(&mut self) -> RecScene {
        support::settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
        )
    }

    /// Settle, then deliver a pop's result: the navigator flushes a pusher's
    /// `on_result` at the start of the first event pass **after** the pop's
    /// rebuild, so a harmless `Move` is what actually hands the answer over.
    fn flush(&mut self) {
        self.settle();
        self.settle();
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, SCRIM_POINT));
    }

    /// Pump the UI-thread task queue and rebuild until `done` accepts the
    /// rendered text, or fail at [`PUMP_DEADLINE`] rather than hang.
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

    /// Pump and rebuild until `done` accepts the harness itself — the
    /// depth-watching sibling of
    /// [`pump_until_values`](Self::pump_until_values).
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

    /// Wait for the async first load (spaces, tabs, lines, session name).
    fn await_load(&mut self) {
        self.pump_until_values("the seeded workspace", |values| {
            values.iter().any(|v| v == DEVBOX_TITLE)
        });
    }

    /// A full tap at `point`.
    fn tap(&mut self, point: Point) {
        tap(&mut self.root, &mut self.state, point);
    }

    /// Tap the first node *labelled* `label` (the expand button, the sheet's
    /// close button).
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

    /// Type `text` into whatever holds focus, one already-resolved character
    /// event at a time (the shape a platform delivers).
    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.root.event(&mut self.state, &char_key(&c.to_string()));
        }
    }

    /// Press a named key against the focused chain.
    fn press(&mut self, key: NamedKey) {
        self.root.event(&mut self.state, &named_key(key));
    }

    /// The mounted navigator's page-stack depth. Its own initial page counts,
    /// so this is [`REST_DEPTH`] with nothing up and one more per overlay.
    fn depth(&self) -> usize {
        self.nav.depth()
    }

    /// Paint far enough past `from_ms` for a frame-clock animation to settle
    /// **and** for layout to pick the settled value up.
    ///
    /// Three frames, because the card's geometry is a *layout* fact: paint
    /// advances the driver, and the next layout is what turns the new progress
    /// into a box (the same one-frame relationship `crate::ui::topbar`'s reveal
    /// has). The first frame only seeds the driver's clock — an
    /// `AnimationController`'s first `advance` after `forward()` is a zero
    /// delta by construction.
    fn settle_motion(&mut self, from_ms: f64) -> RecScene {
        self.frame_at(from_ms);
        self.frame_at(from_ms + 1_000.0);
        self.frame_at(from_ms + 1_010.0);
        self.frame_at(from_ms + 1_020.0)
    }

    /// Open the app bar's overflow menu, exactly as `crate::shell` does.
    fn open_menu(&mut self) -> RecScene {
        open_terminal_menu(&mut self.state);
        self.settle()
    }

    /// Choose menu row `index` — see the [module docs](self)'s note on why
    /// this pops with the widget's own result rather than tapping a row.
    fn choose_menu_row(&mut self, index: usize) {
        self.nav.pop_with_result(PopResult::of(index));
        self.flush();
    }

    /// Open the "panes & tabs" sheet through the menu, returning the frames
    /// before and after so the sheet's own chrome can be isolated.
    fn open_sheet(&mut self) -> (RecScene, RecScene) {
        let before = self.frame();
        self.open_menu();
        self.choose_menu_row(ROW_PANES);
        let after = self.settle();
        (before, after)
    }

    /// Push a stand-in for the route a sheet floats over, and answer the depth
    /// the stack now rests at.
    ///
    /// **Without it a stray pop is invisible.** A navigator always keeps one
    /// page (`NavigatorController::pop`'s own contract: "a pop of the
    /// last/root page is ignored"), so a second, unwanted pop against the bare
    /// harness is silently absorbed and [`Harness::depth`] never moves. The
    /// app's real stack never looks like that — `/terminal` is always under the
    /// sheet, and *that* page coming off is exactly the defect the races below
    /// exist for. This page stands in for it, so a stray pop shows up as a
    /// depth that fell through the floor.
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

    /// Pump real frames for at least `span` of wall clock.
    ///
    /// The delayed close runs on the executor's own timer, so "the late close
    /// has had every chance to fire" is a wall-clock fact rather than a frame
    /// count — this is what makes an assertion *after* it a statement about
    /// the guard and not about the pump.
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

    /// The sheet's own close button, located the way
    /// [`the_sheets_close_button_dismisses_it`] locates it: the one 26px disc
    /// it paints (it lives inside the navigator, so it has no semantics node).
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

    /// Unmount the screen — the harness's stand-in for a back-nav off
    /// `/terminal` (the `Component`'s `on_cleanup` is what actually runs).
    fn unmount_screen(&mut self) {
        self.mounted.set(false);
        self.settle();
    }

    /// Every text run currently on screen.
    fn values(&self) -> Vec<String> {
        node_values(&self.root)
    }

    /// Whether any run on screen prints `value`.
    fn shows(&self, value: &str) -> bool {
        self.values().iter().any(|v| v == value)
    }

    /// Pump until the **sessions** repository holds a session named `name` —
    /// how this suite observes a rename that went through the sibling feature's
    /// own store (rather than through anything this screen keeps).
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

/// The terminal card — the biggest rounded fill on screen, which is the card's
/// own body — as `(origin, size, radius)`.
fn card(scene: &RecScene) -> (Point, Size, f64) {
    let ink = Theme::glyph_baseline()
        .extension::<frust::GlyphInk>()
        .copied()
        .expect("the Glyph baseline carries GlyphInk");
    let mut cards = rounded_in(scene, ink.terminal_bg);
    cards.sort_by(|a, b| (b.1.width * b.1.height).total_cmp(&(a.1.width * a.1.height)));
    cards
        .first()
        .copied()
        .expect("the card paints its own body every frame")
}

/// How many caret blocks the frame painted (`0` or `1`).
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

/// The overlay panel in `added` — the biggest rect an overlay added.
fn panel(added: &[(Point, Size)]) -> (Point, Size) {
    largest(added).expect("an overlay always paints its own panel")
}

/// The sheet's pill rects, in reading order (top to bottom, then left to
/// right).
///
/// Every pill is the same height — one 11px run inside 8px of vertical padding
/// — and there are nine of them, so the **most common height** among the
/// sheet's own rects is the pills' and nothing else's (the panel, the drag
/// handle, the close disc, the pane item, its mark box and the `focused` chip
/// are one apiece). Picking them that way keeps the locator independent of the
/// type role's line box, which is a theme number rather than a mock one.
fn pills(added: &[(Point, Size)]) -> Vec<(Point, Size)> {
    let mut heights: Vec<(f64, usize)> = Vec::new();
    for (_, size) in added {
        match heights
            .iter_mut()
            .find(|(height, _)| (height - size.height).abs() < EPSILON)
        {
            Some((_, count)) => *count += 1,
            None => heights.push((size.height, 1)),
        }
    }
    let (pill_height, _) = heights
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .expect("the sheet paints something");
    let mut pills: Vec<(Point, Size)> = added
        .iter()
        .copied()
        .filter(|(_, size)| (size.height - pill_height).abs() < EPSILON)
        .collect();
    pills.sort_by(|a, b| {
        a.0.y
            .total_cmp(&b.0.y)
            .then_with(|| a.0.x.total_cmp(&b.0.x))
    });
    pills
}

/// A theme with `reduce_motion` forced on.
fn reduced() -> Theme {
    let mut theme = Theme::glyph_baseline();
    theme.motion.reduce_motion = true;
    theme
}

// ---------------------------------------------------------------------------
// 1 — the card renders the mock's own world
// ---------------------------------------------------------------------------

#[test]
fn the_card_prints_the_active_tabs_title_and_every_one_of_its_lines() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::mount();

    // The status row's pane title, and the body as one transcript block.
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
    let scene = h.frame();

    let ink = Theme::glyph_baseline()
        .extension::<frust::GlyphInk>()
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
    // …and specifically not `terminal.html`'s own `--term-body`/`--term-header`.
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

// ---------------------------------------------------------------------------
// 2 — the staggered reveal
// ---------------------------------------------------------------------------

/// How many glyph runs a frame at `at_ms` paints — the reveal's observable,
/// since a line whose window has not opened paints nothing at all.
fn runs_at(h: &mut Harness, at_ms: f64) -> usize {
    h.frame_at(at_ms).glyph_runs
}

#[test]
fn the_lines_stagger_in_one_after_another_rather_than_all_at_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // The driver is armed on the first paint after the load, at the zero
    // clock; every frame below is that many milliseconds into the reveal.
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

    // No cascade at all: the very first frame prints what the settled one does.
    let first = runs_at(&mut h, 0.0);
    let settled = runs_at(&mut h, 2_000.0);
    assert_eq!(first, settled, "a reduced reveal is not a reveal");
}

// ---------------------------------------------------------------------------
// 3 — the caret
// ---------------------------------------------------------------------------

#[test]
fn the_caret_blinks_on_the_prompt_line() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // Past the reveal, so the caret's line is fully shown either way.
    assert_eq!(carets(&h.frame_at(1_200.0)), 1, "lit in the first half");
    assert_eq!(carets(&h.frame_at(1_700.0)), 0, "…and dark in the second");
    assert_eq!(carets(&h.frame_at(2_300.0)), 1, "…and the cycle repeats");
}

#[test]
fn reduce_motion_freezes_the_caret_lit_rather_than_hiding_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::themed(reduced());

    // A caret is a *position* cue, so collapsing the animation must not
    // collapse the caret.
    for at in [1_200.0, 1_700.0, 2_300.0] {
        assert_eq!(carets(&h.frame_at(at)), 1, "still lit at {at}ms");
    }
}

// ---------------------------------------------------------------------------
// 4 — fullscreen, both directions
// ---------------------------------------------------------------------------

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
    // The button stays put and swaps its mark; only its name changes.
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
    // Partway through the theme's `slow` (340ms) transition: the card has left
    // its gutter but has not reached the edge. Two frames — the first seeds
    // the driver's clock, the second advances it, and the *third* is the one
    // whose layout has turned that progress into a box.
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
    // Two frames, not three: nothing animates at all, so the only wait is the
    // one layout pass that turns the snapped value into a box.
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

    // The regression this exists for: a back-nav straight out of fullscreen
    // must not strand a hidden bar on the route below.
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

    // Seed the new stage's clock, then advance it 80ms into `durations.slow`
    // (340ms) — comfortably inside the `t ∈ [0.45, 1.0]` band where Glyph's
    // `spatial` curve overshoots past `1.0`. Easing the *interpolated
    // position* (the bug `FullscreenStage::value` fixed) pins the card flush
    // against the edge for the whole first half of the transition; easing
    // the *driver's progress* (the fix) already has it visibly off the edge
    // by here.
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
    // Seed the driver's clock, then sample the wrap padding across the whole
    // transition and a little past it.
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
    // ~60ms into the 340ms expand: mid-flight, not yet arrived.
    let mid_expand = card(&h.frame_at(61.0)).0.x;
    assert!(
        mid_expand > EPSILON,
        "should still be off the fully-expanded edge: {mid_expand:?}"
    );

    // Interrupt with a collapse. The bug this guards against: seeding the new
    // stage's `from` with the *painted* (already-eased) value and running it
    // through the curve a second time lands in the overshoot band and snaps
    // the card straight back to full-bleed before it ever collapses — a
    // regression from the interrupt point rather than a continuation past it.
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

    // …and it still settles cleanly at the windowed rest state.
    let (origin, size, radius) = card(&h.settle_motion(3_000.0));
    assert!((origin.x - WRAP_PAD).abs() < EPSILON, "{origin:?}");
    assert!((size.width - (SURFACE.width - 2.0 * WRAP_PAD)).abs() < EPSILON);
    assert!((radius - CARD_RADIUS).abs() < EPSILON, "{radius}");
}

// ---------------------------------------------------------------------------
// 5 — the app bar's overflow menu
// ---------------------------------------------------------------------------

#[test]
fn the_terminal_route_carries_the_kebab_and_it_opens_a_menu() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    assert_eq!(
        route_meta("/terminal").and_then(|meta| meta.action),
        Some(BarAction::TerminalMenu),
        "the shell's bar is what mounts the `⋮`"
    );

    let before = h.frame();
    let after = h.open_menu();
    assert_eq!(h.depth(), REST_DEPTH + 1, "the menu is a pushed page");
    assert!(
        !added(&before.rounded, &after.rounded).is_empty(),
        "…and it painted its own panel"
    );
}

// ---------------------------------------------------------------------------
// 6 — the "panes & tabs" sheet
// ---------------------------------------------------------------------------

#[test]
fn the_menus_first_row_opens_the_sheet_with_every_pill_row_and_the_pane_item() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (before, after) = h.open_sheet();
    assert_eq!(
        h.depth(),
        REST_DEPTH + 1,
        "the sheet replaced the menu rather than stacking over it"
    );

    let sheet = added(&before.rounded, &after.rounded);
    let (origin, size) = panel(&sheet);
    assert!(
        origin.y + size.height > SURFACE.height,
        "bottom-anchored, like every muxr sheet: {origin:?}/{size:?}"
    );
    // Three spaces + `+ space`, then four tabs + `+`.
    assert_eq!(
        pills(&sheet).len(),
        3 + 1 + 4 + 1,
        "every pill row, add pills included: {sheet:?}"
    );
    // The pane item's own mark box, and the dashed new-pane row (which paints
    // no fill at all, only strokes).
    assert!(
        !added(&before.strokes, &after.strokes).is_empty(),
        "the dashed rows and hairlines"
    );
}

#[test]
fn a_tab_pill_switches_the_card_and_then_closes_the_sheet() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);
    // Reading order: the four space pills, then the five tab ones. `1 btop` is
    // the first of the second row.
    let btop = pills(&sheet)[4];
    h.tap(center(btop));

    // The switch is async (a repository read), and the sheet closes a beat
    // after it lands.
    h.pump_until_values("the btop tab", |values| {
        values.iter().any(|v| v == BTOP_TITLE)
    });
    assert!(h.shows(BTOP_TITLE), "the card followed the pill");
    h.pump_until("the sheet to close", |h| h.depth() == REST_DEPTH);
    assert_eq!(h.depth(), REST_DEPTH, "…and the sheet closed behind it");
}

#[test]
fn a_tab_switch_replays_the_reveal_from_the_top() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // Settle the first reveal.
    let settled = runs_at(&mut h, 2_000.0);

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);
    h.tap(center(pills(&sheet)[4]));
    h.pump_until_values("the btop tab", |values| {
        values.iter().any(|v| v == BTOP_TITLE)
    });
    h.pump_until("the sheet to close", |h| h.depth() == REST_DEPTH);

    // The replayed reveal starts from nothing again, then fills back in.
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
    // The close button is the sheet's one *labelled* control — a real
    // `Role::Button`, unlike the pills (which are gesture targets over runs).
    // It lives inside the navigator, so it is tapped by the rect it paints.
    let scene = h.frame();
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

// ---------------------------------------------------------------------------
// 6b — the sheet closes once, whatever happens during the close beat
// ---------------------------------------------------------------------------
//
// A tab pill leaves the sheet up for `CLOSE_DELAY` so the selection visibly
// moves first, and the sheet stays fully interactive for that whole beat. Every
// test here fires a *second* dismissal inside it and asserts the same two
// things: the sheet closes once, and the page under it (the `/terminal` route
// in the app, [`Harness::push_route_floor`]'s stand-in here) is still there
// afterwards. Before `ui::sheet`'s dismissal gate, each of these popped it.

#[test]
fn a_second_tab_pill_inside_the_close_beat_still_closes_the_sheet_only_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let floor = h.push_route_floor();

    let (before, after) = h.open_sheet();
    assert_eq!(h.depth(), floor + 1, "the sheet is up over the route");
    let sheet = added(&before.rounded, &after.rounded);
    let pills = pills(&sheet);
    // Reading order: the four space pills, then the tabs — `1 btop` first,
    // `2 minefit-api` second. Both taps land inside the same close beat.
    h.tap(center(pills[4]));
    h.tap(center(pills[5]));

    // Latest tap wins: the second selection is the one that lands…
    h.pump_until_values("the minefit tab", |values| {
        values.iter().any(|v| v == MINEFIT_TITLE)
    });
    // …one close fires (`<=`, not `==`: two stacked closes can land in the
    // same rebuild, and this test must report *that* rather than time out)…
    h.pump_until("the sheet to close", |h| h.depth() <= floor);
    // …and the superseded one, given every chance to wake, lands on nothing.
    h.pump_for(2 * CLOSE_DELAY);

    assert_eq!(
        h.depth(),
        floor,
        "the second close never popped the route under the sheet"
    );
    assert!(
        h.shows(MINEFIT_TITLE) && !h.shows(BTOP_TITLE),
        "the second selection is the one applied: {:?}",
        h.values()
    );
}

#[test]
fn a_scrim_dismissal_inside_the_close_beat_leaves_the_route_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let floor = h.push_route_floor();

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);
    h.tap(center(pills(&sheet)[4]));
    // The sheet is still up — and still dismissible — for the whole beat.
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), floor, "the scrim closed it, once");

    h.pump_for(2 * CLOSE_DELAY);
    assert_eq!(
        h.depth(),
        floor,
        "…and the pill's late close found the sheet already gone"
    );
}

#[test]
fn the_close_button_inside_the_close_beat_leaves_the_route_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let floor = h.push_route_floor();

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);
    h.tap(center(pills(&sheet)[4]));
    let close = h.close_disc();
    h.tap(close);
    h.flush();
    assert_eq!(h.depth(), floor, "the close button closed it, once");

    h.pump_for(2 * CLOSE_DELAY);
    assert_eq!(
        h.depth(),
        floor,
        "…and the pill's late close found the sheet already gone"
    );
    assert!(
        !node_labels(&h.root).iter().any(|l| l == CLOSE),
        "the sheet is gone, not reopened"
    );
}

// ---------------------------------------------------------------------------
// 7 — rename and detach
// ---------------------------------------------------------------------------

#[test]
fn the_menus_rename_row_round_trips_into_the_sessions_repository() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let before = h.frame();
    h.open_menu();
    h.choose_menu_row(ROW_RENAME);
    let after = h.settle();
    assert_eq!(h.depth(), REST_DEPTH + 1, "the rename dialog is up");

    // The dialog's field is its one full-content-width child.
    let dialog = added(&before.rounded, &after.rounded);
    let content_w = panel(&dialog).1.width - 2.0 * PANEL_PADDING;
    let field = by_width(&dialog, content_w).expect("the name field");
    h.tap(center(field));
    // The dialog seeds itself with the attached session's *current* name, so
    // the typed text lands after it — clear it first.
    for _ in 0..SESSION_NAME.len() {
        h.press(NamedKey::Backspace);
    }
    h.type_text("dev-2");
    h.press(NamedKey::Enter);
    h.flush();

    assert_eq!(h.depth(), REST_DEPTH, "a submit closes the dialog");
    // The write went through the *sessions* feature's own repository, which is
    // the whole point: the list reflects it on the way back.
    h.await_repository_name("dev-2");
}

/// Whether the sessions repository currently holds a session named `name` —
/// driven synchronously off the controller's own signal-free read path.
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
fn the_menus_detach_row_pops_the_terminal_route() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    assert_eq!(
        app_nav().bar().meta,
        route_meta("/terminal"),
        "the app's route mirror has /terminal on top"
    );

    h.open_menu();
    h.choose_menu_row(ROW_DETACH);
    // Detach is async; the pop lands once the repository answers.
    let deadline = Instant::now() + PUMP_DEADLINE;
    while app_nav().bar().meta.is_some() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the detach"
        );
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    assert_eq!(
        app_nav().bar().meta,
        None,
        "the terminal route came off the stack"
    );
}

#[test]
fn a_second_detach_while_the_first_is_in_flight_pops_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    // Detach is async (a repository round trip) and the menu can be reopened
    // while it is in flight — two landings would pop `/sessions` off the stack
    // as well as `/terminal`. Neither `choose_menu_row` nor `settle` pumps the
    // task queue, so both requests are made before either can land.
    h.open_menu();
    h.choose_menu_row(ROW_DETACH);
    h.open_menu();
    h.choose_menu_row(ROW_DETACH);

    let deadline = Instant::now() + PUMP_DEADLINE;
    while app_nav().bar().meta.is_some() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the detach"
        );
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    // Give a second landing every chance to arrive before counting.
    h.pump_for(2 * CLOSE_DELAY);

    // The detach's own toast is raised on the same line as the pop, so
    // counting toasts *is* counting pops (the app's stack itself is behind the
    // record-only navigator here — see the module docs).
    let detached = app_toasts()
        .pending_untracked()
        .into_iter()
        .filter(|message| message == DETACHED_TOAST)
        .count();
    assert_eq!(detached, 1, "one detach, one pop, one toast");
}

// ---------------------------------------------------------------------------
// 8 — vector chrome
// ---------------------------------------------------------------------------

#[test]
fn every_affordance_on_this_screen_is_a_vector_and_announces_a_word() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let scene = h.frame();

    // The expand button draws exactly one filled path (its mark) and no glyph
    // of its own — the "chrome icons are vectors" rule, as geometry.
    assert!(
        !scene.paths.is_empty(),
        "the `⛶` is a path, not a font glyph"
    );
    // …and it is named, not shaped, in the accessibility tree.
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

    // The five marks `terminal.html` sets as *text* — none of which is in the
    // bundled Glyph faces, and each of which this screen draws as a vector
    // instead. A run carrying one would be a silent per-platform difference.
    for banned in ["\u{26f6}", "\u{2921}", "\u{25a4}", "\u{23fb}", "\u{22ee}"] {
        assert!(
            !h.values().iter().any(|v| v.contains(banned)),
            "{banned:?} is set as text somewhere: {:?}",
            h.values()
        );
    }
}

// ---------------------------------------------------------------------------
// 9 — the route param
// ---------------------------------------------------------------------------

#[test]
fn the_screen_reads_the_attached_session_off_the_route_it_was_pushed_with() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // The proof is exact, if indirect: the rename dialog seeds itself with the
    // attached session's *current* name, which the screen can only know from
    // `?session=s-1`. So delete one character and submit — what lands in the
    // repository is that seed minus its last letter, and nothing else.
    let before = h.frame();
    h.open_menu();
    h.choose_menu_row(ROW_RENAME);
    let after = h.settle();
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
    // A guard against the empty-body flash: the screen paints its card from
    // its very first frame, before any repository has answered.
    let state = MuxrApp.init();
    let nav: NavigatorController<AppState> = NavigatorController::new();
    let record_only: NavigatorController<AppState> = NavigatorController::new();
    let _router = install_app_router(&record_only, &nav, expect_connection_controller());
    let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
    root.set_theme(Box::new(Theme::glyph_baseline()));
    let logic_nav = nav.clone();
    let mut logic: Logic = Box::new(move |_state: &mut AppState| terminal_page(logic_nav.clone()));
    let mut tcx = TextContext::new();
    let mut state = state;

    let scene = frame(&mut root, &mut logic, &mut state, &mut tcx);
    let ink = Theme::glyph_baseline()
        .extension::<frust::GlyphInk>()
        .copied()
        .expect("GlyphInk");
    assert!(
        !rounded_in(&scene, ink.terminal_bg).is_empty(),
        "the card is there from frame one"
    );
    let _ = FrameTime::ZERO;
}

// ---------------------------------------------------------------------------
// 10 — responsive width-clamp pass (FINDINGS #21)
// ---------------------------------------------------------------------------

#[test]
fn wide_metrics_grow_the_wrap_padding_but_keep_the_card_full_bleed() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_with_metrics(Some(metrics_at(SURFACE)));

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
}

#[test]
fn fullscreen_stays_edge_to_edge_even_with_wide_metrics() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_with_metrics(Some(metrics_at(SURFACE)));

    h.tap_label(EXPAND);
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
    // Explicit narrow metrics, even though the harness still paints at the
    // full-width `SURFACE` — proving the branch keys on the *published*
    // width, not the paint surface.
    let mut h = Harness::mount_with_metrics(Some(metrics_at(Size::new(400.0, SURFACE.height))));

    let (origin, size, radius) = card(&h.frame());
    assert!(
        (origin.x - WRAP_PAD).abs() < EPSILON,
        "narrow metrics take the same unwidened branch as no metrics: {origin:?}"
    );
    assert!((size.width - (SURFACE.width - 2.0 * WRAP_PAD)).abs() < EPSILON);
    assert!((radius - CARD_RADIUS).abs() < EPSILON);
}
