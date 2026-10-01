//! Headless behavior coverage for the `/terminal` screen (`src/features/terminal/**`) — no GPU,
//! no window: a `RenderRoot` over the real page builder.
//!
//! [`Harness`] mounts the screen as the **bottom layer of a `Stack`** with a navigator over it,
//! because `NavigatorWidget` contributes no `Widget::semantics`: a screen mounted *inside* one is
//! invisible to the accessibility tree. Two navigators, deliberately: the **mounted** one stands
//! in for the app's root overlay host the screen pushes overlays through (exercised end to end,
//! located by painted geometry); the **record-only** one carries the app's `Nav` handle, so
//! pushing `/terminal?session=zellij:dev` records that query in the route-meta mirror (the only way
//! this screen learns its session id) while the page op it enqueues is never drained. Without the
//! split, that push would build a second terminal screen inside the mounted navigator and the
//! two would fight over the published chrome.
//!
//! That mirror also makes two invisible facts assertable: `Nav::bar().fullscreen` is "the shell's
//! bar has been told to get out of the way", and `Nav::bar().meta` going `None` is "the terminal
//! route was popped".
//!
//! A menu row is reachable neither by semantics nor by a rect of its own, but its contract is
//! exact: a selection pops the menu page with `PopResult::of(index)`, so
//! [`Harness::choose_menu_row`] pops with that result, running the very `on_result` the widget
//! would have. The card's reveal, caret, and fullscreen transition advance off
//! `PaintCtx::frame_time`, so they are **stepped** with [`support::frame_at`] rather than waited on.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, GetUntracked, NavigatorController, PopResult, Set, SizedBox, Stack, Theme,
    WindowMetrics, any, navigator, provide_context,
};
use frust_core::{FrameTime, NamedKey, PointerPhase, RenderRoot, TOUCH_SLOP};
use frust_text::TextContext;
use kurbo::{Point, Size};
use peniko::Color;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::sessions::data::repositories::mock_sessions_repository::SEEDED_DEV_ID;
use muxr::features::sessions::di::expect_sessions_repository;
use muxr::features::terminal::data::repositories::mock_terminal_repository::{
    clear_verb_log_for_tests, verb_log_for_tests,
};
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
const SESSION_ID: &str = SEEDED_DEV_ID;
const SESSION_NAME: &str = "dev";

/// `docs/terminal.html`'s `TABS`, as the screen must print them.
const DEVBOX_TITLE: &str = "ed@devbox: ~";
const DEVBOX_FIRST_LINE: &str = "ed@devbox:~$ systemctl status muxrd";
const BTOP_TITLE: &str = "ed@precision: ~";
/// How many lines each tab carries (and so how many the reveal staggers).
const LINES_PER_TAB: usize = 4;

/// The expand button's two accessible names, transcribed from
/// `presentation::widgets::status` (the implementation's constants; a test that
/// imported them could not catch one of them changing).
const EXPAND: &str = "Fullscreen";
const COLLAPSE: &str = "Exit fullscreen";
/// …and the sheet's close button's.
const CLOSE: &str = "Close";

/// `.term-wrap{padding:14px}` and `.term-card{border-radius:22px}`,
/// transcribed — the two numbers fullscreen animates to zero.
const WRAP_PAD: f64 = 14.0;
const CARD_RADIUS: f64 = 22.0;
/// Wrap padding's wide-window target past `crate::responsive::WIDE_BREAKPOINT_PX` (transcribed,
/// private to `presentation::pages`) — the card stays full-bleed at any width; only the space
/// around it grows from [`WRAP_PAD`]'s 14px to this.
const WIDE_WRAP_PAD: f64 = 22.0;
/// The narrow/portrait window's **top-only** bar-to-card gap (transcribed, private) — the same
/// step-up the wide branch already applied on all four sides.
const PORTRAIT_TOP_GAP: f64 = WIDE_WRAP_PAD - WRAP_PAD;
/// `presentation::widgets::status`'s status-row height (transcribed): the 28px expand button
/// plus its tightened 6px padding either side.
const STATUS_ROW_H: f64 = 40.0;
/// `.cur{width:7px;height:13px}` — the caret's own box, which is how a frame
/// is asked whether the caret is lit.
const CARET: Size = Size::new(7.0, 13.0);

/// The mounted navigator's depth with no overlay up: its own (empty, full-bleed)
/// initial page.
const REST_DEPTH: usize = 1;

/// `presentation::pages`'s `SHEET_CLOSE_DELAY` (transcribed, private): how long a tab pill
/// leaves the sheet up so the selection visibly moves first.
const CLOSE_DELAY: Duration = Duration::from_millis(260);

/// What a landed detach says (transcribed from `presentation::pages`).
const DETACHED_TOAST: &str = "detached";

/// What a control a read-only credential may not use says instead, same provenance.
const READ_ONLY_TOAST: &str = "read-only session";

/// `AppFailure::Internal`'s `user_message()` (transcribed) — what all three of the sheet's create
/// affordances show once the repository answers, since `MockTerminalRepository` leaves
/// `create_space`/`new_tab`/`new_pane` at `TerminalRepository`'s "unsupported" default. Seeing
/// this toast is this suite's proof an affordance actually reached the repository.
const INTERNAL_FAILURE_TOAST: &str = "Something went wrong — please try again.";

/// `presentation::widgets::side_sheet_tree`'s `ADD_PANE_LABEL` (private) — the
/// tree's add-pane row label, and the only way this suite locates it: the row
/// paints a **transparent**-filled, dashed-bordered box (shows up in neither
/// [`RecScene::rounded`] nor [`RecScene::strokes`]), found by its accessible
/// *value* instead. Unlike the flat sheet's row it replaced, this one carries
/// no long-press affordance (`presentation::pages::Actions::new_pane`'s own
/// doc records the gap).
const NEW_PANE_LABEL: &str = "new pane";

/// `side_sheet_tree`'s `ADD_SPACE_LABEL`/`ADD_TAB_LABEL` (private) — the tree's
/// two create rows' dim label runs, found the same way [`NEW_PANE_LABEL`] is.
const ADD_SPACE_LABEL: &str = "new space";
const ADD_TAB_LABEL: &str = "new tab";
/// `new_tab_dialog`'s own confirm label (private there). A button announces its
/// text as an accessible **name**, not as a run, so this one is tapped through
/// [`Harness::tap_label`] rather than the create rows' value lookup.
const NEW_TAB_SUBMIT_LABEL: &str = "Create";

/// The second tab's pane title (`docs/terminal.html`'s `TABS[1]`) — what the
/// card prints once a pane row inside it has been committed.
const MINEFIT_TITLE: &str = "ed@minefit: ~/api";

/// `presentation::pages`'s `FOOTER_RENAME_LABEL`/`FOOTER_DETACH_LABEL`
/// (private there) — the side sheet's sticky footer's two icon-only
/// actions, the same session actions the overflow menu used to carry.
const FOOTER_RENAME_LABEL: &str = "Rename session";
const FOOTER_DETACH_LABEL: &str = "Detach session";
/// `presentation::pages`'s `RENAME_TITLE` — the dialog that footer row opens,
/// printed as a text **run**, unlike the icon row's own accessible label above.
const RENAME_DIALOG_TITLE: &str = "Rename session";

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
        Self::themed(frust_glyph::baseline())
    }

    /// [`Harness::mount`], first publishing `metrics` into context when `Some`
    /// — the shape `muxr`'s pages read for the responsive width-clamp pass.
    /// `None` leaves context unset, exactly the state every other test in this
    /// suite mounts under, so this is purely additive.
    fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Self {
        support::mount_with_metrics(metrics, Self::mount)
    }

    /// [`mount`](Self::mount) attached to `session` instead of [`SESSION_ID`] —
    /// how the suite reaches a **herdr** session, whose id prefix is the one
    /// thing the sheet's spaces section is gated on.
    fn mount_session(session: &str) -> Self {
        Self::themed_session(frust_glyph::baseline(), session)
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

    /// The tree's add-pane row tap point — see [`NEW_PANE_LABEL`] for why this
    /// goes through its accessible *value* rather than painted geometry.
    ///
    /// **The last match, not the first**: every tab carries its own add-pane
    /// row under the same label (`side_sheet_tree`'s `tab_group` builds one
    /// per tab, collapsed ones included), so this suite's own session — whose
    /// active, pre-expanded tab (`devbox`) is seeded *last* — needs the last
    /// one, the only row genuinely on screen.
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

    /// The tree's add-space row tap point — the identical [`ADD_SPACE_LABEL`]
    /// value idiom [`new_pane_row_point`] uses, since it is a full-size row
    /// rather than one of the tree's own card/pill shapes.
    fn add_space_row_point(&self) -> Point {
        self.value_point(ADD_SPACE_LABEL)
    }

    /// …and the tree's add-tab row's, the identical shape.
    fn add_tab_row_point(&self) -> Point {
        self.value_point(ADD_TAB_LABEL)
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

    /// [`long_press`](Self::long_press)'s cancelled twin: a press held past the
    /// threshold, but dragged past [`TOUCH_SLOP`] first — `frust_widgets::gesture`'s
    /// own conformance suite (`hold_then_drag_then_up_fires_neither`) proves the
    /// primitive; this proves the tree's own composed row (a real dialog, a real
    /// toggle behind it) inherits the contract rather than routing around it.
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

    /// Open the "panes & tabs" side sheet, exactly as `crate::shell` forwards
    /// the app bar's menu-button press — directly, with no intervening
    /// dropdown. Returns the frames before and after so the sheet's own
    /// chrome can be isolated via `added`.
    fn open_sheet(&mut self) -> (RecScene, RecScene) {
        let before = self.frame();
        open_terminal_menu(&mut self.state);
        let after = self.settle();
        (before, after)
    }

    /// The first node valued `value`, panicking with what's on screen instead
    /// — the shared lookup [`new_pane_row_point`]/[`add_space_row_point`]/
    /// [`add_tab_row_point`] all build on.
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

    /// The first node labelled `label`, panicking with what's on screen
    /// instead — [`value_point`]'s label-side sibling, for an icon-only
    /// affordance (`crate::ui::topbar::icon_button`'s own `Role::Button`
    /// node) whose accessible name is a **label**, not a text run's
    /// **value**. The footer's rename/detach taps build on this.
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

    /// Tap the sheet's footer "Rename session" icon and settle the staged
    /// close it chains its dialog push through
    /// ([`crate::features::terminal::presentation::pages`]'s own doc on why
    /// that footer action closes via `close_then`).
    fn tap_footer_rename(&mut self) -> RecScene {
        let point = self.label_point(FOOTER_RENAME_LABEL);
        self.tap(point);
        self.settle()
    }

    /// Tap the sheet's footer "Detach session" icon and flush the sheet's own
    /// pop — the detach request itself is async and lands on its own pump
    /// (see [`the_menus_detach_row_pops_the_terminal_route`]'s doc).
    fn tap_footer_detach(&mut self) {
        let point = self.label_point(FOOTER_DETACH_LABEL);
        self.tap(point);
        self.flush();
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

/// The card's status row — the band `GlyphInk::terminal_head` fills, at the
/// top of the card — as `(origin, size)`.
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

/// `side_sheet_tree`'s own mid-level tab radius (private there, transcribed
/// the same way this suite already transcribes other private module
/// constants) — herdr's nested tab **rect**, never worn by a promoted zellij
/// tab (`side_sheet_tree`'s own tests carry the identical fact for the tree
/// body in isolation).
const TREE_TAB_RADIUS: f64 = 7.0;

/// Every rounded fill in `scene` painted at exactly `radius`, as `(origin,
/// size)` — the tree's own radius-per-level idiom
/// (`side_sheet_tree`'s test module), reused here at the integration layer.
fn rounded_fills_at(scene: &RecScene, radius: f64) -> Vec<(Point, Size)> {
    scene
        .rounded_fills
        .iter()
        .filter(|(_, _, r, _)| (*r - radius).abs() < EPSILON)
        .map(|(origin, size, _, _)| (*origin, *size))
        .collect()
}

/// The tree's pane rows in `scene`, top to bottom: every **non-transparent**
/// [`muxr::ui::paint::RADIUS_FULL`] rect **wide enough to be a row** — the
/// two filters together exclude both the pill-shaped mark box/focused dot
/// each row also carries at the same radius (narrower) and the add-pane row
/// (transparent-filled, but stretched to the same width by the tree's own
/// column: `Container::paint` still calls `fill_rounded_rect_radii` for a
/// `Some(Color::TRANSPARENT)` fill, so it paints at this radius too).
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

/// Which of `scene`'s [`tree_pane_rows`] is focused, or `None` — a focused
/// row's own fill is the theme's `amber_faint` wash (`side_sheet_tree`'s
/// `pane_row`: the *whole* row tints, not a contained mark), so this is the
/// tree's own counterpart of the flat sheet's now-removed `focused_row`.
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

/// A theme with `reduce_motion` forced on.
fn reduced() -> Theme {
    let mut theme = frust_glyph::baseline();
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

// ---------------------------------------------------------------------------
// 6 — the "panes & tabs" side sheet
// ---------------------------------------------------------------------------

#[test]
fn opening_the_sheet_shows_the_tree_pre_expanded_on_the_sessions_own_tab() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let (_before, after) = h.open_sheet();
    assert_eq!(h.depth(), REST_DEPTH + 1, "the sheet is a pushed page");

    // This session is zellij, which has no spaces at all — its tabs promote
    // to the card shape, and nothing wears the mid-level tab rect
    // (`side_sheet_tree`'s own test carries the same fact for the tree body
    // in isolation).
    assert!(
        rounded_fills_at(&after, TREE_TAB_RADIUS).is_empty(),
        "a zellij session's tabs never wear the nested tab rect"
    );

    // Opening seeds the tree's resting expansion on the session's own tab, so
    // both of its panes print without a tap.
    assert!(h.shows("ed@devbox zsh"), "devbox's second pane is visible");
}

#[test]
fn a_herdr_session_is_the_only_one_offered_spaces() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    let (_before, after) = h.open_sheet();

    // herdr nests tabs a level in, under the space card — the fact this
    // session (unlike the zellij one) wears the mid-level tab rect at all.
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
    // The active (devbox) tab opens pre-expanded; tapping its own header
    // collapses it rather than browsing anything, so this taps the row for a
    // *different*, still-collapsed tab instead — found by its own title.
    let point = h.value_point("btop");
    h.tap(point);
    h.settle();

    // Expanding a tab is a browse: it reveals that tab's own panes, out of
    // the tab world the one load already carried…
    assert!(h.shows("ed@precision zsh"), "btop's panes are now visible");
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

    h.open_sheet();
    // Browse to btop — its own collapsed header expands it (and, by the
    // tree's own accordion, collapses devbox's).
    h.tap(h.value_point("btop"));
    let browsed = h.settle();
    // …then commit its first pane — the tree's one committing gesture.
    h.tap(center(tree_pane_rows(&browsed)[0]));

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
fn a_landed_herdr_commit_closes_the_sheet_on_the_documented_beat() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    let (_before, after) = h.open_sheet();
    // The second pane of the tab the session is on: `FocusPane` alone, which on
    // herdr is the whole move (the server re-points this connection's relay).
    h.tap(center(tree_pane_rows(&after)[1]));
    assert_eq!(
        h.depth(),
        REST_DEPTH + 1,
        "the beat starts with the sheet still up"
    );

    // `pump_until_closed` rides out exactly two `CLOSE_DELAY` beats, so a
    // commit that waited on anything behind the verb would time out here.
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

    // Commit the *second* pane of the tab the session is already on — no tab
    // move, just the focus.
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

    // Settle the first reveal.
    let settled = runs_at(&mut h, 2_000.0);

    h.open_sheet();
    h.tap(h.value_point("btop"));
    let browsed = h.settle();
    h.tap(center(tree_pane_rows(&browsed)[0]));
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

// The sheet closes once, whatever happens during the close beat. A pane row leaves it up for
// `CLOSE_DELAY` so the selection visibly moves first, staying fully interactive that whole beat.
// Every test below fires a *second* dismissal inside it and asserts the sheet closes once and the
// page under it survives — before `ui::sheet`'s dismissal gate, each of these popped it too.

#[test]
fn a_second_pane_row_inside_the_close_beat_still_closes_the_sheet_only_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let floor = h.push_route_floor();

    h.open_sheet();
    assert_eq!(h.depth(), floor + 1, "the sheet is up over the route");
    // Browse `btop`, commit its first pane, then — inside the same close beat — browse
    // `minefit-api` and commit its first pane too.
    h.tap(h.value_point("btop"));
    let scene = h.settle();
    h.tap(center(tree_pane_rows(&scene)[0]));
    h.tap(h.value_point("minefit-api"));
    let scene = h.settle();
    h.tap(center(tree_pane_rows(&scene)[0]));

    // The commit is single-flight, so the second row is *dropped* rather than raced against the
    // first — the first selection is the one that lands…
    h.pump_until_values("the btop tab", |values| {
        values.iter().any(|v| v == BTOP_TITLE)
    });
    // …one close fires (`<=`, not `==`: two stacked closes can land in the same rebuild, and this
    // test must report *that* rather than time out)…
    h.pump_until_closed("the sheet to close", |h| h.depth() <= floor);
    // …and the dropped row, given every chance to wake, lands on nothing.
    h.pump_for(2 * CLOSE_DELAY);

    assert_eq!(
        h.depth(),
        floor,
        "no second close popped the route under the sheet"
    );
    assert!(
        h.shows(BTOP_TITLE) && !h.shows(MINEFIT_TITLE),
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

// ---------------------------------------------------------------------------
// 6b — browsing a non-selected space, and committing across one
// ---------------------------------------------------------------------------

#[test]
fn browsing_a_non_selected_space_lists_its_tabs_without_switching_the_connection() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);

    h.open_sheet();
    // "zelli_mobile" is a space other than the one this connection is on
    // ("forgekit") — expanding it is a read-only peek
    // (`TerminalController::expand_space`'s own doc: "never switch_space").
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
        "cupline is collapsed and unread before the tap"
    );

    // "cupline" is a herdr space seeded with no tabs at all.
    h.tap(h.value_point("cupline"));
    h.pump_until("cupline to be read", |h| h.shows("no tabs yet"));
}

#[test]
fn the_add_tab_row_under_an_empty_browsed_space_switches_onto_it_first() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    app_toasts().clear();

    h.open_sheet();
    let floor = h.depth();

    // "cupline" is seeded with no tabs at all, so it draws no pane row to
    // commit through: its add-tab row is the only way onto the space, and the
    // switch that row performs is what makes an empty one reachable.
    h.tap(h.value_point("cupline"));
    h.pump_until("cupline to be read", |h| h.shows("no tabs yet"));
    // Runs the collapsing/expanding reveals forward, so the row tapped below is
    // where it looks (the same pinned-clock hazard the cross-space commit hits).
    h.settle_motion(0.0);

    // The first add-tab row in tree order is cupline's own — the seeded space
    // list draws it first, and the two behind it are collapsed.
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
        ["cupline"],
        "the create row switched onto the space it sits under, so the tab it \
         is about to name lands there rather than in the space left behind"
    );

    // …and the dialog a spawned switch pushed is a live one: confirming it
    // reaches the repository (which refuses `new_tab`, hence the toast).
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
    // The peek is still in flight and the auto-drill behind it still sleeping
    // when the screen goes away — both resume against signals whose owner is
    // gone. The assertion is the pump itself: a plain read there panics, and
    // the panic surfaces on this thread's own frame pump.
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

    // Browse "zelli_mobile" — its own active tab ("logs") auto-drills open
    // once its own cascade settles. Waited on through the controller's own
    // signal (not painted geometry): the space this connection was already
    // on ("forgekit") has its own settled reveal to collapse first, and a
    // paint pinned at `FrameTime::ZERO` (every `pump_frames` call) never
    // advances that tween — so its stale geometry would otherwise still be
    // sitting exactly where a fresher browse's own rows are about to land.
    h.tap(h.value_point("zelli_mobile"));
    h.pump_until("zelli_mobile's own tab to auto-expand", |_| {
        debug_controller()
            .expect("mounted")
            .expanded_tab
            .get_untracked()
            .as_deref()
            == Some("logs")
    });
    // Runs the collapsing/expanding reveals forward for real, so this frame
    // paints the settled result rather than forgekit's own stale geometry
    // (its own reveal needs elapsed paint-clock time to visually retract,
    // which `pump_until`'s pinned-zero clock never advances).
    let scene = h.settle_motion(0.0);

    // Commit the tab's own (already-focused) pane — the browse already
    // opened this exact tab, so the commit exercises `switch_space` then
    // `focus_pane` without also needing a `go_to_tab` hop (that step, and the
    // full three-call order, are pinned at the controller level over a
    // repository this suite's own mock carries no matching tab-lines fixture
    // for this space's own tabs).
    h.tap(center(tree_pane_rows(&scene)[0]));

    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);
    assert_eq!(
        h.depth(),
        REST_DEPTH,
        "one close, and no double pop of the route under it"
    );

    // Three writes prove the `switch_space` landed and was published: the tab
    // world is now zelli_mobile's own (promoted from the browse's cache rather
    // than re-read), and the space list's own `selected` flag moved with it —
    // the flag every later sheet open seeds its card from.
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

    // One pump starts the commit and parks it in its own `switch_space` round
    // trip; the screen then goes away underneath it. Everything past that await
    // — the tab resolve, the optimistic badge — resumes against signals whose
    // owner is gone. The assertion is the pump itself: a plain read there
    // panics, and the panic surfaces on this thread's own frame pump.
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

    // Browse a space other than the one this connection is on — its own tab
    // count caches under `TerminalController::browsed_space_tabs`.
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

    // Reopening seeds fresh: nothing survives from the stale browse to leak
    // onto a space row nobody has re-expanded yet.
    h.open_sheet();
    assert!(
        !h.shows("2 tabs"),
        "the dismissal reset the browse cache: {:?}",
        h.values()
    );
}

// ---------------------------------------------------------------------------
// 7 — rename and detach
// ---------------------------------------------------------------------------

#[test]
fn the_footers_rename_row_round_trips_into_the_sessions_repository() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    let before = h.frame();
    h.open_sheet();
    let after = h.tap_footer_rename();
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
fn the_footers_detach_row_pops_the_terminal_route() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    app_toasts().clear();

    // No navigator-derived signal reaches this route's "/terminal is on top"
    // fact in this harness: `Nav::bar().meta` is derived from the *inner*
    // navigator's live `RouteObserver`, which `crate::shell::shell_page`
    // attaches only once the shell page is actually **built** — and
    // `NavigatorController::depth()` reads the same drained, committed stack
    // (`crates/frust-widgets`' nav module: "a queued-but-undrained op is not
    // in it"). This harness never builds either navigator (the module docs'
    // record-only one), so a push queued against it sits forever undrained —
    // permanently `None`/`0` either way, not a signal to assert against or
    // wait on. `Actions::detach`'s own toast, raised on the same async
    // landing as its `nav.pop()`, is the one real, observable side effect
    // left (`a_second_detach_while_the_first_is_in_flight_pops_once` uses the
    // same signal for the same reason).
    h.open_sheet();
    h.tap_footer_detach();
    // Detach is async; the toast lands once the repository answers.
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

    // Detach is async and the sheet can be reopened while it is in flight —
    // two landings would pop `/sessions` off the stack as well as `/terminal`.
    // Neither `tap_footer_detach` nor `settle` pumps the task queue, so both
    // requests are made before either can land.
    h.open_sheet();
    h.tap_footer_detach();
    h.open_sheet();
    h.tap_footer_detach();

    // See `the_footers_detach_row_pops_the_terminal_route`'s doc: the toast is
    // this harness's one real "a detach landed" signal — neither
    // `Nav::bar().meta` nor `NavigatorController::depth()` ever moves here.
    let deadline = Instant::now() + PUMP_DEADLINE;
    while app_toasts().pending_untracked().is_empty() {
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
// A read-only credential
// ---------------------------------------------------------------------------

/// Flips `ConnectionController::is_read_only` directly — the mock backend always answers
/// `false`, so no mock connect could produce one. Same seam `tests/sessions.rs`/`tests/settings.rs`
/// already use.
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
    // Neither add-space, add-tab nor add-pane row is offered at all.
    assert!(
        !h.shows(ADD_SPACE_LABEL) && !h.shows(ADD_TAB_LABEL) && !h.shows(NEW_PANE_LABEL),
        "a read-only session is offered no create row: {:?}",
        h.values()
    );
    // The tab rows stay tappable — browsing one reads what is already loaded
    // and moves nothing.
    h.tap(h.value_point("btop"));
    h.settle();
    assert!(h.shows("ed@precision zsh"), "the browse moved");
}

#[test]
fn a_read_only_credential_navigates_across_spaces() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    app_toasts().clear();
    make_read_only(&mut h);

    h.open_sheet();
    // "zelli_mobile" is a herdr space other than the one this connection is
    // on ("forgekit") — expanding it is a read-only-safe peek, and its own
    // active tab cascades open on its own.
    h.tap(h.value_point("zelli_mobile"));
    // Waited on through the controller's own signal, NOT painted geometry: the
    // space this connection is already on ("forgekit") has its own settled
    // reveal to collapse first, and `pump_until`'s pinned-zero paint clock never
    // advances that tween — so forgekit's stale rows would still be sitting
    // exactly where zelli_mobile's are about to land, and row [0] below would be
    // devbox's pane instead of this browse's.
    h.pump_until("zelli_mobile's own tab to auto-expand", |_| {
        debug_controller()
            .expect("mounted")
            .expanded_tab
            .get_untracked()
            .as_deref()
            == Some("logs")
    });
    // Runs those reveals forward for real so this frame paints the settled
    // result rather than forgekit's stale geometry.
    //
    // Settled three times over: on a read-only tree one settle still paints a
    // single pane row while the tap lands on forgekit's not-yet-retracted one,
    // so the commit targets the space the connection is already on (measured:
    // `commit_pane_in_space("forgekit")`, no switch, no toast).
    h.settle_motion(0.0);
    h.settle_motion(2_000.0);
    let scene = h.settle_motion(4_000.0);
    h.tap(center(tree_pane_rows(&scene)[0]));
    h.pump_until_closed("the sheet to close", |h| h.depth() == REST_DEPTH);

    // A read-only credential can navigate across spaces: `SwitchSpace` carries
    // no read-only gate on the wire (muxrd/src/grpc/space_ops.rs).
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

    // Committing inside the tab the session is on is `FocusPane` alone, which
    // the wire allows a read-only token — so it lands, and the sheet closes.
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

    // A read-only credential can navigate across tabs: `GoToTab` carries no
    // read-only gate on the wire (muxrd/src/grpc/tab_ops.rs).
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

    // A read-only credential can navigate panes on herdr: `FocusPane` carries
    // no read-only gate on the wire (muxrd/src/grpc/pane_ops.rs).
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
    // The sheet is a page of the root overlay host, so it outlives the screen
    // that pushed it — its tree body empties itself, but the footer rebuilds
    // whole, and this press is what would seed a dialog off a disposed signal.
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

    // Its neighbour is gated the same way: a detach here would pop a route
    // stack that no longer carries `/terminal`, taking `/sessions` with it.
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

// ---------------------------------------------------------------------------
// 8 — vector chrome
// ---------------------------------------------------------------------------

#[test]
fn every_affordance_on_this_screen_is_a_vector_and_announces_a_word() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    let scene = h.settle_motion(0.0);

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
    // `?session=zellij:dev`, the name-derived id the mock mints. So delete one
    // character and submit.
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
    // A guard against the empty-body flash: the screen paints its card from
    // its very first frame, before any repository has answered.
    let state = MuxrApp.init();
    let nav: NavigatorController<AppState> = NavigatorController::new();
    let record_only: NavigatorController<AppState> = NavigatorController::new();
    let _router = install_app_router(&record_only, &nav, expect_connection_controller());
    let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
    root.set_theme(Box::new(frust_glyph::baseline()));
    let logic_nav = nav.clone();
    let mut logic: Logic = Box::new(move |_state: &mut AppState| terminal_page(logic_nav.clone()));
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

// 10 — responsive width-clamp pass.

/// A phone in landscape: past the clamp tier's width, short of the Tablet class.
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
    // No `BAR_HEIGHT` term any more: the shell scaffold's `app_bar` slot
    // reserves the bar's own band above this widget's box in the real app
    // (`crate::shell::shell_page`); mounted standalone here (this suite's own
    // `Harness`, no scaffold), the stage's box already starts where the bar
    // would have ended, so the step-up is the only clearance left to assert.
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

    // Portrait used to sit flush under the floating bar while landscape read
    // as spaced — the gap is the *top* inset only, so the sides stay at the
    // narrow window's own `WRAP_PAD` (asserted above). No `BAR_HEIGHT` term:
    // the shell scaffold reserves the bar's own band in the real app, and
    // this suite's `Harness` mounts the stage starting where that band would
    // have ended (see `wide_metrics_grow_the_wrap_padding…`'s doc).
    let (origin, _, _) = card(&h.frame());
    assert!(
        (origin.y - PORTRAIT_TOP_GAP).abs() < EPSILON,
        "the card breathes by the portrait step-up: {origin:?}"
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

// The sheet's three create affordances. Request shapes are already pinned at the
// repository/mock-server level, and the happy-path reload/select-by-label flow at the controller
// level (`MockTerminalRepository` leaves these three verbs at the shared "unsupported" default),
// so what's left to pin here is *flow*: both the space and tab rows open a dialog, the space one
// keeps its non-blank guard while the tab one deliberately doesn't, and the sheet stays up
// around all three regardless of how the repository answers.

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

// Every chip's trailing edit icon. Request shapes and the settled reload are pinned at the
// controller level (`MockTerminalRepository` leaves the three renames at the shared
// "unsupported" default), so what these pin is *flow* and, above all, that the icon's tap
// region is disjoint from the chip's main one: tapping it opens a prefilled rename dialog and
// never also selects/browses/commits, while a plain tap on the rest of the chip still does.

#[test]
fn holding_a_space_row_renames_it_without_toggling_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_session(HERDR_SESSION_ID);
    app_toasts().clear();

    h.open_sheet();
    let floor = h.depth();
    let before = h.frame();

    // "cupline" is a collapsed, empty, non-selected space — the tree's own
    // press-and-hold (`side_sheet_tree`'s `pressable`), not a trailing edit
    // icon, is what opens its rename dialog now.
    h.long_press(h.value_point("cupline"));
    let after = h.settle();

    assert_eq!(h.depth(), floor + 1, "the rename dialog opened");
    assert!(
        h.shows("cupline"),
        "prefilled with the space's current name"
    );

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

    // "btop" opens collapsed — devbox is the one the session is on, seeded
    // pre-expanded.
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
    // The second pane row ("ed@devbox zsh") — the mock seeds the first one
    // focused, so committing the second would be an observable move.
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
    // Give a commit's own close-beat every chance to fire before checking —
    // the hold must not have started one.
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

    // A plain tap on the same row still commits it.
    let scene = h.settle_motion(0.0);
    h.tap(center(tree_pane_rows(&scene)[1]));
    // `floor - 1`: depth is already `floor` (the sheet, still up) before this
    // tap — the wait has to watch for the sheet's own pop, not the level it is
    // already sitting at.
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
    // "cupline" is a collapsed, empty, non-selected space — a landed toggle
    // would expand it and show its hint row, and a landed hold would open its
    // rename dialog; a drag past slop must fire neither.
    let point = h.value_point("cupline");

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
    // Holding any row — a promoted space's card, a tab, a pane — opens no
    // dialog at all: `TreeMutations` is withdrawn wholesale for a read-only
    // credential, so the tree wires no `on_long_press` in the first place.
    h.long_press(h.value_point("btop"));
    h.settle();

    assert_eq!(
        h.depth(),
        floor,
        "a read-only session offers no rename affordance anywhere on the sheet"
    );
}

// ---------------------------------------------------------------------------
// The Tablet's docked rail
// ---------------------------------------------------------------------------

/// `docs/tablet.html`'s three frame sizes: landscape (Full band) and the
/// collapsed band's square. The paint surface stays [`SURFACE`]; only the
/// published metrics pick the structure.
const TABLET_FULL: (f64, f64) = (1024.0, 720.0);
const TABLET_COLLAPSED: (f64, f64) = (720.0, 720.0);
/// The zellij mock's header line (four tabs, two panes apiece).
const RAIL_SUMMARY: &str = "4 tabs \u{b7} 8 panes";
const EXIT_IMMERSIVE: &str = "Exit immersive";

/// [`Harness::mount_session`] under Tablet metrics of `size`.
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
    h.tap(h.value_point("ed@precision zsh"));
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
        "gohora rings: {labels:?}"
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
    assert!(h.shows("ed@precision zsh"), "the browse moved");
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

/// The Tablet kebab's menu entries, by index (Rename, Immersive, Show key bar,
/// a separator, Detach).
const MENU_IMMERSIVE: usize = 1;
const MENU_KEY_BAR: usize = 2;
const MENU_DETACH: usize = 4;

impl Harness {
    /// Press the kebab and pick entry `index` of the menu it opens.
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

/// The read-only cross-space commit that once dropped when the tap followed
/// the browse closely, on the docked rail: the commit lands in order, and the
/// rail rests on where it landed.
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
    // The space's and then the tab's reveal, each run out on the paint clock.
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

/// Tab keys are unique only within one layout (an unnamed tab is keyed by its display
/// name), so a browsed space can carry a tab keyed like the resting one. The mock's
/// poll always reports `devbox` active: to a rail resting on `btop`, another client
/// moving the focus back.
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

/// A current-thread runtime to drive one controller call to completion.
fn current_thread() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
}

/// Browsing another space caches its tabs; re-expanding the selected one (the rail's
/// auto-drill reopening the active tab) puts the rail back at rest, so a poll that
/// reports another client's focus must be followed despite that cache.
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

// ---------------------------------------------------------------------------
// The deep link's supersede rule
// ---------------------------------------------------------------------------

/// Only a move that lands supersedes the deep link: one whose RPC fails — a space
/// switch or a tree commit (both claimed), or a grid tap (no claim: a failed verb never
/// lands) — leaves it pending for the next publish.
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

/// A space create moves no focus, so it keeps the deep link — even with its claim won.
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

// ---------------------------------------------------------------------------
// The key bar's two tiers
// ---------------------------------------------------------------------------

/// The Tablet tier's `Home` cap and its `Fn` cap, by accessible name.
const HOME_CAP: &str = "Home";
const FN_CAP: &str = "Function keys";

impl Harness {
    /// Enter the fixture replay (the live pane) and settle the build that mounts it.
    fn enter_live(&mut self) {
        debug_controller().expect("mounted").enter_live();
        self.flush();
    }

    /// Whether a node labelled `label` is on the tree.
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

    assert!(node_labels(&h.root).iter().any(|l| l == "minefit-api"));
    h.tap_label("minefit-api");
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
