//! Headless coverage for muxr's **navigation motion**: the shell's app bar
//! adapter (`src/routes.rs`'s `route_meta`/`Nav::bar`, consumed by
//! `src/shell.rs::shell_page`'s `frust::scaffold`), and the GlyphSlide page
//! transition every path navigation carries.
//!
//! This suite mounts the *whole* app (only the real `MuxrApp` puts the
//! scaffold, and so the bar, in the tree). The bar itself is
//! `frust_glyph::AppBarView`; its paint/crossfade/reveal are Glyph's to test.
//! This suite owns the **adapter** — which route gets a bar and what it shows
//! (`routes::route_meta`), shell behavior (bar-less Connect, a titled route,
//! direction on back, fullscreen suppression), and overlay-host arbitration
//! (R44-back, a modal's bar dimming) — read off the accessibility tree
//! (`Harness::has_bar`/`values`/`labels`), what a screen reader actually sees.
//!
//! **Three navigators, one depth reads only one of them.**
//! [`Harness::depth`] reads the **outer** navigator (`AppState::nav`) — `/`
//! plus, once placed, the one shell page. `/sessions`, `/terminal`,
//! `/settings` are children of that same `shell_route`, so navigating between
//! them lands on the shell's **inner** navigator and never moves `h.depth()`
//! — only crossing the shell boundary does. Tests over an in-shell push
//! assert the retitle instead (`round_trip_through`'s doc). The third
//! navigator is the **overlay host** ([`Harness::overlay_depth`]): every
//! menu/sheet/dialog is a page there, and an open one makes the app
//! underneath, bar included, inert.
//!
//! [`spend_cold_start`] burns the one-per-process splash claim first.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use frust::{AnyView, Component, GetUntracked, SizedBox, Theme, any};
use frust_core::accesskit::Role;
use frust_core::{InputEvent, NamedKey, PointerPhase, RenderRoot};
use frust_reactive::TrackedScope;
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::routes::{app_nav, sessions_back_signal};
use muxr::ui::dialogs::confirm_dialog;
use muxr::ui::sheet::{SheetHandle, show_sheet};
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, center, frame, frame_at, largest, ms, named_key,
    node_centers_by_label, node_labels, node_roles, node_values, pointer, serial, settle, setup,
    smallest, tap,
};

/// The mock connection every bar-titled test runs behind (the session guard
/// bounces `/sessions`/`/terminal` otherwise).
const HOST: &str = "mock.example";
const PORT: u16 = 7777;

/// The bar's brand wordmark, and its two slot affordances' **accessible
/// names** — transcribed rather than imported: `ui::topbar`'s constants are
/// the implementation's, and a test that imported them could not catch one of
/// them changing. See [`BANNED_GLYPHS`] for why they're words, not marks.
const BRAND: &str = "muxr";
const BACK: &str = "Back";
const GEAR: &str = "Settings";

/// The two glyphs that used to label [`BACK`]/[`GEAR`] as text. **Nothing in
/// the bar may carry either again**: neither is in the bundled Glyph faces, so
/// each platform's font-fallback chain answered differently (the `⚙` was
/// simply absent on iPhone) — a device-verified regression this suite pins shut.
const BANNED_GLYPHS: [&str; 2] = ["\u{2039}", "\u{2699}"];

/// Where the pre-split probe samples the Glyph push, inside the leaving
/// page's fade-out (`resolve_layers`'s `Glyph` arm,
/// `../../crates/frust-widgets/src/nav/transition.rs`). Progress is eased —
/// `p = curve.transform(t / 340)` on `Cubic(0.34, 1.35, 0.64, 1.0)`
/// (`GLYPH_ENTER`/`GLYPH_SPATIAL_CURVE`, matching
/// `frust_glyph::baseline()`'s `MotionScheme`) — and `GLYPH_FADE_SPLIT`
/// (0.44) crosses at `t ≈ 44ms`. At `t = 24ms`, `p ≈ 0.258`: leaving
/// `alpha ≈ 0.41` (painted, `dx ≈ -4.1px`), entering `alpha == 0` (discarded).
const PRE_SPLIT_MS: f64 = 24.0;

/// Where the post-split probe samples, past the ≈44ms crossing above. At
/// `t = 60ms` (the old single-sample `MID_TRANSITION_MS`'s value, unchanged),
/// `p ≈ 0.566`: leaving `alpha == 0` — into `DiscardScene` since frust
/// `5fc1513a` — entering `alpha ≈ 0.226` (painted, `dx ≈ 6.9px`).
const POST_SPLIT_MS: f64 = 60.0;

/// A mock connect target — only `connected` flipping and the `host:port`
/// endpoint matter here.
fn mock_target() -> ConnectTarget {
    ConnectTarget {
        host: HOST.to_string(),
        port: PORT,
        trust_mode: TrustMode::System,
        label: "Mock rig".to_string(),
        offer_credential: false,
        token: zeroize::Zeroizing::new(String::new()),
        remember: false,
        fingerprint: None,
    }
}

/// A Glyph theme with `reduce_motion` forced on/off — what
/// `nav::transition::resolve_spec` reads to decide whether to collapse the
/// route transition.
fn theme(reduce_motion: bool) -> Theme {
    let mut theme = frust_glyph::baseline();
    theme.motion.reduce_motion = reduce_motion;
    theme
}

/// Burn this process's one cold-start splash claim (see the [module docs](self)).
fn spend_cold_start() {
    static SPENT: OnceLock<()> = OnceLock::new();
    SPENT.get_or_init(|| {
        let mut throwaway = MuxrApp.init();
        let _ = MuxrApp.build(&mut throwaway);
    });
}

/// The app-logic closure `RenderRoot::rebuild` drives.
type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

/// The whole app, themed, warm-started.
struct Harness {
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    /// A strictly-increasing clock for [`Harness::park`] — see there.
    clock: f64,
    /// The scope every [`Harness::park`] frame rebuilds inside, so a signal
    /// written after that rebuild is observable exactly as
    /// `frust-shell-desktop` observes it. See [`Harness::requested_frame`].
    scope: TrackedScope,
}

impl Harness {
    /// Mount at `/` with motion enabled.
    fn mount() -> Self {
        Self::themed(theme(false))
    }

    /// Mount at `/` under `theme`.
    fn themed(theme: Theme) -> Self {
        spend_cold_start();
        let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
        root.set_theme(Box::new(theme));
        let mut harness = Harness {
            root,
            logic: Box::new(|state: &mut AppState| MuxrApp.build(state)),
            state: MuxrApp.init(),
            tcx: TextContext::new(),
            clock: support::SETTLE_MS,
            scope: TrackedScope::new(),
        };
        harness.frame();
        harness
    }

    /// Mount, connect (mock), and land on `/sessions` — the app's first titled
    /// route, reached the way the Connect screen reaches it.
    fn on_sessions() -> Self {
        let mut harness = Harness::mount();
        expect_connection_controller().connect(mock_target());
        harness.push("/sessions");
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

    fn frame_at(&mut self, at_ms: f64) -> RecScene {
        frame_at(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            ms(at_ms),
        )
    }

    /// Drive whatever was just pushed/popped past its transition and the
    /// bar's own crossfades.
    fn settle(&mut self) -> RecScene {
        settle(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
        )
    }

    /// Push `location` through the app's real handle — the seam that carries
    /// the route transition and records the bar mirror — and settle it.
    fn push(&mut self, location: &str) {
        app_nav().push(location);
        self.settle();
    }

    /// One frame, run the way `frust-shell-desktop`'s loop runs one: rebuild
    /// inside a [`TrackedScope`], then layout, then paint. Returns the painted
    /// frame and whether the app asked for another — an animation still in
    /// flight (`PaintOutcome::needs_frame`) or a tracked write since the
    /// rebuild (`TrackedScope::is_dirty`), the two conditions shell turns into
    /// a `request_redraw`.
    ///
    /// A frame whose every request was `TickClass::CosmeticLoop` (a loop that
    /// never settles by design, e.g. `/terminal`'s caret blink) reports `false`
    /// here instead: counting it would make [`Harness::park`] unusable on the
    /// menu → sheet chain, and costs nothing, since such a frame by
    /// construction has no transition, reveal or staging in flight.
    fn requested_frame(&mut self) -> (RecScene, bool) {
        self.clock += FRAME_MS;
        let root = &mut self.root;
        let logic = &mut self.logic;
        let state = &mut self.state;
        let scope = &self.scope;
        scope.track(|| root.rebuild(logic, state));
        let tcx: &mut dyn Any = &mut self.tcx;
        self.root.layout_with_text(support::surface(), tcx);
        let mut scene = RecScene::default();
        let outcome = self.root.paint(&mut scene, ms(self.clock));
        let converging = outcome.needs_frame && !outcome.needs_frame_paced_only;
        let more = converging || self.scope.is_dirty();
        (scene, more)
    }

    /// Run frames for exactly as long as the app asks for them, and stop where
    /// the desktop loop parks on `Wait`; returns the last frame painted.
    ///
    /// **The honest half of section 3c.** Every other helper here paints
    /// frames unconditionally — a courtesy no shell pays: nothing renders
    /// between one `request_redraw` and the next input. So whatever the bar
    /// looks like at *this* point is what a user is looking at.
    fn park(&mut self) -> RecScene {
        for _ in 0..PARK_FRAME_CAP {
            let (scene, more) = self.requested_frame();
            if !more {
                return scene;
            }
        }
        panic!("the app never stopped asking for frames ({PARK_FRAME_CAP} of them)");
    }

    /// Deliver `event` and run the loop to its parking point — the two halves
    /// of `ShellHandler::dispatch` (route the event, schedule a frame if it
    /// dirtied anything) and nothing else.
    fn dispatch(&mut self, event: &InputEvent) {
        if self.root.event(&mut self.state, event).needs_redraw {
            self.park();
        }
    }

    /// A full tap at `point`, each half dispatched through the production
    /// loop above (rather than [`support::tap`], which paints no frames at
    /// all — an overlay's staged exit needs them).
    fn tap_parked(&mut self, point: Point) {
        self.dispatch(&pointer(PointerPhase::Down, point));
        self.dispatch(&pointer(PointerPhase::Up, point));
    }

    fn values(&self) -> Vec<String> {
        node_values(&self.root)
    }

    fn labels(&self) -> Vec<String> {
        node_labels(&self.root)
    }

    /// Whether the shell's bar is on screen at all.
    fn has_bar(&self) -> bool {
        node_roles(&self.root).contains(&Role::TitleBar)
    }

    /// Tap the first node labelled `label` (the bar's `‹` or `⚙`).
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
        tap(&mut self.root, &mut self.state, point);
    }

    /// The **route** navigator's page-stack depth — never moved by an overlay
    /// any more (see the module docs' two-navigator note).
    fn depth(&self) -> usize {
        self.state.nav.controller().depth()
    }

    /// The **root overlay host**'s page-stack depth: [`REST_DEPTH`] with
    /// nothing open (its one page being the whole app), one more per overlay.
    fn overlay_depth(&self) -> usize {
        self.state.overlays.depth()
    }
}

/// The overlay host's depth with nothing open: its root page is the entire app
/// (chrome, route navigator and all).
const REST_DEPTH: usize = 1;

/// Assert the bar is showing `title`, and nothing else is.
fn assert_titled(h: &Harness, title: &str) {
    assert!(h.has_bar(), "the shell bar is mounted");
    // The title is `Role::TitleBar`'s own **label** (`support::node_labels`'s
    // doc), not a separate text-run `value` child.
    assert!(
        h.labels().iter().any(|l| l == title),
        "the bar titles {title:?}: {:?}",
        h.labels()
    );
}

// ---------------------------------------------------------------------------
// 1 — the bar's presence is route-driven
// ---------------------------------------------------------------------------

#[test]
fn connect_carries_no_bar_at_all() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::mount();

    // `/` is the full-bleed Connect design (and the route the splash overlays),
    // so the bar is not merely blank — it contributes no node and no height.
    assert!(!h.has_bar(), "roles on screen: {:?}", node_roles(&h.root));
    assert!(
        !h.values().iter().any(|v| v == BRAND),
        "…and no brand wordmark with it: {:?}",
        h.values()
    );
}

#[test]
fn sessions_wears_the_brand_the_connected_host_and_the_gear() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::on_sessions();

    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "the stack's first titled route wears the brand: {:?}",
        h.values()
    );
    assert!(
        h.labels().iter().any(|l| l == GEAR),
        "…and the ⚙ that reaches /settings: {:?}",
        h.labels()
    );
    assert!(
        !h.labels().iter().any(|l| l == BACK),
        "…but no back button at the bottom of the stack: {:?}",
        h.labels()
    );
}

// ---------------------------------------------------------------------------
// 2 — a pushed route retitles the bar and grows a back button
// ---------------------------------------------------------------------------

/// Push `route`, assert the bar became `title` + `‹`, then take the `‹` back
/// and assert the bar returned to `/sessions`' face. Both pushed routes run
/// this identically, which is the point: the bar is route metadata, not
/// per-screen chrome.
///
/// **No `h.depth()` here**: `/sessions`/`/terminal`/`/settings` share one
/// `shell_route`, so a push between them lands on the shell's *inner*
/// navigator and never moves the outer one `h.depth()` reads — the retitle is
/// the observable instead.
fn round_trip_through(route: &str, title: &str) {
    let mut h = Harness::on_sessions();

    h.push(route);
    assert_titled(&h, title);
    assert!(
        h.labels().iter().any(|l| l == BACK),
        "a pushed route wears the ‹: {:?}",
        h.labels()
    );
    assert!(
        !h.values().iter().any(|v| v == BRAND),
        "…in place of the brand, not beside it: {:?}",
        h.values()
    );
    assert!(
        !h.labels().iter().any(|l| l == GEAR),
        "…and drops /sessions' ⚙: {:?}",
        h.labels()
    );

    h.tap_label(BACK);
    h.settle();

    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "…and the brand came back with it: {:?}",
        h.values()
    );
    assert!(
        !h.labels().iter().any(|l| l == BACK),
        "…leaving no back button behind: {:?}",
        h.labels()
    );
}

#[test]
fn settings_is_titled_and_backed_and_pops_home() {
    let _g = serial();
    let _ambient = setup();
    round_trip_through("/settings", "server settings");
}

#[test]
fn the_terminal_route_is_titled_and_backed_and_pops_home() {
    let _g = serial();
    let _ambient = setup();
    // The design deviation, asserted: the mock's terminal bar shows the brand
    // and a ⋮; muxr gives every pushed route the ‹ instead (see
    // `routes::BarLeading::Back`).
    round_trip_through("/terminal?session=s-1", "terminal");
}

#[test]
fn the_bars_gear_reaches_settings() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    h.tap_label(GEAR);
    h.settle();

    // In-shell push (`round_trip_through`'s doc): the retitle is the
    // observable, not `h.depth()`.
    assert_titled(&h, "server settings");
}

// ---------------------------------------------------------------------------
// 3 — the bar ignores overlays, and follows a pop it never saw
// ---------------------------------------------------------------------------

/// Push an inert transparent page onto the **overlay host** — a stand-in for a
/// sheet or a dialog: it adds a page above every piece of chrome, paints
/// nothing worth recording, and swallows nothing, so whatever the bar does is
/// the bar's own doing.
fn open_overlay(h: &mut Harness) {
    h.state
        .overlays
        .push_transparent(|| any(SizedBox::<AppState>(Some(10.0), Some(10.0))));
    h.settle();
}

/// Dismiss whatever [`open_overlay`] put up — the host's own pop, standing in
/// for a scrim tap on a real overlay.
fn close_overlay(h: &mut Harness) {
    h.state.overlays.pop();
    h.settle();
}

#[test]
fn a_transparent_overlay_over_sessions_never_retitles_the_bar() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    let covered_depth = h.depth();

    // A sheet/dialog is pushed as a page *builder* straight through a
    // controller, never through `Nav` — so it carries no route metadata, and
    // the route the bar is *for* does not change. Since the host landed it does
    // not even land on the same stack: the route depth is untouched, and only
    // the host's grows. The bar goes modal while it is up (below); what must
    // not happen is a retitle.
    open_overlay(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the overlay is its own page on the host"
    );
    assert_eq!(
        h.depth(),
        covered_depth,
        "…and the route stack under it is untouched"
    );

    close_overlay(&mut h);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "…and dismissing it pops only it"
    );
    assert_eq!(h.depth(), covered_depth, "the route stack, still untouched");
    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "…leaving the covered route's own bar, brand and all: {:?}",
        h.values()
    );
}

// ---------------------------------------------------------------------------
// 3b — an open overlay dims the bar and takes its input, from the host
// ---------------------------------------------------------------------------
//
// Both halves are the root overlay host's doing, not the bar's own: it routes
// input and publishes accessibility nodes for its **top page only**, and an
// overlay always paints over everything underneath (outermost in the
// composition) — a compositing fact this suite no longer pixel-verifies
// (`frust_glyph::AppBarView`'s own paint order is Glyph's to test). What it
// still owns: the bar goes modal the instant an overlay opens, asserted below
// through the accessibility tree.

#[test]
fn the_gear_does_nothing_under_an_open_overlay_and_works_again_after() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    // Where the ⚙ lives while the bar is live — recorded *before* the modal,
    // because a modal bar publishes no node to find it by.
    let gear = node_centers_by_label(&h.root, GEAR)
        .first()
        .copied()
        .expect("the settings affordance is on screen");

    open_overlay(&mut h);
    assert!(
        !h.has_bar(),
        "a modal bar publishes no node: an affordance that routes nowhere is \
         worse than none, and a screen reader must not be offered a way round \
         the modality: {:?}",
        node_roles(&h.root)
    );

    // The modality hole this closes: before the root host, this press pushed
    // /settings out from under an open sheet.
    tap(&mut h.root, &mut h.state, gear);
    h.settle();

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the overlay is still the only thing up"
    );
    // The strong check: `h.depth()` reads the same either way (in-shell
    // push, `round_trip_through`'s doc) — the retitle is what a fired push
    // would show.
    assert!(
        !h.values().iter().any(|v| v == "server settings"),
        "…and nothing retitled: {:?}",
        h.values()
    );

    // Dismiss, and the very same press works again.
    close_overlay(&mut h);
    assert!(h.has_bar(), "the bar is live again");
    h.tap_label(GEAR);
    h.settle();
    assert_titled(&h, "server settings");
}

// ---------------------------------------------------------------------------
// 3c — a closed modal gives the bar back, on the frames production actually runs
// ---------------------------------------------------------------------------
//
// Section 3b asserts *what* the bar does under a modal; this one asserts
// **when** — the only section here that refuses the harness's courtesy frames
// (see [`Harness::park`]).
//
// The defect it pins shut: a bar deriving modality from
// `NavigatorController::depth` would still see the stale dimmed state on the
// frame right after a modal's pop (`RenderRoot::rebuild` runs before the
// navigator drains its queued ops), leaving it dark and inert until unrelated
// input woke it. The host reconciles before the page under it rebuilds, so
// this is asserted with no frames in between, against a **real** overlay
// (`show_sheet`/`confirm_dialog`).

/// The sheet body these tests open — big enough to place the panel, small
/// enough to leave most of the surface scrim.
const SHEET_BODY: f64 = 100.0;
/// A point comfortably above any of those panels, and so on the scrim: the
/// dismissal gesture (a press that both starts and ends outside the panel).
const SHEET_SCRIM_POINT: Point = Point::new(4.0, 200.0);
/// How far [`Harness::requested_frame`] advances the clock — one 60Hz frame,
/// the cadence a desktop `request_redraw` actually produces.
const FRAME_MS: f64 = 16.0;
/// The frame budget [`Harness::park`] gives the app to stop asking. Ten
/// seconds of frames: every animation in this app is a few hundred
/// milliseconds, so hitting it means something is looping forever.
const PARK_FRAME_CAP: usize = 600;

/// Open the app's **real** bottom sheet over `h` (the same helper every screen
/// calls), run to the parking point, and report where its panel is painting —
/// the biggest *path* the sheet added to the covered screen's own chrome, a
/// Glyph sheet's panel being top-rounded only and so painted as a path.
fn open_real_sheet(h: &mut Harness) -> (Point, Size) {
    let before = h.park();
    show_sheet(&h.state.overlays, |_sheet| {
        any(SizedBox::<AppState>(Some(SHEET_BODY), Some(SHEET_BODY)))
    });
    let open = h.park();
    assert!(
        !h.has_bar(),
        "a sheet dims the bar and takes its input: {:?}",
        node_roles(&h.root)
    );
    largest(&added(&before.paths, &open.paths)).expect("the sheet paints its own panel")
}

#[test]
fn a_sheet_dismissed_by_its_scrim_gives_the_bar_back_before_the_loop_parks() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    let covered_depth = h.depth();

    open_real_sheet(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the sheet is its own page on the host"
    );
    assert_eq!(h.depth(), covered_depth, "…over an untouched route stack");

    // The dismissal, and then *nothing*: the sheet's staged exit plays out
    // inside this dispatch, the pop lands, and the loop stops exactly where
    // the shell would park on `Wait`.
    h.tap_parked(SHEET_SCRIM_POINT);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the scrim tap popped the sheet"
    );
    assert_eq!(h.depth(), covered_depth, "…and only the sheet");
    assert!(
        h.has_bar(),
        "…and the bar is lit and addressable again with no further input: {:?}",
        node_roles(&h.root)
    );
    // Lit is half of it; live is the other half — the retitle is the observable.
    h.tap_label(GEAR);
    h.settle();
    assert_titled(&h, "server settings");
}

#[test]
fn escape_on_a_sheet_gives_the_bar_back_before_the_loop_parks() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    let covered_depth = h.depth();

    // A press on the panel itself claims focus (and does not dismiss), which
    // is what gives the key event a chain to travel — the same thing a real
    // interaction with the sheet's own body does.
    let panel = open_real_sheet(&mut h);
    h.tap_parked(center(panel));
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "a panel press keeps it open"
    );

    h.dispatch(&named_key(NamedKey::Escape));

    assert_eq!(h.overlay_depth(), REST_DEPTH, "Escape dismissed the sheet");
    assert_eq!(h.depth(), covered_depth, "…and left the route stack alone");
    assert!(
        h.has_bar(),
        "…and the bar came back with it: {:?}",
        node_roles(&h.root)
    );
}

#[test]
fn a_dialog_action_gives_the_bar_back_before_the_loop_parks() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    let before = h.park();
    let covered_depth = h.depth();

    // The path with no staged frame at all: a dialog action pops immediately,
    // so nothing app-side schedules the frame the lit bar is painted on — the
    // host's own reconcile is what has to be early enough.
    confirm_dialog(
        &h.state.overlays,
        "Kill session?",
        "Any attached client is disconnected immediately.",
        "Kill",
        |_state: &mut AppState| {},
    );
    let open = h.park();
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the dialog is its own page on the host"
    );
    assert_eq!(h.depth(), covered_depth, "…over an untouched route stack");
    assert!(!h.has_bar(), "…and the bar is inert under it");

    // The danger action is the smallest thing the dialog added to the frame's
    // stroked outlines (its panel's own border being the other one) — the
    // `tests/ui_substrate.rs` idiom, diffed against the covered screen.
    let action = smallest(&added(&before.strokes, &open.strokes))
        .expect("the danger action strokes its border");
    h.tap_parked(center(action));

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the action closed the dialog"
    );
    assert_eq!(h.depth(), covered_depth, "…and only the dialog");
    assert!(
        h.has_bar(),
        "…and the bar is live again with no further input: {:?}",
        node_roles(&h.root)
    );
}

/// **R44-back, end to end** — and the reason the root host had to be a `host`
/// rather than one more navigator. The press is delivered through
/// `frust::push_back_press`, the process-wide source the facade arbitrates
/// behind (a root overlay host outranks every plain navigator); nothing here
/// names a controller to pop — that choice **is** what is under test. It
/// doubles as the original defect's guard: the sheet dismisses itself
/// (`BackPolicy::DismissAnimated`), so the bar has to come back live with no
/// app code ever having known about the overlay.
#[test]
fn an_android_back_press_dismisses_the_overlay_and_leaves_the_route_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    let covered_depth = h.depth();

    open_real_sheet(&mut h);
    assert_eq!(h.overlay_depth(), REST_DEPTH + 1, "the sheet is up");

    frust::push_back_press();
    h.settle();

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the back press dismissed the overlay"
    );
    assert_eq!(
        h.depth(),
        covered_depth,
        "…and NOT the route under it (the #44 back bug)"
    );
    assert!(
        h.has_bar(),
        "…and the bar is live again: {:?}",
        node_roles(&h.root)
    );
    h.tap_label(GEAR);
    h.settle();
    assert_titled(&h, "server settings");
}

// ---------------------------------------------------------------------------
// 3d — an overlay's whole life leaves the route mirror untouched
// ---------------------------------------------------------------------------
//
// Section 3c asserts that a *closed* modal gives the bar back. This one covers
// two handler shapes that used to corrupt the route-meta mirror by reading a
// one-rebuild-stale depth as fact: a handler that closes an overlay *and*
// pushes a route in the same rebuild (Sessions' Attach row), and an overlay
// opened out of the overlay before it (the terminal's menu → sheet chain).
// Both are structurally impossible now — an overlay is a page of the host, so
// the route depth never moves — and both are still asserted end to end, at the
// parking point, with no courtesy frames.

/// Open the app's real sheet over `h` and hand back the [`SheetHandle`] its
/// body's rows close it through — captured out of the body builder exactly as
/// a row captures one (`features::sessions`' `sheet_action`).
fn open_sheet_with_handle(h: &mut Harness) -> SheetHandle {
    let captured: Rc<RefCell<Option<SheetHandle>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&captured);
    show_sheet(&h.state.overlays, move |sheet| {
        *sink.borrow_mut() = Some(sheet.clone());
        any(SizedBox::<AppState>(Some(SHEET_BODY), Some(SHEET_BODY)))
    });
    h.park();
    let handle = captured.borrow().clone();
    handle.expect("the sheet's body builder ran and minted a handle")
}

/// A harmless pointer move — the input pass that hands a pushed page's
/// **pop-completion callback** over (`NavigatorWidget::event` flushes the
/// results queued at the previous rebuild). Production gets one from the
/// user's next touch; a test that drives the navigator from outside a handler
/// has to supply it.
const IDLE_MOVE: Point = Point::new(4.0, 400.0);

#[test]
fn a_route_pushed_as_a_sheet_closes_keeps_its_bar_entry_under_the_next_overlay() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();

    let sheet = open_sheet_with_handle(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the sheet is its own page on the host"
    );

    // The Attach row's own two calls (`sheet.close(); actions.attach(&id)`),
    // back to back with no frame between them. In-shell push
    // (`round_trip_through`'s doc): the retitle is the observable.
    sheet.close();
    app_nav().push("/settings");
    h.park();

    assert_eq!(h.overlay_depth(), REST_DEPTH, "the sheet's page went");
    assert_titled(&h, "server settings");

    // The next input pass — the user's next touch — delivers the sheet's
    // pop-completion callback, which is what latches its handle.
    h.dispatch(&pointer(PointerPhase::Move, IDLE_MOVE));

    // THE Concern-4 stale-bar scenario, still asserted end to end: an overlay
    // opening and closing over `/settings` must not evict its bar entry.
    let overlay = open_sheet_with_handle(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the overlay is its own page"
    );
    assert!(
        !h.has_bar(),
        "…and it dims the bar over the route: {:?}",
        node_roles(&h.root)
    );

    overlay.close();
    h.park();

    assert_eq!(h.overlay_depth(), REST_DEPTH, "the overlay popped");
    assert_titled(&h, "server settings");
    assert!(
        h.labels().iter().any(|l| l == BACK),
        "…back affordance and all — the route's own entry survived: {:?}",
        h.labels()
    );
    assert!(
        !h.values().iter().any(|v| v == HOST),
        "…and the bar did not fall back to the route underneath: {:?}",
        h.values()
    );
}

/// The terminal route's menu affordance, by its accessible name —
/// transcribed from `crate::shell`, like [`BACK`]/[`GEAR`] above.
const TERMINAL_MENU: &str = "Terminal menu";

#[test]
fn the_terminal_menu_button_opens_its_side_sheet_directly_and_gives_the_bar_back_after_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();

    h.push("/terminal?session=s-1");
    let covered_depth = h.depth();
    h.park();
    assert_titled(&h, "terminal");

    // The real menu button, opening the "panes & tabs" side sheet directly —
    // no intervening dropdown.
    h.tap_label(TERMINAL_MENU);
    h.park();
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the side sheet is its own page on the host — above the bar it \
         anchors to, rather than clipped under it"
    );
    assert_eq!(h.depth(), covered_depth, "…over an untouched route stack");
    assert!(
        !h.has_bar(),
        "…and it takes the bar it anchors to out of the tree with it: {:?}",
        node_roles(&h.root)
    );

    // THE assertion: the overlay gives the bar back on the frames production
    // actually runs, with no unrelated input to save it — the host owes no
    // bookkeeping across the dismissal; it pops, and the app under it is live
    // again.
    h.tap_parked(SHEET_SCRIM_POINT);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the scrim tap popped the side sheet"
    );
    assert_eq!(h.depth(), covered_depth, "…and the route stayed put");
    assert!(
        h.has_bar(),
        "…and the bar is lit and addressable again with no further input: {:?}",
        node_roles(&h.root)
    );
    assert_titled(&h, "terminal");
    // In-shell pop too — the retitle shows the ‹ actually popped it.
    h.tap_label(BACK);
    h.settle();
    assert_titled(&h, HOST);
}

#[test]
fn the_bar_carries_no_font_fallback_glyph_any_more() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    // Both slots are vectors now; nothing in the accessibility tree may name
    // itself with the marks they used to set as text (see `BANNED_GLYPHS`).
    for glyph in BANNED_GLYPHS {
        assert!(
            !h.labels().iter().any(|l| l.contains(glyph)),
            "{glyph:?} survives as a label: {:?}",
            h.labels()
        );
        assert!(
            !h.values().iter().any(|v| v.contains(glyph)),
            "{glyph:?} survives as a run: {:?}",
            h.values()
        );
    }

    // …and the same on a back-led route, whose leading slot is the other one.
    h.push("/settings");
    for glyph in BANNED_GLYPHS {
        assert!(
            !h.labels().iter().any(|l| l.contains(glyph)),
            "{glyph:?} survives on a pushed route: {:?}",
            h.labels()
        );
    }
    assert!(
        h.labels().iter().any(|l| l == BACK),
        "…the back affordance is still there, named: {:?}",
        h.labels()
    );
}

/// **THE Concern-4 stale-bar scenario, made explicit end to end.** Pre-`d1`,
/// `Nav` mirrored `RouteMeta`/`BarState` behind the bar, corrected only "on
/// the next depth-fall reconcile" — the seam a mis-recorded depth used to
/// desync, evicting or stranding a route's bar entry. That mirror is deleted:
/// [`crate::routes::Nav::bar`] now derives every frame off the inner
/// navigator's live [`frust::RouteObserver`], nothing left to go stale. Walks
/// the full settings → sessions → connect round trip, asserting bar state at
/// every step off the accessibility tree.
#[test]
fn settings_back_sessions_back_connect_retitles_the_bar_at_every_step() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "/sessions wears the brand"
    );

    // settings — an in-shell push, off the same live observer.
    h.push("/settings");
    assert_titled(&h, "server settings");
    assert!(h.labels().iter().any(|l| l == BACK));

    // back → sessions — an in-shell pop, same observer, no mirror to desync.
    h.tap_label(BACK);
    h.settle();
    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "the brand is back too"
    );

    // back → connect — `/sessions` carries no ‹ (the shell's landing route),
    // so this is the platform back gesture: its own disconnect confirmation
    // (`back_on_sessions_asks_instead…`'s own test), confirmed here to
    // actually leave the shell.
    press_back(&mut h);
    assert!(
        showing_confirmation(&h),
        "the disconnect confirmation is up"
    );
    let action = confirmation_action(&h, DISCONNECT_ACTION);
    h.tap_parked(action);
    h.settle();
    h.park();

    assert!(
        !h.has_bar(),
        "connect is bar-less, the last step of the round trip: {:?}",
        node_roles(&h.root)
    );
}

// ---------------------------------------------------------------------------
// 3e — back on /sessions is a question, not a pop
// ---------------------------------------------------------------------------
//
// `/sessions` is the one route pushed with `BackPolicy::DismissAnimated`, so a
// back press there **bumps the page's dismiss cell instead of popping it** and
// the screen raises its own disconnect confirmation. This belongs here, not
// `sessions.rs`, which mounts the screen beside a navigator rather than as a
// page pushed through `Nav`, so nothing there carries the policy read here.
//
// What the *bar* contributes is the negative half: neither stack moves, so the
// mirror never hears of the press — `/sessions` is still titled once the
// question is answered "no", and while it is up the bar is not addressable at
// all (the host forwards its top page only).

/// The confirmation's title, as the user reads it — transcribed rather than
/// imported, like [`BRAND`]/[`BACK`]/[`GEAR`] above (`features::sessions`'
/// constants are the implementation's; a test that imported them could not
/// catch one of them changing).
const DISCONNECT_TITLE: &str = "Disconnect from server?";
/// …its destructive action…
const DISCONNECT_ACTION: &str = "Disconnect";
/// …and the affordance that answers "no" (`ui::dialogs`' shared spelling).
const CANCEL_ACTION: &str = "Cancel";

/// Whether the app still holds its (mock) connection.
fn connected() -> bool {
    expect_connection_controller().connected.get_untracked()
}

/// Whether the disconnect confirmation is the thing on screen — title, body
/// naming the connected host, and destructive action.
fn showing_confirmation(h: &Harness) -> bool {
    h.values().iter().any(|v| v == DISCONNECT_TITLE)
        && h.values().iter().any(|v| v.contains(HOST))
        && h.labels().iter().any(|l| l == DISCONNECT_ACTION)
}

/// One platform back press, driven to the app's parking point.
fn press_back(h: &mut Harness) {
    frust::push_back_press();
    h.settle();
    h.park();
}

/// Where a tap must land to take the confirmation's `label` action.
fn confirmation_action(h: &Harness, label: &str) -> Point {
    node_centers_by_label(&h.root, label)
        .first()
        .copied()
        .unwrap_or_else(|| {
            panic!(
                "the confirmation carries no {label:?} action: {:?}",
                h.labels()
            )
        })
}

#[test]
fn back_on_sessions_asks_instead_of_popping_and_bumps_the_cell_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    let route_depth = h.depth();
    let generation = sessions_back_signal().get();

    press_back(&mut h);

    assert_eq!(
        h.depth(),
        route_depth,
        "the route stack is untouched — the page was asked, not popped"
    );
    assert_eq!(
        sessions_back_signal().get(),
        generation + 1,
        "…the navigator bumped the page's dismiss cell exactly once"
    );
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "…and the screen turned that into one confirmation"
    );
    assert!(
        showing_confirmation(&h),
        "…the disconnect confirmation, naming the host: {:?} / {:?}",
        h.values(),
        h.labels()
    );
    assert!(connected(), "nothing is disconnected until it is confirmed");
    assert!(
        !h.has_bar(),
        "…and the app under the confirmation is inert, bar included: {:?}",
        node_roles(&h.root)
    );
}

#[test]
fn a_bump_that_lands_while_the_confirmation_is_up_is_swallowed() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();

    press_back(&mut h);
    assert_eq!(h.overlay_depth(), REST_DEPTH + 1, "the confirmation is up");

    // A bump reaching the screen while its confirmation is already showing —
    // what the navigator would do if a press ever got past the overlay host
    // (it cannot: R44-back gives the host the press, which is the *next* test).
    // The screen banks it and raises nothing.
    let signal = sessions_back_signal();
    signal.set(signal.get().wrapping_add(1));
    h.park();

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "still exactly one confirmation, not two stacked"
    );
    assert!(showing_confirmation(&h), "…and it is still the same one");

    // The half that makes the assertion above mean something: a swallowed bump
    // must be *spent*, not merely deferred. Dismiss the confirmation and the
    // screen owes nothing — an unbanked bump would raise its dialog here, one
    // press behind the user.
    press_back(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the dismissal left no queued second confirmation: {:?}",
        h.values()
    );
    assert!(connected(), "…and nothing was disconnected");
}

#[test]
fn a_second_back_press_cancels_the_confirmation_and_stays_connected() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    let route_depth = h.depth();

    press_back(&mut h);
    assert_eq!(h.overlay_depth(), REST_DEPTH + 1, "the confirmation is up");

    // The overlay host outranks the route navigator (R44-back), so the second
    // press dismisses the confirmation rather than reaching the page again —
    // "back again" reads as a cancel with no code in the screen at all.
    press_back(&mut h);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the second press dismissed the confirmation"
    );
    assert_eq!(h.depth(), route_depth, "…and popped no route with it");
    assert!(connected(), "…leaving the connection alone");
    assert_titled(&h, HOST);

    // The seam re-arms: a dismissed confirmation is not a spent one.
    press_back(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the next press asks again"
    );
    assert!(showing_confirmation(&h));

    // …and the explicit "no" reads the same as the press did.
    let cancel = confirmation_action(&h, CANCEL_ACTION);
    h.tap_parked(cancel);
    h.settle();
    h.park();

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "Cancel dismissed the confirmation"
    );
    assert_eq!(h.depth(), route_depth, "…popping no route");
    assert!(connected(), "…and disconnecting nothing");
    assert_titled(&h, HOST);
}

#[test]
fn confirming_the_disconnect_drops_the_connection_and_returns_to_connect() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();

    press_back(&mut h);
    let action = confirmation_action(&h, DISCONNECT_ACTION);

    h.tap_parked(action);
    h.settle();
    h.park();

    assert!(!connected(), "the confirmed answer disconnected");
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "…the confirmation closed with it"
    );
    assert_eq!(h.depth(), 1, "…and `/sessions` popped back to Connect");
    assert!(
        !h.has_bar(),
        "…which is the bar-less route: {:?}",
        node_roles(&h.root)
    );
}

#[test]
fn back_on_the_terminal_route_still_pops_to_sessions_with_no_question_asked() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    h.push("/terminal?session=s-1");
    assert_titled(&h, "terminal");
    let generation = sessions_back_signal().get();

    // Only `/sessions` carries `DismissAnimated`; every other route keeps the
    // default `Pop`, so this press is the plain (in-shell) pop it always was.
    press_back(&mut h);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "…asking nothing on the way: {:?}",
        h.values()
    );
    assert_eq!(
        sessions_back_signal().get(),
        generation,
        "…and never touching the `/sessions` cell"
    );
    assert!(connected(), "…nor the connection");
    assert_titled(&h, HOST);
}

// ---------------------------------------------------------------------------
// 4 — the GlyphSlide route transition, and its reduced-motion collapse
// ---------------------------------------------------------------------------

/// The x of the biggest rounded rect on screen — the Connect screen's own
/// chrome, which dwarfs anything the bar paints (its brand cells are 7×7). A
/// slide moves a page's pod origin, so this one number tracks the whole
/// leaving page.
fn leaving_page_x(scene: &RecScene) -> f64 {
    largest(&scene.rounded)
        .unwrap_or_else(|| panic!("the leaving page painted no chrome to track"))
        .0
        .x
}

/// The recorded rect in `rects` matching `size` within [`EPSILON`] on both
/// dimensions — re-identifies a page's chrome by SHAPE once its position or
/// presence changes. Local to this file (width-only `support::by_width`
/// would be ambiguous here; `tests/support/mod.rs` is read-only for this card).
fn rect_sized(rects: &[(Point, Size)], size: Size) -> Option<(Point, Size)> {
    rects.iter().copied().find(|(_, s)| {
        (s.width - size.width).abs() < EPSILON && (s.height - size.height).abs() < EPSILON
    })
}

/// The biggest rounded rect in `page` whose size nothing in `other` shares —
/// chrome that names this page alone once both are on screen at once.
fn distinctive_chrome(page: &[(Point, Size)], other: &[(Point, Size)]) -> Option<(Point, Size)> {
    let alone: Vec<(Point, Size)> = page
        .iter()
        .copied()
        .filter(|(_, size)| rect_sized(other, *size).is_none())
        .collect();
    largest(&alone)
}

/// Push `/settings` off `/`; report the settled leaving-page frame plus
/// frames at [`PRE_SPLIT_MS`]/[`POST_SPLIT_MS`] — either side of the split.
fn probe_push_transition(reduce_motion: bool) -> (RecScene, RecScene, RecScene) {
    let mut h = Harness::themed(theme(reduce_motion));
    let settled = h.frame();

    // Pushed off `/`, so the leaving page is Connect — the one screen that
    // paints plenty of trackable chrome. The connection is what the session
    // guard wants of every in-shell route.
    expect_connection_controller().connect(mock_target());
    app_nav().push("/settings");
    h.frame_at(0.0);
    let pre_split = h.frame_at(PRE_SPLIT_MS);
    let post_split = h.frame_at(POST_SPLIT_MS);
    (settled, pre_split, post_split)
}

#[test]
fn a_route_push_slides_the_leaving_page_then_reveals_the_entering_one() {
    let _g = serial();
    let _ambient = setup();

    let (settled, pre_split, post_split) = probe_push_transition(false);

    // Settings' resting chrome — a fresh, un-transitioned push, not the
    // transitioning harness above.
    let mut rest = Harness::mount();
    expect_connection_controller().connect(mock_target());
    rest.push("/settings");
    let entering_rest = rest.frame();

    // Each page is re-identified by size once displaced, so its fingerprint
    // must be chrome the OTHER page never paints: both screens carry the same
    // 38×22 toggle track (Connect's trust switch, Settings' bell row), and the
    // biggest rect alone would match it on the wrong side of the split.
    let (settled_pos, settled_size) = distinctive_chrome(&settled.rounded, &entering_rest.rounded)
        .expect("the settled leaving page painted chrome Settings does not");
    let (entering_pos, entering_size) =
        distinctive_chrome(&entering_rest.rounded, &settled.rounded)
            .expect("the settled entering page painted chrome Connect does not");

    // Pre-split: Connect still paints, displaced by `-p * 16dp`; Settings'
    // alpha is exactly `0` there, so it paints nothing yet.
    let (pre_pos, _) = rect_sized(&pre_split.rounded, settled_size).unwrap_or_else(|| {
        panic!(
            "Connect's {settled_size:?} chrome is gone before the fade split: {:?}",
            pre_split.rounded
        )
    });
    assert!(
        pre_pos.x < settled_pos.x - EPSILON,
        "the leaving page is displaced before the split ({} vs {})",
        pre_pos.x,
        settled_pos.x
    );
    assert!(
        settled_pos.x - pre_pos.x <= 16.0 + EPSILON,
        "…by at most the preset's 16dp travel ({} px)",
        settled_pos.x - pre_pos.x
    );
    assert!(
        rect_sized(&pre_split.rounded, entering_size).is_none(),
        "Settings' {entering_size:?} chrome already paints before the split: {:?}",
        pre_split.rounded
    );

    // Post-split: Glyph is a split cross-fade (`resolve_layers`'s `Glyph`
    // arm) — one page paints per instant, the other goes to `DiscardScene`
    // (frust `5fc1513a`), so "both pages paint" is only assertable ACROSS
    // these two samples, never within one frame. Connect's chrome is gone…
    assert!(
        rect_sized(&post_split.rounded, settled_size).is_none(),
        "Connect's {settled_size:?} chrome still paints past the split: {:?}",
        post_split.rounded
    );
    // …Settings has taken over instead, displaced by `(1 - p) * 16dp`.
    let (post_pos, _) = rect_sized(&post_split.rounded, entering_size).unwrap_or_else(|| {
        panic!(
            "Settings' {entering_size:?} chrome hasn't painted past the split: {:?}",
            post_split.rounded
        )
    });
    assert!(
        post_pos.x > entering_pos.x + EPSILON,
        "the entering page is displaced past the split ({} vs {})",
        post_pos.x,
        entering_pos.x
    );
    assert!(
        post_pos.x - entering_pos.x <= 16.0 + EPSILON,
        "…by at most the preset's 16dp travel ({} px)",
        post_pos.x - entering_pos.x
    );
}

#[test]
fn reduce_motion_collapses_the_route_slide_to_a_pure_crossfade() {
    let _g = serial();
    let _ambient = setup();

    let (settled, _pre_split, post_split) = probe_push_transition(true);
    let base_x = leaving_page_x(&settled);
    let mid_x = leaving_page_x(&post_split);

    // The Glyph design system's hard accessibility rule, as geometry: a reduced
    // transition is a ≤120ms linear cross-fade with **no** slide, so the
    // leaving page never moves. It is still painting (the cross-fade needs it),
    // which is what makes this an assertion rather than a vacuous read.
    assert!(
        (mid_x - base_x).abs() < EPSILON,
        "reduced motion must not slide the leaving page ({mid_x} vs {base_x})"
    );
}

#[test]
fn the_bar_still_retitles_under_reduce_motion() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::themed(theme(true));
    expect_connection_controller().connect(mock_target());

    // The bar's own crossfades collapse to `PatternSwitcher`'s fast linear
    // cross-fade under `reduce_motion`; what must survive the collapse is the
    // content swap itself.
    h.push("/sessions");
    assert_titled(&h, HOST);
    h.push("/settings");
    assert_titled(&h, "server settings");
    assert!(
        h.labels().iter().any(|l| l == BACK),
        "…back button included: {:?}",
        h.labels()
    );
}

// Section 5 (the title crossfade staging two runs) is gone: the bar is
// `frust_glyph::AppBarView` now, and its crossfade is Glyph's to test —
// `assert_titled` above already exercises a live title change end to end.

// ---------------------------------------------------------------------------
// 6 — the stable-container invariant, at the shell boundary
// ---------------------------------------------------------------------------
//
// `a_hidden_bar_routes_no_events_and_reserves_no_box` (the old floating-bar
// hit-test tripwire) is gone: `/` never places the shell, so there is no
// scaffold and so no bar to be mounted-but-invisible — impossible by
// construction now, covered by `connect_carries_no_bar_at_all`.

#[test]
fn the_navigator_survives_the_bar_appearing_and_disappearing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target());

    // A torn-down outer navigator loses its whole page stack, so `h.depth()`
    // reads "the shell page reconciled in place" across placement, an
    // in-shell push, and removal (`tests/shell_handoff.rs`: the same
    // container-identity mechanism, on widget identity).
    let start_depth = h.depth();
    h.push("/sessions");
    assert!(h.has_bar());
    assert_eq!(
        start_depth + 1,
        h.depth(),
        "placing the shell is the one outer op"
    );

    // In-shell push (sibling route, same `shell_route`): zero further outer ops.
    h.push("/settings");
    assert_eq!(start_depth + 1, h.depth(), "still the one shell page");

    // A bare pop, not a back press (`/sessions`' press is a *question* now,
    // section 3e) — the stack mutation a confirmed answer makes.
    h.tap_label(BACK);
    h.settle();
    h.state.nav.controller().pop();
    h.settle();

    assert_eq!(
        h.depth(),
        start_depth,
        "back at Connect, the shell page gone with it"
    );
    assert!(!h.has_bar(), "…and the bar collapsed with it");
}

// ---------------------------------------------------------------------------
// 7 — gone: the bar no longer floats
// ---------------------------------------------------------------------------
//
// This section used to pixel-track the floating bar's own entrance/exit
// (brand-mark `y`, conn-row dot, Sessions' full-surface tone) since nothing
// else observed it, and to pin "a page's box never reflows". The scaffold
// inverts that premise on purpose — `frust::scaffold` *does* reflow the body
// around a mounted `app_bar` (R-B4-inset), which is the fix for the
// double-inset bug this arc kills — and the bar's own entrance/exit/paint
// order are `frust_glyph::AppBarView`'s to test now. What survives, above,
// through the accessibility tree: bar presence per route (section 1),
// retitling (section 2), overlay modality (3/3b/3c/3d), and the full
// settings → sessions → connect round trip
// (`settings_back_sessions_back_connect_retitles_the_bar_at_every_step`).
