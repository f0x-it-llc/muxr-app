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
//!   filled vector path the sheet adds (the `✓`);
//! - **what the rows say** — by picking one and watching the row value on the
//!   screen behind change, which is the round trip that matters anyway.
//!
//! Row *k* is addressed relative to the selected row's measured rect: every
//! `.pick-row` is one line tall, so the pitch is that rect's own height.

use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, GetUntracked, NavigatorController, Resolution, Set, SizedBox, Stack,
    WindowMetrics, any, navigator, provide_context,
};
use frust_core::accesskit::{Node, Rect, Toggled};
use frust_core::{NamedKey, PointerPhase, RenderRoot, WindowInsets};
use frust_reactive::ReactiveRuntime;
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::connect::di::{expect_saved_servers, mock_credentials};
use muxr::features::connect::domain::entities::{SavedServer, epoch_millis};
use muxr::features::settings::data::repositories::mock_auth_tokens_repository::MOCK_MINTED_SECRET_PREFIX;
use muxr::features::settings::presentation::pages::settings_page;
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

    /// The overlay's own panel — the biggest rect it painted.
    fn panel(&self) -> (Point, Size) {
        largest(&self.rounded).expect("an overlay always paints its own panel")
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
fn tapping_a_connection_row_says_it_is_not_wired_yet() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    app_toasts().clear();

    h.tap_value("server url");
    h.frame();

    let pending = app_toasts().pending_untracked();
    assert!(
        pending.iter().any(|m| m.contains("Not yet available")),
        "the row is honest rather than dead: {pending:?}"
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
    h.pump_until_values("the revoked token to disappear", |values| {
        !values.iter().any(|v| v == SEEDED_TOKENS[0])
    });
    assert_eq!(h.depth(), 1);
    assert!(
        node_values(&h.root).iter().any(|v| v == SEEDED_TOKENS[1]),
        "…and only the one token went"
    );
}

/// Open the dashed create row's dialog, type `name` into its field, flip the
/// read-only toggle when `read_only`, and submit.
///
/// The two submit paths differ on purpose: with no toggle tap the field still
/// holds focus, so Enter submits it exactly like
/// `ui::dialogs::name_input_dialog`'s own field, reading the just-typed value
/// the framework hands its `on_submit` directly. Tapping the toggle row first
/// blurs that focus (`docs/CODE_STANDARDS.md`'s "a `Down` that doesn't
/// (re)claim focus … blurs the chain"), so Enter would route nowhere; the Save
/// button submits instead. Save's own "disabled" gate
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
const PICKERS: [(&str, usize, usize, &str); 4] = [
    ("font family", 4, 0, "JetBrains Mono"),
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
            sheet.paths.len(),
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
