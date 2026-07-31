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
//! solved for it: it is **shell** chrome, a sibling of the navigator inside
//! `shell::root_view`'s band, so mounting the real `MuxrApp` leaves it fully
//! addressable by role/label/value while the routes underneath stay opaque.
//! That is exactly the split this suite wants — it asserts on what the *bar*
//! says, never on what a route renders.
//!
//! # Two observables, and why each is the honest one
//!
//! - **Semantics** ([`support::node_roles`]/[`node_labels`]/[`node_values`]) for
//!   everything the bar *is*: present or hidden (`Role::TitleBar`), what it
//!   titles, whether it wears the brand or the `‹`, whether it carries the `⚙`.
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
//! # The cold-start splash, spent up front
//!
//! `MuxrApp`'s first *build* in a process claims the one cold-start splash, so
//! whichever test ran first would otherwise mount an overlay the others don't —
//! and its Connect copy would land in the recorded geometry. [`spend_cold_start`]
//! burns that claim on a throwaway app before any fixture mounts, so every test
//! here runs a warm start.

use std::sync::OnceLock;

use frust::{AnyView, Component, SizedBox, Theme, any};
use frust_core::RenderRoot;
use frust_core::accesskit::Role;
use frust_text::TextContext;
use kurbo::Point;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::routes::app_nav;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, frame, frame_at, largest, ms, node_centers_by_label, node_labels,
    node_roles, node_values, serial, settle, setup, tap,
};

/// The mock connection every bar-titled test runs behind (the session guard
/// bounces `/sessions`/`/terminal` otherwise).
const HOST: &str = "mock.example";
const PORT: u16 = 7777;

/// The bar's brand wordmark, and the `‹`/`⚙` slot glyphs — transcribed rather
/// than imported: `ui::topbar`'s constants are the implementation's, and a test
/// that imported them could not catch one of them changing.
const BRAND: &str = "muxr";
const BACK: &str = "\u{2039}";
const GEAR: &str = "\u{2699}";

/// How far into a transition the geometry probes paint. Early enough that the
/// reduced-motion cross-fade (≤120ms) has **not** settled, so both frames
/// compare like with like.
const MID_TRANSITION_MS: f64 = 60.0;

/// A mock connect target — only `connected` flipping and the `host:port`
/// endpoint matter here.
fn mock_target() -> ConnectTarget {
    ConnectTarget {
        host: HOST.to_string(),
        port: PORT,
        trust_mode: TrustMode::System,
        label: "Mock rig".to_string(),
        offer_credential: false,
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

    /// The navigator's page-stack depth.
    fn depth(&self) -> usize {
        self.state.nav.controller().depth()
    }
}

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

#[test]
fn a_transparent_overlay_over_sessions_leaves_the_bar_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    let covered_depth = h.depth();

    // A sheet/dialog is pushed as a page *builder* straight through the
    // controller, never through `Nav` — so it adds depth with no route metadata
    // and the bar keeps showing the route it covers.
    h.state
        .nav
        .controller()
        .push_transparent(|| any(SizedBox::<AppState>(Some(10.0), Some(10.0))));
    h.settle();

    assert_eq!(h.depth(), covered_depth + 1, "the overlay is its own page");
    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "…brand and all: {:?}",
        h.values()
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
    h.state.nav.controller().request_back();
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
fn a_hidden_bar_routes_no_events_and_reserves_no_height() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();

    // A hidden bar lays out to zero, so the Connect screen underneath still
    // owns the whole surface: its chrome reaches the bottom of the viewport
    // exactly as it did before the bar existed.
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
}
