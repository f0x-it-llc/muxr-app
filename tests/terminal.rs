//! Headless behavior coverage for the `/terminal` screen
//! (`src/features/terminal/**`) — no GPU, no window: a `RenderRoot` over the real
//! page builder, driven through `tests/support`.
//!
//! [`Harness`] mounts the screen as the **bottom layer of a `Stack`** with a
//! navigator over it, because `NavigatorWidget` contributes no
//! `Widget::semantics`: a screen mounted *inside* one is invisible to the
//! accessibility tree. Two navigators, deliberately — the **mounted** one stands
//! in for the app's root overlay host, which is what the screen pushes its
//! overlays through, so each is exercised end to end and located by painted
//! geometry; the **record-only** one carries the app's `Nav` handle, so pushing
//! `/terminal?session=s-1` records that query in the route-meta mirror (the only
//! way this screen learns its session id) while the page op it enqueues is never
//! drained. Without the split, that push would build a second terminal screen
//! inside the mounted navigator and the two would fight over the published chrome.
//!
//! That mirror also makes two invisible facts assertable:
//! `Nav::bar().fullscreen` is "the shell's bar has been told to get out of the
//! way", and `Nav::bar().meta` going `None` is "the terminal route was popped".
//!
//! A menu row is reachable neither by semantics nor by a rect of its own, but its
//! contract is exact: a selection pops the menu page with `PopResult::of(index)`,
//! so [`Harness::choose_menu_row`] pops with that result, running the very
//! `on_result` the widget would have. The card's reveal, caret and fullscreen
//! transition advance off `PaintCtx::frame_time`, so they are **stepped** with
//! [`support::frame_at`] rather than waited on.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, GetUntracked, NavigatorController, PopResult, Set, SizedBox, Stack, Theme,
    WindowMetrics, any, navigator, provide_context,
};
use frust_core::{FrameTime, NamedKey, PointerPhase, RenderRoot, WindowInsets};
use frust_text::TextContext;
use kurbo::{Point, Size};
use peniko::Color;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::sessions::di::expect_sessions_repository;
use muxr::features::terminal::presentation::pages::{
    debug_controller, open_terminal_menu, terminal_page,
};
use muxr::routes::{BarAction, app_nav, install_app_router, route_meta};
use muxr::ui::toasts::app_toasts;
use muxr::ui::topbar::BAR_HEIGHT;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, by_width, center, char_key, frame, frame_at_sized, largest, ms,
    named_key, node_centers_by_label, node_centers_by_value, node_labels, node_values, pointer,
    pump_frames, rounded_in, serial, setup, tap,
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
/// `presentation::pieces`' own `EDIT_LABEL` (private there) — every chip's
/// trailing edit icon shares this one accessible name.
const EDIT_LABEL: &str = "Rename";

/// `.term-wrap{padding:14px}` and `.term-card{border-radius:22px}`,
/// transcribed — the two numbers fullscreen animates to zero.
const WRAP_PAD: f64 = 14.0;
const CARD_RADIUS: f64 = 22.0;
/// The wrap padding's wide-window target past
/// `crate::responsive::WIDE_BREAKPOINT_PX`, transcribed because it is private to
/// `presentation::pages` — the card stays full-bleed at any width; only the space
/// around it grows from [`WRAP_PAD`]'s 14px to this.
const WIDE_WRAP_PAD: f64 = 22.0;
/// The narrow/portrait window's **top-only** bar-to-card gap, transcribed
/// (private to `presentation::pages`) — the same step-up the wide branch
/// already applied on all four sides.
const PORTRAIT_TOP_GAP: f64 = WIDE_WRAP_PAD - WRAP_PAD;
/// `presentation::pieces`' own status-row height, transcribed: the 28px expand
/// button plus its tightened 6px padding either side.
const STATUS_ROW_H: f64 = 40.0;
/// `.cur{width:7px;height:13px}` — the caret's own box, which is how a frame
/// is asked whether the caret is lit.
const CARET: Size = Size::new(7.0, 13.0);

/// Menu row indices into the built entry list — the contract `show_glyph_menu`
/// pops with, transcribed for the same reason as the labels. The menu carries no
/// separators, so a row's index is its position.
const ROW_PANES: usize = 0;
const ROW_RENAME: usize = 1;
const ROW_DETACH: usize = 2;

/// The mounted navigator's depth with no overlay up: its own (empty, full-bleed)
/// initial page.
const REST_DEPTH: usize = 1;

/// `presentation::pages`' own `SHEET_CLOSE_DELAY`, transcribed (it is private
/// there, and a test that imported it could not catch it changing): how long a
/// tab pill leaves the sheet up so the selection visibly moves first.
const CLOSE_DELAY: Duration = Duration::from_millis(260);

/// What a landed detach says, transcribed from `presentation::pages`.
const DETACHED_TOAST: &str = "detached";

/// …and what a control a read-only credential may not use says instead, same
/// provenance.
const READ_ONLY_TOAST: &str = "read-only session";

/// `muxr::failure::AppFailure::Internal`'s own `user_message()`, transcribed —
/// what every one of the sheet's three create affordances shows once the
/// repository answers, since `MockTerminalRepository` deliberately leaves
/// `create_space`/`new_tab`/`new_pane` at `TerminalRepository`'s own "unsupported"
/// default. Seeing this toast is this suite's proof an affordance actually reached
/// the repository.
const INTERNAL_FAILURE_TOAST: &str = "Something went wrong — please try again.";

/// `presentation::pieces`' own `NEW_PANE_LABEL` (private there) — the new-pane
/// row's label, and the only way this suite locates it: the row paints a
/// **transparent**-filled, dashed-bordered box, so it shows up in neither
/// [`RecScene::rounded`] nor [`RecScene::strokes`], and is found by its accessible
/// *value* instead.
const NEW_PANE_LABEL: &str = "new pane \u{b7} long-press for floating";

/// `presentation::pages`' own `ADD_SPACE_LABEL`/`ADD_TAB_LABEL`, private there —
/// the two full-size create rows' dim label runs, found the identical way
/// [`NEW_PANE_LABEL`] is, and for the same reason.
const ADD_SPACE_LABEL: &str = "new space";
const ADD_TAB_LABEL: &str = "new tab";

/// The second tab's pane title (`docs/terminal.html`'s `TABS[1]`) — what the
/// card prints once a pane row inside it has been committed.
const MINEFIT_TITLE: &str = "ed@minefit: ~/api";

/// The sheet's three section labels, transcribed from `presentation::pages`
/// (private there). The third names the tab whose panes it is listing, which
/// is the **browsed** tab — the sheet's own record of where the browse is.
const SECTION_SPACES: &str = "spaces";
const SECTION_TABS: &str = "tabs";
const SECTION_PANES: &str = "panes \u{2014} ";

/// `presentation::pieces`' own `--amber-faint` alpha, transcribed — the wash a
/// selected pill and the `focused` chip share (see [`focused_chip`]).
const AMBER_FAINT: f32 = 0.12;

/// A **herdr** session id: the one prefix that turns the sheet's spaces section on.
/// The sessions mock carries no session under this id, which is fine — only the
/// rename dialog's seed reads a session's name, and no test here does both.
const HERDR_SESSION_ID: &str = "herdr:work";

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

    /// [`Harness::mount`], first publishing `metrics` into context when `Some`
    /// — the shape `muxr`'s pages read for the responsive width-clamp pass.
    /// `None` leaves context unset, exactly the state every other test in this
    /// suite mounts under, so this is purely additive.
    fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Self {
        if let Some(metrics) = metrics {
            provide_context(metrics);
        }
        Self::mount()
    }

    /// [`mount`](Self::mount) attached to `session` instead of [`SESSION_ID`] —
    /// how the suite reaches a **herdr** session, whose id prefix is the one
    /// thing the sheet's spaces section is gated on.
    fn mount_session(session: &str) -> Self {
        Self::themed_session(Theme::glyph_baseline(), session)
    }

    /// [`mount`](Self::mount) under an explicit theme — how the suite drives
    /// `reduce_motion`.
    fn themed(theme: Theme) -> Self {
        Self::themed_session(theme, SESSION_ID)
    }

    /// [`themed`](Self::themed), attached to `session`.
    fn themed_session(theme: Theme, session: &str) -> Self {
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
        app_nav().push(&format!("/terminal?session={session}"));

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

    /// Wait for a sheet closed through
    /// [`SheetHandle::close_after`](crate::ui::sheet::SheetHandle::close_after)
    /// to actually pop — every call site here is `commit_pane`'s own delayed
    /// close, `CLOSE_DELAY` out.
    ///
    /// Two beats, on purpose rather than one real-time-tracked loop:
    /// [`pump_for`](Self::pump_for) rides out `close_after`'s own delay at the
    /// zero paint clock, so nothing here nudges a paint-clock-driven animation
    /// live in the same tree; then a small **fixed** clock advance carries the
    /// sheet's short staged exit (`durations.fast`, ~150ms) to completion.
    /// Fixed rather than driven by however long the first beat took in real
    /// time: this screen's staggered reveal restarts on the very same commit,
    /// and a clock racing ahead would bake in "already settled" before a caller
    /// reads it fresh. 300ms clears the exit with room to spare.
    fn pump_until_closed(&mut self, what: &str, done: impl Fn(&Harness) -> bool) {
        self.pump_for(2 * CLOSE_DELAY);
        self.frame_at(0.0);
        self.frame_at(300.0);
        self.frame_at(300.0);
        assert!(done(self), "timed out waiting for {what}");
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

    /// Tap the open sheet's `index`-th pane row, top to bottom — the sheet's
    /// one committing gesture (see [`pane_rows`]). Renders its own frame, so
    /// the rows are the ones the *current* browse lists.
    fn tap_pane_row(&mut self, index: usize) {
        let rows = pane_rows(&self.frame());
        let row = *rows
            .get(index)
            .unwrap_or_else(|| panic!("pane row {index}; the sheet lists {}", rows.len()));
        self.tap(center(row));
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

    /// The new-pane row's own tap point — see [`NEW_PANE_LABEL`] for why this
    /// goes through its accessible *value* rather than painted geometry.
    fn new_pane_row_point(&self) -> Point {
        node_centers_by_value(&self.root, NEW_PANE_LABEL)
            .first()
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "no node valued {NEW_PANE_LABEL:?}; values on screen: {:?}",
                    node_values(&self.root)
                )
            })
    }

    /// The add-space row's own tap point — the identical [`ADD_SPACE_LABEL`]
    /// value idiom [`new_pane_row_point`] uses, since it is a full-size row
    /// rather than one of [`pills`]'s own rects.
    fn add_space_row_point(&self) -> Point {
        node_centers_by_value(&self.root, ADD_SPACE_LABEL)
            .first()
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "no node valued {ADD_SPACE_LABEL:?}; values on screen: {:?}",
                    node_values(&self.root)
                )
            })
    }

    /// …and the add-tab row's, the identical shape.
    fn add_tab_row_point(&self) -> Point {
        node_centers_by_value(&self.root, ADD_TAB_LABEL)
            .first()
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "no node valued {ADD_TAB_LABEL:?}; values on screen: {:?}",
                    node_values(&self.root)
                )
            })
    }

    /// A full long-press at `point` — [`support::long_press`]'s sized-surface
    /// twin: that helper hardcodes the unsized default surface, and this
    /// suite's [`SURFACE`] is taller.
    fn long_press(&mut self, point: Point) {
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Down, point));
        self.frame_at(0.0);
        self.frame_at(support::HELD_MS);
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Up, point));
    }

    /// Pump until a toast reading exactly `message` is pending — how this suite
    /// observes an async repository call's failure landing on the failure sink,
    /// rather than guessing how many frames that takes.
    fn await_toast(&mut self, message: &str) {
        self.pump_until(&format!("a {message:?} toast"), |_| {
            app_toasts()
                .pending_untracked()
                .iter()
                .any(|m| m == message)
        });
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
    /// **and** for layout to pick the settled value up. Three frames, because
    /// the card's geometry is a *layout* fact: paint advances the driver, and
    /// the next layout turns the new progress into a box. The first frame only
    /// seeds the driver's clock — an `AnimationController`'s first `advance`
    /// after `forward()` is a zero delta by construction.
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
    /// page, so a second, unwanted pop against the bare harness is silently
    /// absorbed and [`Harness::depth`] never moves. The app's real stack never
    /// looks like that — `/terminal` is always under the sheet, and *that* page
    /// coming off is exactly the defect the races below exist for.
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

    /// Pump real frames for at least `span` of wall clock. The delayed close
    /// runs on the executor's own timer, so "the late close has had every
    /// chance to fire" is a wall-clock fact rather than a frame count.
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

/// The card's status row — the band `GlyphInk::terminal_head` fills, at the
/// top of the card — as `(origin, size)`.
fn status_band(scene: &RecScene) -> (Point, Size) {
    let ink = Theme::glyph_baseline()
        .extension::<frust::GlyphInk>()
        .copied()
        .expect("the Glyph baseline carries GlyphInk");
    let bands = rounded_in(scene, ink.terminal_head);
    let bands: Vec<(Point, Size)> = bands
        .into_iter()
        .map(|(origin, size, _)| (origin, size))
        .collect();
    largest(&bands).expect("the status row paints its own band")
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

/// The sheet's pane rows, top to bottom — the `--bg-base` tiles
/// (`presentation::pieces`' own `Palette::base`, the scheme's
/// `surface_container_low`), which nothing else on this screen paints.
fn pane_rows(scene: &RecScene) -> Vec<(Point, Size)> {
    let base = Theme::glyph_baseline().scheme().surface_container_low;
    let mut rows: Vec<(Point, Size)> = rounded_in(scene, base)
        .into_iter()
        .map(|(origin, size, _)| (origin, size))
        .collect();
    rows.sort_by(|a, b| a.0.y.total_cmp(&b.0.y));
    rows
}

/// Which pane row wears the focused mark — its amber mark box and its `focused`
/// chip — or `None` when no row does. A selected pill shares that same amber-faint
/// wash, so the row is found by *containment*: the chip and the mark box are the
/// only amber-faint tiles that sit inside a [`pane_rows`] rect.
fn focused_row(scene: &RecScene) -> Option<usize> {
    let theme = Theme::glyph_baseline();
    let faint = theme.scheme().primary.with_alpha(AMBER_FAINT);
    let rows = pane_rows(scene);
    rounded_in(scene, faint)
        .into_iter()
        .find_map(|(origin, size, _)| {
            let point = center((origin, size));
            rows.iter().position(|row| inside(*row, point))
        })
}

/// Whether `point` sits inside `rect`.
fn inside((origin, size): (Point, Size), point: Point) -> bool {
    point.x >= origin.x
        && point.x <= origin.x + size.width
        && point.y >= origin.y
        && point.y <= origin.y + size.height
}

/// The [`EDIT_LABEL`]-labelled icon whose center falls inside `rect` — a chip's
/// trailing edit icon, located by *containment* rather than a fixed index into
/// every "Rename" node on the sheet (there is one per renamable chip).
fn edit_icon_in(root: &RenderRoot<AppState, AnyView<AppState>>, rect: (Point, Size)) -> Point {
    node_centers_by_label(root, EDIT_LABEL)
        .into_iter()
        .find(|point| inside(rect, *point))
        .unwrap_or_else(|| panic!("no {EDIT_LABEL:?} icon inside {rect:?}"))
}

/// A point inside `rect`'s own leading edge — reliably inside a chip's *main* tap
/// region regardless of label width: `pieces::pill`'s icon sits at the trailing
/// edge, so `rect`'s own [`center`] can land on it for a short enough label.
fn leading_point((origin, size): (Point, Size)) -> Point {
    Point::new(origin.x + 8.0, origin.y + size.height / 2.0)
}

/// Focus a just-opened [`name_input_dialog`](muxr::ui::dialogs::name_input_dialog)'s
/// one field, diffed against `under` (the frame painted just before it opened): a
/// field with no recorded focus routes neither a keystroke nor the submitting
/// Enter.
fn tap_dialog_field(h: &mut Harness, under: &RecScene) {
    let dialog_frame = h.settle();
    let dialog = added(&under.rounded, &dialog_frame.rounded);
    let content_w = panel(&dialog).1.width - 2.0 * PANEL_PADDING;
    let field = by_width(&dialog, content_w).expect("the name field");
    h.tap(center(field));
}

/// The sheet's pill rects, in reading order (top to bottom, then left to right).
/// Every pill is the same height — one 11px run inside 8px of vertical padding —
/// and there are nine of them, so the **most common height** among the sheet's own
/// rects is the pills' and nothing else's. Picking them that way keeps the locator
/// independent of the type role's line box, a theme number rather than a mock one.
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

    // The button's *visual* is a 28px circle centred in the 40px row, so a thumb
    // landing 2px below the card's top edge misses the circle — and must still
    // fire the toggle, or the shorter row would have cost the press target slack.
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
    // its gutter but has not reached the edge. Two frames seed and advance the
    // driver, and the *third* is the one whose layout turns that into a box.
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
    // `spatial` curve overshoots past `1.0`. Easing the *interpolated position*
    // pins the card flush against the edge for the whole first half; easing the
    // *driver's progress* (the fix) already has it visibly off the edge by here.
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

    // Interrupt with a collapse. The bug this guards: seeding the new stage's
    // `from` with the *painted* (already-eased) value and running it through the
    // curve again lands in the overshoot band and snaps the card straight back.
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
fn the_menus_first_row_opens_the_sheet_on_the_tab_row_and_the_browsed_tabs_panes() {
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
    // The panel is a *path*, not a rounded rect: only its top corners are
    // rounded, which `fill_rounded_rect` cannot express.
    let (origin, size) = panel(&added(&before.paths, &after.paths));
    assert!(
        (origin.y + size.height - SURFACE.height).abs() < EPSILON,
        "bottom-anchored, like every muxr sheet: {origin:?}/{size:?}"
    );
    // Four tabs, and **no** spaces row at all: this session is zellij, which has
    // no spaces. The add-tab row is not one of these — it is a full-size row of
    // its own, not a `.pill`-shaped one `pills` would pick up.
    assert_eq!(
        pills(&sheet).len(),
        4,
        "the tab row and nothing above it: {sheet:?}"
    );
    assert!(!h.shows(SECTION_SPACES), "no label, and no gap under one");
    assert!(h.shows(SECTION_TABS));
    // The sheet opens browsing the tab the session is on, and lists *its*
    // panes — both of the mock's, not one hardcoded row.
    assert!(h.shows(&format!("{SECTION_PANES}devbox")));
    assert_eq!(pane_rows(&after).len(), 2, "every pane of the browsed tab");
    // The pane rows' mark boxes, and the dashed new-pane row (which paints
    // no fill at all, only strokes).
    assert!(
        !added(&before.strokes, &after.strokes).is_empty(),
        "the dashed rows and hairlines"
    );
}

#[test]
fn a_herdr_session_is_the_only_one_offered_spaces() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);

    assert!(
        h.shows(SECTION_SPACES),
        "herdr has spaces, so the sheet does"
    );
    // Three spaces, then four tabs — the two add rows paint full-size and
    // dashed, so `pills` (a `.pill`-shaped-rect scan) no longer picks either up.
    assert_eq!(
        pills(&sheet).len(),
        3 + 4,
        "every pill row, the two full-size add rows excluded: {sheet:?}"
    );
}

#[test]
fn a_tab_pill_browses_locally_and_leaves_the_card_and_the_sheet_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);
    // Reading order: the four tab pills, then `+`. `1 btop` is the first.
    h.tap(center(pills(&sheet)[0]));
    h.settle();

    // The pane section re-sliced to the browsed tab, out of the tab world the
    // one load already carried…
    assert!(h.shows(&format!("{SECTION_PANES}btop")));
    assert_eq!(pane_rows(&h.frame()).len(), 2);
    // …while the card stayed exactly where the session is…
    assert!(
        h.shows(DEVBOX_TITLE),
        "the card is the session's, not the browse's"
    );
    assert!(!h.shows(BTOP_TITLE));
    // …and the sheet never closed, however long the close beat is given.
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

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);
    h.tap(center(pills(&sheet)[0]));
    h.settle();
    // The browsed tab's first pane — the one commit path there is.
    h.tap_pane_row(0);

    // The commit is async (a tab move, then a pane focus), and the sheet
    // closes a beat after it lands.
    h.pump_until_values("the btop tab", |values| {
        values.iter().any(|v| v == BTOP_TITLE)
    });
    assert!(h.shows(BTOP_TITLE), "the card followed the pane");
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);
    assert_eq!(h.depth(), REST_DEPTH, "…and the sheet closed behind it");
}

#[test]
fn the_focused_badge_follows_a_commit_the_next_time_the_sheet_opens() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (_, opened) = h.open_sheet();
    assert_eq!(
        focused_row(&opened),
        Some(0),
        "the mock seeds the first pane focused"
    );

    // Commit the *second* pane of the tab the session is already on — no tab
    // move, just the focus.
    h.tap_pane_row(1);
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);

    let (_, reopened) = h.open_sheet();
    assert_eq!(
        focused_row(&reopened),
        Some(1),
        "the badge moved with the commit, without waiting for a reload"
    );
}

#[test]
fn a_browsed_tab_shows_no_focused_badge_at_all() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);
    assert_eq!(
        focused_row(&after),
        Some(0),
        "the session's own tab has one"
    );

    h.tap(center(pills(&sheet)[0]));
    let browsed = h.settle();

    // The browsed tab's panes carry whatever the last load recorded, and the
    // session is in none of them — so no row claims the badge.
    assert_eq!(
        focused_row(&browsed),
        None,
        "a stale focus flag must not read as where the session is"
    );
}

#[test]
fn a_pane_commit_replays_the_reveal_from_the_top() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // Settle the first reveal.
    let settled = runs_at(&mut h, 2_000.0);

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);
    h.tap(center(pills(&sheet)[0]));
    h.settle();
    h.tap_pane_row(0);
    h.pump_until_values("the btop tab", |values| {
        values.iter().any(|v| v == BTOP_TITLE)
    });
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);

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

// 6b — the sheet closes once, whatever happens during the close beat.
//
// A pane row leaves the sheet up for `CLOSE_DELAY` so the selection visibly
// moves first, and the sheet stays fully interactive for that whole beat. Every
// test here fires a *second* dismissal inside it and asserts the same two
// things: the sheet closes once, and the page under it is still there
// afterwards. Before `ui::sheet`'s dismissal gate, each of these popped it.

#[test]
fn a_second_pane_row_inside_the_close_beat_still_closes_the_sheet_only_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let floor = h.push_route_floor();

    let (before, after) = h.open_sheet();
    assert_eq!(h.depth(), floor + 1, "the sheet is up over the route");
    let sheet = added(&before.rounded, &after.rounded);
    // Browse `1 btop`, commit its first pane, then — inside the same close
    // beat — browse `2 minefit-api` and commit its first pane too.
    h.tap(center(pills(&sheet)[0]));
    h.settle();
    h.tap_pane_row(0);
    h.tap(center(pills(&sheet)[1]));
    h.settle();
    h.tap_pane_row(0);

    // Latest tap wins: the second commit is the one that lands…
    h.pump_until_values("the minefit tab", |values| {
        values.iter().any(|v| v == MINEFIT_TITLE)
    });
    // …one close fires (`<=`, not `==`: two stacked closes can land in the
    // same rebuild, and this test must report *that* rather than time out)…
    h.pump_until_closed("the sheet to close", |h| h.depth() <= floor);
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

    let (_before, after) = h.open_sheet();
    h.tap(center(pane_rows(&after)[0]));
    // The sheet is still up — and still dismissible — for the whole beat.
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
    h.tap(center(pane_rows(&after)[0]));
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

    // Detach is async and the menu can be reopened while it is in flight — two
    // landings would pop `/sessions` off the stack as well as `/terminal`.
    // Neither `choose_menu_row` nor `settle` pumps the task queue, so both
    // requests are made before either can land.
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
// 7b — a read-only credential
// ---------------------------------------------------------------------------

/// Flipping `muxr::connection::ConnectionController::is_read_only` directly is how
/// a suite reaches this state: the mock backend always answers
/// `is_read_only: false`, so no mock connect could produce one — the same seam
/// `tests/sessions.rs` and `tests/settings.rs` already use.
fn make_read_only(h: &mut Harness) {
    expect_connection_controller().is_read_only.set(true);
    h.frame();
}

#[test]
fn a_read_only_credential_withdraws_the_sheets_three_create_affordances() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    make_read_only(&mut h);

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);

    // Three spaces and four tabs, and neither `+ space` nor `+` beside them.
    assert_eq!(
        pills(&sheet).len(),
        3 + 4,
        "a read-only session is offered no create: {sheet:?}"
    );
    // The pills stay tappable — browsing a tab reads what is already loaded,
    // and switching a space is a per-connection *view* change.
    h.tap(center(pills(&sheet)[3]));
    h.settle();
    assert!(h.shows(&format!("{SECTION_PANES}btop")), "the browse moved");
}

#[test]
fn a_read_only_credential_refuses_a_commit_that_would_move_tabs() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();
    make_read_only(&mut h);

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);

    // Committing inside the tab the session is on is `FocusPane` alone, which
    // the wire allows a read-only token — so it lands, and the sheet closes.
    h.tap(center(pane_rows(&after)[1]));
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);
    assert!(
        !app_toasts()
            .pending_untracked()
            .iter()
            .any(|message| message == READ_ONLY_TOAST),
        "a same-tab commit is not a mutation: {:?}",
        app_toasts().pending_untracked()
    );

    // A commit that has to move tabs first is `GoToTab`, which is MUTATING —
    // so the row stays on screen and says why, and nothing moves.
    h.open_sheet();
    h.tap(center(pills(&sheet)[0]));
    h.settle();
    h.tap_pane_row(0);
    h.pump_for(2 * CLOSE_DELAY);

    assert_eq!(h.depth(), REST_DEPTH + 1, "the sheet is still up");
    assert!(h.shows(DEVBOX_TITLE), "…and the card never moved");
    assert!(
        app_toasts()
            .pending_untracked()
            .iter()
            .any(|message| message == READ_ONLY_TOAST),
        "the row says why: {:?}",
        app_toasts().pending_untracked()
    );
}

#[test]
fn a_read_only_credential_refuses_the_rename_dialog_and_says_why() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();
    make_read_only(&mut h);

    h.open_menu();
    h.choose_menu_row(ROW_RENAME);
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
    // `?session=s-1`. So delete one character and submit.
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

// 10 — responsive width-clamp pass.

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
    assert!(
        (origin.y - (BAR_HEIGHT + PORTRAIT_TOP_GAP)).abs() < EPSILON,
        "the bar clearance plus the same step-up on top, unchanged: {origin:?}"
    );
}

#[test]
fn fullscreen_stays_edge_to_edge_even_with_wide_metrics() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_with_metrics(Some(metrics_at(SURFACE)));

    h.tap_label(EXPAND);
    // Mid-flight first: the wide branch's extra padding changes value on the
    // toggle, and it must do so *without* the stage being rebuilt under the
    // transition, or the card would arrive by snapping.
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

#[test]
fn narrow_metrics_lift_the_card_off_the_bar_by_the_portrait_gap() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_with_metrics(Some(metrics_at(Size::new(400.0, SURFACE.height))));

    // Portrait used to sit flush under the floating bar while landscape read
    // as spaced — the gap is the *top* inset only, so the sides stay at the
    // narrow window's own `WRAP_PAD` (asserted above).
    let (origin, _, _) = card(&h.frame());
    assert!(
        (origin.y - (BAR_HEIGHT + PORTRAIT_TOP_GAP)).abs() < EPSILON,
        "the card clears the bar and then breathes: {origin:?}"
    );

    // …and fullscreen still takes neither gap: its own transition owns that
    // geometry, top edge included.
    h.tap_label(EXPAND);
    let (origin, _, _) = card(&h.settle_motion(1_000.0));
    assert!(
        origin.y.abs() < EPSILON,
        "fullscreen keeps the card flush against the top edge: {origin:?}"
    );
}

// 11 — the sheet's three create affordances.
//
// Request shapes are already pinned at the repository/mock-server level, and the
// happy-path reload/select-by-label flow at the controller level —
// `MockTerminalRepository` deliberately leaves these three verbs at
// `TerminalRepository`'s shared "unsupported" default — so what is left to pin
// here is *flow*: both the space and tab rows open a dialog, the space one keeps
// its non-blank guard while the tab one deliberately does not, and the sheet
// stays up around all three regardless of how the repository answers.

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

    // Prefilled empty: an unmodified Enter submits nothing at all (the
    // dialog's own non-blank guard) — a stray seed left over from the rename
    // dialog's own pattern would have submitted here instead.
    h.press(NamedKey::Enter);
    h.flush();
    assert_eq!(h.depth(), floor + 1, "nothing to submit yet — still open");

    // A name with punctuation the server, not this screen, gets to judge. Diffed
    // against `after` (the sheet, still up underneath) rather than `before` — the
    // sheet's own chrome must not leak into this isolation.
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
    // The repository actually saw the attempt — `MockTerminalRepository` rejects
    // it, which the withdrawn `unavailable()` stub could never have produced,
    // since it never touched a repository at all.
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

    // The field with no recorded focus routes neither a keystroke nor the
    // submitting Enter, so it is tapped — and, unlike every other name prompt in
    // this app, left untyped: a blank tab name is this create's own valid
    // default, so the unmodified Enter DOES submit here.
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

    // Escape is this app's own "cancelled" dismissal route, the same reading a
    // scrim tap or a back press gets — but only once the *dialog itself* holds
    // focus. A tap on its own field would claim focus there instead, and
    // `TextInput` handles `Escape` itself (a self-blur), consuming it before the
    // dialog sees it — so this taps the panel's own title corner, clear of the
    // field and both buttons.
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

#[test]
fn a_long_press_on_the_new_pane_row_also_opens_no_dialog_and_leaves_the_sheet_up() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    h.open_sheet();
    let floor = h.depth();
    let point = h.new_pane_row_point();

    h.long_press(point);
    h.settle();

    assert_eq!(h.depth(), floor, "no dialog on a long-press either");
    h.await_toast(INTERNAL_FAILURE_TOAST);
    assert_eq!(h.depth(), floor, "…and the sheet stayed up");
}

// 12 — every chip's trailing edit icon.
//
// Request shapes and the reload each verb settles with are pinned at the
// controller level; `MockTerminalRepository` leaves the three renames at the
// shared "unsupported" default — so what these pin is *flow* and, above all,
// that the icon's own tap region is disjoint from the chip's main one: tapping
// it opens a prefilled rename dialog and never also selects/browses/commits,
// while a plain tap on the rest of the same chip still does exactly that.

#[test]
fn a_space_pills_edit_icon_renames_it_without_selecting_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    app_toasts().clear();

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);
    let floor = h.depth();

    // "cupline" is the sheet's first space pill — and, per the seed, not the
    // selected one (`forgekit` is).
    let cupline = pills(&sheet)[0];
    h.tap(edit_icon_in(&h.root, cupline));
    tap_dialog_field(&mut h, &after);

    assert_eq!(h.depth(), floor + 1, "the rename dialog opened");
    assert!(h.shows("cupline"), "prefilled with the pill's current name");

    // The edit tap never reached the pill's own `on_tap`: submitting a change
    // reaches the repository (proving the dialog's flow is live) without
    // "cupline" ever having become selected.
    h.type_text("renamed");
    h.press(NamedKey::Enter);
    h.flush();
    assert_eq!(h.depth(), floor, "the submit closed the dialog");
    h.await_toast(INTERNAL_FAILURE_TOAST);
    // `await_toast` pumps the async task queue at a pinned paint clock, which is
    // not the same as settling the dialog's own pop transition — `settle()`'s
    // "required before tapping a pushed page" contract applies just as much to
    // the page a pop just re-exposed.
    h.settle();
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
        "the edit tap and its dialog never selected the pill they renamed"
    );

    // …while a plain tap on the same pill still selects it — the two regions are
    // disjoint, not one having swallowed the other's gesture. Its own leading
    // edge, not its center: a short label's pill can center over the trailing
    // icon, the exact overlap this rules out. `select_space` is async, so this
    // waits for it to land rather than reading right away.
    h.tap(leading_point(cupline));
    h.pump_until("cupline to become selected", |_| {
        debug_controller()
            .expect("mounted")
            .spaces
            .get_untracked()
            .iter()
            .any(|space| space.selected && space.name == "cupline")
    });
    let selected: Vec<String> = debug_controller()
        .expect("mounted")
        .spaces
        .get_untracked()
        .into_iter()
        .filter(|space| space.selected)
        .map(|space| space.name)
        .collect();
    assert_eq!(selected, ["cupline"], "…and a plain tap still selects");
}

#[test]
fn a_tab_pills_edit_icon_renames_it_without_browsing_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    let (before, after) = h.open_sheet();
    let sheet = added(&before.rounded, &after.rounded);
    let floor = h.depth();

    // "btop" is the sheet's first tab pill — the session opens browsing
    // `devbox` instead (see `DEVBOX_TITLE`).
    let btop = pills(&sheet)[0];
    h.tap(edit_icon_in(&h.root, btop));
    tap_dialog_field(&mut h, &after);

    assert_eq!(h.depth(), floor + 1, "the rename dialog opened");
    assert!(h.shows("btop"), "prefilled with the tab's current label");
    assert_eq!(
        debug_controller()
            .expect("mounted")
            .browsed_tab
            .get_untracked(),
        "devbox",
        "the edit tap never browsed to the tab it names"
    );

    h.type_text("renamed");
    h.press(NamedKey::Enter);
    h.flush();
    assert_eq!(h.depth(), floor, "the submit closed the dialog");
    h.await_toast(INTERNAL_FAILURE_TOAST);
    // See the space test's identical note: settle the pop's own transition
    // before treating the sheet underneath as live again.
    h.settle();
    assert_eq!(
        debug_controller()
            .expect("mounted")
            .browsed_tab
            .get_untracked(),
        "devbox",
        "…still never browsed, even once the repository answered"
    );

    // A plain tap on the same pill still browses it — its leading edge, for
    // the same reason the space test's own equivalent tap does.
    h.tap(leading_point(btop));
    h.settle();
    assert_eq!(
        debug_controller()
            .expect("mounted")
            .browsed_tab
            .get_untracked(),
        "btop",
        "a plain tap still browses"
    );
}

#[test]
fn a_pane_rows_edit_icon_renames_it_without_committing_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    let (_, opened) = h.open_sheet();
    let floor = h.depth();
    // The second pane row ("ed@devbox zsh") — the mock seeds the first one
    // focused, so committing the second is an observable move.
    let unfocused_row = pane_rows(&opened)[1];
    h.tap(edit_icon_in(&h.root, unfocused_row));
    tap_dialog_field(&mut h, &opened);

    assert_eq!(h.depth(), floor + 1, "the rename dialog opened");
    assert!(h.shows("ed@devbox zsh"), "prefilled with the pane's title");

    h.type_text("renamed");
    h.press(NamedKey::Enter);
    h.flush();
    assert_eq!(h.depth(), floor, "the submit closed the dialog");
    h.await_toast(INTERNAL_FAILURE_TOAST);
    // Give a commit's own close-beat every chance to fire before checking —
    // the edit tap must not have started one.
    h.pump_for(2 * CLOSE_DELAY);
    assert_eq!(
        h.depth(),
        floor,
        "the edit tap never committed the pane it names — the sheet is still up"
    );
    assert_eq!(
        focused_row(&h.frame()),
        Some(0),
        "…and the focused pane never moved"
    );

    // A plain tap on the same row still commits it.
    h.tap_pane_row(1);
    // `floor - 1`: depth is already `floor` (the sheet, still up) before this
    // tap — the wait has to watch for the sheet's own pop, not the level it is
    // already sitting at.
    h.pump_until_closed("the sheet to close", |h| h.depth() == floor - 1);
    assert_eq!(h.depth(), floor - 1, "a plain tap still commits and closes");
}

#[test]
fn a_read_only_credential_withdraws_every_chips_edit_icon() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    make_read_only(&mut h);

    h.open_sheet();
    assert!(
        node_centers_by_label(&h.root, EDIT_LABEL).is_empty(),
        "a read-only session offers no rename affordance anywhere on the sheet"
    );
}
