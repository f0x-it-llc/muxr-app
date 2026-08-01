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
use kurbo::{Point, Size};

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
    /// A strictly-increasing clock for [`Harness::settle_modal`] — see there.
    clock: f64,
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

    /// Drive two more frames on a **strictly increasing** clock — what an
    /// animation staged *during* a [`settle`] needs in order to finish.
    ///
    /// [`support::settle`] paints its last two frames at the same timestamp,
    /// so a driver created on the second of them never sees the clock move
    /// again. The bar's modality tint is exactly that case:
    /// `BarState::overlay_active` is a **depth** fact, and
    /// `NavigatorController::depth` is published by the navigator's own
    /// rebuild — so the flag flips one rebuild *after* an overlay is pushed,
    /// a frame later than a route change flips the bar's title.
    fn settle_modal(&mut self) {
        for _ in 0..2 {
            self.clock += support::SETTLE_MS;
            self.frame_at(self.clock);
        }
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

/// Push an inert transparent page over `h` — a stand-in for a sheet or a
/// dialog: it adds page-stack depth, paints nothing worth recording, and
/// swallows nothing, so whatever the bar does is the bar's own doing.
fn open_overlay(h: &mut Harness) {
    h.state
        .nav
        .controller()
        .push_transparent(|| any(SizedBox::<AppState>(Some(10.0), Some(10.0))));
    h.settle();
}

#[test]
fn a_transparent_overlay_over_sessions_never_retitles_the_bar() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    let covered_depth = h.depth();

    // A sheet/dialog is pushed as a page *builder* straight through the
    // controller, never through `Nav` — so it adds depth with no route
    // metadata, and the route the bar is *for* does not change. The bar goes
    // modal while it is up (below); what must not happen is a retitle.
    open_overlay(&mut h);
    assert_eq!(h.depth(), covered_depth + 1, "the overlay is its own page");

    h.state.nav.controller().pop();
    h.settle();

    assert_eq!(h.depth(), covered_depth, "…and dismissing it pops only it");
    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "…leaving the covered route's own bar, brand and all: {:?}",
        h.values()
    );
}

// ---------------------------------------------------------------------------
// 3b — the bar is modal-aware: dimmed AND inert under an overlay
// ---------------------------------------------------------------------------

/// Every plain fill covering exactly the bar's own box, in paint order. At a
/// zero window inset that box is `W × BAR_HEIGHT` at the origin — which the
/// bar's own surface fills on every frame, so a *second* one is the modality
/// tint and nothing else (the sheet's own scrim is full-surface, and no page
/// paints a rect of exactly this size at exactly this origin).
fn bar_band_fills(scene: &RecScene) -> Vec<peniko::Color> {
    support::fills_at(scene, Point::ZERO, Size::new(support::W, BAR_HEIGHT))
}

#[test]
fn an_open_overlay_dims_the_bar_and_a_dismissal_lifts_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    let lit = bar_band_fills(&h.frame());
    assert_eq!(
        lit.len(),
        1,
        "at rest the bar's band carries only its own surface: {lit:?}"
    );

    open_overlay(&mut h);
    h.settle_modal();
    let dimmed = bar_band_fills(&h.frame_at(h.clock));
    assert_eq!(
        dimmed.len(),
        2,
        "under a modal the bar paints a scrim over itself: {dimmed:?}"
    );
    // …at exactly the alpha the sheet dims the page under it with, which is
    // the whole point of mirroring rather than inventing a "disabled" wash.
    let tint = support::alpha_of(dimmed[1]);
    assert!(
        (tint - muxr::ui::sheet::SCRIM_ALPHA).abs() < 1e-6,
        "the bar's tint is the modal's own scrim alpha, got {tint}"
    );

    h.state.nav.controller().pop();
    h.settle();
    h.settle_modal();

    assert_eq!(
        bar_band_fills(&h.frame_at(h.clock)).len(),
        1,
        "dismissing the modal takes the tint with it"
    );
}

#[test]
fn the_bars_tint_fades_rather_than_snapping_on() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    open_overlay(&mut h);
    // One frame a hair after the tint's driver started (`settle`'s own clock,
    // which is where `open_overlay` left it): the scrim is on its way in, not
    // already there — an instant dim could never produce a partial alpha.
    let mid = h.frame_at(support::SETTLE_MS + 40.0);
    let dimmed = bar_band_fills(&mid);
    assert_eq!(dimmed.len(), 2, "the bar is tinting: {dimmed:?}");
    let tint = support::alpha_of(dimmed[1]);
    assert!(
        tint > 0.0 && tint < muxr::ui::sheet::SCRIM_ALPHA - 1e-6,
        "mid-fade the tint is partial ({tint} of {})",
        muxr::ui::sheet::SCRIM_ALPHA
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

    // The modality hole this closes: the bar floats *over* the navigator, so
    // the modal cannot cover it — before this, that press pushed /settings out
    // from under an open sheet.
    tap(&mut h.root, &mut h.state, gear);
    h.settle();

    assert_eq!(
        h.depth(),
        base_depth + 1,
        "the overlay is still the only thing on top — the ⚙ pushed nothing"
    );
    assert!(
        !h.values().iter().any(|v| v == "server settings"),
        "…and nothing retitled: {:?}",
        h.values()
    );

    // Dismiss, and the very same press works again.
    h.state.nav.controller().pop();
    h.settle();
    assert!(h.has_bar(), "the bar is live again");
    h.tap_label(GEAR);
    h.settle();
    assert_eq!(h.depth(), base_depth + 1, "the ⚙ pushes a page once more");
    assert_titled(&h, "server settings");
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
