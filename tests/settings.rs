//! Headless coverage for the `/settings` screen: a `RenderRoot` over the real page builder, driven through `tests/support`.
//! The screen is the bottom layer of a `Stack` under an empty navigator; dialogs and picker sheets are located by painted geometry.
//! Every frame lays out at [`SURFACE`] (the screen runs past 1000px), hence the local `pump_once`.

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

const SURFACE: Size = Size::new(800.0, 1700.0);

const SCRIM_POINT: Point = Point::new(4.0, 4.0);
const PANEL_PADDING: f64 = 20.0;
const PUMP_DEADLINE: Duration = Duration::from_secs(5);
const SETTINGS_CLAMP_WIDTH: f64 = 640.0;
const MOCK_LATENCY_VALUE: &str = "36ms";
const ROW_PAD_X: f64 = 16.0;
const LOCK_AND_DISCONNECT: &str = "Lock & disconnect";
const CONNECTED_CHIP: &str = "connected";
const ADD_SERVER_ROW: &str = "add server";

const SEEDED_TOKENS: [&str; 2] = ["zelli-mobile-100-71-31-57-mr3g6w5g", "token_2"];
const GROUPS: [&str; 6] = [
    "CONNECTION",
    "AUTH TOKENS",
    "SAVED SERVERS",
    "MUXR PRO",
    "TERMINAL APPEARANCE",
    "SESSION POLL",
];

const CRED_ENDPOINT: &str = "cred.example:50051";
const CRED_LABEL: &str = "Cred rig";
const CRED_TOKEN: &str = "stored-s3cr3t";
const CRED_TOKEN_NAME: &str = "token_2";

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

fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Harness {
    support::mount_with_metrics(metrics, Harness::mount)
}

fn bounds_by_label(root: &RenderRoot<AppState, AnyView<AppState>>, label: &str) -> Rect {
    root.semantics()
        .nodes
        .iter()
        .find(|(_, node): &&(_, Node)| node.label() == Some(label))
        .and_then(|(_, node)| node.bounds())
        .unwrap_or_else(|| panic!("nothing labelled {label:?}"))
}

fn all_bounds_by_label(root: &RenderRoot<AppState, AnyView<AppState>>, label: &str) -> Vec<Rect> {
    root.semantics()
        .nodes
        .iter()
        .filter(|(_, node): &&(_, Node)| node.label() == Some(label))
        .filter_map(|(_, node)| node.bounds())
        .collect()
}

type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

struct Harness {
    nav: NavigatorController<AppState>,
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
}

impl Harness {
    fn mount() -> Self {
        Self::mount_seeded(|| {})
    }

    fn mount_seeded(seed: impl FnOnce()) -> Self {
        let state = MuxrApp.init();
        seed();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        let _router = install_app_router(&nav, &nav, expect_connection_controller());
        let logic_nav = nav.clone();
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            let screen_nav = logic_nav.clone();
            let overlay_nav = logic_nav.clone();
            any(Stack(vec![
                any(settings_page(screen_nav)),
                any(navigator(&overlay_nav, || {
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

    fn connected_with(trust_mode: TrustMode) -> Self {
        let mut harness = Harness::mount();
        expect_connection_controller().connect(mock_target(trust_mode));
        harness.await_tokens();
        harness
    }

    fn connected() -> Self {
        Harness::connected_with(TrustMode::NoVerification)
    }

    fn frame(&mut self) -> RecScene {
        frame_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
        )
    }

    fn settle(&mut self) -> RecScene {
        settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
        )
    }

    /// Settle, then deliver a pop's result at the first event pass after the pop's rebuild.
    fn flush(&mut self) {
        self.settle();
        self.settle();
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, SCRIM_POINT));
    }

    fn pump_once(&mut self) {
        let runtime =
            ReactiveRuntime::get().expect("pump_once: ReactiveRuntime::init must have run");
        runtime.pump_local();
        std::thread::sleep(Duration::from_millis(1));
        self.frame();
    }

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

    fn tap_value(&mut self, value: &str) {
        let point = self.center_of(value);
        self.tap(point);
    }

    fn tap_label(&mut self, label: &str) {
        let point = node_center_by_label(&self.root, label).unwrap_or_else(|| {
            panic!(
                "nothing labelled {label:?}; labels on screen: {:?}",
                node_labels(&self.root)
            )
        });
        self.tap(point);
    }

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

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.root.event(&mut self.state, &char_key(&c.to_string()));
        }
    }

    fn press(&mut self, key: NamedKey) {
        self.root.event(&mut self.state, &named_key(key));
    }

    fn bell_state(&self) -> Option<Toggled> {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("bell notifications"))
            .and_then(|(_, node)| node.toggled())
    }

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

    fn font_progress_bar_visible(&self) -> bool {
        self.root
            .semantics()
            .nodes
            .iter()
            .any(|(_, node)| node.role() == Role::ProgressIndicator)
    }

    fn depth(&self) -> usize {
        self.nav.depth()
    }

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

    fn panel(&self) -> (Point, Size) {
        let mut candidates = self.rounded.clone();
        candidates.extend(self.paths.iter().copied());
        largest(&candidates).expect("an overlay always paints its own panel")
    }

    fn vectors(&self) -> Vec<(Point, Size)> {
        let panel = self.panel();
        self.paths
            .iter()
            .copied()
            .filter(|painted| *painted != panel)
            .collect()
    }

    fn content_child(&self) -> (Point, Size) {
        let content_w = self.panel().1.width - 2.0 * PANEL_PADDING;
        by_width(&self.rounded, content_w)
            .unwrap_or_else(|| panic!("no full-width child in {:?}", self.rounded))
    }

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

fn open_picker(h: &mut Harness, row: &str) -> Chrome {
    let before = h.frame();
    h.tap_value(row);
    let after = h.settle();
    let sheet = Chrome::diff(&before, &after);
    assert_eq!(h.depth(), 2, "the picker is pushed as its own page");
    sheet
}

fn pick(h: &mut Harness, sheet: &Chrome, selected: usize, target: usize) {
    h.tap(pick_row(sheet, selected, target));
    h.flush();
}

fn pick_row(sheet: &Chrome, selected: usize, target: usize) -> Point {
    let (origin, size) = sheet.selected_pick_row();
    let offset = target as f64 - selected as f64;
    Point::new(
        origin.x + size.width / 2.0,
        origin.y + offset * size.height + size.height / 2.0,
    )
}

fn open_dialog_by_label(h: &mut Harness, label: &str) -> Chrome {
    let before = h.frame();
    h.tap_label(label);
    let after = h.settle();
    Chrome::diff(&before, &after)
}

fn open_dialog_by_value(h: &mut Harness, value: &str) -> Chrome {
    let before = h.frame();
    h.tap_value(value);
    let after = h.settle();
    Chrome::diff(&before, &after)
}

fn confirm(h: &mut Harness, dialog: &Chrome) {
    let action = smallest(&dialog.strokes).expect("the danger action strokes its border");
    h.tap(center(action));
    h.flush();
}

#[test]
fn all_six_groups_and_the_disconnect_action_render() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    assert_eq!(PRO_GROUP_LABEL.to_uppercase(), "MUXR PRO");

    let values = node_values(&h.root);
    for group in GROUPS {
        assert!(
            values.iter().any(|v| v == group),
            "the {group:?} group label renders: {values:?}"
        );
    }
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
    assert!(values.iter().any(|v| v == "create new token"), "{values:?}");
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
    assert!(
        values.iter().any(|v| v == "read-write") && values.iter().any(|v| v == "read-only"),
        "both scope chips render: {values:?}"
    );
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
    let clock = AnimationClock::pinned();

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

    let dialog = open_dialog_by_label(&mut h, "revoke");
    confirm(&mut h, &dialog);
    clock.advance(TIMELINE_MS + 1.0);
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

/// The revoke timeline runs off a wall clock; pinning it through a test seam makes "three scramble
/// frames in" exact. Restores the wall clock on drop.
struct AnimationClock;

impl AnimationClock {
    fn pinned() -> Self {
        set_animation_clock_for_tests(Some(0.0));
        AnimationClock
    }

    fn advance(&self, ms: f64) {
        advance_animation_clock_for_tests(ms);
    }
}

impl Drop for AnimationClock {
    fn drop(&mut self) {
        set_animation_clock_for_tests(None);
    }
}

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

fn mount_with_reduced_motion() -> Harness {
    let mut theme = frust_glyph::baseline();
    theme.motion.reduce_motion = true;
    provide_context(theme);
    let mut harness = Harness::mount();
    expect_connection_controller().connect(mock_target(TrustMode::NoVerification));
    harness.await_tokens();
    harness
}

fn pump_until(h: &mut Harness, what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + PUMP_DEADLINE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.pump_once();
    }
}

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

    // Hold the timeline early while the RPC and its refresh land.
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
    for name in SEEDED_TOKENS {
        assert!(values.iter().any(|v| v == name), "{name:?}: {values:?}");
    }
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
    assert_eq!(
        values.iter().filter(|v| *v == "read-only").count(),
        2,
        "the toggle's scope survives the reload: {values:?}"
    );
}

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

    h.tap_label("read-only");
    h.frame();
    let (_, state) = h
        .read_only_toggle()
        .expect("the dialog's toggle still contributes a Switch node");
    assert_eq!(state, Toggled::True, "the tap landed on the switch itself");
}

#[test]
fn a_rejected_mint_never_claims_to_have_created_anything() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    app_toasts().clear();

    mint_token(&mut h, SEEDED_TOKENS[1], false);
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
    assert_eq!(h.depth(), 1, "a rejected mint opens no secret reveal");
    let values = node_values(&h.root);
    assert_eq!(
        values.iter().filter(|v| *v == "created 2026-07-31").count(),
        0,
        "nothing was minted: {values:?}"
    );
}

#[test]
fn a_read_only_connected_session_disables_minting_and_revoking() {
    // The mock connect flow always logs in read-write, so this flips the read-only signal directly.
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

    let values = node_values(&h.root);
    for name in SEEDED_TOKENS {
        assert!(values.iter().any(|v| v == name), "{name:?}: {values:?}");
    }
}

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

    frame_sized(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, se);
    let baseline_save = bounds_by_label(&h.root, "Save");
    assert!(
        baseline_save.y1 > free_bottom,
        "sanity: full-window centering overlaps where the keyboard would sit \
         at this size: {baseline_save:?} vs free_bottom={free_bottom}"
    );

    // Publish the keyboard inset, as the shell's IME listener does.
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
        let viewport = h.viewport_around(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
        assert!(
            viewport.x0 <= origin.x + EPSILON
                && origin.x + size.width <= viewport.x1 + EPSILON
                && viewport.y0 <= origin.y + EPSILON
                && origin.y + size.height <= viewport.y1 + EPSILON,
            "{row}: the fill {origin:?} {size:?} escapes the scroll viewport {viewport:?}"
        );
        let first_row_top = origin.y - selected as f64 * size.height;
        assert!(
            first_row_top > panel_origin.y,
            "{row}: the first row sits inside the panel: {first_row_top} vs {panel_origin:?}"
        );

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
    // The picker's close is immediate, but a pop only lands at the next rebuild.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let floor = h.push_route_floor();

    let before = h.frame();
    h.tap_value("cursor style");
    let after = h.settle();
    let sheet = Chrome::diff(&before, &after);
    assert_eq!(h.depth(), floor + 1, "the picker is up over the route");

    // Twice, with no frame in between.
    let row = pick_row(&sheet, 0, 2);
    h.tap(row);
    h.tap(row);
    h.flush();

    assert_eq!(h.depth(), floor, "one pop, and the route is still under it");
    h.pump_until_values("bar", |values| values.iter().any(|v| v == "bar"));
}

#[test]
fn a_picked_value_is_still_checked_when_the_sheet_is_reopened() {
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
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let sheet = open_picker(&mut h, "font size");
    pick(&mut h, &sheet, 2, 10);

    h.pump_until_values("20px", |values| values.iter().any(|v| v == "20px"));
    assert_eq!(h.depth(), 1, "the sheet closed behind the choice");
}

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

/// The bytes every fixture download serves: a real, parseable TTF.
const FIXTURE_FONT: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Light.ttf");

const FIXTURE_A_FAMILY: &str = "FixtureA Nerd Font Mono";
const FIXTURE_A_DISPLAY: &str = "Fixture A Nerd Font Mono";
const FIXTURE_B_FAMILY: &str = "Fixture B Nerd Font Mono";

const CACHED_FAMILY: &str = "Cached Nerd Font Mono";
const CACHED_DISPLAY: &str = "Cached Display Nerd Font";

struct FixtureFontSource {
    manifest: Vec<u8>,
    fetched: Arc<Mutex<Vec<String>>>,
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
            // Still a font by its magic bytes; only the hash can catch this.
            bytes[400] ^= 0xff;
        }
        Ok(bytes)
    }
}

fn use_fixture_catalog() -> Arc<Mutex<Vec<String>>> {
    use_fixture_catalog_serving(false)
}

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

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

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

fn open_font_picker(h: &mut Harness) -> Chrome {
    let before = h.frame();
    h.tap_value("font family");
    let after = h.settle();
    let sheet = Chrome::diff(&before, &after);
    assert_eq!(h.depth(), 2, "the font sheet is pushed as its own page");
    sheet
}

fn close_sheet(h: &mut Harness) {
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 1, "the sheet dismissed");
}

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
    let _g = serial();
    let _ambient = setup();
    let cache = fresh_font_cache("anti-vanish");
    seed_cached_family(&cache);
    let mut h = Harness::connected();

    let sheet = open_font_picker(&mut h);
    let row = pick_row(&sheet, 0, 1);
    h.tap(row);

    h.frame();
    assert_eq!(
        h.depth(),
        2,
        "the sheet is still mounted one frame after the pick — playing its \
         exit, not gone"
    );

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

    let with_catalog = open_font_picker(&mut h).runs;
    assert_eq!(
        with_catalog,
        font_sheet_runs(1, 2, false),
        "the bundled face, then both catalog entries"
    );
    close_sheet(&mut h);

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

    let after_install = open_font_picker(&mut h).runs;
    assert_eq!(
        after_install,
        font_sheet_runs(1, 1, false),
        "exactly one catalog row fewer than the sheet that offered it ({with_catalog})"
    );
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

const STAGED_STEP_DELAY: Duration = Duration::from_millis(15);

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

    let reopened = open_font_picker(&mut h);
    assert!(reopened.runs > 0);
    assert!(
        !h.font_progress_bar_visible(),
        "a re-opened, already-installed row still carries no progress bar"
    );
}

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

    h.tap_label("bell notifications");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while h.bell_state() != Some(Toggled::True) {
        assert!(Instant::now() < deadline, "the bell never came back on");
        h.pump_once();
    }
    assert_eq!(h.depth(), 1, "the bell row pushes nothing");
}

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

    h.tap_label("bell notifications");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while h.bell_state() != Some(Toggled::False) {
        assert!(Instant::now() < deadline, "the switch never turned off");
        h.pump_once();
    }
}

#[test]
fn disconnect_confirms_first_then_deauthenticates_and_lands_on_connect() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    assert!(expect_connection_controller().connected.get_untracked());

    open_dialog_by_label(&mut h, LOCK_AND_DISCONNECT);
    assert_eq!(h.depth(), 2);
    h.tap(SCRIM_POINT);
    h.flush();
    assert!(
        expect_connection_controller().connected.get_untracked(),
        "cancelling leaves the connection up"
    );

    let dialog = open_dialog_by_label(&mut h, LOCK_AND_DISCONNECT);
    confirm(&mut h, &dialog);

    let connection = expect_connection_controller();
    assert!(!connection.connected.get_untracked(), "deauthenticated");
    assert_eq!(connection.server_info.get_untracked(), None);
    h.frame();
    assert!(
        node_values(&h.root).iter().any(|v| v == "not connected"),
        "the connection group cleared: {:?}",
        node_values(&h.root)
    );
    match app_nav().resolve("/sessions") {
        Resolution::Matched { location, .. } => assert_eq!(
            location.path, "/",
            "an unauthenticated /sessions is bounced to Connect"
        ),
        Resolution::Error { .. } => panic!("/sessions is a real route"),
    }
}

#[test]
fn wide_metrics_clamp_and_center_the_groups_column() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_with_metrics(Some(support::metrics_at(SURFACE.width, 500.0)));
    h.frame();

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

struct GatedStore {
    inner: MockEntitlementRepository,
    gate: Arc<Gate>,
}

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

    let pro = expect_pro_controller();
    pro.busy.set(true);
    h.pump_once();
    h.pump_once();
    assert!(
        !restoring(&node_values(&h.root)),
        "a purchase is not a restore"
    );

    app_toasts().clear();
    h.tap_label("restore");
    pump_until(&mut h, "the busy toast", || {
        !app_toasts().pending_untracked().is_empty()
    });
    assert_eq!(app_toasts().pending_untracked().len(), 1);
    assert!(!restoring(&node_values(&h.root)), "a refused restore");
    pro.busy.set(false);

    app_toasts().clear();
    h.tap_label("restore");
    pump_until(&mut h, "the restore to land", || {
        !app_toasts().pending_untracked().is_empty()
    });
    assert!(!restoring(&node_values(&h.root)), "released once it lands");
}

const NARROW_SURFACE: Size = Size::new(360.0, 3000.0);

#[test]
fn the_saved_server_cards_forget_button_never_clips_off_the_narrow_frame() {
    let _g = serial();
    let _ambient = setup();
    let mut h = with_saved_credential(true);

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
