//! Headless behavior coverage for the `/settings` screen
//! (`src/features/settings/**`) — no GPU, no window: a `RenderRoot` over the
//! real page builder, driven through `tests/support`.
//!
//! `NavigatorWidget` publishes no semantics for its pages, so [`Harness`] mounts
//! the screen as the bottom layer of a `Stack`, with an otherwise-empty
//! navigator over it — the same shape `tests/sessions.rs` uses. The screen's
//! rows stay reachable by value, while its dialogs and picker sheets are
//! exercised end to end through a real navigator and located by painted
//! geometry.
//!
//! Six groups plus Disconnect run past 1000px, so every frame here lays out at
//! [`SURFACE`], not `support::surface()`'s default — hence this file's own
//! [`Harness::pump_once`] rather than `support::pump_frames`, which frames at
//! the default (smaller) size and would silently re-layout what the next probe
//! is about to measure.
//!
//! A picker sheet's rows are invisible to `node_values` (a sheet is a navigator
//! page), so they're read through three painted facts instead: row count via
//! the `glyph_runs` delta (`options + 1` for the title), the selected row via
//! its full-panel-width fill plus its `✓` vector, and a picked value via the
//! row on the screen behind changing. Row *k* is addressed relative to the
//! selected row's own height, since every `.pick-row` is one line tall.

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use clean_signals::Failure;
use frust::{
    AnyView, Component, GetUntracked, NavigatorController, Resolution, Set, SizedBox, Stack,
    WindowMetrics, any, navigator, provide_context, spawn_local,
};
use frust_core::accesskit::{Node, Rect, Role, Toggled};
use frust_core::{InputEvent, NamedKey, PointerPhase, RenderRoot, ScrollDelta};
use frust_reactive::ReactiveRuntime;
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::failure::AppFailure;
use muxr::features::connect::data::repositories::MockCredentialService;
use muxr::features::connect::data::repositories::mock_saved_servers::MockSavedServersRepository;
use muxr::features::connect::di::{
    KeyringHandle, expect_keyring, expect_saved_servers, mock_credentials,
};
use muxr::features::connect::domain::entities::{
    CredentialStatus, SavedServer, StoreRequest, UnlockRequest, UnlockedCredential,
    UnlockedKeyring, epoch_millis,
};
use muxr::features::connect::domain::keyring::Keyring;
use muxr::features::connect::domain::repositories::CredentialService;
use muxr::features::connect::presentation::pages::ADD_SERVER_HEADING;
use muxr::features::pro::data::repositories::MockEntitlementRepository;
use muxr::features::pro::di::{
    SharedEntitlementRepository, expect_pro_controller, mock_entitlement,
};
use muxr::features::pro::domain::entities::{
    Entitlement, ProOffer, PurchaseOutcome, RestoreResult,
};
use muxr::features::pro::domain::repositories::EntitlementRepository;
use muxr::features::pro::presentation::controller::ProController;
use muxr::features::pro::presentation::{NOT_NOW_LABEL, NOTHING_TO_RESTORE_TOAST, RESTORE_LABEL};
use muxr::features::settings::data::repositories::mock_auth_tokens_repository::MOCK_MINTED_SECRET_PREFIX;
use muxr::features::settings::data::repositories::{
    MockAuthTokensRepository, MockPreferencesRepository,
};
use muxr::features::settings::di::{SharedAuthTokensRepository, expect_preferences_repository};
use muxr::features::settings::domain::entities::{
    AppearancePrefs, AuthToken, CreateTokenRequest, MintedToken,
};
use muxr::features::settings::domain::repositories::AuthTokensRepository;
use muxr::features::settings::presentation::controller::SettingsController;
use muxr::features::settings::presentation::pages::settings_page;
use muxr::features::settings::presentation::scramble::{
    TIMELINE_MS, advance_animation_clock_for_tests, set_animation_clock_for_tests,
};
use muxr::features::settings::presentation::widgets::{PLAN_LABEL, PRO_GROUP_LABEL};
use muxr::fonts::catalog::{CatalogStatus, FontState};
use muxr::fonts::source::FontSource;
use muxr::routes::{app_nav, install_app_router};
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, by_width, center, char_key, frame_sized, ime_bottom_inset, largest,
    named_key, node_center_by_label, node_centers_by_label, node_centers_by_value, node_labels,
    node_values, pointer, serial, settle_sized, setup, smallest, tap,
};

/// The surface every frame here lays out at — tall enough that the whole screen
/// (six groups, the dashed create row, the saved-server rows and Disconnect)
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
/// The groups column's max width past `crate::responsive::WIDE_BREAKPOINT_PX`
/// (the screen's own width-clamp pass), transcribed (private to
/// `presentation::pages`).
const SETTINGS_CLAMP_WIDTH: f64 = 640.0;
/// The mock connection's latency reading, printed (private to `connection`).
const MOCK_LATENCY_VALUE: &str = "36ms";
/// `.disconnect-btn`'s own horizontal margin, transcribed (private to
/// `presentation::widgets`) — what the Disconnect button spans the width
/// *between*, inside whatever box the scroller itself is laid out at.
const ROW_PAD_X: f64 = 16.0;
/// The screen's closing action: its button, the Tablet card's button and the
/// confirmation's own title and confirm.
const LOCK_AND_DISCONNECT: &str = "Lock & disconnect";
/// The connected saved server's chip, and the saved group's closing row.
const CONNECTED_CHIP: &str = "connected";
const ADD_SERVER_ROW: &str = "add server";

/// The seeded token names `data`'s mock repository ships — the redesign's two
/// `.token-item`s, in order.
const SEEDED_TOKENS: [&str; 2] = ["zelli-mobile-100-71-31-57-mr3g6w5g", "token_2"];
/// The six group labels, as the screen prints them (the source's
/// `text-transform:uppercase`, applied in Rust — `text` carries no transform).
const GROUPS: [&str; 6] = [
    "CONNECTION",
    "AUTH TOKENS",
    "SAVED SERVERS",
    "MUXR PRO",
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

/// [`Harness::mount`], first publishing `metrics` into context when `Some` —
/// the shape `muxr`'s pages read for the responsive width-clamp pass. `None`
/// leaves context unset, the state every other test in this suite mounts under.
fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Harness {
    support::mount_with_metrics(metrics, Harness::mount)
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
    /// controller reads them (which happens once, at construction).
    fn mount_seeded(seed: impl FnOnce()) -> Self {
        // The real composition root: publishes the connection controller and
        // every feature's `di::provide`, including `connect`'s own for the
        // saved-server group.
        let state = MuxrApp.init();
        seed();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        // Re-points `routes::app_nav()` at this fixture's own navigator, real
        // route table and session guard included (what makes Disconnect's
        // `go("/")` real navigation) — playing both of production's
        // navigators, so every overlay stays a page of this one mounted one.
        let _router = install_app_router(&nav, &nav, expect_connection_controller());
        let logic_nav = nav.clone();
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            let screen_nav = logic_nav.clone();
            let overlay_nav = logic_nav.clone();
            any(Stack(vec![
                any(settings_page(screen_nav)),
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

    /// The smallest scroll viewport holding the point `(x, y)`.
    fn viewport_around(&self, x: f64, y: f64) -> Rect {
        self.root
            .semantics()
            .nodes
            .iter()
            .filter(|(_, node): &&(_, Node)| node.role() == Role::ScrollView)
            .filter_map(|(_, node)| node.bounds())
            .filter(|b| b.x0 <= x && x <= b.x1 && b.y0 <= y && y <= b.y1)
            .min_by(|a, b| a.area().total_cmp(&b.area()))
            .unwrap_or_else(|| panic!("no scroll viewport holds ({x}, {y})"))
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

    /// The bell switch's announced state — the `Role::Switch` node the real
    /// `frust_glyph::toggle` in `widgets::misc::bell_notifications_row` contributes itself.
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

    /// Whether the screen currently paints a `Role::ProgressIndicator` node —
    /// the Downloading font row's determinate glyph progress bar
    /// (`widgets::font_picker::font_row_progress`; `frust_widgets::glyph::progress` is the
    /// only widget in this app's tree that contributes this role).
    fn font_progress_bar_visible(&self) -> bool {
        self.root
            .semantics()
            .nodes
            .iter()
            .any(|(_, node)| node.role() == Role::ProgressIndicator)
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
fn all_six_groups_and_the_disconnect_action_render() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    // The literal `GROUPS` entry is what the screen actually renders
    // (`text-transform:uppercase` applied in Rust); tying it back to
    // `PRO_GROUP_LABEL` here is what makes a drift between the two a test
    // failure instead of a silent mismatch.
    assert_eq!(PRO_GROUP_LABEL.to_uppercase(), "MUXR PRO");

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
        PLAN_LABEL,
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
    // The freshly-mocked install is Free, so the plan row prints it.
    assert!(values.iter().any(|v| v == "Free"), "{values:?}");
    let labels = node_labels(&h.root);
    assert!(
        labels.iter().any(|l| l == LOCK_AND_DISCONNECT),
        "{labels:?}"
    );
}

#[test]
fn the_connection_group_prints_the_mocks_latency_after_the_tls_row() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    let at = |needle: &str| {
        values
            .iter()
            .position(|v| v == needle)
            .unwrap_or_else(|| panic!("{needle:?} renders: {values:?}"))
    };
    assert!(at("latency") > at("tls certificate"), "{values:?}");
    assert_eq!(at(MOCK_LATENCY_VALUE), at("latency") + 1, "{values:?}");
}

#[test]
fn a_disconnected_latency_row_says_not_connected() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::mount();

    let values = node_values(&h.root);
    let at = values
        .iter()
        .position(|v| v == "latency")
        .unwrap_or_else(|| panic!("the latency row renders: {values:?}"));
    assert_eq!(values[at + 1], "not connected", "{values:?}");
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

/// The revoke timeline runs off a wall clock; pinning it (through
/// `presentation::scramble`'s test seam) makes "three scramble frames in" an
/// exact position instead of an "eventually". Restores the wall clock on drop,
/// so a later test is never left holding a frozen one.
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

/// The rendered value that is `name` mid-scramble, if one is on screen. The
/// noise substitutes characters one for one and never a space, so a scrambled
/// name is exactly as long as `name`, whitespace-free, and not `name` itself —
/// which no other value this screen prints at that length is.
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
    let mut theme = frust_glyph::baseline();
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
/// read-only toggle when `read_only`, and submit. The two submit paths differ
/// on purpose: with no toggle tap the field still holds focus, so Enter
/// submits it; tapping the switch first blurs it, so Save submits instead. An
/// explicit [`Harness::frame`] after typing lets `can_submit` see the typed
/// name before either path fires (it's captured at the body's last rebuild).
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

/// A landed mint's dialog pop and its secret-reveal sheet push land in the
/// same async continuation, so [`Harness::pump_until_values`] often returns
/// mid-transition; one extra [`Harness::settle`] finishes it.
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

    // No outer row-wide `GestureDetector` wraps the switch, so a tap must land
    // inside its own semantics bounds. `RwSignal::set` is synchronous, so one
    // `Harness::frame` is enough to see it reflected.
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

// -- Keyboard-aware dialogs ---------------------------------------------------
//
// App-level pin for `frust-widgets`' keyboard-avoidance behavior (the panel
// centers in the keyboard-free band, not the full window), exercised against
// the New Token dialog — the tallest content any dialog here pushes. No app
// production code changed for this; it's the framework's path dep proven at
// this app's own scale.
//
// The other half — a `Down` on dead panel space blurs the field without
// dismissing the dialog — is pinned below.

#[test]
fn new_token_dialog_actions_clear_the_keyboard_on_a_short_display() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    open_dialog_by_value(&mut h, "create new token");
    assert_eq!(h.depth(), 2, "the dialog is pushed as its own page");

    // An iPhone-SE-class display with a keyboard up.
    let se = Size::new(320.0, 568.0);
    let ime_bottom = 216.0;
    let free_bottom = se.height - ime_bottom;

    // Sanity first: at this size, with no keyboard published, the panel
    // really does reach past where the keyboard-free band would end — so the
    // pin below is exercising real avoidance, not a size that always fits.
    frame_sized(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, se);
    let baseline_save = bounds_by_label(&h.root, "Save");
    assert!(
        baseline_save.y1 > free_bottom,
        "sanity: full-window centering overlaps where the keyboard would sit \
         at this size: {baseline_save:?} vs free_bottom={free_bottom}"
    );

    // Publish the keyboard inset (the shell's IME listener → `set_insets`),
    // then re-layout at the same short frame.
    h.root.set_insets(ime_bottom_inset(ime_bottom));
    frame_sized(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, se);

    let save = bounds_by_label(&h.root, "Save");
    let cancel = bounds_by_label(&h.root, "Cancel");
    for (label, bounds) in [("Save", save), ("Cancel", cancel)] {
        assert!(
            bounds.y0 >= 0.0 && bounds.y1 <= free_bottom,
            "{label} sits fully inside the keyboard-free area (0..{free_bottom}): {bounds:?}"
        );
    }
}

/// The fix's other half (see the [module comment](self) above): a `Down` on
/// the dialog's dead panel space blurs the focused field's IME session
/// without dismissing the dialog, and a second tap re-engages it.
/// `panel.origin + (2, 2)` sits inside `DIALOG_PADDING`, above every child.
#[test]
fn a_dead_space_down_blurs_the_focused_name_field_without_closing_the_dialog() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let dialog = open_dialog_by_value(&mut h, "create new token");
    assert_eq!(h.depth(), 2, "the dialog is pushed as its own page");

    h.tap(center(dialog.content_child()));
    assert!(
        h.root.ime_state().is_some(),
        "precondition: the field owns a live IME session"
    );

    let (panel_origin, _) = dialog.panel();
    let dead = Point::new(panel_origin.x + 2.0, panel_origin.y + 2.0);
    h.tap(dead);

    assert!(
        h.root.ime_state().is_none(),
        "a Down on dead panel space ends the field's IME session — the keyboard retracts"
    );
    assert_eq!(h.depth(), 2, "the dialog itself is still open");

    h.tap(center(dialog.content_child()));
    assert!(
        h.root.ime_state().is_some(),
        "a second tap on the field re-engages it"
    );
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
    // Locked, the delete keeps the row, so it unlocks the keyring rather
    // than claim a vault copy it could not reach: its only prompt is the
    // keyring's own.
    let prompts = credentials.prompts();
    assert!(
        !prompts.is_empty() && prompts.iter().all(|reason| reason.starts_with("Unlock ")),
        "…and the only prompt raised was the keyring unlock: {prompts:?}"
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
// 3b — the keyring: switching servers, eviction, the add-server row
// ---------------------------------------------------------------------------

/// Mount connected (to the mock host, which is no saved row) with
/// [`credential_row`]'s token in the **vault**, the keyring published over it
/// when `unlocked`, and Pro unlocked when `pro` — the seeded rows past the
/// first make a free install's switch a blocked one.
fn with_vault_credential(unlocked: bool, pro: bool) -> Harness {
    let mut harness = Harness::mount_seeded(|| {
        expect_saved_servers()
            .save(credential_row(true))
            .expect("the mock store never fails");
        mock_credentials()
            .expect("tests/support opted this process into the mock")
            .seed_vault(&[(CRED_ENDPOINT, CRED_TOKEN)]);
        if unlocked {
            let mut keyring = Keyring::new();
            keyring.insert(
                CRED_ENDPOINT,
                zeroize::Zeroizing::new(CRED_TOKEN.to_string()),
            );
            expect_keyring().publish(keyring);
        }
        if pro {
            mock_entitlement()
                .expect("tests/support opted this process into the mock")
                .set_entitlement(Entitlement::Pro);
        }
    });
    expect_connection_controller().connect(mock_target(TrustMode::NoVerification));
    harness.await_tokens();
    harness.pump_until_values("the saved-server rows", |values| {
        values.iter().any(|v| v == CRED_LABEL)
    });
    if pro {
        harness.pump_until_values("the plan row to read Pro", |values| {
            values.iter().any(|v| v == "Pro")
        });
    }
    harness
}

/// The connected endpoint, as the connection reports it.
fn connected_endpoint() -> Option<String> {
    expect_connection_controller()
        .server_info
        .get_untracked()
        .map(|info| info.endpoint)
}

#[test]
fn tapping_a_saved_row_with_the_keyring_unlocked_switches_to_it_without_a_prompt() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, true);
    let credentials = mock_credentials().expect("the mock service is published");
    assert_eq!(connected_endpoint().as_deref(), Some("mock.example:7777"));
    assert!(
        !node_values(&h.root).iter().any(|v| v == CONNECTED_CHIP),
        "no saved row is the connected one yet"
    );

    h.tap_value(CRED_LABEL);
    h.pump_once();

    assert_eq!(
        connected_endpoint().as_deref(),
        Some(CRED_ENDPOINT),
        "the connection moved to the tapped row"
    );
    assert!(
        expect_connection_controller().connected.get_untracked(),
        "…and it is up"
    );
    assert!(
        credentials.prompts().is_empty(),
        "the token came straight off the unlocked keyring"
    );
    h.frame();
    assert!(
        node_values(&h.root).iter().any(|v| v == CONNECTED_CHIP),
        "the switched-to row now wears the connected chip: {:?}",
        node_values(&h.root)
    );
}

/// Re-save [`credential_row`] as a trust-anyway row: choosing "Trust anyway"
/// when it was added is the consent, so no switch path asks again.
fn make_credential_row_unverified() {
    let mut row = credential_row(true);
    row.trust_mode = TrustMode::NoVerification;
    expect_saved_servers()
        .save(row)
        .expect("the mock store never fails");
}

const RETIRED_UNVERIFIED_TITLE: &str = "Connect without verification?";

#[test]
fn a_saved_unverified_row_switches_unlocked_with_no_confirm_and_no_prompt() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, true);
    make_credential_row_unverified();
    let credentials = mock_credentials().expect("the mock service is published");

    h.tap_value(CRED_LABEL);
    h.pump_once();

    assert_eq!(connected_endpoint().as_deref(), Some(CRED_ENDPOINT));
    assert!(credentials.prompts().is_empty(), "no biometric prompt");
    assert!(
        !node_values(&h.root)
            .iter()
            .any(|v| v == RETIRED_UNVERIFIED_TITLE),
        "no confirm overlay"
    );
}

#[test]
fn a_saved_unverified_row_switches_locked_with_exactly_one_prompt_and_no_confirm() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(false, true);
    make_credential_row_unverified();
    let credentials = mock_credentials().expect("the mock service is published");

    h.tap_value(CRED_LABEL);
    pump_until(&mut h, "the switch to land", || {
        connected_endpoint().as_deref() == Some(CRED_ENDPOINT)
    });

    assert_eq!(credentials.prompts().len(), 1, "one keyring prompt only");
    assert!(
        !node_values(&h.root)
            .iter()
            .any(|v| v == RETIRED_UNVERIFIED_TITLE),
        "no confirm overlay"
    );
}

/// The mock host's saved row, without a stored credential.
fn mock_row_without_credential() -> SavedServer {
    SavedServer {
        host: "mock.example".to_string(),
        port: 7777,
        trust_mode: TrustMode::NoVerification,
        label: "Mock rig".to_string(),
        last_used_ms: epoch_millis(),
        read_only: true,
        has_credential: false,
        token_name: None,
        fingerprint: None,
    }
}

fn saved_endpoints() -> Vec<String> {
    expect_saved_servers()
        .list()
        .expect("the mock store never fails")
        .iter()
        .map(SavedServer::endpoint)
        .collect()
}

#[test]
fn switching_away_from_a_credentialless_server_via_a_settings_row_forgets_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, true);
    expect_saved_servers()
        .save(mock_row_without_credential())
        .expect("the mock store never fails");

    h.tap_value(CRED_LABEL);
    h.pump_once();

    assert_eq!(connected_endpoint().as_deref(), Some(CRED_ENDPOINT));
    let saved = saved_endpoints();
    assert!(
        !saved.iter().any(|e| e == "mock.example:7777"),
        "the credential-less row is gone: {saved:?}"
    );
    assert!(saved.iter().any(|e| e == CRED_ENDPOINT));
}

#[test]
fn lock_and_disconnect_from_a_credentialless_server_keeps_its_row() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, false);
    expect_saved_servers()
        .save(mock_row_without_credential())
        .expect("the mock store never fails");

    let dialog = open_dialog_by_label(&mut h, LOCK_AND_DISCONNECT);
    confirm(&mut h, &dialog);

    assert!(!expect_connection_controller().connected.get_untracked());
    assert!(
        saved_endpoints().iter().any(|e| e == "mock.example:7777"),
        "only switching away forgets"
    );
}

#[test]
fn tapping_a_saved_row_with_the_keyring_locked_prompts_once_then_switches() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(false, true);
    let credentials = mock_credentials().expect("the mock service is published");

    h.tap_value(CRED_LABEL);
    pump_until(&mut h, "the switch to land", || {
        connected_endpoint().as_deref() == Some(CRED_ENDPOINT)
    });

    assert_eq!(
        credentials.prompts().len(),
        1,
        "one keyring prompt: {:?}",
        credentials.prompts()
    );
    assert!(
        expect_keyring().is_unlocked(),
        "…which published the keyring"
    );
}

#[test]
fn a_blocked_switch_raises_the_paywall_and_dials_nothing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, false);
    let credentials = mock_credentials().expect("the mock service is published");

    h.tap_value(CRED_LABEL);
    h.settle();

    assert_eq!(h.depth(), 2, "the paywall is up");
    assert!(
        node_center_by_label(&h.root, NOT_NOW_LABEL).is_some(),
        "{:?}",
        node_labels(&h.root)
    );
    assert_eq!(
        connected_endpoint().as_deref(),
        Some("mock.example:7777"),
        "the free tier refused the switch before it dialed"
    );
    assert!(credentials.prompts().is_empty(), "…or unlocked anything");
}

#[test]
fn lock_and_disconnect_locks_the_keyring_and_returns_to_connect() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, false);
    let keyring = expect_keyring();
    assert!(keyring.is_unlocked());

    let dialog = open_dialog_by_label(&mut h, LOCK_AND_DISCONNECT);
    assert!(
        node_values(&h.root)
            .iter()
            .any(|v| v.starts_with("Locks the keyring on this device")),
        "the confirmation says what locking means: {:?}",
        node_values(&h.root)
    );
    confirm(&mut h, &dialog);

    assert!(!keyring.is_unlocked(), "the keyring is locked");
    assert!(keyring.endpoints().is_empty(), "…and holds no token");
    assert!(!expect_connection_controller().connected.get_untracked());
    assert_eq!(
        h.nav
            .route_stack()
            .current_route()
            .map(|location| location.path.clone())
            .as_deref(),
        Some("/"),
        "…and the stack went to Connect"
    );
}

#[test]
fn forgetting_with_the_keyring_unlocked_evicts_its_entry() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, false);
    let credentials = mock_credentials().expect("the mock service is published");
    let keyring = expect_keyring();

    let dialog = open_dialog_by_label(&mut h, "forget");
    confirm(&mut h, &dialog);
    h.pump_until_values("the row to disappear", |values| {
        !values.iter().any(|v| v == CRED_LABEL)
    });

    assert!(
        !keyring.endpoints().iter().any(|e| e == CRED_ENDPOINT),
        "the keyring dropped it: {:?}",
        keyring.endpoints()
    );
    assert_eq!(
        credentials.stored(CRED_ENDPOINT),
        None,
        "…and so did the vault"
    );
}

#[test]
fn deleting_a_credential_with_the_keyring_unlocked_rewrites_the_vault_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, false);
    let credentials = mock_credentials().expect("the mock service is published");
    let keyring = expect_keyring();

    let dialog = open_dialog_by_label(&mut h, "delete credential");
    confirm(&mut h, &dialog);
    h.pump_until_values("the credential chip to disappear", |values| {
        !values.iter().any(|v| v == "credential saved")
    });

    assert!(
        !credentials
            .vault_endpoints()
            .unwrap_or_default()
            .iter()
            .any(|e| e == CRED_ENDPOINT),
        "the vault no longer lists it: {:?}",
        credentials.vault_endpoints()
    );
    assert_eq!(credentials.stored(CRED_ENDPOINT), None);
    assert_eq!(credentials.writes(), 1, "one gated vault write");
    assert!(credentials.prompts().is_empty(), "…and no unlock prompt");
    assert!(keyring.endpoints().is_empty(), "the keyring dropped it too");
    assert!(
        node_values(&h.root).iter().any(|v| v == CRED_LABEL),
        "the saved server itself is kept"
    );
}

#[test]
fn forgetting_while_unlocked_drops_the_handles_token_even_when_the_vault_rewrite_is_refused() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, false);
    let credentials = mock_credentials().expect("the mock service is published");
    credentials.set_gate(CredentialStatus::Canceled);
    let keyring = expect_keyring();

    let dialog = open_dialog_by_label(&mut h, "forget");
    confirm(&mut h, &dialog);
    h.pump_until_values("the row to disappear", |values| {
        !values.iter().any(|v| v == CRED_LABEL)
    });

    assert!(
        keyring.token_for(CRED_ENDPOINT).is_none(),
        "the handle dropped it: {:?}",
        keyring.endpoints()
    );
    assert_eq!(
        credentials.vault_endpoints(),
        Some(vec![CRED_ENDPOINT.to_string()]),
        "the refused rewrite left the vault's copy for the next unlock to prune"
    );
}

#[test]
fn tapping_a_row_without_a_credential_opens_the_add_page_and_never_spends_a_keyring_token() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount_seeded(|| {
        expect_saved_servers()
            .save(credential_row(false))
            .expect("the mock store never fails");
        let mut keyring = Keyring::new();
        keyring.insert(
            CRED_ENDPOINT,
            zeroize::Zeroizing::new(CRED_TOKEN.to_string()),
        );
        expect_keyring().publish(keyring);
        mock_entitlement()
            .expect("tests/support opted this process into the mock")
            .set_entitlement(Entitlement::Pro);
    });
    expect_connection_controller().connect(mock_target(TrustMode::NoVerification));
    h.await_tokens();
    h.pump_until_values("the saved-server rows", |values| {
        values.iter().any(|v| v == CRED_LABEL)
    });

    h.tap_value(CRED_LABEL);
    h.settle();

    assert_eq!(h.depth(), 2, "the add-server page is pushed");
    assert!(
        node_values(&h.root).iter().any(|v| v == ADD_SERVER_HEADING),
        "{:?}",
        node_values(&h.root)
    );
    assert_eq!(connected_endpoint().as_deref(), Some("mock.example:7777"));
}

#[test]
fn a_row_that_lost_its_credential_behind_the_screens_back_is_never_dialed_with_a_keyring_token() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, true);
    expect_saved_servers()
        .save(credential_row(false))
        .expect("the mock store never fails");

    h.tap_value(CRED_LABEL);
    h.pump_once();
    h.settle();

    assert_eq!(
        connected_endpoint().as_deref(),
        Some("mock.example:7777"),
        "the switch refused the keyring's token"
    );
    assert!(
        expect_keyring().token_for(CRED_ENDPOINT).is_some(),
        "…without touching the keyring"
    );
}

#[test]
fn the_add_server_row_closes_the_group_and_pushes_the_add_page() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_vault_credential(true, true);
    assert_eq!(h.depth(), 1);

    h.tap_value(ADD_SERVER_ROW);
    h.settle();

    assert_eq!(h.depth(), 2, "the add-server page is pushed");
    assert!(
        node_values(&h.root).iter().any(|v| v == ADD_SERVER_HEADING),
        "{:?}",
        node_values(&h.root)
    );
}

// ---------------------------------------------------------------------------
// 3c — the `muxr pro` group
// ---------------------------------------------------------------------------

#[test]
fn the_pro_group_prints_free_and_its_plan_row_opens_the_paywall() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let values = node_values(&h.root);
    assert!(values.iter().any(|v| v == "Free"), "{values:?}");
    let labels = node_labels(&h.root);
    assert!(
        !labels.iter().any(|l| l == "Upgrade to Pro"),
        "the actions line is gone: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l == "Restore purchases"),
        "…both of them: {labels:?}"
    );

    h.tap_value(PLAN_LABEL);
    h.settle();

    assert_eq!(
        h.depth(),
        2,
        "the plan row opens the paywall as its own page"
    );
    assert!(
        node_center_by_label(&h.root, NOT_NOW_LABEL).is_some(),
        "the sheet's own dismissal action is reachable: {:?}",
        node_labels(&h.root)
    );

    h.tap_label(NOT_NOW_LABEL);
    h.flush();

    assert_eq!(h.depth(), 1, "Not now dismisses the sheet");
}

#[test]
fn a_pro_entitlement_prints_pro_and_the_plan_row_leads_nowhere() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let mock = mock_entitlement().expect("tests/support opted this process into the mock");
    mock.set_entitlement(Entitlement::Pro);

    let controller = expect_pro_controller();
    spawn_local(async move { controller.refresh().await });
    h.pump_until_values("the plan row to flip to Pro", |values| {
        values.iter().any(|v| v == "Pro")
    });

    h.tap_value(PLAN_LABEL);
    h.settle();

    assert_eq!(h.depth(), 1, "an entitled plan row opens nothing");
    let labels = node_labels(&h.root);
    assert!(
        !labels.iter().any(|l| l == "Upgrade to Pro"),
        "an entitled install offers nothing to buy: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l == "Restore purchases"),
        "…nor anything to restore: {labels:?}"
    );
}

#[test]
fn restoring_from_the_paywall_flips_the_plan_row_and_closes_the_sheet() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let mock = mock_entitlement().expect("tests/support opted this process into the mock");
    mock.set_entitlement(Entitlement::Pro);

    h.tap_value(PLAN_LABEL);
    h.settle();

    h.tap_label(RESTORE_LABEL);
    h.pump_until_values("the plan row to flip to Pro", |values| {
        values.iter().any(|v| v == "Pro")
    });
    // The value flip lands on the restore's own reactive write; the sheet's
    // `close_then` still has to play out its staged close before the pop
    // actually lands — the same clock-driven finish `flush` gives Not now.
    h.flush();

    assert_eq!(mock.restore_calls(), 1);
    assert_eq!(
        h.depth(),
        1,
        "the sheet's own close_then lands behind the restore"
    );
}

#[test]
fn mounting_settings_refreshes_the_pro_entitlement_beside_the_apps_own_launch_refresh() {
    // `MuxrApp::init` already fires one launch refresh of its own
    // (`tests/pro.rs`'s `app_init_publishes_the_pro_seam_and_fires_one_launch_refresh`);
    // this pins the *second* call — Settings' own refresh of the entitlement
    // on every mount — landing beside it, not in place of it.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let mock = mock_entitlement().expect("tests/support opted this process into the mock");

    pump_until(
        &mut h,
        "both the app's launch refresh and Settings' own mount refresh",
        || mock.refresh_calls() >= 2,
    );

    assert_eq!(
        mock.refresh_calls(),
        2,
        "one from MuxrApp::init's launch refresh, one from Settings' own mount"
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
    ("font size", 15, 2, "24px"),
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
        // The scroll viewport clips what it holds, so the fill must lie inside
        // it — the bleed past the sheet's own gutter included.
        let viewport = h.viewport_around(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
        assert!(
            viewport.x0 <= origin.x + EPSILON
                && origin.x + size.width <= viewport.x1 + EPSILON
                && viewport.y0 <= origin.y + EPSILON
                && origin.y + size.height <= viewport.y1 + EPSILON,
            "{row}: the fill {origin:?} {size:?} escapes the scroll viewport {viewport:?}"
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

#[test]
fn picking_font_size_20px_through_the_picker_persists_and_reads_back() {
    // The range now spans 10–24, so 20px sits in the middle. Verify the picker
    // can select and persist values beyond the old max of 14px.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let sheet = open_picker(&mut h, "font size");
    // 20px is at index 10 in [10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, ...]
    // The default (currently selected) is at index 2 (12px); pick from there to index 10.
    pick(&mut h, &sheet, 2, 10);

    h.pump_until_values("20px", |values| values.iter().any(|v| v == "20px"));
    assert_eq!(h.depth(), 1, "the sheet closed behind the choice");
}

/// Mount at `width`×`height`, open the font-size picker, wheel its list until
/// the last size is reachable, pick it, and read it back from the row.
fn picks_the_largest_font_size_at(width: f64, height: f64) {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_with_metrics(Some(support::metrics_at(width, height)));
    h.frame();
    let last = format!(
        "{}px",
        AppearancePrefs::FONT_SIZES_PX[AppearancePrefs::FONT_SIZES_PX.len() - 1]
    );

    h.tap_value("font size");
    h.settle();
    let first = h.center_of(&format!("{}px", AppearancePrefs::FONT_SIZES_PX[0]));
    let band_span = h.center_of(&last).y - first.y;
    assert!(
        band_span > height - 220.0,
        "the list overflows its band, so it must scroll: {band_span}"
    );
    for _ in 0..40 {
        h.root.event(
            &mut h.state,
            &InputEvent::Scroll {
                position: first,
                delta: ScrollDelta::Lines(0.0, 3.0),
            },
        );
        h.settle();
    }
    let reached = h.center_of(&last);
    assert!(
        reached.y - first.y < band_span - 100.0,
        "wheeling moved the list: {reached:?} vs {band_span}"
    );
    h.tap(reached);
    h.flush();
    h.pump_until_values(&last, |values| {
        values.iter().filter(|v| **v == last).count() == 1
    });
    assert_eq!(h.depth(), 1, "the sheet closed behind the choice");
}

#[test]
fn the_font_size_picker_scrolls_to_24px_on_a_phone_in_landscape() {
    picks_the_largest_font_size_at(844.0, 390.0);
}

#[test]
fn the_font_size_picker_scrolls_to_24px_on_the_smallest_tablet() {
    picks_the_largest_font_size_at(1024.0, 720.0);
}

// ---------------------------------------------------------------------------
// 4b — the font picker sheet (its own sheet, not one of the four above)
// ---------------------------------------------------------------------------
//
// Two sections — installed faces, and the downloadable catalog — over one
// scrolling body, read the same way as any other sheet (see the module docs).
//
// No test here touches the network: `support::setup` installs the offline
// font source, and the two catalog cases install their own in-memory fixture.

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
fn selecting_a_font_row_plays_the_staged_exit_rather_than_vanishing_the_sheet() {
    // The anti-vanish pin. `SheetHandle::close` rides `GlyphSheetHandle`'s own
    // staged exit (`crate::ui::sheet`'s "`close` plays the same staged exit"),
    // not a bare `NavigatorController::pop` that would enqueue the page's pop
    // immediately: one frame later — enough for the queued op to land (see
    // `a_second_tap_on_a_picker_row_closes_the_sheet_once_and_keeps_the_route`'s
    // own "a pop only *lands* at the next rebuild") — a bare pop would leave
    // the sheet already gone, with no animation played at all, failing the
    // `depth() == 2` assertion below.
    let _g = serial();
    let _ambient = setup();
    let cache = fresh_font_cache("anti-vanish");
    seed_cached_family(&cache);
    let mut h = Harness::connected();

    let sheet = open_font_picker(&mut h);
    // Two installed rows (bundled + cached); pick the second.
    let row = pick_row(&sheet, 0, 1);
    h.tap(row);

    // One frame — the OLD immediate-pop code would already have applied its
    // queued pop by here; the new staged close is still playing its exit.
    h.frame();
    assert_eq!(
        h.depth(),
        2,
        "the sheet is still mounted one frame after the pick — playing its \
         exit, not gone"
    );

    // Settling carries the exit to completion: the pop lands, exactly once,
    // and the choice it carried still reaches the repository behind it.
    h.flush();
    assert_eq!(h.depth(), 1, "…and the pop lands once the exit finishes");
    h.pump_until_values(CACHED_DISPLAY, |values| {
        values.iter().any(|v| v == CACHED_DISPLAY)
    });
    assert_eq!(
        stored_font_family(),
        CACHED_FAMILY,
        "the picked family still persists behind the staged close"
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
    // the body of the very closure `widgets::font_picker::font_picker_sheet`'s `on_download`
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
    assert_eq!(
        catalog.progress_of_entry(&entry),
        None,
        "a failed download leaves no stale progress fraction behind for a retry to inherit"
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

// ---------------------------------------------------------------------------
// 4b — live download progress
// ---------------------------------------------------------------------------

/// How long [`StagedFontSource::fetch_font_with_progress`] sleeps between
/// each of its four staged reports — long enough that a test's own poll loop
/// (which re-checks every ~1ms, see `Harness::pump_once`) reliably observes
/// an intermediate fraction before the download lands, short enough that
/// four of them stay far under [`PUMP_DEADLINE`].
const STAGED_STEP_DELAY: Duration = Duration::from_millis(15);

/// A [`FontSource`] whose font fetch reports progress in four staged steps
/// (25/50/75/100%), each separated by a real sleep on the background thread
/// `download` actually runs on — so a test driving `Harness::pump_once` on
/// the UI thread can observe the fraction mid-flight rather than only ever
/// seeing it jump straight to done.
struct StagedFontSource {
    manifest: Vec<u8>,
}

#[clean_signals::async_trait]
impl FontSource for StagedFontSource {
    async fn fetch_manifest(&self) -> Result<Vec<u8>, AppFailure> {
        Ok(self.manifest.clone())
    }

    async fn fetch_font(&self, _url: &str) -> Result<Vec<u8>, AppFailure> {
        Ok(FIXTURE_FONT.to_vec())
    }

    async fn fetch_font_with_progress(
        &self,
        _url: &str,
        progress: muxr::fonts::source::ProgressSink<'_>,
    ) -> Result<Vec<u8>, AppFailure> {
        let bytes = FIXTURE_FONT.to_vec();
        let total = bytes.len() as u64;
        for step in 1..=4u64 {
            std::thread::sleep(STAGED_STEP_DELAY);
            progress(total * step / 4);
        }
        Ok(bytes)
    }
}

/// A one-entry manifest carrying `family`, whose file's size hint is
/// [`FIXTURE_FONT`]'s real length when `sized`, or `0` (the indeterminate
/// case) otherwise.
fn staged_manifest(family: &str, sized: bool) -> String {
    let digest = sha256_hex(FIXTURE_FONT);
    let bytes = if sized { FIXTURE_FONT.len() } else { 0 };
    format!(
        r#"{{ "version": 1, "fonts": [
          {{ "id": "staged", "displayName": "{family} Display",
             "family": "{family}", "license": "OFL-1.1", "isNerdFont": true,
             "files": [ {{ "weight": 400, "style": "normal", "path": "Staged-Regular.ttf",
                          "bytes": {bytes}, "sha256": "{digest}",
                          "url": "https://example.invalid/Staged-Regular.ttf" }} ] }}
        ] }}"#
    )
}

#[test]
fn downloading_a_font_drives_its_progress_fraction_from_zero_toward_one_with_throttled_writes() {
    let _g = serial();
    let _ambient = setup();
    let _cache = fresh_font_cache("progress-sized");
    const FAMILY: &str = "Staged Sized Nerd Font Mono";
    muxr::fonts::use_font_source_for_tests(Arc::new(StagedFontSource {
        manifest: staged_manifest(FAMILY, true).into_bytes(),
    }));
    let mut h = Harness::connected();

    open_font_picker(&mut h);
    await_catalog(&mut h, CatalogStatus::Ready);
    close_sheet(&mut h);

    let catalog = muxr::fonts::catalog::font_catalog();
    let entry = catalog
        .entries()
        .iter()
        .find(|entry| entry.family == FAMILY)
        .cloned()
        .expect("the staged catalog carries it");
    assert!(entry.total_bytes() > 0, "the fixture carries a size hint");

    let installing = entry.clone();
    frust::spawn_local(async move {
        catalog
            .install(installing)
            .await
            .expect("the staged download verifies");
    });

    // Sample the tracked fraction on every pump, deduplicating consecutive
    // repeats, until the row lands — the throttled-write contract means
    // most pumps see no *new* value, which is the point.
    let mut observed: Vec<f64> = Vec::new();
    let deadline = Instant::now() + PUMP_DEADLINE;
    while catalog.state_of(&entry) != FontState::Installed {
        assert!(
            Instant::now() < deadline,
            "the staged download never landed"
        );
        if let Some(fraction) = catalog.progress_of_entry(&entry)
            && observed.last() != Some(&fraction)
        {
            observed.push(fraction);
        }
        h.pump_once();
    }

    assert!(
        observed.iter().any(|f| *f > 0.0 && *f < 1.0),
        "at least one intermediate fraction must have been observed: {observed:?}"
    );
    assert!(
        observed.windows(2).all(|pair| pair[1] >= pair[0]),
        "progress must never regress: {observed:?}"
    );
    assert!(
        observed.iter().all(|f| (0.0..=1.0).contains(f)),
        "progress must stay clamped to 0..=1: {observed:?}"
    );
    assert_eq!(
        catalog.progress_of_entry(&entry),
        None,
        "a landed download leaves no stale progress fraction behind"
    );
}

#[test]
fn an_entry_with_no_size_hint_stays_indeterminate_while_downloading() {
    let _g = serial();
    let _ambient = setup();
    let _cache = fresh_font_cache("progress-unsized");
    const FAMILY: &str = "Staged Unsized Nerd Font Mono";
    muxr::fonts::use_font_source_for_tests(Arc::new(StagedFontSource {
        manifest: staged_manifest(FAMILY, false).into_bytes(),
    }));
    let mut h = Harness::connected();

    open_font_picker(&mut h);
    await_catalog(&mut h, CatalogStatus::Ready);
    close_sheet(&mut h);

    let catalog = muxr::fonts::catalog::font_catalog();
    let entry = catalog
        .entries()
        .iter()
        .find(|entry| entry.family == FAMILY)
        .cloned()
        .expect("the staged catalog carries it");
    assert_eq!(entry.total_bytes(), 0, "the fixture carries no size hint");

    let installing = entry.clone();
    frust::spawn_local(async move {
        catalog
            .install(installing)
            .await
            .expect("the staged download verifies");
    });

    let deadline = Instant::now() + PUMP_DEADLINE;
    while catalog.state_of(&entry) != FontState::Installed {
        assert!(
            Instant::now() < deadline,
            "the staged download never landed"
        );
        assert_eq!(
            catalog.progress_of_entry(&entry),
            None,
            "an unknown-size entry never reports a determinate fraction"
        );
        h.pump_once();
    }
}

#[test]
fn the_downloading_row_paints_a_progress_bar_and_flips_to_selectable_once_installed() {
    let _g = serial();
    let _ambient = setup();
    let _cache = fresh_font_cache("progress-row");
    const FAMILY: &str = "Staged Row Nerd Font Mono";
    muxr::fonts::use_font_source_for_tests(Arc::new(StagedFontSource {
        manifest: staged_manifest(FAMILY, true).into_bytes(),
    }));
    let mut h = Harness::connected();

    open_font_picker(&mut h);
    await_catalog(&mut h, CatalogStatus::Ready);

    let catalog = muxr::fonts::catalog::font_catalog();
    let entry = catalog
        .entries()
        .iter()
        .find(|entry| entry.family == FAMILY)
        .cloned()
        .expect("the staged catalog carries it");

    let installing = entry.clone();
    frust::spawn_local(async move {
        catalog
            .install(installing)
            .await
            .expect("the staged download verifies");
    });

    // The sheet's body closure re-runs on every navigator rebuild
    // (`open_font_picker`'s own doc), so a rendered `ProgressIndicator` node
    // is proof the row actually swapped its trailing affordance for
    // `widgets::font_picker::font_row_progress`'s bar rather than the plain text every
    // other state prints.
    let deadline = Instant::now() + PUMP_DEADLINE;
    loop {
        h.pump_once();
        if h.font_progress_bar_visible() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the downloading row never painted a progress bar"
        );
    }

    let deadline = Instant::now() + PUMP_DEADLINE;
    while catalog.state_of(&entry) != FontState::Installed {
        assert!(
            Instant::now() < deadline,
            "the staged download never landed"
        );
        h.pump_once();
    }

    let deadline = Instant::now() + PUMP_DEADLINE;
    while h.font_progress_bar_visible() {
        assert!(
            Instant::now() < deadline,
            "an installed row carries no progress bar"
        );
        h.pump_once();
    }
    close_sheet(&mut h);

    // …and the reopened sheet lists it under installed, selectable like any
    // other row — the same post-install contract the plain download test
    // above already pins.
    let reopened = open_font_picker(&mut h);
    assert!(reopened.runs > 0);
    assert!(
        !h.font_progress_bar_visible(),
        "a re-opened, already-installed row still carries no progress bar"
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

    // No outer row-wide `GestureDetector` wraps the switch (see
    // `widgets::misc::bell_notifications_row`'s own doc), so the switch's own
    // semantics bounds are what a tap must land inside to flip it.
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
    open_dialog_by_label(&mut h, LOCK_AND_DISCONNECT);
    assert_eq!(h.depth(), 2);
    h.tap(SCRIM_POINT);
    h.flush();
    assert!(
        expect_connection_controller().connected.get_untracked(),
        "cancelling leaves the connection up"
    );

    // Confirming clears the connection and resets the stack to `/`.
    let dialog = open_dialog_by_label(&mut h, LOCK_AND_DISCONNECT);
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
// 7 — responsive width-clamp pass
// ---------------------------------------------------------------------------

#[test]
fn wide_metrics_clamp_and_center_the_groups_column() {
    let _g = serial();
    let _ambient = setup();
    // A phone in landscape: wide enough to clamp, too short to be a Tablet.
    let mut h = mount_with_metrics(Some(support::metrics_at(SURFACE.width, 500.0)));
    h.frame();

    // Disconnect is the screen's last, full-width row — the cheapest honest
    // read on where the clamped column itself sits: centered,
    // `SETTINGS_CLAMP_WIDTH` wide, minus the row's own `ROW_PAD_X` margin on
    // each side.
    let disconnect = bounds_by_label(&h.root, LOCK_AND_DISCONNECT);
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
    let mut h = mount_with_metrics(Some(support::metrics_at(400.0, SURFACE.height)));
    h.frame();

    let disconnect = bounds_by_label(&h.root, LOCK_AND_DISCONNECT);
    let expected_width = SURFACE.width - 2.0 * ROW_PAD_X;
    assert!(
        (disconnect.width() - expected_width).abs() < 0.5,
        "narrow metrics take the same unclamped branch as no metrics: {:.1}",
        disconnect.width()
    );
    assert!((disconnect.x0 - ROW_PAD_X).abs() < 0.5, "{:?}", disconnect);
}

// ---------------------------------------------------------------------------
// 7b — the Tablet card grid
// ---------------------------------------------------------------------------

/// The y-centre and x-centre of the first node printing `value`.
fn value_center(h: &Harness, value: &str) -> Point {
    h.center_of(value)
}

#[test]
fn a_full_band_tablet_packs_the_cards_into_two_columns() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_with_metrics(Some(support::metrics_at(1024.0, 720.0)));
    h.frame();

    let values = node_values(&h.root);
    for group in GROUPS {
        assert!(values.iter().any(|v| v == group), "{group:?}: {values:?}");
    }
    let connection = value_center(&h, "CONNECTION");
    let tokens = value_center(&h, "AUTH TOKENS");
    assert!(
        (connection.y - tokens.y).abs() < 0.5,
        "the first pair shares a top edge: {connection:?} vs {tokens:?}"
    );
    assert!(tokens.x > connection.x + 100.0, "side by side: {tokens:?}");
    let saved = value_center(&h, "SAVED SERVERS");
    assert!(saved.y > connection.y, "the left column stacks downward");
    // Packed columns, not rows: the short Auth tokens card does not hold the
    // row open, so Muxr Pro climbs above Saved servers (Ed's iPad gate).
    let pro = value_center(&h, "MUXR PRO");
    assert!(
        pro.y < saved.y - 100.0,
        "the right column packs upward: pro {pro:?} vs saved {saved:?}"
    );
    assert!(
        node_labels(&h.root)
            .iter()
            .any(|l| l == LOCK_AND_DISCONNECT),
        "the Disconnect card's button renders"
    );
}

#[test]
fn a_collapsed_band_tablet_stacks_the_cards() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_with_metrics(Some(support::metrics_at(720.0, 720.0)));
    h.frame();

    let connection = value_center(&h, "CONNECTION");
    let tokens = value_center(&h, "AUTH TOKENS");
    assert!(
        (connection.x - tokens.x).abs() < 20.0,
        "one column (title centres differ only by text width): {connection:?} vs {tokens:?}"
    );
    assert!(tokens.y > connection.y, "stacked: {tokens:?}");
}

// ---------------------------------------------------------------------------
// 7c — the `muxr pro` restore row
// ---------------------------------------------------------------------------

#[test]
fn the_restore_row_restores_once_and_toasts_when_nothing_is_found() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let mock = mock_entitlement().expect("tests/support opted this process into the mock");
    app_toasts().clear();

    let values = node_values(&h.root);
    assert!(values.iter().any(|v| v == RESTORE_LABEL), "{values:?}");
    h.tap_label("restore");
    pump_until(&mut h, "the not-found toast", || {
        !app_toasts().pending_untracked().is_empty()
    });

    assert_eq!(mock.restore_calls(), 1);
    assert_eq!(
        app_toasts().pending_untracked(),
        vec![NOTHING_TO_RESTORE_TOAST.to_string()]
    );
    assert_eq!(h.depth(), 1, "the row opens no sheet");
}

#[test]
fn the_restore_row_is_absent_on_pro() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let mock = mock_entitlement().expect("tests/support opted this process into the mock");
    mock.set_entitlement(Entitlement::Pro);
    let controller = expect_pro_controller();
    spawn_local(async move { controller.refresh().await });
    h.pump_until_values("the plan row to flip to Pro", |values| {
        values.iter().any(|v| v == "Pro")
    });

    let values = node_values(&h.root);
    assert!(!values.iter().any(|v| v == RESTORE_LABEL), "{values:?}");
    assert!(!node_labels(&h.root).iter().any(|l| l == "restore"));
}

#[test]
fn a_failed_restore_from_the_row_toasts_the_failure_message_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let mock = mock_entitlement().expect("tests/support opted this process into the mock");
    let failure = AppFailure::Network("store unreachable".to_string());
    mock.fail_next_restore(failure.clone());
    app_toasts().clear();

    h.tap_label("restore");
    pump_until(&mut h, "the failure toast", || {
        !app_toasts().pending_untracked().is_empty()
    });

    assert_eq!(
        app_toasts().pending_untracked(),
        vec![failure.user_message()]
    );
}

/// A store whose restore stays open until its [`Gate`] is opened — the mock
/// resolves without ever yielding, so an in-flight restore needs this.
struct GatedStore {
    inner: MockEntitlementRepository,
    gate: Arc<Gate>,
}

/// One-shot latch: `UntilOpen` parks its waker here, `open` wakes it.
#[derive(Default)]
struct Gate {
    open: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Gate {
    fn open(&self) {
        self.open.store(true, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

struct UntilOpen(Arc<Gate>);

impl Future for UntilOpen {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0.open.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        *self.0.waker.lock().unwrap() = Some(cx.waker().clone());
        Poll::Pending
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl EntitlementRepository for GatedStore {
    fn cached(&self) -> Entitlement {
        self.inner.cached()
    }

    async fn refresh(&self) -> Result<Entitlement, AppFailure> {
        self.inner.refresh().await
    }

    async fn offer(&self) -> Result<Option<ProOffer>, AppFailure> {
        self.inner.offer().await
    }

    async fn purchase(&self) -> Result<PurchaseOutcome, AppFailure> {
        self.inner.purchase().await
    }

    async fn restore(&self) -> Result<RestoreResult, AppFailure> {
        UntilOpen(Arc::clone(&self.gate)).await;
        self.inner.restore().await
    }
}

#[test]
fn a_second_restore_tap_while_the_first_is_in_flight_leaves_restoring_standing() {
    let _g = serial();
    let _ambient = setup();
    let gate = Arc::new(Gate::default());
    let gated = Arc::clone(&gate);
    let mut h = Harness::mount_seeded(move || {
        let store: SharedEntitlementRepository = Arc::new(GatedStore {
            inner: MockEntitlementRepository::free(),
            gate: gated,
        });
        provide_context(Arc::new(ProController::new(store)));
    });
    expect_connection_controller().connect(mock_target(TrustMode::NoVerification));
    h.await_tokens();
    let restoring = |values: &[String]| values.iter().any(|v| v == "restoring\u{2026}");
    app_toasts().clear();

    h.tap_label("restore");
    h.pump_until_values("the first restore to be in flight", |values| {
        restoring(values)
    });
    h.tap_label("restore");
    pump_until(&mut h, "the second tap's busy toast", || {
        !app_toasts().pending_untracked().is_empty()
    });
    assert_eq!(
        app_toasts().pending_untracked(),
        vec!["A store operation is already running".to_string()]
    );
    h.pump_once();
    assert!(
        restoring(&node_values(&h.root)),
        "the first restore is still running: {:?}",
        node_values(&h.root)
    );

    gate.open();
    h.pump_until_values("the first restore to land", |values| !restoring(values));
    assert_eq!(app_toasts().pending_untracked().len(), 2);
}

#[test]
fn a_purchase_side_busy_never_switches_the_restore_row_to_restoring() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let restoring = |values: &[String]| values.iter().any(|v| v == "restoring\u{2026}");

    // A purchase elsewhere holds the pro controller's busy flag.
    let pro = expect_pro_controller();
    pro.busy.set(true);
    h.pump_once();
    h.pump_once();
    assert!(
        !restoring(&node_values(&h.root)),
        "a purchase is not a restore"
    );

    // Tapping restore meanwhile is refused with a toast, not a restoring row.
    app_toasts().clear();
    h.tap_label("restore");
    pump_until(&mut h, "the busy toast", || {
        !app_toasts().pending_untracked().is_empty()
    });
    assert_eq!(app_toasts().pending_untracked().len(), 1);
    assert!(!restoring(&node_values(&h.root)), "a refused restore");
    pro.busy.set(false);

    // A landed restore leaves the row as it found it.
    app_toasts().clear();
    h.tap_label("restore");
    pump_until(&mut h, "the restore to land", || {
        !app_toasts().pending_untracked().is_empty()
    });
    assert!(!restoring(&node_values(&h.root)), "released once it lands");
}

// ---------------------------------------------------------------------------
// 8 — saved-server card clip regression at the phone-narrow width
// ---------------------------------------------------------------------------

/// A frame width matching the device's own narrow branch — a Xiaomi's 1080
/// physical px is ~395 logical px, comfortably under
/// `crate::responsive::WIDE_BREAKPOINT_PX` (600). Tall enough that
/// [`with_saved_credential`]'s freshly-stamped row stays on-screen.
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

// ---------------------------------------------------------------------------
// The app-wide unlock single-flight: a lock wins, and no second prompt
// ---------------------------------------------------------------------------

/// A parked `unlock_all`: the credential service waits here until released.
#[derive(Default)]
struct HeldUnlockPrompt {
    entered: AtomicBool,
    released: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl HeldUnlockPrompt {
    fn release(&self) {
        self.released.store(true, Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            waker.wake();
        }
    }
}

struct HoldUnlock(Arc<HeldUnlockPrompt>);

impl Future for HoldUnlock {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let held = &self.0;
        held.entered.store(true, Ordering::SeqCst);
        if held.released.load(Ordering::SeqCst) {
            return Poll::Ready(());
        }
        *held.waker.lock().unwrap_or_else(|p| p.into_inner()) = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// [`MockCredentialService`] whose `unlock_all` parks on a [`HeldUnlockPrompt`].
struct HeldUnlockVault {
    inner: Arc<MockCredentialService>,
    prompt: Arc<HeldUnlockPrompt>,
}

#[clean_signals::async_trait]
impl CredentialService for HeldUnlockVault {
    async fn store(&self, request: StoreRequest) -> CredentialStatus {
        self.inner.store(request).await
    }

    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential {
        self.inner.unlock(request).await
    }

    async fn unlock_all(&self, reason: &str, endpoints: &[String]) -> UnlockedKeyring {
        HoldUnlock(Arc::clone(&self.prompt)).await;
        self.inner.unlock_all(reason, endpoints).await
    }

    async fn store_in_vault(
        &self,
        request: StoreRequest,
        keyring: &mut Keyring,
    ) -> CredentialStatus {
        self.inner.store_in_vault(request, keyring).await
    }

    async fn delete_from_vault(
        &self,
        server_key: String,
        keyring: &mut Keyring,
    ) -> Result<(), AppFailure> {
        self.inner.delete_from_vault(server_key, keyring).await
    }

    async fn delete(&self, server_key: String) -> Result<(), AppFailure> {
        self.inner.delete(server_key).await
    }

    async fn reset_vault(&self) -> Result<(), AppFailure> {
        self.inner.reset_vault().await
    }
}

/// A headless Settings controller over a locked keyring and one saved row
/// ([`CRED_ENDPOINT`]) whose vault entry sits behind a held `unlock_all`.
fn held_switch_rig() -> (
    Arc<SettingsController>,
    KeyringHandle,
    Arc<MockCredentialService>,
    Arc<HeldUnlockPrompt>,
) {
    let connection = muxr::connection::provide_connection_controller();
    let credentials = Arc::new(MockCredentialService::new());
    credentials.seed_vault(&[(CRED_ENDPOINT, CRED_TOKEN)]);
    let prompt = Arc::new(HeldUnlockPrompt::default());
    let held = Arc::new(HeldUnlockVault {
        inner: Arc::clone(&credentials),
        prompt: Arc::clone(&prompt),
    });
    let keyring = KeyringHandle::new();
    let controller = Arc::new(SettingsController::new(
        Arc::new(MockAuthTokensRepository::seeded()),
        Arc::new(MockPreferencesRepository::default()),
        Arc::new(MockSavedServersRepository::with_servers(vec![
            credential_row(true),
        ])),
        held as _,
        keyring.clone(),
        connection,
    ));
    (controller, keyring, credentials, prompt)
}

fn pump_runtime(rounds: usize) {
    let runtime = ReactiveRuntime::get().expect("setup() installed the runtime");
    for _ in 0..rounds {
        runtime.pump_local();
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_lock_and_disconnect_during_a_pending_switch_unlock_dials_nothing() {
    let _g = serial();
    let _ambient = setup();
    let (controller, keyring, _credentials, prompt) = held_switch_rig();
    let connection = expect_connection_controller();
    let switched = Rc::new(std::cell::Cell::new(false));

    let landed = Rc::clone(&switched);
    controller.switch_server(
        CRED_ENDPOINT.to_string(),
        Box::new(move || landed.set(true)),
    );
    let deadline = Instant::now() + PUMP_DEADLINE;
    while !prompt.entered.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the prompt"
        );
        pump_runtime(1);
    }
    assert!(controller.switching.get_untracked());

    connection.lock_and_disconnect(&keyring);
    prompt.release();
    let deadline = Instant::now() + PUMP_DEADLINE;
    while controller.switching.get_untracked() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the switch"
        );
        pump_runtime(1);
    }
    pump_runtime(20);

    assert!(
        !keyring.is_unlocked() && keyring.endpoints().is_empty(),
        "the late unlock must not re-open a locked keyring"
    );
    assert!(!connection.connected.get_untracked(), "nothing was dialed");
    assert!(!switched.get(), "…and nothing switched");
    assert!(app_toasts().pending_untracked().is_empty(), "no toast");
}

#[test]
fn a_settings_tap_while_a_dock_unlock_is_in_flight_raises_no_prompt() {
    let _g = serial();
    let _ambient = setup();
    let (controller, keyring, credentials, prompt) = held_switch_rig();
    let dock_ticket = keyring.begin_unlock().expect("nothing in flight yet");

    controller.switch_server(CRED_ENDPOINT.to_string(), Box::new(|| {}));
    pump_runtime(20);

    assert!(
        !prompt.entered.load(Ordering::SeqCst) && credentials.prompts().is_empty(),
        "no second biometric prompt"
    );
    assert!(
        !controller.switching.get_untracked(),
        "…and Settings is idle"
    );
    assert!(!keyring.is_unlocked());
    assert!(app_toasts().pending_untracked().is_empty(), "silent");
    drop(dock_ticket);
}
