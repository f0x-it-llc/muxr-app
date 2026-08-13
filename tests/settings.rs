//! Headless behavior coverage for the `/settings` screen
//! (`src/features/settings/**`) — no GPU, no window: a `RenderRoot` over the
//! real page builder, driven through `tests/support`.
//!
//! # The harness, and why the screen is *beside* a navigator rather than in one
//!
//! In production `settings_page` is a navigator page. Mounting it that way here
//! would make every assertion geometric: `NavigatorWidget` contributes no
//! `Widget::semantics` and forwards none of its pages' (the framework gap
//! `tests/ui_substrate.rs` documents), so nothing inside a navigator appears in
//! the accessibility tree at all.
//!
//! So [`Harness`] mounts the screen as the **bottom layer of a `Stack`**, with
//! an otherwise-empty navigator — driven by the *same* `NavigatorController`
//! the screen is handed — stacked over it, exactly as `tests/sessions.rs` does
//! (see that suite's own module docs for why the base page is a full-bleed,
//! childless `SizedBox`). The screen's rows therefore stay reachable by text
//! value, while the dialogs and picker sheets it pushes are exercised end to
//! end through a real navigator and located by painted geometry.
//!
//! # A taller surface than the default
//!
//! Four groups plus the Disconnect action run past 1000px, so every frame here
//! lays out at [`SURFACE`] rather than `support::surface()`'s 800×600 — a row
//! laid out below the fold would be untappable and invisible to a semantics
//! probe. That is also why this file pumps through its own
//! [`Harness::pump_once`] rather than `support::pump_frames`, which frames at
//! the default size: a pump at 600px high would silently re-layout everything
//! the next probe is about to measure. (Inlined here rather than added to
//! `tests/support`, whose additive rights belong to another task in this wave.)
//!
//! # Reading a picker sheet without semantics
//!
//! A sheet is a navigator page, so its rows are invisible to
//! `node_values`. Three painted facts stand in, and between them they pin the
//! whole of `docs/redesign.html`'s screen 6:
//!
//! - **how many rows** — the sheet's `glyph_runs` delta is `options + 1` (the
//!   title);
//! - **which one is selected** — only that row paints a fill, and it is the
//!   one rect as wide as the panel itself (`.pick-row.sel{margin:0 -20px}`
//!   bleeding exactly the sheet's own 20px gutter on each side), plus the one
//!   filled vector path the sheet adds beyond its own panel (the `✓` — see
//!   `Chrome::vectors`);
//! - **what the rows say** — by picking one and watching the row value on the
//!   screen behind change, which is the round trip that matters anyway.
//!
//! Row *k* is addressed relative to the selected row's measured rect: every
//! `.pick-row` is one line tall, so the pitch is that rect's own height.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, GetUntracked, NavigatorController, Resolution, Set, SizedBox, Stack, Theme,
    WindowMetrics, any, navigator, provide_context,
};
use frust_core::accesskit::{Node, Rect, Role, Toggled};
use frust_core::{NamedKey, PointerPhase, RenderRoot, WindowInsets};
use frust_reactive::ReactiveRuntime;
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::failure::AppFailure;
use muxr::features::connect::di::{expect_saved_servers, mock_credentials};
use muxr::features::connect::domain::entities::{SavedServer, epoch_millis};
use muxr::features::settings::data::repositories::MockAuthTokensRepository;
use muxr::features::settings::data::repositories::mock_auth_tokens_repository::MOCK_MINTED_SECRET_PREFIX;
use muxr::features::settings::di::{SharedAuthTokensRepository, expect_preferences_repository};
use muxr::features::settings::domain::entities::{AuthToken, CreateTokenRequest, MintedToken};
use muxr::features::settings::domain::repositories::AuthTokensRepository;
use muxr::features::settings::presentation::pages::settings_page;
use muxr::features::settings::presentation::scramble::{
    TIMELINE_MS, advance_animation_clock_for_tests, set_animation_clock_for_tests,
};
use muxr::fonts::catalog::{CatalogStatus, FontState};
use muxr::fonts::source::FontSource;
use muxr::routes::{app_nav, install_app_router};
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, by_width, center, char_key, frame_sized, largest, named_key,
    node_center_by_label, node_centers_by_label, node_centers_by_value, node_labels, node_values,
    pointer, serial, settle_sized, setup, smallest, tap,
};

/// The surface every frame here lays out at — tall enough that the whole screen
/// (five groups, the dashed create row, the saved-server rows and Disconnect)
/// is on it at once. See the [module docs](self).
const SURFACE: Size = Size::new(800.0, 1700.0);

/// A point in the top-left corner — scrim on every overlay this screen pushes
/// (the dialog centers its panel, the sheet anchors its own to the bottom).
const SCRIM_POINT: Point = Point::new(4.0, 4.0);
/// The panel gutter `glyph::dialog` and `ui::sheet` share, transcribed — a
/// full-bleed child of either is laid out at the panel width minus twice this,
/// and it is exactly what `.pick-row.sel` bleeds back out.
const PANEL_PADDING: f64 = 20.0;
/// How long a pumped wait may run before the test fails rather than hangs.
const PUMP_DEADLINE: Duration = Duration::from_secs(5);
/// The groups column's max width past
/// `crate::responsive::WIDE_BREAKPOINT_PX` (FINDINGS #21's width-clamp pass),
/// transcribed (the constant itself is private to `presentation::pages`).
const SETTINGS_CLAMP_WIDTH: f64 = 640.0;
/// `.disconnect-btn`'s own horizontal margin, transcribed (private to
/// `presentation::pieces`) — what the Disconnect button spans the width
/// *between*, inside whatever box the scroller itself is laid out at.
const ROW_PAD_X: f64 = 16.0;

/// The seeded token names `data`'s mock repository ships — the redesign's two
/// `.token-item`s, in order.
const SEEDED_TOKENS: [&str; 2] = ["zelli-mobile-100-71-31-57-mr3g6w5g", "token_2"];
/// The five group labels, as the screen prints them (the source's
/// `text-transform:uppercase`, applied in Rust — `text` carries no transform).
const GROUPS: [&str; 5] = [
    "CONNECTION",
    "AUTH TOKENS",
    "SAVED SERVERS",
    "TERMINAL APPEARANCE",
    "SESSION POLL",
];

/// The saved server the credential cases operate on — seeded before the screen
/// mounts, stamped fresh so it sorts to the top of the group.
const CRED_ENDPOINT: &str = "cred.example:50051";
/// …its label, which is what the row prints on its top line.
const CRED_LABEL: &str = "Cred rig";
/// …and the secret its stored credential holds.
const CRED_TOKEN: &str = "stored-s3cr3t";
/// The token name that credential's server-side token carries — one of the
/// seeded pair, so a delete has something real to revoke.
const CRED_TOKEN_NAME: &str = "token_2";

// ---------------------------------------------------------------------------
// Local harness
// ---------------------------------------------------------------------------

/// A mock connect target with the given TLS posture.
fn mock_target(trust_mode: TrustMode) -> ConnectTarget {
    ConnectTarget {
        host: "mock.example".to_string(),
        port: 7777,
        trust_mode,
        label: "Mock rig".to_string(),
        offer_credential: false,
        token: zeroize::Zeroizing::new(String::new()),
        remember: false,
        fingerprint: None,
    }
}

/// A [`WindowMetrics`] whose size is `size` — see `mount_with_metrics`.
fn metrics_at(size: Size) -> WindowMetrics {
    WindowMetrics::new(size, 1.0, WindowInsets::default())
}

/// [`Harness::mount`], first publishing `metrics` into context when `Some` —
/// the shape `muxr`'s pages read via `use_context::<WindowMetrics>()` for the
/// responsive width-clamp pass (FINDINGS #21). `None` leaves context unset,
/// exactly the state every other test in this suite mounts under (no shell
/// has published yet), so this helper is purely additive.
fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Harness {
    if let Some(metrics) = metrics {
        provide_context(metrics);
    }
    Harness::mount()
}

/// The bounds of the first node labelled `label` — `tests/connect.rs`'s
/// `Harness::bounds` idiom, free-standing here since this suite's `Harness`
/// doesn't already carry one.
fn bounds_by_label(root: &RenderRoot<AppState, AnyView<AppState>>, label: &str) -> Rect {
    root.semantics()
        .nodes
        .iter()
        .find(|(_, node): &&(_, Node)| node.label() == Some(label))
        .and_then(|(_, node)| node.bounds())
        .unwrap_or_else(|| panic!("nothing labelled {label:?}"))
}

/// The bounds of **every** node labelled `label` — [`bounds_by_label`]'s
/// all-matches sibling, for the saved-server group's `forget` (one per row).
fn all_bounds_by_label(root: &RenderRoot<AppState, AnyView<AppState>>, label: &str) -> Vec<Rect> {
    root.semantics()
        .nodes
        .iter()
        .filter(|(_, node): &&(_, Node)| node.label() == Some(label))
        .filter_map(|(_, node)| node.bounds())
        .collect()
}

/// The app-logic closure `RenderRoot::rebuild` drives.
type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

/// The screen, an overlay navigator over it, and everything a frame needs.
struct Harness {
    nav: NavigatorController<AppState>,
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
}

impl Harness {
    /// Mount the screen (see the [module docs](self) for the shape), leaving
    /// the connection **disconnected** and the first load un-awaited.
    fn mount() -> Self {
        Self::mount_seeded(|| {})
    }

    /// [`Harness::mount`] with `seed` run in between: after `MuxrApp::init` has
    /// published every feature's stores, and **before** the screen's own
    /// controller reads them.
    ///
    /// The window matters — `init` constructs fresh repositories every time it
    /// runs, so anything written before it is discarded, and the screen loads
    /// its lists once at construction.
    fn mount_seeded(seed: impl FnOnce()) -> Self {
        // The real composition root: it publishes the app-scoped connection
        // controller and calls every feature's `di::provide` — including this
        // feature's two repositories and (for the saved-server group)
        // `connect`'s own two.
        let state = MuxrApp.init();
        seed();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        // Re-point the app's one path-pushing handle (`routes::app_nav`, which
        // the screen resolves in its own `init`) at this fixture's navigator,
        // real route table and real session guard included — which is what
        // makes Disconnect's `go("/")` the app's real navigation. The one
        // controller plays both of production's navigators (`AppState::nav` and
        // the root overlay host `AppState::overlays`, where the dialogs and
        // picker sheets are pages since FINDINGS #44), which is what keeps every
        // overlay a page of the *mounted* navigator and so drivable here.
        let _router = install_app_router(&nav, &nav, expect_connection_controller());
        let logic_nav = nav.clone();
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            let screen_nav = logic_nav.clone();
            let overlay_nav = logic_nav.clone();
            any(Stack(vec![
                settings_page(screen_nav),
                any(navigator(&overlay_nav, || {
                    // Full-bleed and childless — see the module docs.
                    any(SizedBox::<AppState>(
                        Some(f64::INFINITY),
                        Some(f64::INFINITY),
                    ))
                })),
            ]))
        });
        let mut harness = Harness {
            nav,
            root: RenderRoot::new(),
            logic,
            state,
            tcx: TextContext::new(),
        };
        harness.frame();
        harness
    }

    /// Mount, mock-connect with `trust_mode`, and wait for the first load.
    fn connected_with(trust_mode: TrustMode) -> Self {
        let mut harness = Harness::mount();
        expect_connection_controller().connect(mock_target(trust_mode));
        harness.await_tokens();
        harness
    }

    /// The common case: connected with no verification, like the redesign's
    /// own mock (its `insecure (plaintext)` row).
    fn connected() -> Self {
        Harness::connected_with(TrustMode::NoVerification)
    }

    /// One frame at [`SURFACE`], on the zero clock.
    fn frame(&mut self) -> RecScene {
        frame_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
        )
    }

    /// Drive a pushed/popped overlay past the navigator's input-blocking
    /// transition, returning the last frame's paint record.
    fn settle(&mut self) -> RecScene {
        settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
        )
    }

    /// Settle, then deliver a pop's result: the navigator flushes a pusher's
    /// `on_result` at the start of the first event pass **after** the pop's
    /// rebuild, so a harmless `Move` is what actually hands the answer over
    /// (`ui_substrate.rs`'s idiom).
    fn flush(&mut self) {
        self.settle();
        self.settle();
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, SCRIM_POINT));
    }

    /// Pump the UI-thread task queue and rebuild once, **at [`SURFACE`]** — see
    /// the module docs for why `support::pump_frames` is not used here.
    fn pump_once(&mut self) {
        let runtime =
            ReactiveRuntime::get().expect("pump_once: ReactiveRuntime::init must have run");
        runtime.pump_local();
        std::thread::sleep(Duration::from_millis(1));
        self.frame();
    }

    /// Pump until `done` accepts the rendered semantics values, or fail at
    /// [`PUMP_DEADLINE`] rather than hang.
    fn pump_until_values(&mut self, what: &str, done: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + PUMP_DEADLINE;
        while !done(&node_values(&self.root)) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; on screen: {:?}",
                node_values(&self.root)
            );
            self.pump_once();
        }
    }

    /// Wait for the async first load (the screen renders both seeded tokens).
    fn await_tokens(&mut self) {
        self.pump_until_values("the seeded tokens", |values| {
            SEEDED_TOKENS
                .iter()
                .all(|name| values.iter().any(|v| v == name))
        });
    }

    /// A full tap at `point`.
    fn tap(&mut self, point: Point) {
        tap(&mut self.root, &mut self.state, point);
    }

    /// Tap the first node whose printed text is `value` — how a test presses a
    /// `.set-row` (its label run) or the dashed create row.
    fn tap_value(&mut self, value: &str) {
        let point = self.center_of(value);
        self.tap(point);
    }

    /// Tap the first node *labelled* `label` (a button, the bell switch).
    fn tap_label(&mut self, label: &str) {
        let point = node_center_by_label(&self.root, label).unwrap_or_else(|| {
            panic!(
                "nothing labelled {label:?}; labels on screen: {:?}",
                node_labels(&self.root)
            )
        });
        self.tap(point);
    }

    /// Where the first node printing `value` sits.
    fn center_of(&self, value: &str) -> Point {
        node_centers_by_value(&self.root, value)
            .first()
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "nothing printing {value:?}; on screen: {:?}",
                    node_values(&self.root)
                )
            })
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

    /// The bell switch's announced state — the `Role::Switch` node
    /// `pieces`' own `switch_semantics` wrapper contributes.
    fn bell_state(&self) -> Option<Toggled> {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("bell notifications"))
            .and_then(|(_, node)| node.toggled())
    }

    /// The create dialog's read-only toggle's announced role and state —
    /// addressed by its own label rather than `Role::Switch` alone
    /// (`tests/connect.rs`'s own `remember_on`, for a screen with only one
    /// switch mounted at a time): the bell row's switch is still mounted
    /// underneath while the dialog is open over it, so a role-only match
    /// would be ambiguous here. `None` once the dialog itself is gone.
    fn read_only_toggle(&self) -> Option<(Role, Toggled)> {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("read-only"))
            .map(|(_, node)| {
                (
                    node.role(),
                    node.toggled()
                        .expect("a switch node always carries a toggled state"),
                )
            })
    }

    /// The navigator's page-stack depth (1 = only the harness's base page).
    fn depth(&self) -> usize {
        self.nav.depth()
    }

    /// Push a stand-in for the route a picker sheet floats over, and answer the
    /// depth the stack now rests at.
    ///
    /// A navigator always keeps one page (`NavigatorController::pop`: "a pop of
    /// the last/root page is ignored"), so an unwanted *second* pop against the
    /// bare harness is silently absorbed and [`Harness::depth`] never moves. In
    /// the app there is always a route under the sheet — `/settings` — and that
    /// page coming off is the defect the race below exists for. This page
    /// stands in for it. (`tests/terminal.rs` carries the same helper, for the
    /// same reason.)
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
}

/// The chrome a pushed overlay added to the frame — the screen underneath is
/// diffed out (see [`added`]).
struct Chrome {
    rounded: Vec<(Point, Size)>,
    strokes: Vec<(Point, Size)>,
    paths: Vec<(Point, Size)>,
    runs: usize,
}

impl Chrome {
    fn diff(before: &RecScene, after: &RecScene) -> Self {
        Chrome {
            rounded: added(&before.rounded, &after.rounded),
            strokes: added(&before.strokes, &after.strokes),
            paths: added(&before.paths, &after.paths),
            runs: after.glyph_runs.saturating_sub(before.glyph_runs),
        }
    }

    /// The overlay's own panel — the biggest surface it filled.
    ///
    /// Both channels, because the two overlays paint their panel differently: a
    /// dialog's is a plain rounded rect, while the Glyph sheet's is a *path*
    /// (only its top corners are rounded, which `fill_rounded_rect` cannot
    /// express).
    fn panel(&self) -> (Point, Size) {
        let mut candidates = self.rounded.clone();
        candidates.extend(self.paths.iter().copied());
        largest(&candidates).expect("an overlay always paints its own panel")
    }

    /// The overlay's own vector paths with its panel taken back out — a picker
    /// row's `✓`, and nothing else (see [`Chrome::panel`]).
    fn vectors(&self) -> Vec<(Point, Size)> {
        let panel = self.panel();
        self.paths
            .iter()
            .copied()
            .filter(|painted| *painted != panel)
            .collect()
    }

    /// The overlay's full-content-width child: a dialog's name field.
    fn content_child(&self) -> (Point, Size) {
        let content_w = self.panel().1.width - 2.0 * PANEL_PADDING;
        by_width(&self.rounded, content_w)
            .unwrap_or_else(|| panic!("no full-width child in {:?}", self.rounded))
    }

    /// A picker sheet's **selected** `.pick-row`: the one rect as wide as the
    /// panel itself but not the panel — `.pick-row.sel{margin:0 -20px}` bleeds
    /// the sheet's own gutter back out on both sides, and no other row paints a
    /// fill at all.
    fn selected_pick_row(&self) -> (Point, Size) {
        let (panel_origin, panel_size) = self.panel();
        let mut rows: Vec<(Point, Size)> = self
            .rounded
            .iter()
            .copied()
            .filter(|(origin, size)| {
                (size.width - panel_size.width).abs() < EPSILON && *origin != panel_origin
            })
            .collect();
        rows.sort_by(|a, b| a.0.y.total_cmp(&b.0.y));
        assert_eq!(
            rows.len(),
            1,
            "exactly one picker row fills, and it is the selected one: {rows:?}"
        );
        rows[0]
    }
}

/// Open a picker sheet by tapping the `.set-row` labelled `row`, and return
/// what the sheet itself painted.
fn open_picker(h: &mut Harness, row: &str) -> Chrome {
    let before = h.frame();
    h.tap_value(row);
    let after = h.settle();
    let sheet = Chrome::diff(&before, &after);
    assert_eq!(h.depth(), 2, "the picker is pushed as its own page");
    sheet
}

/// Take option `target` out of an open picker whose selected row measured
/// `selected_row`, given the index that row sits at.
///
/// Every `.pick-row` is one line tall, so the selected row's own height is the
/// pitch — see the [module docs](self).
fn pick(h: &mut Harness, sheet: &Chrome, selected: usize, target: usize) {
    h.tap(pick_row(sheet, selected, target));
    h.flush();
}

/// Where option `target`'s row sits, given the picker's own selected row and
/// the index it sits at — [`pick`]'s geometry, factored out so a test can tap
/// the same row twice.
fn pick_row(sheet: &Chrome, selected: usize, target: usize) -> Point {
    let (origin, size) = sheet.selected_pick_row();
    let offset = target as f64 - selected as f64;
    Point::new(
        origin.x + size.width / 2.0,
        origin.y + offset * size.height + size.height / 2.0,
    )
}

/// Open a dialog by tapping the node labelled `label`, then return the rects
/// the dialog itself added.
fn open_dialog_by_label(h: &mut Harness, label: &str) -> Chrome {
    let before = h.frame();
    h.tap_label(label);
    let after = h.settle();
    Chrome::diff(&before, &after)
}

/// …and by tapping the node printing `value` (the dashed create row).
fn open_dialog_by_value(h: &mut Harness, value: &str) -> Chrome {
    let before = h.frame();
    h.tap_value(value);
    let after = h.settle();
    Chrome::diff(&before, &after)
}

/// Confirm an open confirmation dialog: its danger action is the smallest
/// border it painted (the panel's own being the other one).
fn confirm(h: &mut Harness, dialog: &Chrome) {
    let action = smallest(&dialog.strokes).expect("the danger action strokes its border");
    h.tap(center(action));
    h.flush();
}

// ---------------------------------------------------------------------------
// 1 — the whole screen renders
// ---------------------------------------------------------------------------

#[test]
fn all_five_groups_and_the_disconnect_action_render() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    for group in GROUPS {
        assert!(
            values.iter().any(|v| v == group),
            "the {group:?} group label renders: {values:?}"
        );
    }
    // One row label per `.set-row` the redesign draws. Only `zellij version`
    // renders here: the mock connection backend reports exactly one backend
    // (`connection.rs`'s own `MockConnectionBackend`), and the connection
    // group prints one version row per backend the server actually reports —
    // never a fixed pair.
    for label in [
        "server url",
        "tls certificate",
        "zellij version",
        "font family",
        "font size",
        "cursor style",
        "layout refresh",
        "bell notifications",
    ] {
        assert!(
            values.iter().any(|v| v == label),
            "the {label:?} row renders: {values:?}"
        );
    }
    // The dashed create row and the Disconnect button (a real `frust::button`,
    // so a label rather than a value).
    assert!(values.iter().any(|v| v == "create new token"), "{values:?}");
    let labels = node_labels(&h.root);
    assert!(labels.iter().any(|l| l == "Disconnect"), "{labels:?}");
}

#[test]
fn no_latency_is_printed_anywhere_on_this_screen() {
    // The one deliberate omission from the mock's data set: a live round-trip
    // time belongs beside the session list it gates (`/sessions`' own strip),
    // not repeated here as a second, differently-stale copy.
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    for value in &values {
        assert!(
            !value.contains("ms"),
            "a latency reading leaked onto Settings: {value:?} in {values:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2 — the `connection` group tracks `ServerInfo`
// ---------------------------------------------------------------------------

#[test]
fn the_connection_group_prints_the_live_servers_facts() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    assert!(
        values.iter().any(|v| v == "mock.example:7777"),
        "the endpoint is the connected one: {values:?}"
    );
    assert!(
        values.iter().any(|v| v == "0.44.3"),
        "the zellij row prints the version without the product name: {values:?}"
    );
    // Exactly one version row: the mock backend reports exactly one entry in
    // `ServerInfo::backend_versions`, and the connection group renders one row
    // per reported backend rather than a fixed pair.
    assert_eq!(
        values
            .iter()
            .filter(|v| v.as_str() == "zellij version")
            .count(),
        1,
        "one version row per reported backend: {values:?}"
    );
}

#[test]
fn the_tls_row_warns_only_for_the_posture_that_skips_validation() {
    let _g = serial();
    let _ambient = setup();

    // Sequential, not interleaved: each harness re-provides the app-scoped
    // connection controller, so only the most recently mounted one is live.
    let insecure = Harness::connected_with(TrustMode::NoVerification);
    let values = node_values(&insecure.root);
    assert!(
        values.iter().any(|v| v == "insecure (plaintext)"),
        "the unverified posture is named by its consequence: {values:?}"
    );
    drop(insecure);

    let validated = Harness::connected_with(TrustMode::System);
    let values = node_values(&validated.root);
    assert!(
        values.iter().any(|v| v == "public CA"),
        "a validated posture reads as one: {values:?}"
    );
    assert!(
        !values.iter().any(|v| v == "insecure (plaintext)"),
        "…and never as the insecure one: {values:?}"
    );
}

#[test]
fn a_disconnected_settings_screen_still_renders_every_row() {
    // `/settings` is deliberately unguarded, so this is reachable — and a
    // blank connection group would be the worse failure.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.await_tokens();

    let values = node_values(&h.root);
    assert!(
        values.iter().filter(|v| *v == "not connected").count() >= 2,
        "the endpoint and TLS rows both say so: {values:?}"
    );
    assert!(
        values.iter().any(|v| v == "server url"),
        "every row still renders: {values:?}"
    );
}

#[test]
fn the_connection_rows_are_informational_like_version_rows() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    app_toasts().clear();

    // Tapping server url and tls certificate rows does nothing — they are
    // inert like version rows, not chevron rows with destinations. The gesture
    // detector is not attached, so no tap handler fires.
    h.tap_value("server url");
    h.frame();
    let pending = app_toasts().pending_untracked();
    assert!(
        pending.is_empty(),
        "server url row is inert, no toast: {pending:?}"
    );
    assert_eq!(h.depth(), 1, "…and it pushed nothing");

    app_toasts().clear();
    h.tap_value("tls certificate");
    h.frame();
    let pending = app_toasts().pending_untracked();
    assert!(
        pending.is_empty(),
        "tls certificate row is inert, no toast: {pending:?}"
    );
    assert_eq!(h.depth(), 1, "…and it pushed nothing");
}

// ---------------------------------------------------------------------------
// 3 — auth tokens
// ---------------------------------------------------------------------------

#[test]
fn the_token_group_is_the_redesigns_two_tokens_verbatim() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    for name in SEEDED_TOKENS {
        assert!(values.iter().any(|v| v == name), "{name:?}: {values:?}");
    }
    for meta in ["created 2026-07-02", "created 2026-07-10"] {
        assert!(values.iter().any(|v| v == meta), "{meta:?}: {values:?}");
    }
    // Both scope chips, which is what the redesign's own note is about
    // ("token scope is now a visible chip on every token, not just some").
    assert!(
        values.iter().any(|v| v == "read-write") && values.iter().any(|v| v == "read-only"),
        "both scope chips render: {values:?}"
    );
    // One `revoke` action per token.
    assert_eq!(
        node_centers_by_label(&h.root, "revoke").len(),
        SEEDED_TOKENS.len()
    );
}

#[test]
fn revoking_a_token_confirms_first_then_removes_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    // A confirmed revoke plays a timeline before the row goes (see the
    // animation section below), so this pins its *end*: with the clock pinned,
    // one advance past `TIMELINE_MS` is the whole animation.
    let clock = AnimationClock::pinned();

    // A dismissed confirmation revokes nothing.
    open_dialog_by_label(&mut h, "revoke");
    assert_eq!(h.depth(), 2, "the confirmation is its own page");
    h.tap(SCRIM_POINT);
    h.flush();
    for _ in 0..4 {
        h.pump_once();
    }
    assert_eq!(h.depth(), 1, "the scrim tap closed it");
    assert!(
        node_values(&h.root).iter().any(|v| v == SEEDED_TOKENS[0]),
        "…and nothing was revoked"
    );

    // Confirming does.
    let dialog = open_dialog_by_label(&mut h, "revoke");
    confirm(&mut h, &dialog);
    clock.advance(TIMELINE_MS + 1.0);
    // The `created` line rather than the name: the name is noise from the first
    // frame of the scramble, so it is gone long before the row is.
    h.pump_until_values("the revoked row to disappear", |values| {
        !values.iter().any(|v| v == "created 2026-07-02")
    });
    assert_eq!(h.depth(), 1);
    let values = node_values(&h.root);
    assert!(
        values.iter().any(|v| v == SEEDED_TOKENS[1]),
        "…and only the one token went: {values:?}"
    );
    assert!(
        scrambled_variant(&values, SEEDED_TOKENS[0]).is_none(),
        "no trace of the collapsed row is left: {values:?}"
    );
}

// ---------------------------------------------------------------------------
// 3a — the revoke animation (scramble, then collapse)
// ---------------------------------------------------------------------------

/// The revoke timeline runs off a wall clock, so a suite that let it run in
/// real time could only ever assert "eventually". Pinning that clock (through
/// `presentation::scramble`'s own test seam) makes "three scramble frames in"
/// and "one frame past the end" exact positions instead. Restores the wall
/// clock on drop, so a later test in the same binary is never left holding a
/// frozen one.
struct AnimationClock;

impl AnimationClock {
    /// Pin the clock at the timeline's own zero.
    fn pinned() -> Self {
        set_animation_clock_for_tests(Some(0.0));
        AnimationClock
    }

    /// Move it `ms` forward.
    fn advance(&self, ms: f64) {
        advance_animation_clock_for_tests(ms);
    }
}

impl Drop for AnimationClock {
    fn drop(&mut self) {
        set_animation_clock_for_tests(None);
    }
}

/// The rendered value that is `name` mid-scramble, if one is on screen.
///
/// The noise substitutes characters one for one and never a space, so a
/// scrambled name is exactly as long as the name, still has no whitespace in
/// it, and is not the name — which no other value this screen prints at that
/// length is (every long one it draws, a saved server's `endpoint · recency`
/// line included, carries spaces).
fn scrambled_variant(values: &[String], name: &str) -> Option<String> {
    values
        .iter()
        .find(|value| {
            value.chars().count() == name.chars().count()
                && !value.chars().any(char::is_whitespace)
                && value.as_str() != name
        })
        .cloned()
}

/// An [`AuthTokensRepository`] that lists and mints like the mock but refuses
/// every revoke — the only way a headless suite reaches the RPC's failure path,
/// since the mock store happily revokes anything it actually holds.
struct RefusingRevoke {
    inner: MockAuthTokensRepository,
}

impl RefusingRevoke {
    fn seeded() -> Self {
        Self {
            inner: MockAuthTokensRepository::seeded(),
        }
    }
}

#[clean_signals::async_trait]
impl AuthTokensRepository for RefusingRevoke {
    async fn list_tokens(&self) -> Result<Vec<AuthToken>, AppFailure> {
        self.inner.list_tokens().await
    }

    async fn create_token(&self, request: CreateTokenRequest) -> Result<MintedToken, AppFailure> {
        self.inner.create_token(request).await
    }

    async fn revoke_token(&self, name: String) -> Result<(), AppFailure> {
        Err(AppFailure::Internal(format!("refused revoking {name}")))
    }
}

/// Mount connected with `reduce_motion` forced on: the theme every piece of
/// this screen reads through `use_context::<Theme>()`, published before the
/// app's own `init` exactly as [`mount_with_metrics`] publishes window metrics.
fn mount_with_reduced_motion() -> Harness {
    let mut theme = Theme::glyph_baseline();
    theme.motion.reduce_motion = true;
    provide_context(theme);
    let mut harness = Harness::mount();
    expect_connection_controller().connect(mock_target(TrustMode::NoVerification));
    harness.await_tokens();
    harness
}

/// Pump until `done`, or fail at [`PUMP_DEADLINE`] rather than hang — the
/// value-blind sibling of [`Harness::pump_until_values`], for the cases below
/// that wait on the toast queue instead of the screen.
fn pump_until(h: &mut Harness, what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + PUMP_DEADLINE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.pump_once();
    }
}

/// Whether the confirmation toast for `name`'s revoke has been raised — which
/// the screen only does on the far side of the RPC *and* the token-list refresh
/// it ends with, so it doubles as "the token has left `tokens`".
fn revoke_landed(name: &str) -> bool {
    app_toasts()
        .pending_untracked()
        .contains(&format!("Revoked {name}"))
}

#[test]
fn a_confirmed_revoke_scrambles_the_row_in_place_before_it_collapses() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let clock = AnimationClock::pinned();

    let second_row = h.center_of(SEEDED_TOKENS[1]).y;
    let dialog = open_dialog_by_label(&mut h, "revoke");
    confirm(&mut h, &dialog);

    // Three of the scramble's fourteen frames in.
    clock.advance(120.0);
    h.frame();

    let values = node_values(&h.root);
    assert!(
        !values.iter().any(|v| v == SEEDED_TOKENS[0]),
        "the name is glitching, not itself: {values:?}"
    );
    let glitched = scrambled_variant(&values, SEEDED_TOKENS[0])
        .unwrap_or_else(|| panic!("no scrambled name on screen: {values:?}"));
    assert!(
        values.iter().any(|v| v == "created 2026-07-02"),
        "…and the whole row is still on screen: {values:?}"
    );
    assert!(
        h.center_of(&glitched).y < h.center_of(SEEDED_TOKENS[1]).y,
        "the scrambling row is still the first of the two"
    );
    assert!(
        (h.center_of(SEEDED_TOKENS[1]).y - second_row).abs() < EPSILON,
        "…and nothing under it moved: the collapse has not started yet"
    );
}

#[test]
fn the_scrambling_row_outlives_the_refresh_that_removes_its_token() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    app_toasts().clear();
    let clock = AnimationClock::pinned();

    let second_row = h.center_of(SEEDED_TOKENS[1]).y;
    let dialog = open_dialog_by_label(&mut h, "revoke");
    confirm(&mut h, &dialog);

    // Hold the timeline early while the RPC — and the refresh it ends with —
    // actually land, which is the moment `tokens` stops holding this token.
    clock.advance(200.0);
    pump_until(&mut h, "the revoke to land", || {
        revoke_landed(SEEDED_TOKENS[0])
    });

    let values = node_values(&h.root);
    let glitched = scrambled_variant(&values, SEEDED_TOKENS[0]).unwrap_or_else(|| {
        panic!("the row stopped rendering when its token left the list: {values:?}")
    });
    assert!(
        values.iter().any(|v| v == "created 2026-07-02"),
        "…the whole snapshot keeps rendering: {values:?}"
    );
    assert!(
        h.center_of(&glitched).y < h.center_of(SEEDED_TOKENS[1]).y,
        "at the index it is vanishing from, above the token that stayed"
    );
    assert!(
        (h.center_of(SEEDED_TOKENS[1]).y - second_row).abs() < EPSILON,
        "…and the row under it never jumped up into the gap"
    );

    // Past the end of the timeline, the row is gone for good.
    clock.advance(TIMELINE_MS);
    h.pump_once();
    let values = node_values(&h.root);
    assert!(
        scrambled_variant(&values, SEEDED_TOKENS[0]).is_none(),
        "{values:?}"
    );
    assert!(
        !values.iter().any(|v| v == "created 2026-07-02"),
        "{values:?}"
    );
    assert!(
        values.iter().any(|v| v == SEEDED_TOKENS[1]),
        "…and only that row went: {values:?}"
    );
    assert_eq!(
        node_centers_by_label(&h.root, "revoke").len(),
        1,
        "the collapsed row's own button went with it"
    );
}

#[test]
fn a_refused_revoke_restores_the_row_and_raises_the_failure_toast() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_seeded(|| {
        // Overrides the repository `settings::di::provide` published moments
        // ago, in the same owner — the screen resolves it later still, in its
        // own `Component::init`.
        let refusing: SharedAuthTokensRepository = Arc::new(RefusingRevoke::seeded());
        provide_context(refusing);
    });
    expect_connection_controller().connect(mock_target(TrustMode::NoVerification));
    h.await_tokens();
    app_toasts().clear();
    let clock = AnimationClock::pinned();

    let dialog = open_dialog_by_label(&mut h, "revoke");
    confirm(&mut h, &dialog);
    pump_until(&mut h, "the refusal to surface", || {
        !app_toasts().pending_untracked().is_empty()
    });

    let toasted = app_toasts().pending_untracked();
    assert!(
        !toasted.iter().any(|m| m.starts_with("Revoked")),
        "a refused revoke confirms nothing: {toasted:?}"
    );
    assert!(
        toasted.iter().any(|m| m.contains("Something went wrong")),
        "…the screen's own failure bridge speaks instead: {toasted:?}"
    );

    // The row keeps collapsing (the refusal landed inside the first frames),
    // and once the timeline is over it is back — intact, and revocable again —
    // rather than stranded at zero height.
    clock.advance(TIMELINE_MS + 1.0);
    h.pump_once();
    h.pump_once();
    let values = node_values(&h.root);
    assert!(
        values.iter().any(|v| v == SEEDED_TOKENS[0]),
        "the refused row is back: {values:?}"
    );
    assert!(
        values.iter().any(|v| v == "created 2026-07-02"),
        "…all of it: {values:?}"
    );
    assert_eq!(
        node_centers_by_label(&h.root, "revoke").len(),
        SEEDED_TOKENS.len(),
        "…with a live revoke button on it again"
    );
}

#[test]
fn reduced_motion_revokes_instantly_with_no_scramble_frames() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_with_reduced_motion();
    app_toasts().clear();
    let _clock = AnimationClock::pinned();

    let dialog = open_dialog_by_label(&mut h, "revoke");
    confirm(&mut h, &dialog);

    // Every frame from the confirmation to the landed RPC: no noise, ever, and
    // the row gone from the first of them — the behaviour that shipped before
    // this animation existed.
    let deadline = Instant::now() + PUMP_DEADLINE;
    while !revoke_landed(SEEDED_TOKENS[0]) {
        let values = node_values(&h.root);
        assert!(
            scrambled_variant(&values, SEEDED_TOKENS[0]).is_none(),
            "reduced motion drew a scramble frame: {values:?}"
        );
        assert!(
            !values.iter().any(|v| v == SEEDED_TOKENS[0]),
            "…and the row went at once: {values:?}"
        );
        assert!(Instant::now() < deadline, "the revoke never landed");
        h.pump_once();
    }

    // Nothing is left waiting on a clock that never moves.
    h.pump_once();
    h.pump_once();
    let values = node_values(&h.root);
    assert!(
        !values.iter().any(|v| v == "created 2026-07-02"),
        "{values:?}"
    );
    assert_eq!(
        node_centers_by_label(&h.root, "revoke").len(),
        1,
        "the timeline was dropped, not left collapsed at zero height"
    );
}

/// Open the dashed create row's dialog, type `name` into its field, flip the
/// read-only toggle when `read_only`, and submit.
///
/// The two submit paths differ on purpose: with no toggle tap the field still
/// holds focus, so Enter submits it exactly like
/// `ui::dialogs::name_input_dialog`'s own field, reading the just-typed value
/// the framework hands its `on_submit` directly. Tapping the switch first
/// blurs that focus (`docs/CODE_STANDARDS.md`'s "a `Down` that doesn't
/// (re)claim focus … blurs the chain") — the switch owns its own tap target
/// and claims no focus of its own, exactly like the `GestureDetector` it
/// replaced — so Enter would route nowhere; the Save button submits instead.
/// Save's own "disabled" gate
/// (`open_create_token_dialog`'s own doc) is a `can_submit` bool captured at
/// the dialog body's *last rebuild*, so an explicit [`Harness::frame`] after
/// typing is what lets it see the typed name before either submit path fires
/// — Save's read of the toggle itself needs no such rebuild, since
/// `get_untracked()` always reads the signal's live value.
fn mint_token(h: &mut Harness, name: &str, read_only: bool) {
    let dialog = open_dialog_by_value(h, "create new token");
    assert_eq!(h.depth(), 2, "the dialog is pushed as its own page");
    h.tap(center(dialog.content_child()));
    h.type_text(name);
    h.frame();
    if read_only {
        h.tap_label("read-only");
        h.tap_label("Save");
    } else {
        h.press(NamedKey::Enter);
    }
    h.flush();
}

/// A landed mint's dialog pop and its follow-up secret-reveal sheet push both
/// land inside the same async continuation (`SettingsController::create_token`
/// refreshes the list and parks the secret before its caller opens the
/// sheet), so [`Harness::pump_until_values`] often returns with the sheet
/// already mid-transition. One extra [`Harness::settle`] finishes it before a
/// test taps into the sheet.
fn await_secret_reveal(h: &mut Harness, minted_name: &str) {
    h.pump_until_values("the minted token", |values| {
        values.iter().any(|v| v == minted_name)
    });
    h.settle();
    assert_eq!(
        h.depth(),
        2,
        "a landed mint opens the one-time secret reveal over the (already-popped) dialog"
    );
}

/// Dismiss the currently open secret-reveal sheet via its `Done` action.
fn dismiss_secret_reveal(h: &mut Harness) {
    h.tap_label("Done");
    h.flush();
    assert_eq!(h.depth(), 1, "dismissing the reveal closes it, and only it");
}

#[test]
fn the_dashed_row_mints_a_token_then_reveals_and_reloads() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    app_toasts().clear();

    mint_token(&mut h, "laptop", false);
    await_secret_reveal(&mut h, "laptop");

    // The reveal prints the mock's own deterministic one-time secret — never
    // shown anywhere before this sheet, and never again after it closes.
    let secret = format!("{MOCK_MINTED_SECRET_PREFIX}laptop");
    let values = node_values(&h.root);
    assert!(
        values.contains(&secret),
        "the reveal prints the one-time secret: {values:?}"
    );

    dismiss_secret_reveal(&mut h);

    let values = node_values(&h.root);
    assert!(
        !values.iter().any(|v| v.contains(MOCK_MINTED_SECRET_PREFIX)),
        "the secret is gone for good once dismissed: {values:?}"
    );
    // Appended, not replacing: both seeded tokens are still there…
    for name in SEEDED_TOKENS {
        assert!(values.iter().any(|v| v == name), "{name:?}: {values:?}");
    }
    // …and the new one carries the mock's own mint date and full scope.
    assert!(
        values.iter().any(|v| v == "created 2026-07-31"),
        "the minted token is dated today (mock): {values:?}"
    );
    assert_eq!(
        values.iter().filter(|v| *v == "read-write").count(),
        2,
        "a minted token is read-write by default: {values:?}"
    );
    let pending = app_toasts().pending_untracked();
    assert!(
        pending.iter().any(|m| m == "Created laptop"),
        "…and the mint is confirmed: {pending:?}"
    );
}

#[test]
fn the_read_only_toggle_mints_a_read_only_token_that_reloads_into_the_list() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    app_toasts().clear();

    mint_token(&mut h, "phone", true);
    await_secret_reveal(&mut h, "phone");
    dismiss_secret_reveal(&mut h);

    let values = node_values(&h.root);
    assert!(values.iter().any(|v| v == "phone"), "{values:?}");
    // The seeded read-only token plus the freshly minted one.
    assert_eq!(
        values.iter().filter(|v| *v == "read-only").count(),
        2,
        "the toggle's scope survives the reload: {values:?}"
    );
}

/// The create dialog's toggle is the real `frust::switch` — a `Role::Switch`
/// node carrying the read-only label, whose state tracks a tap landing on the
/// switch itself. `mint_token`'s own pins exercise the same tap through a
/// full mint round trip; this one pins the semantics node directly, the same
/// shape `tests/connect.rs`'s `remember_on`/`toggling_remember_off_…` pair
/// pins for Connect's remember toggle.
#[test]
fn the_read_only_toggle_is_a_real_switch_role_whose_own_tap_flips_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    open_dialog_by_value(&mut h, "create new token");
    assert_eq!(h.depth(), 2, "the dialog is pushed as its own page");

    let (role, state) = h
        .read_only_toggle()
        .expect("the dialog's toggle contributes a Switch node");
    assert_eq!(
        role,
        Role::Switch,
        "the create dialog's toggle is the real themed switch, not hand-drawn art"
    );
    assert_eq!(state, Toggled::False, "unchecked by default");

    // No outer row-wide `GestureDetector` wraps the switch any more (removed
    // for the pointer-capture conflict `pieces::dialog_read_only_row`'s own
    // doc explains), so the switch's own semantics bounds are what a tap must
    // land inside for this to flip. The dialog body is re-invoked on every
    // navigator rebuild (`open_create_token_dialog`'s own doc), so one
    // `Harness::frame` after the tap is what lets the switch's `checked`
    // reflect the signal the tap just moved — `RwSignal::set` itself is
    // synchronous, unlike the bell row's own async-preferences round trip.
    h.tap_label("read-only");
    h.frame();
    let (_, state) = h
        .read_only_toggle()
        .expect("the dialog's toggle still contributes a Switch node");
    assert_eq!(state, Toggled::True, "the tap landed on the switch itself");
}

#[test]
fn a_rejected_mint_never_claims_to_have_created_anything() {
    // The mock rejects a duplicate name, which is the one reachable failure on
    // this screen — and the confirmation toast must not contradict the failure
    // toast raised for the same call.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    app_toasts().clear();

    mint_token(&mut h, SEEDED_TOKENS[1], false);
    // The mock's own 20ms latency outlasts a handful of pumps, so wait for the
    // call to land rather than counting frames — what lands is the *failure*
    // toast the screen's `use_failure_listener` bridge raises.
    let deadline = Instant::now() + PUMP_DEADLINE;
    while app_toasts().pending_untracked().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the rejected mint surfaced nothing at all"
        );
        h.pump_once();
    }

    let pending = app_toasts().pending_untracked();
    assert!(
        !pending.iter().any(|m| m.starts_with("Created")),
        "a rejected mint raises no confirmation: {pending:?}"
    );
    // A rejection never parks a secret or opens the reveal.
    assert_eq!(h.depth(), 1, "a rejected mint opens no secret reveal");
    // And the list is untouched: still exactly the two seeded tokens.
    let values = node_values(&h.root);
    assert_eq!(
        values.iter().filter(|v| *v == "created 2026-07-31").count(),
        0,
        "nothing was minted: {values:?}"
    );
}

#[test]
fn a_read_only_connected_session_disables_minting_and_revoking() {
    // `ConnectionController::is_read_only` gates both mutating token
    // controls; the mock connect flow always logs in read-write
    // (`connection.rs`'s own `MockConnectionBackend`), so this test flips the
    // signal directly rather than through a login round trip.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    expect_connection_controller().is_read_only.set(true);
    h.frame();
    app_toasts().clear();

    h.tap_value("create new token");
    h.settle();
    assert_eq!(h.depth(), 1, "a read-only session opens no create dialog");
    let toasted = app_toasts().pending_untracked();
    assert!(
        toasted.iter().any(|m| m.contains("Read-only session")),
        "…and says why: {toasted:?}"
    );

    app_toasts().clear();
    h.tap_label("revoke");
    h.settle();
    assert_eq!(
        h.depth(),
        1,
        "a read-only session opens no revoke confirmation"
    );
    let toasted = app_toasts().pending_untracked();
    assert!(
        toasted.iter().any(|m| m.contains("Read-only session")),
        "…and says why: {toasted:?}"
    );

    // Neither guarded control actually touched the store.
    let values = node_values(&h.root);
    for name in SEEDED_TOKENS {
        assert!(values.iter().any(|v| v == name), "{name:?}: {values:?}");
    }
}

// ---------------------------------------------------------------------------
// 3b — saved servers and their credentials
// ---------------------------------------------------------------------------

/// A saved row for [`CRED_ENDPOINT`], stamped now so it sorts to the top of the
/// group (the list is most-recently-used first).
fn credential_row(has_credential: bool) -> SavedServer {
    SavedServer {
        host: "cred.example".to_string(),
        port: 50051,
        trust_mode: TrustMode::System,
        label: CRED_LABEL.to_string(),
        last_used_ms: epoch_millis(),
        read_only: false,
        has_credential,
        token_name: has_credential.then(|| CRED_TOKEN_NAME.to_string()),
        fingerprint: None,
    }
}

/// Mount connected, with [`credential_row`] in the saved-server store and its
/// secret in the credential store.
fn with_saved_credential(has_credential: bool) -> Harness {
    let mut harness = Harness::mount_seeded(|| {
        expect_saved_servers()
            .save(credential_row(has_credential))
            .expect("the mock store never fails");
        if has_credential {
            mock_credentials()
                .expect("tests/support opted this process into the mock")
                .seed(CRED_ENDPOINT, CRED_TOKEN);
        }
    });
    expect_connection_controller().connect(mock_target(TrustMode::NoVerification));
    harness.await_tokens();
    // The saved-server read is the *last* step of the screen's first load (the
    // token list and both preference groups land ahead of it), so awaiting the
    // tokens is not enough to see it.
    harness.pump_until_values("the saved-server rows", |values| {
        values.iter().any(|v| v == CRED_LABEL)
    });
    harness
}

#[test]
fn the_saved_server_group_lists_each_rig_and_flags_the_one_with_a_credential() {
    let _g = serial();
    let _ambient = setup();
    let h = with_saved_credential(true);

    let values = node_values(&h.root);
    assert!(
        values.iter().any(|v| v == CRED_LABEL),
        "the seeded rig's own label heads its row: {values:?}"
    );
    assert!(
        values.iter().any(|v| v.starts_with(CRED_ENDPOINT)),
        "…over its endpoint and recency: {values:?}"
    );
    assert!(
        values.iter().any(|v| v == "credential saved"),
        "the stored credential is visible as such: {values:?}"
    );
    // One `forget` per saved rig (the seeded three plus this one), and a
    // `delete credential` only where there is one to delete.
    assert_eq!(node_centers_by_label(&h.root, "forget").len(), 4);
    assert_eq!(
        node_centers_by_label(&h.root, "delete credential").len(),
        1,
        "a row with no credential offers no way to delete one"
    );
}

#[test]
fn deleting_a_credential_confirms_first_then_clears_the_row_and_revokes_its_token() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_saved_credential(true);
    let credentials = mock_credentials().expect("the mock service is published");

    // A dismissed confirmation deletes nothing.
    open_dialog_by_label(&mut h, "delete credential");
    assert_eq!(h.depth(), 2, "the confirmation is its own page");
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 1);
    assert_eq!(
        credentials.stored(CRED_ENDPOINT).as_deref(),
        Some(CRED_TOKEN),
        "the secret survives a dismissed confirmation"
    );

    // Confirming does.
    let dialog = open_dialog_by_label(&mut h, "delete credential");
    confirm(&mut h, &dialog);
    h.pump_until_values("the credential chip to disappear", |values| {
        !values.iter().any(|v| v == "credential saved")
    });

    assert_eq!(
        credentials.stored(CRED_ENDPOINT),
        None,
        "the secret is gone from the store"
    );
    assert!(
        credentials.prompts().is_empty(),
        "…and no biometric prompt was ever raised: a delete runs no gate"
    );
    let values = node_values(&h.root);
    assert!(
        values.iter().any(|v| v == CRED_LABEL),
        "the saved server itself is kept: {values:?}"
    );
    assert!(
        !values.iter().any(|v| v == CRED_TOKEN_NAME),
        "…and the server-side token behind it was revoked: {values:?}"
    );
}

#[test]
fn forgetting_a_saved_server_removes_the_row_and_its_credential() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_saved_credential(true);
    let credentials = mock_credentials().expect("the mock service is published");

    let dialog = open_dialog_by_label(&mut h, "forget");
    confirm(&mut h, &dialog);
    h.pump_until_values("the row to disappear", |values| {
        !values.iter().any(|v| v == CRED_LABEL)
    });

    assert_eq!(
        credentials.stored(CRED_ENDPOINT),
        None,
        "a secret filed under a forgotten endpoint would be unreachable forever"
    );
    assert_eq!(
        node_centers_by_label(&h.root, "forget").len(),
        3,
        "…and only that row went"
    );
}

// ---------------------------------------------------------------------------
// 4 — the picker sheets (screen 6)
// ---------------------------------------------------------------------------

/// Every picker the screen offers: the row that opens it, how many options it
/// lists, which index is selected out of the box, and what picking the last
/// option should leave in the row's value.
///
/// The `font family` row is **not** here: its options are not a
/// domain-declared list but the faces this device has installed plus a
/// downloadable catalog, so it has a sheet of its own — covered by section 4b
/// below.
const PICKERS: [(&str, usize, usize, &str); 3] = [
    ("font size", 5, 2, "14px"),
    ("cursor style", 3, 0, "bar"),
    ("layout refresh", 4, 1, "every 10s"),
];

#[test]
fn every_picker_opens_with_its_own_options_and_the_stored_one_checked() {
    let _g = serial();
    let _ambient = setup();

    for (row, options, selected, _) in PICKERS {
        let mut h = Harness::connected();
        let sheet = open_picker(&mut h, row);

        assert_eq!(
            sheet.runs,
            options + 1,
            "{row}: one run per option plus the title"
        );
        assert_eq!(
            sheet.vectors().len(),
            1,
            "{row}: exactly one row carries the check vector"
        );

        // The selected row fills amber **past** the sheet's own gutter, which
        // is screen 6's whole "full-bleed" look — and it is the row the stored
        // value sits at.
        let (origin, size) = sheet.selected_pick_row();
        let (panel_origin, panel_size) = sheet.panel();
        assert!(
            (origin.x - panel_origin.x).abs() < EPSILON,
            "{row}: the fill starts at the panel's own edge: {origin:?} vs {panel_origin:?}"
        );
        assert!(
            (size.width - panel_size.width).abs() < EPSILON,
            "{row}: …and spans its whole width"
        );
        // Row 0's top edge is the same distance above the selected row for
        // every picker, so a wrong `selected` shows up as a shifted fill.
        let first_row_top = origin.y - selected as f64 * size.height;
        assert!(
            first_row_top > panel_origin.y,
            "{row}: the first row sits inside the panel: {first_row_top} vs {panel_origin:?}"
        );

        // A scrim tap closes it and changes nothing.
        h.tap(SCRIM_POINT);
        h.flush();
        assert_eq!(h.depth(), 1, "{row}: dismissed");
    }
}

#[test]
fn picking_an_option_persists_it_and_updates_the_row() {
    let _g = serial();
    let _ambient = setup();

    for (row, options, selected, expected) in PICKERS {
        let mut h = Harness::connected();
        let sheet = open_picker(&mut h, row);

        pick(&mut h, &sheet, selected, options - 1);

        h.pump_until_values(expected, |values| values.iter().any(|v| v == expected));
        assert_eq!(h.depth(), 1, "{row}: the sheet closed behind the choice");
    }
}

#[test]
fn a_second_tap_on_a_picker_row_closes_the_sheet_once_and_keeps_the_route() {
    // The picker's close is immediate, but a pop only *lands* at the next
    // rebuild — so for one whole frame the row that just closed the sheet is
    // still there to be tapped again. That second tap used to pop the
    // `/settings` route out from under the picker; `ui::sheet`'s dismissal gate
    // is what makes it a no-op (see that module's "One sheet, one dismissal").
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let floor = h.push_route_floor();

    let before = h.frame();
    h.tap_value("cursor style");
    let after = h.settle();
    let sheet = Chrome::diff(&before, &after);
    assert_eq!(h.depth(), floor + 1, "the picker is up over the route");

    // Twice, with no frame in between — the whole point.
    let row = pick_row(&sheet, 0, 2);
    h.tap(row);
    h.tap(row);
    h.flush();

    assert_eq!(h.depth(), floor, "one pop, and the route is still under it");
    // …and the pick itself still lands exactly once, unchanged.
    h.pump_until_values("bar", |values| values.iter().any(|v| v == "bar"));
}

#[test]
fn a_picked_value_is_still_checked_when_the_sheet_is_reopened() {
    // The round trip that proves the choice reached the repository rather than
    // just the row: the reopened sheet's check has moved to the new row.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let first = open_picker(&mut h, "cursor style");
    let (before_origin, row_height) = first.selected_pick_row();
    pick(&mut h, &first, 0, 2);
    h.pump_until_values("bar", |values| values.iter().any(|v| v == "bar"));

    let second = open_picker(&mut h, "cursor style");
    let (after_origin, _) = second.selected_pick_row();
    let moved = after_origin.y - before_origin.y;
    assert!(
        (moved - 2.0 * row_height.height).abs() < EPSILON,
        "the check moved two rows down: {moved} vs {}",
        2.0 * row_height.height
    );
}

// ---------------------------------------------------------------------------
// 4b — the font picker sheet (its own sheet, not one of the four above)
// ---------------------------------------------------------------------------
//
// The font sheet lists two sections — the faces installed on the device, and
// the downloadable catalog — over one scrolling body. Read the same way every
// other sheet in this suite is (see the module docs): by counted glyph runs,
// by the one full-bleed amber fill the selected row paints, and by the row
// value on the screen behind once a choice lands.
//
// **No test here touches the network.** `support::setup` installs the offline
// font source for the whole process, and the two catalog cases install their
// own in-memory fixture source over it.

/// The bytes every fixture "download" serves — a real, parseable TTF, so the
/// install path's own registration probe has something to resolve. It resolves
/// to `JetBrains Mono` (`crate::fonts`' own unit test pins that), which is
/// exactly what makes it a fixture for the *reported name wins* rule: the
/// manifest below claims something else entirely.
const FIXTURE_FONT: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Light.ttf");

/// The catalog entry the download case installs.
const FIXTURE_A_FAMILY: &str = "FixtureA Nerd Font Mono";
const FIXTURE_A_DISPLAY: &str = "Fixture A Nerd Font Mono";
/// …and the one it leaves alone (whose two names agree, like 7 of the real
/// catalog's 34 entries).
const FIXTURE_B_FAMILY: &str = "Fixture B Nerd Font Mono";

/// The pre-seeded cache's family and catalog label — installed by writing the
/// cache directly, the way a previous launch's download would have left it.
const CACHED_FAMILY: &str = "Cached Nerd Font Mono";
const CACHED_DISPLAY: &str = "Cached Display Nerd Font";

/// A catalog served straight out of memory: two entries, both pointing at
/// [`FIXTURE_FONT`], with `family` and `displayName` deliberately different on
/// the first one.
struct FixtureFontSource {
    manifest: Vec<u8>,
    fetched: Arc<Mutex<Vec<String>>>,
    /// Serve a font whose bytes no longer hash to what the manifest vouched
    /// for — a truncated download, or a tampered mirror.
    corrupt: bool,
}

#[clean_signals::async_trait]
impl FontSource for FixtureFontSource {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, AppFailure> {
        self.fetched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push("manifest".to_string());
        Ok(self.manifest.clone())
    }

    async fn fetch_font(&self, url: &str) -> Result<Vec<u8>, AppFailure> {
        self.fetched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(url.to_string());
        let mut bytes = FIXTURE_FONT.to_vec();
        if self.corrupt {
            // Still a font by its magic bytes — only the hash can catch this.
            bytes[400] ^= 0xff;
        }
        Ok(bytes)
    }
}

/// Install the fixture catalog as this process's font source; answer the log
/// every fetch is recorded in.
fn use_fixture_catalog() -> Arc<Mutex<Vec<String>>> {
    use_fixture_catalog_serving(false)
}

/// [`use_fixture_catalog`], with `corrupt` deciding whether the font bytes it
/// serves still match the hash its own manifest advertises.
fn use_fixture_catalog_serving(corrupt: bool) -> Arc<Mutex<Vec<String>>> {
    let digest = sha256_hex(FIXTURE_FONT);
    let manifest = format!(
        r#"{{ "version": 1, "fonts": [
          {{ "id": "fixture-a", "displayName": "{FIXTURE_A_DISPLAY}",
             "family": "{FIXTURE_A_FAMILY}", "license": "OFL-1.1", "isNerdFont": true,
             "files": [ {{ "weight": 400, "style": "normal", "path": "FixtureA-Regular.ttf",
                          "bytes": {len}, "sha256": "{digest}",
                          "url": "https://example.invalid/FixtureA-Regular.ttf" }} ] }},
          {{ "id": "fixture-b", "displayName": "{FIXTURE_B_FAMILY}",
             "family": "{FIXTURE_B_FAMILY}", "license": "OFL-1.1", "isNerdFont": true,
             "files": [ {{ "weight": 400, "style": "normal", "path": "FixtureB-Regular.ttf",
                          "bytes": {len}, "sha256": "{digest}",
                          "url": "https://example.invalid/FixtureB-Regular.ttf" }} ] }}
        ] }}"#,
        len = FIXTURE_FONT.len(),
    );
    let fetched = Arc::new(Mutex::new(Vec::new()));
    muxr::fonts::use_font_source_for_tests(Arc::new(FixtureFontSource {
        manifest: manifest.into_bytes(),
        fetched: Arc::clone(&fetched),
        corrupt,
    }));
    fetched
}

/// SHA-256 as lowercase hex — the manifest's own spelling (the store's copy of
/// this is private to it).
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Point the font cache at a fresh temp directory of its own and forget every
/// family this process has installed — so each case below starts from "only
/// the bundled faces".
fn fresh_font_cache(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "muxr-settings-fonts-{tag}-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temp font cache");
    muxr::fonts::use_font_cache_dir_for_tests(dir.clone());
    muxr::fonts::reset_installed_fonts_for_tests();
    dir
}

/// Seed `dir` with a downloaded family the way a previous launch would have
/// left it, then run the startup scan that re-registers it.
fn seed_cached_family(dir: &std::path::Path) {
    let family_dir = dir.join("Cached_Nerd_Font_Mono");
    std::fs::create_dir_all(&family_dir).expect("a family dir");
    std::fs::write(family_dir.join("Cached-Regular.ttf"), FIXTURE_FONT).expect("the face");
    std::fs::write(
        family_dir.join("meta"),
        format!(
            "family={CACHED_FAMILY}\ndisplay={CACHED_DISPLAY}\nmanifest={CACHED_FAMILY}\n\
             file={} Cached-Regular.ttf\n",
            sha256_hex(FIXTURE_FONT)
        ),
    )
    .expect("the sidecar");
    let registered = muxr::fonts::store::scan_cache_and_register();
    assert_eq!(
        registered,
        vec![CACHED_FAMILY.to_string()],
        "the seeded family must register before the screen mounts"
    );
}

/// Open the font-family sheet and answer what it painted.
fn open_font_picker(h: &mut Harness) -> Chrome {
    let before = h.frame();
    h.tap_value("font family");
    let after = h.settle();
    let sheet = Chrome::diff(&before, &after);
    assert_eq!(h.depth(), 2, "the font sheet is pushed as its own page");
    sheet
}

/// Dismiss the open sheet through its scrim.
fn close_sheet(h: &mut Harness) {
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 1, "the sheet dismissed");
}

/// Pump until the catalog settles at `expected` — the async fetch the sheet
/// starts when it opens.
fn await_catalog(h: &mut Harness, expected: CatalogStatus) {
    let deadline = Instant::now() + PUMP_DEADLINE;
    while muxr::fonts::catalog::font_catalog().status() != expected {
        assert!(
            Instant::now() < deadline,
            "the catalog never reached {expected:?}"
        );
        h.pump_once();
    }
}

/// The preferences the screen actually persisted — read straight out of the
/// repository `di` published, which is what "persisted" means here.
fn stored_font_family() -> String {
    let repository = expect_preferences_repository();
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
        .block_on(async move {
            repository
                .load_preferences()
                .await
                .expect("the mock store never fails a load")
                .appearance
                .font_family
        })
}

#[test]
fn the_font_sheet_lists_the_bundled_face_and_says_so_quietly_when_the_catalog_is_gone() {
    // Offline (`support::setup`'s own default source) with nothing cached: the
    // installed section still lists the bundled face, the catalog section says
    // one quiet line, and **no toast is raised** — an unreachable catalog is
    // not something the user did.
    let _g = serial();
    let _ambient = setup();
    let _cache = fresh_font_cache("offline");
    let mut h = Harness::connected();
    app_toasts().clear();

    let sheet = open_font_picker(&mut h);
    await_catalog(&mut h, CatalogStatus::Unavailable);

    assert_eq!(
        sheet.vectors().len(),
        1,
        "exactly one row carries the check vector: the face in use"
    );
    assert_eq!(
        sheet.runs,
        font_sheet_runs(1, 0, true),
        "the bundled face, and one quiet line where the catalog would be"
    );
    // The one full-bleed amber fill is the selected installed row — the same
    // geometry screen 6's own picker paints (see `Chrome::selected_pick_row`).
    let (origin, size) = sheet.selected_pick_row();
    let (panel_origin, panel_size) = sheet.panel();
    assert!((origin.x - panel_origin.x).abs() < EPSILON, "{origin:?}");
    assert!((size.width - panel_size.width).abs() < EPSILON);

    assert!(
        app_toasts().pending_untracked().is_empty(),
        "a missing catalog raises no toast: {:?}",
        app_toasts().pending_untracked()
    );
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 1, "dismissed");
}

#[test]
fn a_cached_download_is_offered_beside_the_bundled_face_and_persists_its_family() {
    // The whole point of the two names: the row (and the settings row behind
    // it) prints the catalog's `displayName`, while what reaches the store is
    // the `family` a font stack can actually resolve.
    let _g = serial();
    let _ambient = setup();
    let cache = fresh_font_cache("cached");
    seed_cached_family(&cache);
    let mut h = Harness::connected();

    assert!(
        muxr::fonts::installed_families().contains(&CACHED_FAMILY.to_string()),
        "the cached family is installed for this process: {:?}",
        muxr::fonts::installed_families()
    );

    let sheet = open_font_picker(&mut h);
    // Two installed rows now (bundled + cached), the bundled one still
    // selected: its fill is row 0, and every font row is the same height.
    pick(&mut h, &sheet, 0, 1);

    h.pump_until_values(CACHED_DISPLAY, |values| {
        values.iter().any(|v| v == CACHED_DISPLAY)
    });
    assert_eq!(h.depth(), 1, "the sheet closed behind the choice");
    assert_eq!(
        stored_font_family(),
        CACHED_FAMILY,
        "the FAMILY is what persists — a display name would resolve to nothing"
    );
    assert!(
        !node_values(&h.root).iter().any(|v| v == CACHED_FAMILY),
        "…and the raw family name is never what the row shows: {:?}",
        node_values(&h.root)
    );
}

#[test]
fn the_catalog_lists_downloadable_fonts_and_a_download_installs_and_flips_the_row() {
    let _g = serial();
    let _ambient = setup();
    let _cache = fresh_font_cache("catalog");
    let fetched = use_fixture_catalog();
    let mut h = Harness::connected();
    app_toasts().clear();

    // Opening the sheet is what fetches the catalog — never app startup.
    open_font_picker(&mut h);
    await_catalog(&mut h, CatalogStatus::Ready);
    assert!(
        fetched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&"manifest".to_string()),
        "the sheet fetched the catalog on open"
    );
    close_sheet(&mut h);

    // Re-opened against a catalog already in hand, the sheet renders both
    // downloadable rows beside the installed one.
    let with_catalog = open_font_picker(&mut h).runs;
    assert_eq!(
        with_catalog,
        font_sheet_runs(1, 2, false),
        "the bundled face, then both catalog entries"
    );
    close_sheet(&mut h);

    // Download the first entry. The rows carry no semantics of their own (a
    // sheet is a navigator page — see the module docs) and the catalog
    // section's offset is not derivable from painted evidence, so this drives
    // the body of the very closure `pieces::font_picker_sheet`'s `on_download`
    // hands each available row.
    let catalog = muxr::fonts::catalog::font_catalog();
    let entry = catalog
        .entries()
        .iter()
        .find(|entry| entry.family == FIXTURE_A_FAMILY)
        .cloned()
        .expect("the fixture catalog carries it");
    let installing = entry.clone();
    frust::spawn_local(async move {
        catalog
            .install(installing)
            .await
            .expect("the fixture download verifies");
    });
    let deadline = Instant::now() + PUMP_DEADLINE;
    while muxr::fonts::store::registered_family_for(FIXTURE_A_FAMILY).is_none() {
        assert!(Instant::now() < deadline, "the download never landed");
        h.pump_once();
    }

    assert!(
        fetched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|url| url.ends_with("FixtureA-Regular.ttf")),
        "the entry's own file was fetched"
    );
    assert_eq!(
        muxr::fonts::store::registered_family_for(FIXTURE_A_FAMILY).as_deref(),
        Some("JetBrains Mono"),
        "the family the bytes really register as wins over the manifest's claim"
    );
    assert_eq!(
        catalog.state_of(&entry),
        FontState::Installed,
        "the row flips to installed"
    );
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "a clean install says nothing: {:?}",
        app_toasts().pending_untracked()
    );

    // …and the sheet, rebuilt, drops that row out of the catalog section: an
    // installed entry is listed once, above, under the family it registered
    // as.
    let after_install = open_font_picker(&mut h).runs;
    assert_eq!(
        after_install,
        font_sheet_runs(1, 1, false),
        "exactly one catalog row fewer than the sheet that offered it ({with_catalog})"
    );
    // The remaining row is the entry nobody downloaded.
    assert_eq!(
        catalog
            .entries()
            .iter()
            .filter(|entry| catalog.state_of(entry) == FontState::Available)
            .map(|entry| entry.family.clone())
            .collect::<Vec<_>>(),
        vec![FIXTURE_B_FAMILY.to_string()]
    );
}

#[test]
fn a_download_whose_bytes_do_not_verify_fails_the_row_and_caches_nothing() {
    // The integrity contract end to end: a served font that no longer hashes
    // to what the manifest vouched for installs nothing, leaves the row
    // retryable, and surfaces one failure toast — whose text is the generic
    // user message, never the hash detail.
    let _g = serial();
    let _ambient = setup();
    let cache = fresh_font_cache("corrupt");
    use_fixture_catalog_serving(true);
    let mut h = Harness::connected();
    app_toasts().clear();

    open_font_picker(&mut h);
    await_catalog(&mut h, CatalogStatus::Ready);
    close_sheet(&mut h);

    let catalog = muxr::fonts::catalog::font_catalog();
    let entry = catalog
        .entries()
        .iter()
        .find(|entry| entry.family == FIXTURE_A_FAMILY)
        .cloned()
        .expect("the fixture catalog carries it");
    let installing = entry.clone();
    let failed = Arc::new(Mutex::new(false));
    let recorded = Arc::clone(&failed);
    frust::spawn_local(async move {
        let outcome = catalog.install(installing).await;
        *recorded.lock().unwrap_or_else(|p| p.into_inner()) = outcome.is_err();
        if let Err(failure) = outcome {
            muxr::ui::toasts::app_toasts().raise(&failure);
        }
    });

    let deadline = Instant::now() + PUMP_DEADLINE;
    while !*failed.lock().unwrap_or_else(|p| p.into_inner()) {
        assert!(Instant::now() < deadline, "the install never answered");
        h.pump_once();
    }

    assert_eq!(
        catalog.state_of(&entry),
        FontState::Failed,
        "the row stays retryable rather than claiming to be installed"
    );
    assert!(
        !muxr::fonts::installed_families().contains(&FIXTURE_A_FAMILY.to_string()),
        "nothing was registered: {:?}",
        muxr::fonts::installed_families()
    );
    assert_eq!(
        std::fs::read_dir(&cache)
            .expect("the cache root")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .count(),
        0,
        "…and nothing was cached"
    );
    let toasts = app_toasts().pending_untracked();
    assert_eq!(toasts.len(), 1, "exactly one failure toast: {toasts:?}");
    assert!(
        !toasts[0].contains("sha256") && !toasts[0].contains(FIXTURE_A_FAMILY),
        "the toast never leaks the integrity detail: {toasts:?}"
    );
}

/// What the font sheet paints, in glyph runs: its title and caption, one
/// heading per section, two runs for an installed row (display name over meta
/// line) and three for an available one (those two plus its download
/// affordance), plus the catalog section's one-line note when it has one.
///
/// Spelled as a sum rather than a magic number so a failure says *which* part
/// of the sheet drifted.
fn font_sheet_runs(installed: usize, available: usize, note: bool) -> usize {
    const HEAD: usize = 2;
    const SECTION_HEADING: usize = 1;
    const INSTALLED_ROW: usize = 2;
    const AVAILABLE_ROW: usize = INSTALLED_ROW + 1;
    HEAD + SECTION_HEADING
        + installed * INSTALLED_ROW
        + SECTION_HEADING
        + usize::from(note)
        + available * AVAILABLE_ROW
}

// ---------------------------------------------------------------------------
// 5 — the bell switch
// ---------------------------------------------------------------------------

#[test]
fn the_bell_row_toggles_its_preference() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    assert_eq!(
        h.bell_state(),
        Some(Toggled::True),
        "the mock seeds the bell on, as the redesign draws it"
    );

    h.tap_label("bell notifications");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while h.bell_state() != Some(Toggled::False) {
        assert!(Instant::now() < deadline, "the bell never turned off");
        h.pump_once();
    }

    // …and back, so the row is a toggle rather than a one-way switch.
    h.tap_label("bell notifications");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while h.bell_state() != Some(Toggled::True) {
        assert!(Instant::now() < deadline, "the bell never came back on");
        h.pump_once();
    }
    assert_eq!(h.depth(), 1, "the bell row pushes nothing");
}

/// The bell switch is the real themed switch widget, not hand-drawn art. The
/// [`bell_state`] harness helper pins this by looking for a Switch role labelled
/// "bell notifications"; this test pins that the switch's own tap flips the
/// preference.
#[test]
fn the_bell_switch_is_a_real_switch_role_whose_own_tap_flips_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    assert_eq!(
        h.bell_state(),
        Some(Toggled::True),
        "the mock seeds the bell on, and bell_state() finds a Switch role"
    );

    // No outer row-wide `GestureDetector` wraps the switch (removed for the
    // pointer-capture conflict `pieces::bell_notifications_row`'s own doc
    // explains), so the switch's own semantics bounds are what a tap must land
    // inside to flip it. The settings screen doesn't rebuild on preference
    // change (unlike the dialog, which rebuilds on navigator mount), so the
    // preference signal update flows back through the async round trip.
    h.tap_label("bell notifications");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while h.bell_state() != Some(Toggled::False) {
        assert!(Instant::now() < deadline, "the switch never turned off");
        h.pump_once();
    }
}

// ---------------------------------------------------------------------------
// 6 — Disconnect
// ---------------------------------------------------------------------------

#[test]
fn disconnect_confirms_first_then_deauthenticates_and_lands_on_connect() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    assert!(expect_connection_controller().connected.get_untracked());

    // A dismissed confirmation disconnects nothing.
    open_dialog_by_label(&mut h, "Disconnect");
    assert_eq!(h.depth(), 2);
    h.tap(SCRIM_POINT);
    h.flush();
    assert!(
        expect_connection_controller().connected.get_untracked(),
        "cancelling leaves the connection up"
    );

    // Confirming clears the connection and resets the stack to `/`.
    let dialog = open_dialog_by_label(&mut h, "Disconnect");
    confirm(&mut h, &dialog);

    let connection = expect_connection_controller();
    assert!(!connection.connected.get_untracked(), "deauthenticated");
    assert_eq!(connection.server_info.get_untracked(), None);
    // The screen behind, still mounted by the fixture, agrees.
    h.frame();
    assert!(
        node_values(&h.root).iter().any(|v| v == "not connected"),
        "the connection group cleared: {:?}",
        node_values(&h.root)
    );
    // …and the guard now bounces the routes that need a session, which is what
    // "landed on Connect" means to the router.
    match app_nav().resolve("/sessions") {
        Resolution::Matched { location, .. } => assert_eq!(
            location.path, "/",
            "an unauthenticated /sessions is bounced to Connect"
        ),
        Resolution::Error { .. } => panic!("/sessions is a real route"),
    }
}

// ---------------------------------------------------------------------------
// 7 — responsive width-clamp pass (FINDINGS #21)
// ---------------------------------------------------------------------------

#[test]
fn wide_metrics_clamp_and_center_the_groups_column() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_with_metrics(Some(metrics_at(SURFACE)));
    h.frame();

    // Disconnect is the screen's last, full-width row — the cheapest honest
    // read on where the clamped column itself sits: centered,
    // `SETTINGS_CLAMP_WIDTH` wide, minus the row's own `ROW_PAD_X` margin on
    // each side.
    let disconnect = bounds_by_label(&h.root, "Disconnect");
    let margin = (SURFACE.width - SETTINGS_CLAMP_WIDTH) / 2.0;
    assert!(
        (disconnect.x0 - (margin + ROW_PAD_X)).abs() < 0.5,
        "the column centers within the surface: left edge at {:.1}, expected {:.1}",
        disconnect.x0,
        margin + ROW_PAD_X
    );
    assert!(
        (disconnect.width() - (SETTINGS_CLAMP_WIDTH - 2.0 * ROW_PAD_X)).abs() < 0.5,
        "Disconnect spans the clamped column, not the whole surface: {:.1}",
        disconnect.width()
    );
}

#[test]
fn narrow_metrics_leave_the_groups_column_unclamped_like_no_metrics_at_all() {
    let _g = serial();
    let _ambient = setup();
    // Explicit narrow metrics, even though the harness still paints at the
    // full-width `SURFACE` — proving the branch keys on the *published*
    // width, not the paint surface.
    let mut h = mount_with_metrics(Some(metrics_at(Size::new(400.0, SURFACE.height))));
    h.frame();

    let disconnect = bounds_by_label(&h.root, "Disconnect");
    let expected_width = SURFACE.width - 2.0 * ROW_PAD_X;
    assert!(
        (disconnect.width() - expected_width).abs() < 0.5,
        "narrow metrics take the same unclamped branch as no metrics: {:.1}",
        disconnect.width()
    );
    assert!((disconnect.x0 - ROW_PAD_X).abs() < 0.5, "{:?}", disconnect);
}

// ---------------------------------------------------------------------------
// 8 — saved-server card clip regression at the phone-narrow width (G47)
// ---------------------------------------------------------------------------

/// A frame width matching the device's own narrow branch — the Xiaomi's 1080
/// physical px is ~395 logical px at its density, comfortably under
/// `crate::responsive::WIDE_BREAKPOINT_PX` (600); `360` here sits inside that
/// same narrow band. Tall enough that the saved-server group — which sorts
/// [`with_saved_credential`]'s freshly-stamped row to its top — is still
/// laid out on-screen rather than below the fold (this suite's own module
/// docs note why an off-screen row is invisible to its semantics probe).
const NARROW_SURFACE: Size = Size::new(360.0, 3000.0);

#[test]
fn the_saved_server_cards_forget_button_never_clips_off_the_narrow_frame() {
    let _g = serial();
    let _ambient = setup();
    // A credentialed row renders both `delete credential` and `forget` on the
    // one action row — the widest case, and the one the old layout's
    // inflexible meta-row tail actually clipped.
    let mut h = with_saved_credential(true);

    // One more frame at `NARROW_SURFACE` — `Harness::frame` always paints at
    // `SURFACE`'s 800px wide, which never exercised the phone-narrow width
    // the bug only shows up under.
    frame_sized(
        &mut h.root,
        &mut h.logic,
        &mut h.state,
        &mut h.tcx,
        NARROW_SURFACE,
    );

    let forgets = all_bounds_by_label(&h.root, "forget");
    assert!(
        !forgets.is_empty(),
        "the saved-server group must still render its forget rows at this width"
    );
    for rect in &forgets {
        assert!(
            rect.x1 <= NARROW_SURFACE.width + EPSILON,
            "forget's right edge ({:.1}) must stay inside the {:.0}px frame — the \
             old all-inflexible-but-for-one-spacer meta row clipped it off-screen \
             instead of ever wrapping or shrinking: {rect:?}",
            rect.x1,
            NARROW_SURFACE.width,
        );
    }
}
