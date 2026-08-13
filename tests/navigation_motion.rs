//! Headless coverage for muxr's **navigation motion**: the shell's persistent
//! top bar (`src/shell.rs` + `src/ui/topbar.rs`), the route-meta mirror behind
//! it (`src/routes.rs`), and the GlyphSlide page transition every path
//! navigation carries.
//!
//! # Why this suite mounts the *whole* app
//!
//! Every other screen suite deliberately mounts its screen *beside* a
//! navigator, because `NavigatorWidget` contributes no `Widget::semantics` and
//! forwards none of its pages' — so a page inside a navigator is invisible to
//! the accessibility tree. The bar under test here has the opposite problem
//! solved for it: it is **shell** chrome, a fixed-index sibling of the
//! navigator in `shell::root_view`'s root `Stack`, so mounting the real
//! `MuxrApp` leaves it fully addressable by role/label/value while the routes
//! underneath stay opaque. That is exactly the split this suite wants — it
//! asserts on what the *bar* says, never on what a route renders.
//!
//! # Two observables, and why each is the honest one
//!
//! - **Semantics** ([`support::node_roles`]/[`node_labels`]/[`node_values`]) for
//!   everything the bar *is*: present or hidden (`Role::TitleBar`), what it
//!   titles, whether it wears the brand or the `‹`, whether it carries the `⚙`.
//!   A bar **mid-reveal** publishes nothing either (it routes nothing, so a
//!   node would lie), which is why every semantics assertion here settles first.
//! - **Recorded geometry** for the page transition. A `TransitionSpec` is not
//!   readable back off a `NavigatorController`, so "the Glyph slide was
//!   applied" is asserted where it is actually visible: mid-flight, the leaving
//!   page is still painting **and** displaced along x by the preset's 16dp
//!   travel. `resolve_spec`'s `reduce_motion` collapse is the same assertion
//!   with the displacement required to be *zero* — a pure cross-fade, the Glyph
//!   design system's hard accessibility rule. (The spec's identity —
//!   `PageTransition::Glyph` under `Timing::ThemeDefault` — is pinned by
//!   `routes.rs`' own unit tests, which can name the types directly.)
//!
//! # …and what the geometry proves about the bar *floating*
//!
//! Section 7 is the payoff of the shell restructure: the bar no longer shares a
//! band with the navigator, so a route gaining or losing it must not reflow the
//! page. Two rects carry that:
//!
//! - **The Sessions page tone** ([`page_tone`]) — `pieces::page_tone`'s
//!   full-bleed fill, and so the biggest rect that screen paints, which makes
//!   it a direct read on the *box the navigator laid the page out in*. It must
//!   be the whole surface, mid-transition and settled alike.
//! - **The brand-mark cells** ([`brand_mark_top`]) — `.brand-mark span`'s four
//!   7×7 tiles, the highest chrome on a barred screen, and so a direct read on
//!   where the *bar* is painting while it slides in or out. (The strip's
//!   `.dot-ok` is 7×7 too, which is why this reads the topmost one: the bar is
//!   above the strip at rest and only travels further up.)
//!
//! # Two navigators, and which depth an assertion means
//!
//! The app has **two** page stacks since the root overlay host landed
//! (`muxr::AppState::overlays`, FINDINGS #44): the *route* navigator
//! (`AppState::nav`'s controller, [`Harness::depth`]) and the *host* above all
//! the chrome, where every menu, sheet and dialog is a page
//! ([`Harness::overlay_depth`], `1` at rest). The pair is what the sections
//! below assert against, and the split is itself an assertion: **an overlay
//! never perturbs the route stack**, which is why every overlay test here reads
//! both numbers rather than the one it used to.
//!
//! It is also why an overlay makes the bar unaddressable at all: the host
//! forwards accessibility nodes (and routes input) for its top page only, so
//! the whole app under an open overlay — the bar included — is omitted from the
//! tree, and inert. That is the *only* mechanism now; the bar's own modality
//! mirroring (a counted flag, a scrim it painted over itself) was deleted with
//! the adoption, and sections 3b–3d are what prove nothing went with it.
//!
//! # The cold-start splash, spent up front
//!
//! `MuxrApp`'s first *build* in a process claims the one cold-start splash, so
//! whichever test ran first would otherwise mount an overlay the others don't —
//! and its Connect copy would land in the recorded geometry. [`spend_cold_start`]
//! burns that claim on a throwaway app before any fixture mounts, so every test
//! here runs a warm start.

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
/// them changing.
///
/// Both slots used to be labelled by the glyph they set as text (`‹`/`⚙`),
/// because the mark *was* the label. They are vectors now (see `ui::topbar`'s
/// "Chrome icons are vectors" rule) and carry names instead — which is also
/// what a screen reader now announces in place of "single left-pointing angle
/// quotation mark".
const BRAND: &str = "muxr";
const BACK: &str = "Back";
const GEAR: &str = "Settings";

/// The two glyphs that used to stand in for those affordances. **Nothing in
/// the bar may carry either again**: neither is in the bundled Glyph faces, so
/// each platform's font-fallback chain answered differently (the `⚙` was
/// simply absent on iPhone) — the round-4 device finding this suite pins shut.
const BANNED_GLYPHS: [&str; 2] = ["\u{2039}", "\u{2699}"];

/// How far into a transition the geometry probes paint. Early enough that the
/// reduced-motion cross-fade (≤120ms) has **not** settled, so both frames
/// compare like with like.
const MID_TRANSITION_MS: f64 = 60.0;

/// `.brand-mark span` — one of the mark's four 7×7 tiles (`MARK_SIZE 16` less
/// the 2px gap, halved). Transcribed rather than imported, like the glyphs
/// above: `ui::topbar`'s constants are the implementation's.
const MARK_CELL: f64 = 7.0;
/// `.topbar{height:46px}` — the band the bar occupies and a bar-bearing page
/// clears. Transcribed for the same reason.
const BAR_HEIGHT: f64 = 46.0;
/// `.conn-row{padding:13px 16px}` — the strip's own top padding, inside the
/// clearance. Transcribed.
const STRIP_PAD_Y: f64 = 13.0;
/// The `.dot-ok`'s resting `y` on `/sessions`, at a zero window inset: the
/// [`BAR_HEIGHT`] band the page clears, [`STRIP_PAD_Y`] of strip padding, and
/// the row's own centering of the 7px dot in its `body_medium` line box.
///
/// A **transcribed-from-the-build** tripwire rather than a derived number (the
/// last term is a shaped-text metric): it is the y the pre-float, band-based
/// shell produced, so a change here means the restructure moved resting
/// content — the one thing it promised not to do.
const STRIP_DOT_Y: f64 = 65.5;

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
    let mut theme = Theme::glyph_baseline();
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

    /// One frame, run the way `frust-shell-desktop`'s loop runs one: the
    /// rebuild inside a [`TrackedScope`] (so a signal written *after* it is
    /// observable), then layout, then paint. Returns the painted frame and
    /// whether the app asked for another — `PaintOutcome::needs_frame` (an
    /// animation still in flight) or a tracked write since the rebuild
    /// (`TrackedScope::is_dirty`), which are precisely the two conditions that
    /// shell turns into a `request_redraw`.
    ///
    /// # The one request that does not count as "still converging"
    ///
    /// A frame whose **every** request was `TickClass::CosmeticLoop`
    /// (`PaintOutcome::needs_frame_paced_only`) reports `false` here. That is
    /// the framework's own name for a loop that never settles by design — on
    /// `/terminal` it is the caret's 1.1s blink — and the mobile frame gate
    /// throttles it for exactly that reason. Counting it would make
    /// [`Harness::park`] unusable on the one screen the menu → sheet chain
    /// lives on, and it costs nothing: a paced-only frame is by construction a
    /// frame in which no transition, no reveal and no staging is in flight.
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
    assert!(
        h.values().iter().any(|v| v == title),
        "the bar titles {title:?}: {:?}",
        h.values()
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
fn round_trip_through(route: &str, title: &str) {
    let mut h = Harness::on_sessions();
    let base_depth = h.depth();

    h.push(route);
    assert_eq!(h.depth(), base_depth + 1, "{route} was pushed");
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

    assert_eq!(h.depth(), base_depth, "the ‹ popped the route");
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
    let base_depth = h.depth();

    h.tap_label(GEAR);
    h.settle();

    assert_eq!(h.depth(), base_depth + 1, "the ⚙ pushes a page");
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
// Both halves used to be the bar's own doing: a counted modality flag on
// `BarState` drove a scrim `ui::topbar` painted over itself and withdrew the
// bar's input and semantics. It was deleted with the root overlay host, because the
// host does them properly — it routes input (and publishes accessibility
// nodes) for its **top page only**, and an overlay's scrim is painted at the
// root, over the floating bar rather than mirrored onto it. These tests are the
// proof that nothing user-visible went with the deletion.

/// Every plain fill covering exactly the bar's own box, in paint order. At a
/// zero window inset that box is `W × BAR_HEIGHT` at the origin, and the bar's
/// own surface fills it on every frame it paints.
fn bar_band_fills(scene: &RecScene) -> Vec<peniko::Color> {
    support::fills_at(scene, Point::ZERO, Size::new(support::W, BAR_HEIGHT))
}

/// Every plain fill covering the **whole surface**, paired with its index in
/// paint order — where an overlay's own scrim lands, since it fills
/// `ctx.origin()..ctx.size()` and at the root that rect *is* the window.
fn full_surface_fills(scene: &RecScene) -> Vec<(usize, peniko::Color)> {
    let whole = (Point::ZERO, Size::new(support::W, support::H));
    scene
        .fills
        .iter()
        .enumerate()
        .filter(|(_, (o, s, _))| {
            (o.x - whole.0.x).abs() < EPSILON
                && (o.y - whole.0.y).abs() < EPSILON
                && (s.width - whole.1.width).abs() < EPSILON
                && (s.height - whole.1.height).abs() < EPSILON
        })
        .map(|(index, (_, _, color))| (index, *color))
        .collect()
}

#[test]
fn a_sheets_own_scrim_is_painted_over_the_bar_and_lifts_with_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    // At rest the only full-surface fill is the page's own opaque tone;
    // nothing dims anything.
    let lit = h.park();
    assert!(
        full_surface_fills(&lit)
            .iter()
            .all(|(_, color)| support::alpha_of(*color) >= 1.0 - 1e-6),
        "nothing is dimmed at rest: {:?}",
        full_surface_fills(&lit)
    );

    open_real_sheet(&mut h);
    let dimmed = h.park();

    // The sheet's scrim, at the alpha it dims the page under it with — and
    // there is only ONE of them, because the bar no longer mirrors it with a
    // second wash of its own.
    let scrims: Vec<_> = full_surface_fills(&dimmed)
        .into_iter()
        .filter(|(_, color)| {
            (support::alpha_of(*color) - muxr::ui::sheet::SCRIM_ALPHA).abs() < 1e-6
        })
        .collect();
    assert_eq!(scrims.len(), 1, "exactly one scrim: {scrims:?}");

    // THE assertion: it is painted *after* the bar's own surface, so it covers
    // the bar rather than sliding up under it (FINDINGS #44's whole point).
    let bar_band = dimmed
        .fills
        .iter()
        .position(|(o, s, _)| {
            (o.x - 0.0).abs() < EPSILON
                && (o.y - 0.0).abs() < EPSILON
                && (s.width - support::W).abs() < EPSILON
                && (s.height - BAR_HEIGHT).abs() < EPSILON
        })
        .expect("the bar paints its own band");
    assert!(
        scrims[0].0 > bar_band,
        "the scrim ({}) is painted over the bar's band ({bar_band})",
        scrims[0].0
    );
    // …and the bar's band itself carries no second fill: the mirrored tint is
    // gone, not merely invisible.
    assert_eq!(
        bar_band_fills(&dimmed).len(),
        1,
        "the bar paints its surface and nothing else: {:?}",
        bar_band_fills(&dimmed)
    );

    h.tap_parked(SHEET_SCRIM_POINT);
    let lifted = h.park();
    assert!(
        full_surface_fills(&lifted)
            .iter()
            .all(|(_, color)| support::alpha_of(*color) >= 1.0 - 1e-6),
        "dismissing the sheet takes the dim with it: {:?}",
        full_surface_fills(&lifted)
    );
}

#[test]
fn the_gear_does_nothing_under_an_open_overlay_and_works_again_after() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    let base_depth = h.depth();

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

    // The modality hole this closes: the bar floats *over* the route
    // navigator, so an overlay pushed **there** could never cover it — before
    // the root host, that press pushed /settings out from under an open sheet.
    // Two things stop it now: the bar mirrors the modality itself (it dims and
    // withdraws its nodes), and the host routes input to its top page only, so
    // the press never reaches the bar at all.
    tap(&mut h.root, &mut h.state, gear);
    h.settle();

    // Asserted as "the route stack never moved" rather than the old "the
    // overlay is still the only thing on top": the overlay is not on the route
    // stack any more, so a ⚙ that *did* fire would show up here as a push
    // rather than as a second page over the modal.
    assert_eq!(h.depth(), base_depth, "the ⚙ pushed nothing");
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "…and the overlay is still the only thing up"
    );
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
    assert_eq!(h.depth(), base_depth + 1, "the ⚙ pushes a page once more");
    assert_titled(&h, "server settings");
}

// ---------------------------------------------------------------------------
// 3c — a closed modal gives the bar back, on the frames production actually runs
// ---------------------------------------------------------------------------
//
// Section 3b asserts *what* the bar does under a modal; this one asserts
// **when** — and it is the only section here that refuses the harness's
// courtesy frames (see [`Harness::park`]).
//
// The defect it pins shut: the bar's modality used to be derived from
// `NavigatorController::depth`, and `RenderRoot::rebuild` runs the whole
// app-logic closure — this bar included — *before* the navigator drains its
// queued ops. So the one frame that followed a modal's pop still published the
// dimmed state, nothing asked for another, and the loop parked with the bar
// dark and its affordances inert until unrelated input happened along. The
// host answers on the frame it actually applies the op (it reconciles before
// the page under it rebuilds), which is what let the app-side accounting that
// papered over the gap be deleted. Every test below dismisses a **real**
// overlay (the app's own `show_sheet`/`confirm_dialog`) and then asserts with
// no frames in between.

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
    // Lit is half of it; live is the other half. The very affordance the
    // modality suppressed works again.
    h.tap_label(GEAR);
    h.settle();
    assert_eq!(h.depth(), covered_depth + 1, "the ⚙ pushes /settings again");
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
/// rather than one more navigator.
///
/// The press is delivered the way a platform delivers one: through
/// `frust::push_back_press`, the process-wide source the facade's own wiring
/// arbitrates behind (a root overlay host outranks every plain navigator, so an
/// open overlay claims the press ahead of the route stack under it). Nothing
/// here names a controller to pop — that choice **is** what is under test.
///
/// It doubles as the original defect's guard: the press reaches this sheet as
/// `BackPolicy::DismissAnimated`, so the widget plays its own staged exit and
/// pops itself, and the app hears about the dismissal only through the page's
/// completion latch. The bar must be lit and live again regardless — which it
/// is, because it never knew about the overlay in the first place.
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
    assert_eq!(h.depth(), covered_depth + 1, "the ⚙ works again");
}

// ---------------------------------------------------------------------------
// 3d — an overlay's whole life leaves the route mirror untouched
// ---------------------------------------------------------------------------
//
// Section 3c asserts that a *closed* modal gives the bar back. This one covers
// the two handler shapes that used to corrupt the route-meta mirror while an
// overlay opened or closed, both of them one-rebuild staleness read as fact:
//
// - a handler that closes an overlay *and* pushes a route in one rebuild (the
//   Sessions Attach row, verbatim) recorded that route one too deep —
//   permanently — because `NavigatorController::depth` still counted the
//   overlay's page, and the next depth fall past it evicted the route's own bar
//   entry;
// - an overlay opened out of the overlay before it (the terminal's menu →
//   sheet chain), whose intermediate frames used to strand a dimmed, inert bar.
//
// Both are structurally impossible now — an overlay is a page of the host, so
// the route depth never moves at all — and both are still asserted end to end,
// at the parking point, with no courtesy frames: this is where that claim is
// paid for rather than assumed.

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
    let covered_depth = h.depth();

    let sheet = open_sheet_with_handle(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the sheet is its own page on the host"
    );

    // The Attach row's own two calls (`features::sessions`' `sheet_action`:
    // `sheet.close(); actions.attach(&id)`), run against the **real** handle
    // and back to back — which is the whole shape that matters: no frame lands
    // between them, so the close's queued pop is still counted by the host's
    // depth while the route push below is being recorded. (Driven from here
    // rather than by pressing a row: a sheet's body lives inside a navigator,
    // where nothing is addressable by semantics.)
    sheet.close();
    app_nav().push("/settings");
    h.park();

    assert_eq!(
        h.depth(),
        covered_depth + 1,
        "the route pushed onto the stack the sheet never joined"
    );
    assert_eq!(h.overlay_depth(), REST_DEPTH, "…and the sheet's page went");
    assert_titled(&h, "server settings");

    // The next input pass — the user's next touch — delivers the sheet's
    // pop-completion callback, which is what latches its handle.
    h.dispatch(&pointer(PointerPhase::Move, IDLE_MOVE));

    // And now the eviction the mis-recorded depth used to cause: the *first*
    // overlay to open and close over that route (on `/terminal` it is the ⋮
    // menu) drops the live depth back past the entry, and a retain measured
    // against a too-deep record takes the route's own bar with it.
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
    assert_eq!(h.depth(), covered_depth + 1, "…and it alone");
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

/// The terminal route's overflow affordance, by its accessible name —
/// transcribed from `crate::shell`, like [`BACK`]/[`GEAR`] above.
const TERMINAL_MENU: &str = "Terminal menu";
/// `frust::glyph::menu`'s own panel gutter and item-row height, transcribed
/// (both are private to that module, and a test that imported them could not
/// catch one of them changing).
///
/// # Why this suite pixel-hunts a menu row where `tests/terminal.rs` refuses to
///
/// That suite selects a row by popping the menu page with its documented
/// `PopResult::of(index)` contract, because it only needs the *action* a row
/// runs. This one needs the widget's own dismissal path: the menu pops **itself
/// from inside its paint**, with no app seam in between, and that self-pop is
/// precisely the unobserved depth fall the defect below turns on. So the row is
/// pressed for real — and a row is reachable by neither semantics (it lives in
/// the navigator) nor a rect of its own (the panel paints its rows internally),
/// leaving its geometry: the first item is one [`MENU_PANEL_PAD`] into the
/// panel, [`MENU_ROW_H`] tall.
const MENU_PANEL_PAD: f64 = 6.0;
const MENU_ROW_H: f64 = 34.0;

#[test]
fn the_terminal_menu_chains_into_its_sheet_and_gives_the_bar_back_after_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();

    h.push("/terminal?session=s-1");
    let covered_depth = h.depth();
    let before = h.park();
    assert_titled(&h, "terminal");

    // The real ⋮, opening the real `frust::glyph` menu — the one overlay in
    // the app the app itself never gets to observe closing: a menu dismissal
    // is popped from inside the widget's own paint, with no app seam in
    // between. Nothing app-side has to hear about it any more, which is the
    // point of the chain below.
    h.tap_label(TERMINAL_MENU);
    let open = h.park();
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the menu is its own page on the host — above the bar it anchors to, \
         rather than clipped under it"
    );
    assert_eq!(h.depth(), covered_depth, "…over an untouched route stack");
    assert!(
        !h.has_bar(),
        "…and it takes the bar it is anchored to out of the tree with it — a \
         menu paints no scrim, so inertness is the whole of its modality: {:?}",
        node_roles(&h.root)
    );

    // "Panes & tabs" — the menu's first row, and the one that chains straight
    // into another overlay.
    let panel =
        largest(&added(&before.rounded, &open.rounded)).expect("the menu paints its own panel");
    let first_row = Point::new(
        panel.0.x + panel.1.width / 2.0,
        panel.0.y + MENU_PANEL_PAD + MENU_ROW_H / 2.0,
    );
    h.tap_parked(first_row);

    // No hand-off pass, no gap: the rebuild that pops the menu also flushes
    // its completion callback (the navigator's housekeeping broadcast), so the
    // `open_sheet` it carries runs on the pop's own frame — the sheet is
    // already standing where the menu was by the time the park returns,
    // without a single further input.
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the sheet took the menu's place on the pop's own frame"
    );
    assert!(
        !h.has_bar(),
        "…and the bar is modal again under it: {:?}",
        node_roles(&h.root)
    );

    // THE assertion, and the one an overlay opened *out of another overlay*
    // used to fail: the second dismissal has to give the bar back on the frames
    // production actually runs, with no unrelated input to save it. The old
    // accounting could only manage that if the menu's unobserved depth fall had
    // been spent before the sheet counted itself; the host owes no such
    // bookkeeping — it pops, and the app under it is live again.
    h.tap_parked(SHEET_SCRIM_POINT);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the scrim tap popped the sheet"
    );
    assert_eq!(h.depth(), covered_depth, "…and the route stayed put");
    assert!(
        h.has_bar(),
        "…and the bar is lit and addressable again with no further input: {:?}",
        node_roles(&h.root)
    );
    assert_titled(&h, "terminal");
    h.tap_label(BACK);
    h.settle();
    assert_eq!(h.depth(), covered_depth - 1, "…and its ‹ pops the route");
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

#[test]
fn a_back_press_that_bypassed_the_handle_still_retitles_the_bar() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    let base_depth = h.depth();

    h.push("/settings");
    assert_titled(&h, "server settings");

    // The Android path: the platform back press routes through the navigator's
    // own `request_back`, which `Nav` — and so the bar's mirror — never sees.
    // Only the depth falling gives it away.
    h.state.nav.controller().request_back();
    h.settle();

    assert_eq!(h.depth(), base_depth, "the back press popped the route");
    assert_titled(&h, HOST);
    assert!(
        !h.labels().iter().any(|l| l == BACK),
        "…and the ‹ went with it: {:?}",
        h.labels()
    );
}

// ---------------------------------------------------------------------------
// 3e — back on /sessions is a question, not a pop
// ---------------------------------------------------------------------------
//
// `/sessions` is the one route pushed with `BackPolicy::DismissAnimated`
// (`routes::Nav::push`), so a back press there **bumps the page's dismiss cell
// instead of popping it** and the screen raises its own disconnect
// confirmation. This suite is where that belongs — not `sessions.rs`, which
// mounts the screen *beside* a navigator rather than as a page pushed through
// `Nav`, so nothing there carries the policy the arbitration reads. Here the
// press is delivered exactly as section 3c's R44-back test delivers one
// (`frust::push_back_press`, the process-wide platform source), against the
// whole app, and every claim below is read off the two real stacks.
//
// What the *bar* contributes is the negative half, which is the whole point of
// the feature: neither stack moves, so the mirror never hears of the press —
// `/sessions` is still titled once the question is answered "no", and while it
// is up the bar is not addressable at all (the host forwards its top page
// only).

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
    let base_depth = h.depth();

    h.push("/terminal?session=s-1");
    assert_titled(&h, "terminal");
    let generation = sessions_back_signal().get();

    // Only `/sessions` carries `DismissAnimated`; every other route keeps the
    // default `Pop`, so this press is the plain pop it always was.
    press_back(&mut h);

    assert_eq!(h.depth(), base_depth, "the back press popped `/terminal`");
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

/// Push `/settings` off `/` and report `(settled x, mid-transition x)` for the
/// leaving Connect page, plus the mid-transition glyph-run count.
fn probe_push_transition(reduce_motion: bool) -> (f64, f64, usize) {
    let mut h = Harness::themed(theme(reduce_motion));
    let settled = h.frame();
    let base_x = leaving_page_x(&settled);

    // `/settings` is unguarded, so this needs no connection — which keeps the
    // probe on the one screen that paints plenty of trackable chrome.
    app_nav().push("/settings");
    h.frame_at(0.0);
    let mid = h.frame_at(MID_TRANSITION_MS);
    (base_x, leaving_page_x(&mid), mid.glyph_runs)
}

#[test]
fn a_route_push_slides_the_leaving_page_and_paints_both() {
    let _g = serial();
    let _ambient = setup();

    let (base_x, mid_x, mid_runs) = probe_push_transition(false);

    // The Glyph preset's leaving layer travels `-p * 16dp` along x…
    assert!(
        mid_x < base_x - EPSILON,
        "the leaving page is displaced mid-transition ({mid_x} vs {base_x})"
    );
    assert!(
        base_x - mid_x <= 16.0 + EPSILON,
        "…by at most the preset's 16dp travel ({} px)",
        base_x - mid_x
    );

    // …and both pages paint while it does, which an un-animated push would
    // never do (a `PageTransition::None` push swaps in one frame).
    let mut settled = Harness::mount();
    settled.push("/settings");
    let after = settled.frame();
    assert!(
        mid_runs > after.glyph_runs,
        "mid-transition both pages paint ({mid_runs} runs) where the settled \
         frame paints only the new one ({} runs)",
        after.glyph_runs
    );
}

#[test]
fn reduce_motion_collapses_the_route_slide_to_a_pure_crossfade() {
    let _g = serial();
    let _ambient = setup();

    let (base_x, mid_x, _) = probe_push_transition(true);

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

// ---------------------------------------------------------------------------
// 5 — the title crossfade actually stages two runs
// ---------------------------------------------------------------------------

/// The bar's own crossfade, mounted standalone.
///
/// Inside the app the bar's runs are lost among a route's, so this drives
/// [`muxr::ui::topbar::muxr_topbar`] alone and counts glyph runs: a bar showing
/// one title paints the wordmark plus that title, and a bar **mid-crossfade**
/// paints the outgoing run as well. That extra run is the whole assertion —
/// an instant title swap could never produce it.
#[test]
fn a_title_change_paints_both_runs_while_it_crosses_over() {
    use std::cell::RefCell;
    use std::rc::Rc;

    let _g = serial();
    let _ambient = setup();

    let title = Rc::new(RefCell::new(String::from("alpha")));
    let title_for_logic = Rc::clone(&title);
    let mut logic = move |_: &mut ()| -> AnyView<()> {
        any(muxr::ui::topbar::muxr_topbar::<()>(
            title_for_logic.borrow().clone(),
        ))
    };
    let mut root: RenderRoot<(), AnyView<()>> = RenderRoot::new();
    root.set_theme(Box::new(theme(false)));
    let mut state = ();
    let mut tcx = TextContext::new();

    let settled_before = frame(&mut root, &mut logic, &mut state, &mut tcx);

    *title.borrow_mut() = String::from("beta");
    frame_at(&mut root, &mut logic, &mut state, &mut tcx, ms(0.0));
    let mid = frame_at(&mut root, &mut logic, &mut state, &mut tcx, ms(30.0));

    // Past the 220ms crossfade, the outgoing run is dropped on the next
    // rebuild — so this settles back to a one-title bar.
    frame_at(&mut root, &mut logic, &mut state, &mut tcx, ms(2_000.0));
    let settled_after = frame_at(&mut root, &mut logic, &mut state, &mut tcx, ms(2_000.0));

    assert_eq!(
        settled_before.glyph_runs, settled_after.glyph_runs,
        "a settled bar paints the same runs before and after a title change"
    );
    assert!(
        mid.glyph_runs > settled_after.glyph_runs,
        "mid-crossfade the bar paints the outgoing title too ({} runs vs {})",
        mid.glyph_runs,
        settled_after.glyph_runs
    );
}

// ---------------------------------------------------------------------------
// 6 — the stable-container invariant, at the seam this task moved
// ---------------------------------------------------------------------------

#[test]
fn the_navigator_survives_the_bar_appearing_and_disappearing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target());

    // A torn-down navigator loses its whole page stack, so depth is the
    // cheapest end-to-end read on "the band reconciled in place" across the
    // bar's hidden → visible → hidden trip. (`tests/shell_handoff.rs` asserts
    // the same mechanism directly, on widget identity.)
    let start_depth = h.depth();
    h.push("/sessions");
    assert!(h.has_bar());
    assert_eq!(h.depth(), start_depth + 1);

    h.push("/settings");
    assert_eq!(h.depth(), start_depth + 2);

    h.tap_label(BACK);
    h.settle();
    // A bare pop, not a back press: a press on `/sessions` is a *question* now
    // (section 3e), and what this test is about is the band reconciling in
    // place — the pop is exactly the stack mutation a confirmed answer makes.
    h.state.nav.controller().pop();
    h.settle();

    assert_eq!(
        h.depth(),
        start_depth,
        "back at Connect, one live navigator"
    );
    assert!(!h.has_bar(), "…and the bar collapsed again");
}

/// A tap that lands nowhere in particular — the bar must not swallow it.
const AWAY_FROM_THE_BAR: Point = Point::new(4.0, 400.0);

#[test]
fn a_hidden_bar_routes_no_events_and_reserves_no_box() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // A hidden bar lays out to zero — it floats over the navigator now, so the
    // Connect screen owns the whole surface either way, but a zero box is what
    // keeps the bar out of the hit test entirely.
    let scene = h.frame();
    let painted_bottom = scene
        .rounded
        .iter()
        .map(|(origin, size)| origin.y + size.height)
        .fold(0.0f64, f64::max);
    assert!(
        painted_bottom > 0.0,
        "Connect painted its chrome under a zero-height bar"
    );

    // …and a press aimed at the page is not intercepted by a bar with no box.
    tap(&mut h.root, &mut h.state, AWAY_FROM_THE_BAR);
    h.frame();
    assert!(!h.has_bar());

    // The point of the restructure, at its cheapest: a tap in the band the bar
    // *would* occupy still reaches the page under it. Nothing on `/` responds
    // to it, so what this proves is the negative — a bar-less route is not
    // quietly shadowed by a mounted-but-invisible bar.
    tap(&mut h.root, &mut h.state, Point::new(4.0, BAR_HEIGHT / 2.0));
    h.frame();
    assert!(!h.has_bar(), "…and the bar did not wake up to catch it");
}

// ---------------------------------------------------------------------------
// 7 — the bar floats: no page reflow, and a real entrance/exit
// ---------------------------------------------------------------------------

/// The Sessions page's own full-bleed tone rect (`pieces::page_tone`) — the
/// biggest thing that screen paints, and so a direct read on the box the
/// navigator laid the page out in.
fn page_tone(scene: &RecScene) -> (Point, Size) {
    largest(&scene.rounded).unwrap_or_else(|| panic!("the Sessions page painted no tone"))
}

/// The `y` of every 7×7 tile in `scene`. Three things on a `/sessions` frame
/// are that size: the bar's four `.brand-mark span` cells, the conn-row's
/// `.dot-ok`, and — on the Connect page sliding past it — that screen's own
/// saved-server dots, none of which sit anywhere near the top (its first piece
/// is a 60px mark tile whose cells are 15.5px, so nothing 7×7 paints above
/// `y ≈ 112`).
///
/// Told apart by **band**, never by `x`: a transition slides both pages
/// horizontally, so an x filter would find the strip at rest and lose it
/// mid-flight — which is precisely the frame these probes exist for.
fn tiles(scene: &RecScene) -> Vec<f64> {
    scene
        .rounded
        .iter()
        .filter(|(_, size)| {
            (size.width - MARK_CELL).abs() < EPSILON && (size.height - MARK_CELL).abs() < EPSILON
        })
        .map(|(origin, _)| origin.y)
        .collect()
}

/// Where the bar is painting: the top of its brand mark, which is the highest
/// chrome on screen at rest and only ever travels further up.
fn brand_mark_top(scene: &RecScene) -> f64 {
    let top = tiles(scene).into_iter().fold(f64::INFINITY, f64::min);
    assert!(
        top < BAR_HEIGHT,
        "the topmost 7x7 tile is the bar's brand mark, inside its own band: {top}"
    );
    top
}

/// Where the conn-row strip's dot is painting — the one tile inside the strip's
/// own band, immediately under the bar's.
fn strip_dot_top(scene: &RecScene) -> f64 {
    let band: Vec<f64> = tiles(scene)
        .into_iter()
        .filter(|y| (BAR_HEIGHT..BAR_HEIGHT + 40.0).contains(y))
        .collect();
    assert_eq!(band.len(), 1, "one strip dot under the bar: {band:?}");
    band[0]
}

/// Mount, connect, and push `/sessions` **without** settling — the fixture
/// every mid-transition probe below starts from. Returns the harness and its
/// mid-transition frame.
fn mid_push_to_sessions(reduce_motion: bool) -> (Harness, RecScene) {
    let mut h = Harness::themed(theme(reduce_motion));
    expect_connection_controller().connect(mock_target());
    app_nav().push("/sessions");
    h.frame_at(0.0);
    let mid = h.frame_at(MID_TRANSITION_MS);
    (h, mid)
}

#[test]
fn a_page_pushed_under_the_bar_lays_out_at_full_height_from_its_first_frame() {
    let _g = serial();
    let _ambient = setup();

    // The round-3 glitch, as an assertion. When the bar shared a band with the
    // navigator, arriving at `/sessions` shrank every page by the bar's height
    // — so the incoming page laid out short and its content sat 46px lower than
    // where it settles. Floating the bar makes the page's box a constant.
    let (mut h, mid) = mid_push_to_sessions(false);
    let mid_tone = page_tone(&mid);
    let mid_strip = strip_dot_top(&mid);

    let settled = h.settle();
    let settled_tone = page_tone(&settled);

    assert_eq!(
        settled_tone.1.height,
        support::H,
        "a settled page owns the whole surface, bar band included"
    );
    assert_eq!(
        mid_tone.1, settled_tone.1,
        "…and it owned exactly that box mid-transition too — no reflow"
    );
    assert_eq!(
        mid_tone.0.y, settled_tone.0.y,
        "…laid out at the same y, so nothing shifted down as the bar arrived"
    );
    assert_eq!(
        mid_strip,
        strip_dot_top(&settled),
        "the conn-row strip does not move while the bar comes in"
    );
}

#[test]
fn the_conn_row_strip_rests_immediately_under_the_bar_band() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::on_sessions();

    // The clearance seam, pinned end to end: `topbar_clearance` reserves the
    // bar's own box (its unit test pins that number), and the strip is the
    // first thing under it — `.conn-row{padding:13px…}` plus the row's own
    // centering of a 7px dot in its text line box. This is where the band-based
    // build put it, and the whole point of the restructure is that it stayed.
    let mut h = h;
    let scene = h.frame();
    let dot = strip_dot_top(&scene);

    assert!(
        (dot - STRIP_DOT_Y).abs() < EPSILON,
        "the strip's dot rests at {STRIP_DOT_Y}, not {dot}"
    );
    assert!(
        dot >= BAR_HEIGHT + STRIP_PAD_Y,
        "…which is under the bar band plus the strip's own padding, never \
         behind the bar"
    );
}

#[test]
fn the_bar_slides_in_rather_than_popping_in() {
    let _g = serial();
    let _ambient = setup();

    let (mut h, mid) = mid_push_to_sessions(false);
    let mid_mark = brand_mark_top(&mid);
    assert!(
        mid_mark.is_finite(),
        "the bar paints while it arrives — an entrance, not a pop-in"
    );

    let settled = h.settle();
    let settled_mark = brand_mark_top(&settled);

    assert!(
        mid_mark < settled_mark - EPSILON,
        "mid-entrance the bar is still above its resting place ({mid_mark} vs \
         {settled_mark})"
    );
    assert!(
        settled_mark - mid_mark <= BAR_HEIGHT + EPSILON,
        "…by at most its own height, which is the whole travel ({} px)",
        settled_mark - mid_mark
    );
}

#[test]
fn reduce_motion_collapses_the_bars_entrance_to_a_pure_fade() {
    let _g = serial();
    let _ambient = setup();

    let (mut h, mid) = mid_push_to_sessions(true);
    let mid_mark = brand_mark_top(&mid);
    let settled_mark = brand_mark_top(&h.settle());

    // The same hard rule the route slide answers to: a reduced reveal is a
    // ≤120ms linear fade with no geometric motion at all. The bar is painting
    // mid-flight (that is what makes this an assertion and not a vacuous read)
    // — it simply never moves.
    assert!(mid_mark.is_finite(), "the bar paints while it fades in");
    assert!(
        (mid_mark - settled_mark).abs() < EPSILON,
        "reduced motion must not slide the bar in ({mid_mark} vs {settled_mark})"
    );
}

#[test]
fn popping_home_animates_the_bar_out_without_moving_the_page() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    let before = h.frame();
    let before_tone = page_tone(&before);
    let before_mark = brand_mark_top(&before);
    let before_strip = strip_dot_top(&before);

    app_nav().pop();
    h.frame_at(0.0);
    let mid = h.frame_at(MID_TRANSITION_MS);

    // The leaving Sessions page slides *sideways* under the Glyph preset, so
    // this is a vertical claim: its box and its content's y are exactly what
    // they were, while the bar above it is on its way out.
    let mid_tone = page_tone(&mid);
    assert_eq!(
        mid_tone.1, before_tone.1,
        "the leaving page keeps its full-surface box as the bar exits"
    );
    assert_eq!(
        mid_tone.0.y, before_tone.0.y,
        "…and does not jump up to fill the vacated band"
    );
    assert_eq!(
        strip_dot_top(&mid),
        before_strip,
        "…nor does its strip, which is what the reflow used to yank"
    );
    assert!(
        brand_mark_top(&mid) < before_mark - EPSILON,
        "…while the bar itself is genuinely leaving ({} vs {before_mark})",
        brand_mark_top(&mid)
    );

    // And it really does go: settled on `/`, there is no bar at all.
    h.settle();
    assert!(!h.has_bar(), "roles on screen: {:?}", node_roles(&h.root));
}
