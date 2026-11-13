//! Headless behavior coverage for the `/sessions` screen, mounted as the bottom layer of a
//! `Stack` under a real navigator so dialog, sheet and route pushes run end to end.
//! Repository calls are real async work; `Harness::pump_until_values` deadline-waits.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, GetUntracked, NavigatorController, Set, SizedBox, Stack, WindowMetrics,
    any, navigator, provide_context, spawn_local,
};
use frust_core::{NamedKey, PointerPhase, RenderRoot};
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::agents::{AgentStatus, AgentSummary};
use muxr::connection::{ConnectTarget, ServerBackend, TrustMode, expect_connection_controller};
use muxr::failure::AppFailure;
use muxr::features::connect::di::{expect_keyring, expect_saved_servers};
use muxr::features::connect::domain::entities::{SavedServer, epoch_millis};
use muxr::features::connect::domain::keyring::Keyring;
use muxr::features::pro::di::{expect_pro_controller, mock_entitlement};
use muxr::features::pro::domain::entities::Entitlement;
use muxr::features::servers::di::{
    BANNER_CONNECTING_DELAY_MS, expect_servers_controller, set_banner_connecting_delay_for_tests,
    set_banner_timings_for_tests,
};
use muxr::features::servers::di::{
    PEER_TAIL, keep_peers, set_stage_clock_for_tests, stage_settled,
};
use muxr::features::servers::presentation::controller::{SwitchGate, peer_endpoints};
use muxr::features::sessions::data::repositories::mock_sessions_repository::{
    MockSessionsRepository, SEEDED_DEV_ID, SEEDED_HERDR_ID,
};
use muxr::features::sessions::di::SharedSessionsRepository;
use muxr::features::sessions::domain::entities::{
    CreateSessionParams, LayoutSummary, RenameRequest, Session, SessionAgent, SessionBackend,
    SessionStatus, ThirdMetric,
};
use muxr::features::sessions::domain::repositories::SessionsRepository;
use muxr::features::sessions::presentation::controllers::{
    ConnectionTrouble, POLL_PERIOD_SECS, SessionsController,
};
use muxr::features::sessions::presentation::pages::{
    deferred_rain_arms_for_tests, rain_overlay_builds_for_tests, resolve_poll_interval,
    sessions_page, set_loading_rain_for_tests, trouble_reconnects_for_tests,
};
use muxr::features::settings::data::repositories::MockPreferencesRepository;
use muxr::features::settings::di::SharedPreferencesRepository;
use muxr::features::settings::domain::entities::{AppearancePrefs, PollPrefs, Preferences};
use muxr::features::settings::domain::repositories::PreferencesRepository;
use muxr::routes::{app_nav, install_app_router};
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, by_width, center, char_key, frame_at_sized, frame_sized, largest,
    long_press, named_key, node_centers_by_label, node_centers_by_value, node_labels, node_values,
    pointer, pump_frames, serial, settle_sized, setup, smallest, tap,
};

const SCRIM_POINT: Point = Point::new(4.0, 4.0);
const PANEL_PADDING: f64 = 20.0;
const LIST_PAD_X: f64 = 16.0;
const ROW_BORDER_INSET: f64 = 1.0;
const ACCENT_WIDTH: f64 = 2.0;
/// How long a pumped wait may run before the test fails rather than hangs.
const PUMP_DEADLINE: Duration = Duration::from_secs(5);
const SESSIONS_CLAMP_WIDTH: f64 = 640.0;
const PHONE_LANDSCAPE_H: f64 = 390.0;
const FULL_TABLET: Size = Size::new(1024.0, 720.0);
const COLLAPSED_TABLET: Size = Size::new(720.0, 720.0);
const PHONE: Size = Size::new(390.0, 844.0);

const SEEDED: [&str; 3] = ["dev", "jumping-peach", "herdr"];
const RUNNING: usize = 2;

fn mock_target(offer_credential: bool) -> ConnectTarget {
    ConnectTarget {
        host: "mock.example".to_string(),
        port: 7777,
        trust_mode: TrustMode::NoVerification,
        label: "Mock rig".to_string(),
        offer_credential,
        token: zeroize::Zeroizing::new(String::new()),
        remember: false,
        fingerprint: None,
    }
}

fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Harness {
    support::mount_with_metrics(metrics, Harness::mount)
}

type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

struct Harness {
    nav: NavigatorController<AppState>,
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    surface: Size,
}

impl Harness {
    fn mount() -> Self {
        Harness::mount_over(None, support::surface())
    }

    fn mount_over(repository: Option<SharedSessionsRepository>, surface: Size) -> Self {
        Harness::mount_with(repository, None, surface)
    }

    /// [`mount_over`](Self::mount_over), also swapping in `preferences` (the
    /// poll cadence's source) when given.
    fn mount_with(
        repository: Option<SharedSessionsRepository>,
        preferences: Option<SharedPreferencesRepository>,
        surface: Size,
    ) -> Self {
        // The real composition root, unbuilt: publishes DI including this
        // feature's repository, but never plays a splash.
        let state = MuxrApp.init();
        if let Some(repository) = repository {
            provide_context(repository);
        }
        if let Some(preferences) = preferences {
            provide_context(preferences);
        }
        let nav: NavigatorController<AppState> = NavigatorController::new();
        let _router = install_app_router(&nav, &nav, expect_connection_controller());
        let logic_nav = nav.clone();
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            let screen_nav = logic_nav.clone();
            let overlay_nav = logic_nav.clone();
            any(Stack(vec![
                any(sessions_page(screen_nav, logic_nav.clone())),
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
            surface,
        };
        harness.frame();
        harness
    }

    fn connected() -> Self {
        let mut harness = Harness::mount();
        expect_connection_controller().connect(mock_target(false));
        harness.await_sessions();
        harness
    }

    fn frame(&mut self) -> RecScene {
        frame_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            self.surface,
        )
    }

    fn settle(&mut self) -> RecScene {
        settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            self.surface,
        )
    }

    /// The navigator delivers a pusher's `on_result` on the first event pass after the pop's
    /// rebuild, so the trailing Move hands the answer over.
    fn flush(&mut self) {
        self.settle();
        self.settle();
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, SCRIM_POINT));
    }

    fn pump_until(&mut self, what: &str, done: impl Fn(&Harness) -> bool) {
        let deadline = Instant::now() + PUMP_DEADLINE;
        while !done(self) {
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
            if self.surface != support::surface() {
                self.frame();
            }
        }
    }

    fn pump_until_values(&mut self, what: &str, done: impl Fn(&[String]) -> bool) {
        self.pump_until(what, move |h| done(&node_values(&h.root)));
    }

    fn await_sessions(&mut self) {
        self.pump_until_values("the seeded sessions", |values| {
            SEEDED.iter().all(|name| values.iter().any(|v| v == name))
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

    fn long_press_card(&mut self, name: &str) {
        let point = self.center_of(name);
        long_press(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            point,
        );
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.root.event(&mut self.state, &char_key(&c.to_string()));
        }
    }

    fn press(&mut self, key: NamedKey) {
        self.root.event(&mut self.state, &named_key(key));
    }

    fn card_count(&mut self) -> usize {
        let scene = self.frame();
        card_backs(&scene).len()
    }

    /// The navigator's page-stack depth (1 = only the harness's empty base
    /// page).
    fn depth(&self) -> usize {
        self.nav.depth()
    }
}

fn card_backs(scene: &RecScene) -> Vec<(Point, Size)> {
    let width = support::W - 2.0 * LIST_PAD_X - ACCENT_WIDTH;
    scene
        .rounded
        .iter()
        .copied()
        .filter(|(origin, size)| {
            (size.width - width).abs() < EPSILON
                && (origin.x - (LIST_PAD_X + ACCENT_WIDTH)).abs() < EPSILON
        })
        .collect()
}

fn accent_bars(scene: &RecScene) -> Vec<(Point, Size)> {
    let width = support::W - 2.0 * LIST_PAD_X;
    scene
        .rounded
        .iter()
        .copied()
        .filter(|(origin, size)| {
            (size.width - width).abs() < EPSILON && (origin.x - LIST_PAD_X).abs() < EPSILON
        })
        .collect()
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

fn open_manage_sheet(h: &mut Harness, name: &str) -> Chrome {
    let before = h.frame();
    h.long_press_card(name);
    let after = h.settle();
    Chrome::diff(&before, &after)
}

struct Chrome {
    rounded: Vec<(Point, Size)>,
    strokes: Vec<(Point, Size)>,
    paths: Vec<(Point, Size)>,
}

impl Chrome {
    fn diff(before: &RecScene, after: &RecScene) -> Self {
        Chrome {
            rounded: added(&before.rounded, &after.rounded),
            strokes: added(&before.strokes, &after.strokes),
            paths: added(&before.paths, &after.paths),
        }
    }

    fn panel(&self) -> (Point, Size) {
        let mut candidates = self.rounded.clone();
        candidates.extend(self.paths.iter().copied());
        largest(&candidates).expect("an overlay always paints its own panel")
    }

    fn content_child(&self) -> (Point, Size) {
        let content_w = self.panel().1.width - 2.0 * PANEL_PADDING;
        by_width(&self.rounded, content_w)
            .unwrap_or_else(|| panic!("no full-width child in {:?}", self.rounded))
    }

    fn content_rows(&self) -> Vec<(Point, Size)> {
        let content_w = self.panel().1.width - 2.0 * PANEL_PADDING;
        let mut rows: Vec<(Point, Size)> = self
            .rounded
            .iter()
            .copied()
            .filter(|(_, size)| (size.width - content_w).abs() < EPSILON)
            .collect();
        rows.sort_by(|a, b| a.0.y.total_cmp(&b.0.y));
        rows
    }

    fn destructive_row(&self) -> (Point, Size) {
        let content_w = self.panel().1.width - 2.0 * PANEL_PADDING;
        let mut borders: Vec<(Point, Size)> = self
            .strokes
            .iter()
            .copied()
            .filter(|(_, size)| (size.width - (content_w - ROW_BORDER_INSET)).abs() < EPSILON)
            .collect();
        borders.sort_by(|a, b| a.0.y.total_cmp(&b.0.y));
        let row = *borders
            .last()
            .unwrap_or_else(|| panic!("no bordered row in {:?}", self.strokes));
        let lowest_fill = self
            .content_rows()
            .last()
            .map(|(origin, _)| origin.y)
            .unwrap_or_default();
        assert!(
            row.0.y > lowest_fill,
            "the destructive row is the sheet's last: {row:?} vs fills ending at {lowest_fill}"
        );
        row
    }
}

#[test]
fn the_conn_row_prints_the_connections_facts() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    assert!(
        values.iter().any(|v| v == "zellij 0.44.3"),
        "the strip prints every detected backend's version: {values:?}"
    );
    assert!(
        values.iter().any(|v| v == "36ms"),
        "…and the live latency as its own trailing run: {values:?}"
    );
    assert!(
        !values.iter().any(|v| v.contains("mock.example")),
        "no endpoint anywhere in the strip: {values:?}"
    );
    assert!(
        !values.iter().any(|v| v == "muxr"),
        "the screen no longer mounts a top bar of its own: {values:?}"
    );

    let labels = node_labels(&h.root);
    assert!(
        labels.iter().any(|l| l == "insecure"),
        "…and the insecure warning chip: {labels:?}"
    );
}

#[test]
fn the_seeded_sessions_render_as_cards_with_their_own_third_metric() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    for name in SEEDED {
        assert!(
            values.iter().any(|v| v == name),
            "the seeded session {name:?} renders: {values:?}"
        );
    }
    for span in [
        "\u{25a3} 9 panes",
        "\u{25e7} 4 tabs",
        "\u{25ce} 0 clients",
        "\u{25c6} 5 spaces",
        "0 tabs saved",
    ] {
        assert!(
            values.iter().any(|v| v == span),
            "the meta row prints {span:?}: {values:?}"
        );
    }
    assert!(
        !values.iter().any(|v| v.contains("created")),
        "no `created` line survives the redesign: {values:?}"
    );
    let labels = node_labels(&h.root);
    for gone in ["Attach", "Rename", "Kill", "Switch", "+"] {
        assert!(
            !labels.iter().any(|l| l == gone),
            "the {gone:?} button row is gone: {labels:?}"
        );
    }
    assert!(
        labels.iter().any(|l| l == "running") && labels.iter().any(|l| l == "resurrectible"),
        "both status chips are reachable from the seed: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == "zellij") && labels.iter().any(|l| l == "herdr"),
        "…and both backend chips: {labels:?}"
    );
}

#[test]
fn an_idle_card_carries_no_accent_and_no_pane_pills() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let scene = h.frame();

    assert_eq!(card_backs(&scene).len(), SEEDED.len(), "one fill per card");
    assert_eq!(
        accent_bars(&scene).len(),
        RUNNING,
        "only the running cards paint the success accent: {:?}",
        accent_bars(&scene)
    );

    let values = node_values(&h.root);
    for pill in ["btop", "ed@workbench:~"] {
        assert!(
            values.iter().any(|v| v == pill),
            "a running card previews {pill:?}: {values:?}"
        );
    }
    assert_eq!(
        values.iter().filter(|v| *v == "\u{b7}\u{b7}\u{b7}").count(),
        2,
        "herdr's two placeholder pills, and no others: {values:?}"
    );
}

fn connected_with(seed: impl FnOnce(&MockSessionsRepository)) -> Harness {
    let mock = Arc::new(MockSessionsRepository::seeded());
    seed(&mock);
    let mut h = Harness::mount_over(Some(mock), support::surface());
    expect_connection_controller().connect(mock_target(false));
    h.await_sessions();
    h
}

fn summary(blocked: u32, done: u32, working: u32, idle: u32, kinds: &[&str]) -> AgentSummary {
    AgentSummary {
        total: blocked + done + working + idle,
        blocked,
        done,
        working,
        idle,
        unknown: 0,
        kinds: kinds.iter().map(|k| k.to_string()).collect(),
    }
}

fn position(labels: &[String], wanted: &str) -> usize {
    labels
        .iter()
        .position(|l| l == wanted)
        .unwrap_or_else(|| panic!("{wanted:?} missing: {labels:?}"))
}

#[test]
fn a_herdr_card_with_agents_renders_the_strip_and_the_needs_you_rollup() {
    let _g = serial();
    let _ambient = setup();
    let mut h = connected_with(|_| {});
    h.frame();

    let labels = node_labels(&h.root);
    let values = node_values(&h.root);
    for tile in ["C", "X", "G"] {
        assert!(
            labels.iter().any(|l| l == tile),
            "tile {tile:?}: {labels:?}"
        );
    }
    assert!(values.iter().any(|v| v == "4 agents"), "count: {values:?}");
    assert!(
        labels
            .iter()
            .any(|l| l == "4 agents: 1 needs you, 1 done, 1 working, 1 idle"),
        "summary line: {labels:?}"
    );
    let order: Vec<usize> = ["\u{25c9} 1", "\u{2713} 1", "\u{25cf} 1", "\u{25cb} 1"]
        .iter()
        .map(|p| position(&labels, p))
        .collect();
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "pill order: {order:?}"
    );
    // A blocked agent: the herdr card's badge reads `needs you`, so only the
    // zellij card still says `running`.
    assert_eq!(labels.iter().filter(|l| *l == "needs you").count(), 1);
    assert_eq!(labels.iter().filter(|l| *l == "running").count(), 1);
}

#[test]
fn zero_count_pills_are_absent_and_an_unblocked_card_stays_running() {
    let _g = serial();
    let _ambient = setup();
    let mut h = connected_with(|repo| {
        repo.seed_agents_for_tests(SEEDED_HERDR_ID, Some(summary(0, 0, 2, 0, &["claude"])));
    });
    h.frame();

    let labels = node_labels(&h.root);
    assert!(labels.iter().any(|l| l == "\u{25cf} 2"), "{labels:?}");
    for gone in ["\u{25c9}", "\u{2713}", "\u{25cb}"] {
        assert!(
            !labels.iter().any(|l| l.starts_with(gone)),
            "no {gone} pill: {labels:?}"
        );
    }
    assert!(!labels.iter().any(|l| l == "needs you"), "{labels:?}");
    assert_eq!(labels.iter().filter(|l| *l == "running").count(), 2);
}

#[test]
fn a_zellij_card_and_an_idle_card_render_no_strip() {
    let _g = serial();
    let _ambient = setup();
    let mut h = connected_with(|repo| {
        let some = Some(summary(1, 0, 1, 0, &["claude", "codex"]));
        repo.seed_agents_for_tests("zellij:jumping-peach", some);
    });
    h.frame();

    let values = node_values(&h.root);
    assert_eq!(values.iter().filter(|v| v.ends_with(" agents")).count(), 1);
    assert!(values.iter().any(|v| v == "4 agents"), "{values:?}");
    assert!(!values.iter().any(|v| v == "2 agents"), "{values:?}");
}

#[test]
fn a_session_without_agents_renders_as_it_always_did() {
    let _g = serial();
    let _ambient = setup();
    let mut with = connected_with(|_| {});
    let scene_with = with.frame();
    let mut without = connected_with(|repo| repo.seed_agents_for_tests(SEEDED_HERDR_ID, None));
    let scene_without = without.frame();

    let labels = node_labels(&without.root);
    assert!(!labels.iter().any(|l| l == "needs you"), "{labels:?}");
    assert!(
        !node_values(&without.root)
            .iter()
            .any(|v| v.contains("agents")),
        "no count text"
    );
    assert_eq!(card_backs(&scene_without).len(), SEEDED.len());
    assert_eq!(accent_bars(&scene_without).len(), RUNNING);
    let tallest = |scene: &RecScene| {
        card_backs(scene)
            .iter()
            .map(|(_, size)| size.height)
            .fold(0.0, f64::max)
    };
    assert!(tallest(&scene_with) > tallest(&scene_without));
}

fn tap_outcome(connected: bool) -> (usize, usize) {
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target(false));
    h.await_sessions();
    if !connected {
        expect_connection_controller().disconnect();
        h.frame();
    }

    let before = h.frame();
    h.tap_value(SEEDED[0]);
    let after = h.settle();
    (h.depth(), added(&before.rounded, &after.rounded).len())
}

#[test]
fn a_card_tap_pushes_the_terminal_route_and_the_guard_still_applies() {
    let _g = serial();
    let _ambient = setup();

    let (connected_depth, _) = tap_outcome(true);
    let (guarded_depth, guarded_rects) = tap_outcome(false);

    assert_eq!(connected_depth, 2, "a card tap places the shell");
    assert_eq!(guarded_depth, 1, "…and a bounced one pushes nothing");
    assert_eq!(
        guarded_rects, 0,
        "…leaving the frame it was tapped on untouched"
    );
}

#[test]
fn a_long_press_opens_the_management_sheet_without_attaching() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let sheet = open_manage_sheet(&mut h, SEEDED[0]);
    assert_eq!(h.depth(), 2, "the sheet is pushed as its own page");

    let (origin, size) = sheet.panel();
    assert!(
        (origin.y + size.height - support::H).abs() < EPSILON,
        "a sheet, not a dialog: {origin:?}/{size:?}"
    );
    assert_eq!(
        sheet.content_rows().len(),
        2,
        "Attach and Rename… : {:?}",
        sheet.content_rows()
    );
    sheet.destructive_row();

    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 1, "a scrim tap dismisses it, and nothing else");
    assert!(
        node_values(&h.root).iter().any(|v| v == SEEDED[0]),
        "…back to the list, not to the terminal"
    );
}

#[test]
fn the_sheets_rename_row_round_trips() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let sheet = open_manage_sheet(&mut h, SEEDED[0]);
    let rename = sheet.content_rows()[1];
    let before = h.frame();
    h.tap(center(rename));
    let after = h.settle();
    let dialog = Chrome::diff(&before, &after);
    assert_eq!(
        h.depth(),
        2,
        "the sheet popped and the rename dialog took its place"
    );

    let field = dialog.content_child();
    h.tap(Point::new(
        field.0.x + field.1.width - 4.0,
        field.0.y + field.1.height / 2.0,
    ));
    h.type_text("-2");
    h.press(NamedKey::Enter);
    h.flush();

    h.pump_until_values("the renamed card", |values| {
        values.iter().any(|v| v == "dev-2")
    });
    let values = node_values(&h.root);
    assert!(
        !values.iter().any(|v| v == "dev"),
        "the old name is gone, not duplicated: {values:?}"
    );
    assert_eq!(
        h.card_count(),
        SEEDED.len(),
        "a rename never changes the list length"
    );
}

#[test]
fn the_rename_row_leaves_the_sheet_mounted_until_its_exit_completes_before_the_dialog_lands() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let sheet = open_manage_sheet(&mut h, SEEDED[0]);
    let sheet_depth = h.depth();
    let rename = sheet.content_rows()[1];
    let before = h.frame();
    h.tap(center(rename));

    h.frame();
    assert_eq!(
        h.depth(),
        sheet_depth,
        "the sheet is still mounted one frame after Rename — playing its \
         exit, the dialog not yet pushed"
    );

    let after = h.settle();
    assert_eq!(
        h.depth(),
        sheet_depth,
        "…and the dialog lands once the exit finishes, in the sheet's own place"
    );
    let dialog = Chrome::diff(&before, &after);
    dialog.content_child();
}

#[test]
fn the_sheets_kill_row_confirms_first_then_removes_the_card() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    kill_through_the_sheet(&mut h, SEEDED[0], false);
    assert_eq!(h.depth(), 1, "the scrim tap closed the dialog");
    assert_eq!(h.card_count(), SEEDED.len(), "…and nothing was killed");

    kill_through_the_sheet(&mut h, SEEDED[0], true);
    h.pump_until_values("the killed card to disappear", |values| {
        !values.iter().any(|v| v == SEEDED[0])
    });
    assert_eq!(h.depth(), 1);
    assert_eq!(h.card_count(), SEEDED.len() - 1);
}

fn kill_through_the_sheet(h: &mut Harness, name: &str, confirm: bool) {
    let sheet = open_manage_sheet(h, name);
    let kill = sheet.destructive_row();
    let before = h.frame();
    h.tap(center(kill));
    let after = h.settle();
    let dialog = Chrome::diff(&before, &after);

    if confirm {
        let action = smallest(&dialog.strokes).expect("the danger action strokes its border");
        h.tap(center(action));
    } else {
        h.tap(SCRIM_POINT);
    }
    h.flush();
    pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 4);
}

#[test]
fn the_dashed_row_creates_a_session() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    assert_eq!(h.card_count(), SEEDED.len());

    let dialog = open_dialog_by_value(&mut h, "new session");
    assert_eq!(h.depth(), 2, "the dialog is pushed as its own page");

    h.tap(center(dialog.content_child()));
    h.type_text("staging");
    h.press(NamedKey::Enter);
    h.flush();

    h.pump_until_values("the created session's card", |values| {
        values.iter().any(|v| v == "staging")
    });
    assert_eq!(h.depth(), 1, "the dialog closed behind it");
    assert_eq!(
        h.card_count(),
        SEEDED.len() + 1,
        "the new card joined the list rather than replacing one"
    );
}

#[test]
fn a_dead_space_down_blurs_the_focused_name_field_without_closing_the_dialog() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let dialog = open_dialog_by_value(&mut h, "new session");
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

#[test]
fn killing_every_session_shows_the_empty_state_and_creates_from_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    for _ in 0..SEEDED.len() {
        let name = node_values(&h.root)
            .into_iter()
            .find(|v| SEEDED.contains(&v.as_str()))
            .expect("a card is still on screen");
        kill_through_the_sheet(&mut h, &name, true);
        h.pump_until_values("the killed card to disappear", move |values| {
            !values.contains(&name)
        });
    }

    let labels = node_labels(&h.root);
    assert!(
        labels.iter().any(|l| l == "No sessions"),
        "an emptied list shows the empty state: {labels:?}"
    );
    assert_eq!(h.card_count(), 0, "…and no cards");

    let dialog = open_dialog_by_label(&mut h, "+ new session");
    h.tap(center(dialog.content_child()));
    h.type_text("first");
    h.press(NamedKey::Enter);
    h.flush();
    h.pump_until_values("a session created from the empty state", |values| {
        values.iter().any(|v| v == "first")
    });
    assert!(
        !node_labels(&h.root).iter().any(|l| l == "No sessions"),
        "…and creating one leaves the empty state behind"
    );
}

#[test]
fn the_screen_carries_no_settings_affordance_of_its_own() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let labels = node_labels(&h.root);
    assert!(
        !labels.iter().any(|l| l == "\u{2699}"),
        "the gear moved to the shell's bar: {labels:?}"
    );
}

const OFFER_TITLE: &str = "Save this server?";

#[test]
fn a_pending_credential_offer_paints_nothing_on_this_screen() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target(true));
    h.await_sessions();

    assert_eq!(
        expect_connection_controller()
            .credential_offer
            .get_untracked(),
        Some("Mock rig".to_string()),
        "the offer is genuinely pending — this test would pass for the wrong \
         reason otherwise"
    );
    let labels = node_labels(&h.root);
    assert!(
        !labels.iter().any(|l| l == OFFER_TITLE),
        "the offer is answered on Connect, before the redirect that lands here \
         (see `crate::connection`'s credential-offer placement): {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l == "Not now"),
        "…and so is neither of its actions: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l == "Save"),
        "…including the one that raised the biometric prompt over this screen: \
         {labels:?}"
    );
}

#[test]
fn this_screen_never_touches_the_pending_offer() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target(true));
    h.await_sessions();
    // Whatever this screen does — load, poll, rebuild — the offer is not its
    // state to consume: only Connect's own dialog (or a teardown) clears it.
    h.frame();
    h.frame();

    assert_eq!(
        expect_connection_controller()
            .credential_offer
            .get_untracked(),
        Some("Mock rig".to_string()),
        "Sessions neither renders nor clears the offer any more"
    );
}

fn card_backs_in(scene: &RecScene, left: f64, width: f64) -> Vec<(Point, Size)> {
    let card_width = width - ACCENT_WIDTH;
    scene
        .rounded
        .iter()
        .copied()
        .filter(|(origin, size)| {
            (size.width - card_width).abs() < EPSILON
                && (origin.x - (left + ACCENT_WIDTH)).abs() < EPSILON
        })
        .collect()
}

#[test]
fn wide_metrics_clamp_and_center_the_list_column_but_not_the_conn_row_strip() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_with_metrics(Some(support::metrics_at(support::W, PHONE_LANDSCAPE_H)));
    expect_connection_controller().connect(mock_target(false));
    h.await_sessions();

    let scene = h.frame();
    let margin = (support::W - SESSIONS_CLAMP_WIDTH) / 2.0;
    let list_left = margin + LIST_PAD_X;
    let list_width = SESSIONS_CLAMP_WIDTH - 2.0 * LIST_PAD_X;
    let cards = card_backs_in(&scene, list_left, list_width);
    assert_eq!(
        cards.len(),
        SEEDED.len(),
        "every seeded card still renders, just narrower and centered"
    );

    const STRIP_PAD_X: f64 = 16.0;
    const DOT: f64 = 7.0;
    const RING_SLOT: f64 = 20.0;
    let dot = scene
        .rounded
        .iter()
        .find(|(_, size)| (size.width - DOT).abs() < EPSILON && (size.height - DOT).abs() < EPSILON)
        .copied()
        .expect("the strip's success dot paints its own rounded fill");
    assert!(
        (dot.0.x - (STRIP_PAD_X + (RING_SLOT - DOT) / 2.0)).abs() < EPSILON,
        "the strip is not centered/clamped like the list column: {dot:?}"
    );
}

#[test]
fn narrow_metrics_leave_the_list_column_unclamped_like_no_metrics_at_all() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_with_metrics(Some(support::metrics_at(400.0, support::H)));
    expect_connection_controller().connect(mock_target(false));
    h.await_sessions();

    let scene = h.frame();
    let cards = card_backs(&scene);
    assert_eq!(
        cards.len(),
        SEEDED.len(),
        "narrow metrics take the same unclamped branch as no metrics at all"
    );
}

#[test]
fn read_only_disables_create_and_the_sheets_rename_and_kill_rows() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    expect_connection_controller().is_read_only.set(true);
    h.frame();

    let before = h.frame();
    h.tap_value("new session");
    let after = h.settle();
    assert_eq!(h.depth(), 1, "a read-only tap opens no create dialog");
    assert!(
        added(&before.rounded, &after.rounded).is_empty(),
        "…and paints no new chrome"
    );

    open_manage_sheet(&mut h, SEEDED[0]);
    let labels = node_labels(&h.root);
    assert!(labels.iter().any(|l| l == "Attach"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "Rename\u{2026}"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "Kill\u{2026}"), "{labels:?}");

    let before = h.frame();
    h.tap_label("Rename\u{2026}");
    let after = h.settle();
    assert!(
        Chrome::diff(&before, &after).rounded.is_empty(),
        "a read-only rename tap opens no dialog"
    );
    assert_eq!(h.depth(), 2, "…and the sheet is still the top page");

    let before = h.frame();
    h.tap_label("Kill\u{2026}");
    let after = h.settle();
    assert!(
        Chrome::diff(&before, &after).rounded.is_empty(),
        "a read-only kill tap opens no confirm dialog"
    );
    assert_eq!(h.depth(), 2);
}

#[test]
fn read_only_leaves_the_empty_states_own_create_action_disabled_too() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    for name in SEEDED {
        kill_through_the_sheet(&mut h, name, true);
        h.pump_until_values("the killed card to disappear", move |values| {
            !values.iter().any(|v| v == name)
        });
    }
    assert!(
        node_labels(&h.root).iter().any(|l| l == "No sessions"),
        "an emptied list shows the empty state"
    );

    expect_connection_controller().is_read_only.set(true);
    h.frame();

    let before = h.frame();
    h.tap_label("+ new session");
    let after = h.settle();
    assert_eq!(h.depth(), 1, "a read-only tap opens no create dialog");
    assert!(added(&before.rounded, &after.rounded).is_empty());
}

/// A minimal [`Session`], distinguishable only by `name` — the one field
/// either test below reads.
fn stub_session(name: &str) -> Session {
    Session {
        id: name.to_string(),
        name: name.to_string(),
        status: SessionStatus::Running,
        backend: SessionBackend::Zellij,
        age_secs: 0,
        has_bell: false,
        is_current: false,
        layout: LayoutSummary {
            tabs: 1,
            panes: 1,
            third: ThirdMetric::Clients(0),
            preview: Vec::new(),
            tabs_detail: Vec::new(),
            space: None,
        },
        agents: None,
    }
}

struct CountingSessionsRepo {
    calls: AtomicU32,
}

impl CountingSessionsRepo {
    fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for CountingSessionsRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }
}

#[tokio::test]
async fn a_tick_mid_flight_does_not_spawn_a_second_poll_refresh() {
    let repo = Arc::new(CountingSessionsRepo::new());
    let controller = SessionsController::new(Arc::clone(&repo) as SharedSessionsRepository);
    controller.refresh().await;
    assert_eq!(repo.calls(), 1, "the first load ran exactly once");

    assert!(
        controller.take_poll_tick(),
        "the first tick claims the in-flight guard"
    );
    assert!(
        !controller.take_poll_tick(),
        "a second tick before the claimed refresh finishes is skipped, not stacked"
    );

    controller.refresh_from_poll().await;
    assert_eq!(
        repo.calls(),
        2,
        "exactly one poll-triggered fan-out ran, not two"
    );

    assert!(
        controller.take_poll_tick(),
        "the claim released once the poll refresh finished, so the next tick is taken again"
    );
}

struct RaceSessionsRepo {
    calls: AtomicU32,
}

impl RaceSessionsRepo {
    fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for RaceSessionsRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            clean_signals::time::sleep(Duration::from_millis(80)).await;
            Ok(vec![stub_session("stale")])
        } else {
            clean_signals::time::sleep(Duration::from_millis(5)).await;
            Ok(vec![stub_session("fresh")])
        }
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }
}

#[tokio::test]
async fn a_slow_stale_refresh_completing_after_a_newer_one_does_not_clobber_it() {
    let controller = SessionsController::new(Arc::new(RaceSessionsRepo::new()));

    tokio::join!(controller.refresh(), controller.refresh());

    let names: Vec<String> = controller
        .sessions
        .get_untracked()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        names,
        vec!["fresh".to_string()],
        "the newer, faster refresh's result wins, not the older, slower one: {names:?}"
    );
}

#[test]
fn a_restart_ignores_the_old_generations_late_reply_and_clears_the_selection() {
    use frust_reactive::ReactiveRuntime;

    let _g = serial();
    let _ambient = setup();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let controller = Arc::new(SessionsController::new(Arc::new(RaceSessionsRepo::new())));
    controller.select("stale");
    runtime.block_on(async {
        let late = {
            let controller = Arc::clone(&controller);
            async move { controller.refresh().await }
        };
        let restart = {
            let controller = Arc::clone(&controller);
            async move {
                tokio::task::yield_now().await;
                controller.restart();
            }
        };
        tokio::join!(late, restart);
    });
    assert!(
        controller.sessions.get_untracked().is_empty(),
        "the late old-generation reply was ignored"
    );
    assert_eq!(controller.selected.get_untracked(), None);
    assert!(!controller.loaded.get_untracked(), "the first load is anew");

    let pump = ReactiveRuntime::get().expect("runtime installed by setup()");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while !controller.first_load_settled.get_untracked() {
        assert!(Instant::now() < deadline, "the restarted load never landed");
        pump.pump_local();
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(5)).await });
    }
    let names: Vec<String> = controller
        .sessions
        .get_untracked()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["fresh".to_string()]);
}

struct SwitchableRepo {
    names: Mutex<Vec<String>>,
    gated: std::sync::atomic::AtomicBool,
    release: tokio::sync::Notify,
}

impl SwitchableRepo {
    fn open(&self) {
        self.gated.store(false, Ordering::SeqCst);
        self.release.notify_waiters();
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for SwitchableRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        while self.gated.load(Ordering::SeqCst) {
            let parked = self.release.notified();
            if !self.gated.load(Ordering::SeqCst) {
                break;
            }
            parked.await;
        }
        let names = self.names.lock().expect("ok").clone();
        Ok(names.iter().map(|n| stub_session(n)).collect())
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the switch test")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the switch test")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the switch test")
    }
}

/// Opts the loading rain in for one test and back out when dropped, even on a panic.
struct RainOn;

impl RainOn {
    fn new() -> Self {
        set_loading_rain_for_tests(true);
        RainOn
    }
}

impl Drop for RainOn {
    fn drop(&mut self) {
        set_loading_rain_for_tests(false);
    }
}

fn switchable(names: &[&str], wait_for: &str) -> (Harness, Arc<SwitchableRepo>) {
    let repo = Arc::new(SwitchableRepo {
        names: Mutex::new(names.iter().map(|n| n.to_string()).collect()),
        gated: std::sync::atomic::AtomicBool::new(false),
        release: tokio::sync::Notify::new(),
    });
    let mut h = Harness::mount_over(
        Some(Arc::clone(&repo) as SharedSessionsRepository),
        support::surface(),
    );
    expect_connection_controller().connect(mock_target(false));
    h.pump_until_values("host A's list", |v| v.iter().any(|s| s == wait_for));
    (h, repo)
}

fn set_names(repo: &SwitchableRepo, names: &[&str]) {
    *repo.names.lock().expect("ok") = names.iter().map(|n| n.to_string()).collect();
}

fn assert_only_beta(h: &mut Harness) {
    let values = node_values(&h.root);
    assert!(
        !values.iter().any(|s| s.starts_with("alpha")),
        "host A's cards are gone: {values:?}"
    );
    assert_eq!(h.card_count(), 1);
}

#[test]
fn switching_servers_in_one_frame_replaces_the_old_hosts_cards() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, repo) = switchable(&["alpha-one", "alpha-two"], "alpha-two");
    set_names(&repo, &["beta-one"]);
    let connection = expect_connection_controller();
    connection.disconnect();
    connection.connect(mock_target(false));

    h.pump_until_values("host B's list", |v| v.iter().any(|s| s == "beta-one"));
    assert_only_beta(&mut h);
}

#[test]
fn switching_servers_across_frames_replaces_the_old_hosts_cards_and_selection() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, repo) = switchable(&["alpha-one", "alpha-two"], "alpha-two");
    let connection = expect_connection_controller();

    connection.disconnect();
    for _ in 0..3 {
        h.frame();
    }
    assert!(
        node_values(&h.root).iter().any(|s| s == "alpha-two"),
        "the screen is still mounted, showing host A's last list"
    );

    set_names(&repo, &["beta-one"]);
    repo.gated.store(true, Ordering::SeqCst);
    connection.connect(mock_target(false));
    h.pump_until("the dial to settle", |_| {
        connection.connected.get_untracked()
    });
    for _ in 0..2 {
        h.frame();
    }
    let values = node_values(&h.root);
    assert!(
        !values.iter().any(|s| s.starts_with("alpha")),
        "host A's cards are gone as soon as host B is up, before its list lands: {values:?}"
    );
    assert_eq!(h.card_count(), 0);

    repo.open();
    h.pump_until_values("host B's list", |v| v.iter().any(|s| s == "beta-one"));
    assert_only_beta(&mut h);
}

#[test]
fn a_switch_replays_the_loading_rain_and_the_new_first_load_releases_it() {
    let _g = serial();
    let _ambient = setup();
    let _rain = RainOn::new();
    let (mut h, repo) = switchable(&["alpha-one", "alpha-two"], "alpha-two");
    let mut clock = 0.0;
    let run_to = |h: &mut Harness, clock: &mut f64, until: f64| {
        let mut scene = h.frame();
        while *clock < until {
            *clock += 100.0;
            scene = frame_at_sized(
                &mut h.root,
                &mut h.logic,
                &mut h.state,
                &mut h.tcx,
                h.surface,
                support::ms(*clock),
            );
        }
        scene
    };

    let up = h.frame();
    assert!(up.glyph_runs > RAIN_RUNS, "the first load opens under rain");
    let released = run_to(&mut h, &mut clock, 5_000.0);
    assert!(
        released.glyph_runs < RAIN_RUNS,
        "the first load released it"
    );

    set_names(&repo, &["beta-one"]);
    let connection = expect_connection_controller();
    connection.disconnect();
    connection.connect(mock_target(false));
    h.pump_until_values("host B's list", |v| v.iter().any(|s| s == "beta-one"));
    assert_only_beta(&mut h);
    let replay = h.frame();
    assert!(
        replay.glyph_runs > RAIN_RUNS,
        "the switch re-mounts the rain: {} runs",
        replay.glyph_runs
    );
    let until = clock + 5_000.0;
    let released = run_to(&mut h, &mut clock, until);
    assert!(
        released.glyph_runs < RAIN_RUNS,
        "the new first load releases it: {} runs",
        released.glyph_runs
    );
}

/// A pinned stage clock that is released when dropped, even on a panic.
struct StageClock(Instant);

impl StageClock {
    fn closing() -> Self {
        let at = Instant::now();
        set_stage_clock_for_tests(Some(at));
        keep_peers(true);
        keep_peers(false);
        assert!(!stage_settled(), "the close tail is running");
        StageClock(at)
    }

    fn advance(&self, by: Duration) {
        set_stage_clock_for_tests(Some(self.0 + by));
    }
}

impl Drop for StageClock {
    fn drop(&mut self) {
        set_stage_clock_for_tests(None);
    }
}

fn rain_builds_over(h: &mut Harness, wall: Duration) -> usize {
    let before = rain_overlay_builds_for_tests();
    let end = Instant::now() + wall;
    h.frame();
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
        h.frame();
    }
    rain_overlay_builds_for_tests() - before
}

fn rain_released_then_gated(h: &mut Harness, repo: &SwitchableRepo) {
    let mut clock = 0.0;
    let mut runs = h.frame().glyph_runs;
    assert!(runs > RAIN_RUNS, "the first load opens under rain");
    while runs >= RAIN_RUNS {
        assert!(clock < 5_000.0, "the first rain never ended");
        clock += 100.0;
        runs = frame_at_sized(
            &mut h.root,
            &mut h.logic,
            &mut h.state,
            &mut h.tcx,
            h.surface,
            support::ms(clock),
        )
        .glyph_runs;
    }
    repo.gated.store(true, Ordering::SeqCst);
}

fn switch_hosts(h: &mut Harness) {
    let connection = expect_connection_controller();
    connection.disconnect();
    connection.connect(mock_target(false));
    h.pump_until("the dial to settle", |_| {
        connection.connected.get_untracked()
    });
    for _ in 0..2 {
        h.frame();
    }
}

#[test]
fn a_switch_during_the_stage_defers_the_rain_until_the_stage_settles() {
    let _g = serial();
    let _ambient = setup();
    let _rain = RainOn::new();
    let (mut h, repo) = switchable(&["alpha-one"], "alpha-one");
    rain_released_then_gated(&mut h, &repo);
    let stage = StageClock::closing();
    let arms = deferred_rain_arms_for_tests();

    set_names(&repo, &["beta-one"]);
    switch_hosts(&mut h);
    assert!(
        rain_builds_over(&mut h, Duration::from_millis(250)) == 0,
        "no rain during the handshake and the tail"
    );
    assert_eq!(deferred_rain_arms_for_tests(), arms);

    stage.advance(PEER_TAIL);
    assert!(
        rain_builds_over(&mut h, Duration::from_millis(400)) > 0,
        "the rain arms once the stage settles, the first load still pending"
    );
    assert_eq!(deferred_rain_arms_for_tests(), arms + 1);
}

#[test]
fn a_first_load_settling_before_the_stage_skips_the_rain() {
    let _g = serial();
    let _ambient = setup();
    let _rain = RainOn::new();
    let (mut h, repo) = switchable(&["alpha-one"], "alpha-one");
    rain_released_then_gated(&mut h, &repo);
    let stage = StageClock::closing();
    let arms = deferred_rain_arms_for_tests();

    set_names(&repo, &["beta-one"]);
    switch_hosts(&mut h);
    repo.open();
    h.pump_until_values("host B's list", |v| v.iter().any(|s| s == "beta-one"));
    stage.advance(PEER_TAIL);
    assert!(
        rain_builds_over(&mut h, Duration::from_millis(400)) == 0,
        "the rain never arms over a settled load"
    );
    assert_eq!(deferred_rain_arms_for_tests(), arms);
}

#[test]
fn a_switch_with_the_stage_at_rest_arms_the_rain_at_once() {
    let _g = serial();
    let _ambient = setup();
    let _rain = RainOn::new();
    let (mut h, repo) = switchable(&["alpha-one"], "alpha-one");
    rain_released_then_gated(&mut h, &repo);
    let arms = deferred_rain_arms_for_tests();

    set_names(&repo, &["beta-one"]);
    let connection = expect_connection_controller();
    connection.disconnect();
    connection.connect(mock_target(false));
    h.pump_until("the dial to settle", |_| {
        connection.connected.get_untracked()
    });
    assert!(
        rain_builds_over(&mut h, Duration::from_millis(100)) > 0,
        "a Settings-initiated switch replays the rain immediately"
    );
    assert_eq!(deferred_rain_arms_for_tests(), arms, "no deferral");
}

#[test]
fn a_second_switch_cancels_the_pending_deferred_arm() {
    let _g = serial();
    let _ambient = setup();
    let _rain = RainOn::new();
    let (mut h, repo) = switchable(&["alpha-one"], "alpha-one");
    rain_released_then_gated(&mut h, &repo);
    let stage = StageClock::closing();
    let arms = deferred_rain_arms_for_tests();

    set_names(&repo, &["beta-one"]);
    switch_hosts(&mut h);
    rain_builds_over(&mut h, Duration::from_millis(120));
    switch_hosts(&mut h);

    stage.advance(PEER_TAIL);
    assert!(rain_builds_over(&mut h, Duration::from_millis(500)) > 0);
    assert_eq!(
        deferred_rain_arms_for_tests(),
        arms + 1,
        "the stale arm did not also fire"
    );
}

const RAIN_RUNS: usize = 100;

const AGENTS_SESSION: &str = "herdr:agents";

struct AgentsRepo {
    agents: Mutex<Vec<SessionAgent>>,
    fail: std::sync::atomic::AtomicBool,
    calls: AtomicU32,
    lists: AtomicU32,
    stall: std::sync::atomic::AtomicBool,
    release: tokio::sync::Notify,
}

impl AgentsRepo {
    fn new(agents: Vec<SessionAgent>) -> Self {
        Self {
            agents: Mutex::new(agents),
            fail: std::sync::atomic::AtomicBool::new(false),
            calls: AtomicU32::new(0),
            lists: AtomicU32::new(0),
            stall: std::sync::atomic::AtomicBool::new(false),
            release: tokio::sync::Notify::new(),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    fn lists(&self) -> u32 {
        self.lists.load(Ordering::SeqCst)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for AgentsRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        self.lists.fetch_add(1, Ordering::SeqCst);
        let mut session = stub_session(AGENTS_SESSION);
        session.backend = SessionBackend::Herdr;
        session.agents = Some(summary(1, 0, 1, 0, &["claude"]));
        Ok(vec![session, stub_session("other")])
    }

    async fn list_agents(&self, _session_id: String) -> Result<Vec<SessionAgent>, AppFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.stall.swap(false, Ordering::SeqCst) {
            self.release.notified().await;
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(AppFailure::Network("list_agents refused".to_string()));
        }
        Ok(self.agents.lock().expect("ok").clone())
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the agent-list tests")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the agent-list tests")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the agent-list tests")
    }
}

fn session_agent(pane_id: u32, status: AgentStatus) -> SessionAgent {
    SessionAgent {
        pane_id,
        tab_position: Some(0),
        space_id: None,
        space_name: None,
        tab_name: "work".to_string(),
        kind: "claude".to_string(),
        name: Some(format!("agent{pane_id}")),
        display_kind: None,
        status,
        title: None,
        summary: None,
        change_seq: 1,
    }
}

fn agent_rows(controller: &SessionsController) -> Vec<(u32, AgentStatus)> {
    controller
        .agents
        .get_untracked()
        .iter()
        .map(|agent| (agent.pane_id, agent.status))
        .collect()
}

async fn selected_with_agents(repo: &Arc<AgentsRepo>) -> Arc<SessionsController> {
    let controller = Arc::new(SessionsController::new(
        Arc::clone(repo) as SharedSessionsRepository
    ));
    controller.set_detail_visible(true);
    controller.refresh().await;
    controller.select(AGENTS_SESSION);
    controller.refresh_agents().await;
    controller
}

async fn tick(controller: &SessionsController) {
    assert!(controller.take_poll_tick(), "the poll is armed");
    controller.refresh_from_poll().await;
}

#[tokio::test]
async fn swapping_two_agents_statuses_at_constant_counts_updates_the_rows_on_the_next_tick() {
    let repo = Arc::new(AgentsRepo::new(vec![
        session_agent(1, AgentStatus::Blocked),
        session_agent(2, AgentStatus::Working),
    ]));
    let controller = selected_with_agents(&repo).await;
    assert_eq!(
        agent_rows(&controller),
        [(1, AgentStatus::Blocked), (2, AgentStatus::Working)]
    );

    // The roll-up still reads one blocked, one working: only the rows moved.
    *repo.agents.lock().expect("ok") = vec![
        session_agent(1, AgentStatus::Working),
        session_agent(2, AgentStatus::Blocked),
    ];
    tick(&controller).await;

    assert_eq!(
        agent_rows(&controller),
        [(2, AgentStatus::Blocked), (1, AgentStatus::Working)]
    );
}

#[tokio::test]
async fn a_failed_agent_read_keeps_the_previous_rows_and_the_next_tick_retries() {
    let repo = Arc::new(AgentsRepo::new(vec![session_agent(
        1,
        AgentStatus::Blocked,
    )]));
    let controller = selected_with_agents(&repo).await;

    repo.fail.store(true, Ordering::SeqCst);
    *repo.agents.lock().expect("ok") = vec![session_agent(1, AgentStatus::Idle)];
    tick(&controller).await;
    assert_eq!(agent_rows(&controller), [(1, AgentStatus::Blocked)]);

    repo.fail.store(false, Ordering::SeqCst);
    tick(&controller).await;
    assert_eq!(agent_rows(&controller), [(1, AgentStatus::Idle)]);
}

#[tokio::test]
async fn a_hidden_detail_never_reads_agents_and_a_selection_change_empties_the_list_at_once() {
    let repo = Arc::new(AgentsRepo::new(vec![session_agent(
        1,
        AgentStatus::Blocked,
    )]));
    let controller = selected_with_agents(&repo).await;
    assert_eq!(repo.calls(), 1);

    controller.select("other");
    assert!(
        controller.agents.get_untracked().is_empty(),
        "cleared on the change"
    );
    tick(&controller).await;
    assert_eq!(repo.calls(), 1, "a session without agents is never asked");

    controller.select(AGENTS_SESSION);
    controller.set_detail_visible(false);
    tick(&controller).await;
    assert_eq!(repo.calls(), 1, "a Compact window pays for nothing");
    assert!(controller.agents.get_untracked().is_empty());
}

#[tokio::test]
async fn a_stalled_agents_read_does_not_delay_the_next_list_poll() {
    let repo = Arc::new(AgentsRepo::new(vec![session_agent(
        1,
        AgentStatus::Blocked,
    )]));
    let controller = selected_with_agents(&repo).await;
    let (calls, lists) = (repo.calls(), repo.lists());

    repo.stall.store(true, Ordering::SeqCst);
    assert!(controller.take_poll_tick(), "the poll is armed");
    let first = controller.refresh_from_poll();
    let second = async {
        while repo.calls() == calls {
            tokio::task::yield_now().await;
        }
        assert!(
            controller.take_poll_tick(),
            "the list poll's claim is free while the agents read hangs"
        );
        controller.refresh_from_poll().await;
        assert_eq!(repo.lists(), lists + 2, "the second list refresh ran");
        assert_eq!(
            repo.calls(),
            calls + 1,
            "the hung agents read is single-flight: no second one stacked"
        );
        repo.release.notify_one();
    };
    tokio::join!(first, second);

    tick(&controller).await;
    assert_eq!(
        repo.calls(),
        calls + 2,
        "the claim released: agents read again"
    );
}

#[test]
fn a_visible_edge_with_a_selected_session_fetches_the_agents_without_a_tick() {
    use frust_reactive::ReactiveRuntime;

    let _g = serial();
    let _ambient = setup();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let repo = Arc::new(AgentsRepo::new(vec![session_agent(
        1,
        AgentStatus::Blocked,
    )]));
    let controller = Arc::new(SessionsController::new(
        Arc::clone(&repo) as SharedSessionsRepository
    ));
    runtime.block_on(async {
        controller.refresh().await;
        controller.select(AGENTS_SESSION);
    });
    assert_eq!(repo.calls(), 0, "hidden: nothing read yet");

    controller.set_detail_visible(true);
    let pump = ReactiveRuntime::get().expect("runtime installed by setup()");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while repo.calls() == 0 {
        assert!(Instant::now() < deadline, "the edge never fetched");
        pump.pump_local();
        runtime.block_on(tokio::task::yield_now());
    }
    while agent_rows(&controller).is_empty() {
        assert!(Instant::now() < deadline, "the rows never landed");
        pump.pump_local();
        runtime.block_on(tokio::task::yield_now());
    }
    assert_eq!(agent_rows(&controller), [(1, AgentStatus::Blocked)]);

    controller.set_detail_visible(true);
    assert_eq!(repo.calls(), 1, "a repeated value is not an edge");
    controller.set_detail_visible(false);
    assert!(
        controller.agents.get_untracked().is_empty(),
        "the hidden edge clears the list"
    );
}

struct FailingPreferencesRepo;

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl PreferencesRepository for FailingPreferencesRepo {
    async fn load_preferences(&self) -> Result<Preferences, AppFailure> {
        Err(AppFailure::Storage("simulated read failure".to_string()))
    }

    async fn save_appearance(
        &self,
        _prefs: AppearancePrefs,
    ) -> Result<AppearancePrefs, AppFailure> {
        unreachable!("not exercised by the cadence tests")
    }

    async fn save_poll_prefs(&self, _prefs: PollPrefs) -> Result<PollPrefs, AppFailure> {
        unreachable!("not exercised by the cadence tests")
    }
}

#[tokio::test]
async fn the_default_pref_resolves_to_the_three_second_pin() {
    let preferences: SharedPreferencesRepository = Arc::new(MockPreferencesRepository::seeded());

    let interval = resolve_poll_interval(&preferences).await;

    assert_eq!(interval, Duration::from_secs(u64::from(POLL_PERIOD_SECS)));
}

#[tokio::test]
async fn changing_the_pref_mid_mount_changes_the_next_scheduled_interval_for_both_loops() {
    let preferences: SharedPreferencesRepository = Arc::new(MockPreferencesRepository::seeded());

    let first = resolve_poll_interval(&preferences).await;
    assert_eq!(
        first,
        Duration::from_secs(3),
        "the shipped default, before any settings edit"
    );

    preferences
        .save_poll_prefs(PollPrefs {
            layout_refresh_secs: 10,
            bell_notifications: true,
        })
        .await
        .expect("a legal choice-set value is accepted");

    let second = resolve_poll_interval(&preferences).await;
    assert_eq!(
        second,
        Duration::from_secs(10),
        "the next resolve — the one either loop's next scheduled sleep reads — sees the live value"
    );
}

#[tokio::test]
async fn a_pref_read_failure_falls_back_to_the_default_without_killing_the_loop() {
    let preferences: SharedPreferencesRepository = Arc::new(FailingPreferencesRepo);

    let interval = resolve_poll_interval(&preferences).await;

    assert_eq!(interval, Duration::from_secs(u64::from(POLL_PERIOD_SECS)));
}

fn sized(size: Size, repository: Option<SharedSessionsRepository>) -> Harness {
    let metrics = support::metrics_at(size.width, size.height);
    let mut h =
        support::mount_with_metrics(Some(metrics), || Harness::mount_over(repository, size));
    expect_connection_controller().connect(mock_target(false));
    h.frame();
    h
}

fn count_values(h: &Harness, value: &str) -> usize {
    node_values(&h.root).iter().filter(|v| *v == value).count()
}

fn has_label(h: &Harness, label: &str) -> bool {
    node_labels(&h.root).iter().any(|l| l == label)
}

fn has_value(h: &Harness, value: &str) -> bool {
    count_values(h, value) > 0
}

fn stat_value(h: &Harness, label: &str) -> Option<String> {
    h.root
        .semantics()
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label) && node.description().is_some())
        .and_then(|(_, node)| node.description().map(str::to_string))
}

fn tap_last_value(h: &mut Harness, value: &str) {
    let point = *node_centers_by_value(&h.root, value)
        .last()
        .unwrap_or_else(|| panic!("nothing printing {value:?}"));
    h.tap(point);
}

#[test]
fn a_tablet_card_tap_selects_it_and_the_detail_header_names_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    assert!(
        has_value(&h, "loading sessions\u{2026}") && !has_label(&h, "Select a session"),
        "the first load in flight: the column's loading line, skeletons in the detail"
    );
    h.await_sessions();
    assert!(has_label(&h, "Select a session"), "nothing selected yet");
    assert_eq!(count_values(&h, SEEDED[1]), 1, "only the card names it");

    h.tap_value(SEEDED[1]);
    h.settle();

    assert_eq!(h.depth(), 1, "a Tablet tap selects, it never attaches");
    assert_eq!(count_values(&h, SEEDED[1]), 2, "the header names it too");
    let labels = node_labels(&h.root);
    assert!(
        labels.iter().any(|l| l.contains("attach")),
        "the header's actions carry attach: {labels:?}"
    );
    assert!(!has_label(&h, "Select a session"));
    assert!(has_label(&h, "\u{2192} attach to resurrect"), "{labels:?}");
    assert!(has_value(&h, "saved, not running"));
    let values = node_values(&h.root);
    assert!(
        !values
            .iter()
            .any(|v| v.starts_with("tabs \u{2192} panes") || v.starts_with("active tab")),
        "a resurrectible session draws no layout card: {values:?}"
    );
}

#[test]
fn the_tabs_to_panes_card_opens_the_focused_tab_on_its_pane_pills() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();

    h.tap_value(SEEDED[0]);
    h.settle();

    assert!(has_label(&h, "\u{2192} attach"));
    assert!(has_value(&h, "tabs \u{2192} panes"));
    assert!(has_value(&h, "active tab \u{b7} main"));
    for tab in ["main", "build", "edit", "logs"] {
        assert!(has_value(&h, tab), "every tab is listed: {tab}");
    }
    assert!(has_label(&h, "focused tab"));
    assert!(has_label(&h, "float"), "the floating pane carries its tag");
    assert!(has_value(&h, "scratch"), "the focused tab's pane pills");
    assert!(
        !has_value(&h, "cargo watch") && !has_value(&h, "journalctl"),
        "only the focused tab is open"
    );
    assert_eq!(stat_value(&h, "tabs").as_deref(), Some("4"));
    assert_eq!(stat_value(&h, "panes").as_deref(), Some("9"));
    assert_eq!(stat_value(&h, "clients").as_deref(), Some("0"));
    assert_eq!(stat_value(&h, "age").as_deref(), Some("3 hours ago"));

    tap_last_value(&mut h, "scratch");
    h.settle();
    assert_eq!(h.depth(), 2, "a pane pill pushes the terminal route");
    let nav = muxr::routes::app_nav();
    let param = |key: &str| nav.param(key);
    assert_eq!(
        param(muxr::routes::TERMINAL_SESSION_QUERY_KEY).as_deref(),
        Some(SEEDED_DEV_ID)
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_TAB_QUERY_KEY).as_deref(),
        Some("0")
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_PANE_QUERY_KEY).as_deref(),
        Some("2")
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_SPACE_QUERY_KEY),
        None,
        "a pane pill names no space"
    );
}

#[test]
fn the_tablet_detail_lists_the_seeded_agents_in_priority_order_and_a_row_tap_deep_links() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();

    h.tap_value(SEEDED[2]);
    h.settle();
    h.pump_until("the agent list", |h| has_value(h, "AI AGENTS \u{b7} 4"));

    let values = node_values(&h.root);
    let at = |wanted: &str| values.iter().position(|v| v == wanted);
    let order: Vec<_> = [
        "reviewer \u{b7} claude",
        "planner \u{b7} codex",
        "scout \u{b7} gemini",
        "notes \u{b7} claude",
    ]
    .map(|row| at(row).unwrap_or_else(|| panic!("no row {row:?} in {values:?}")))
    .into();
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "blocked, done, working, idle: {values:?}"
    );
    assert!(
        at("AI AGENTS \u{b7} 4") < at("tabs \u{2192} panes \u{b7} space home"),
        "the agents sit above the tabs card"
    );
    assert!(has_value(&h, "home \u{203a} work \u{203a} p0"));

    tap_last_value(&mut h, "reviewer \u{b7} claude");
    h.settle();
    assert_eq!(h.depth(), 2, "an agent row pushes the terminal route");
    let nav = muxr::routes::app_nav();
    let param = |key: &str| nav.param(key);
    assert_eq!(
        param(muxr::routes::TERMINAL_SESSION_QUERY_KEY).as_deref(),
        Some(SEEDED_HERDR_ID)
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_SPACE_QUERY_KEY).as_deref(),
        Some("home")
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_TAB_QUERY_KEY).as_deref(),
        Some("0")
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_PANE_QUERY_KEY).as_deref(),
        Some("0")
    );
}

#[test]
fn a_session_without_agents_draws_no_agent_section() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();

    h.tap_value(SEEDED[0]);
    h.settle();

    assert!(
        !node_values(&h.root)
            .iter()
            .any(|v| v.starts_with("AI AGENTS"))
    );
}

#[test]
fn the_herdr_detail_names_its_space_on_the_collapsed_band() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(COLLAPSED_TABLET, None);
    h.await_sessions();

    h.tap_value(SEEDED[2]);
    h.settle();

    assert_eq!(h.depth(), 1);
    assert!(has_value(&h, "space \u{b7} home \u{b7} tab \u{b7} work"));
    assert!(has_value(&h, "tabs \u{2192} panes \u{b7} space home"));
    assert_eq!(stat_value(&h, "spaces").as_deref(), Some("5"));
}

struct FailFirstRepo {
    calls: AtomicU32,
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for FailFirstRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(AppFailure::Network("first load".to_string()));
        }
        Ok(vec![stub_session("alpha")])
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the retry test")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the retry test")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the retry test")
    }
}

#[test]
fn a_failed_first_load_offers_a_retry_that_reruns_it() {
    let _g = serial();
    let _ambient = setup();
    let repo = Arc::new(FailFirstRepo {
        calls: AtomicU32::new(0),
    });
    let mut h = sized(
        FULL_TABLET,
        Some(Arc::clone(&repo) as SharedSessionsRepository),
    );
    h.pump_until("the failed state", |h| {
        has_label(h, "Couldn't load sessions")
    });
    assert!(has_label(&h, "Retry"));
    assert!(
        has_label(&h, "Select a session"),
        "the detail shows no selection"
    );
    assert_eq!(repo.calls.load(Ordering::SeqCst), 1);

    h.tap_label("Retry");
    h.pump_until_values("the retried load", |values| {
        values.iter().any(|v| v == "alpha")
    });

    assert_eq!(
        repo.calls.load(Ordering::SeqCst),
        2,
        "Retry re-ran the load"
    );
    assert!(!has_label(&h, "Couldn't load sessions"));
}

#[test]
fn a_selected_tablet_card_fills_with_the_opaque_amber_wash_not_a_translucent_tint() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();
    h.tap_value(SEEDED[1]);
    let scene = h.settle();
    let colors = muxr::ui::palette::Palette::from_theme(&frust_glyph::baseline());

    let fills = &scene.rounded_fills;
    let area = |s: &Size| s.width * s.height;
    let (accent_idx, accent_origin, accent_size) = fills
        .iter()
        .enumerate()
        .filter(|(_, (_, _, _, c))| *c == colors.amber)
        .map(|(i, (o, s, _, _))| (i, *o, *s))
        .max_by(|a, b| area(&a.2).partial_cmp(&area(&b.2)).unwrap())
        .expect("the selected card's accent box");
    let inset_like = |o: &Point, s: &Size| {
        (o.x - (accent_origin.x + 2.0)).abs() < EPSILON
            && (o.y - accent_origin.y).abs() < EPSILON
            && (s.height - accent_size.height).abs() < EPSILON
    };
    let wash_idx = fills
        .iter()
        .position(|(o, s, _, c)| *c == colors.amber_wash && inset_like(o, s))
        .expect("the opaque amber wash inset by the strip");
    assert!(accent_idx < wash_idx, "the wash paints over the accent box");
    assert!(
        !fills
            .iter()
            .any(|(o, s, _, c)| *c == colors.amber_faint && inset_like(o, s)),
        "no translucent tint rides directly on the accent box any more"
    );
}

#[test]
fn read_only_on_tablet_chips_the_strip_and_leaves_only_attach_working() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();
    expect_connection_controller().is_read_only.set(true);
    h.frame();
    assert!(has_label(&h, "read-only"), "the strip's read-only chip");

    h.tap_value(SEEDED[0]);
    h.settle();
    let toasts = muxr::ui::toasts::app_toasts();
    toasts.clear();
    for action in ["rename", "kill"] {
        h.tap_label(action);
        h.settle();
        assert_eq!(h.depth(), 1, "a read-only {action} opens nothing");
    }
    let raised = toasts.pending_untracked();
    assert_eq!(raised.len(), 1, "one refusal, deduped: {raised:?}");
    assert!(raised.iter().all(|t| t.starts_with("Read-only session")));
    muxr::ui::toasts::advance_toast_clock_for_tests(
        muxr::ui::toasts::DEDUPE_WINDOW + Duration::from_millis(1),
    );
    h.tap_label("kill");
    h.settle();
    let raised = toasts.pending_untracked();
    assert_eq!(raised.len(), 2, "past the window it shows: {raised:?}");
    assert_eq!(h.depth(), 1, "still nothing opened");

    h.tap_label("\u{2192} attach");
    h.settle();
    assert_eq!(h.depth(), 2, "attach is never read-only-gated");
}

#[test]
fn the_detail_headers_rename_opens_the_shared_dialog() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();
    h.tap_value(SEEDED[0]);
    h.settle();

    h.tap_label("rename");
    h.settle();

    assert_eq!(h.depth(), 2, "the rename dialog is up");
}

#[test]
fn renaming_the_selected_session_from_the_detail_keeps_the_detail_on_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();
    h.tap_value(SEEDED[0]);
    h.settle();

    let before = h.frame();
    h.tap_label("rename");
    let after = h.settle();
    let dialog = Chrome::diff(&before, &after);
    let field = dialog.content_child();
    h.tap(Point::new(
        field.0.x + field.1.width - 4.0,
        field.0.y + field.1.height / 2.0,
    ));
    h.type_text("-2");
    h.press(NamedKey::Enter);
    h.flush();
    h.pump_until_values("the renamed card", |values| {
        values.iter().any(|v| v == "dev-2")
    });
    h.settle();

    assert!(
        !has_label(&h, "Select a session"),
        "the rename renamed the session's id too; the selection followed it"
    );
    assert_eq!(
        count_values(&h, "dev-2"),
        2,
        "the list card and the detail header both show the renamed session"
    );
}

#[test]
fn read_only_adds_the_strip_chip_on_compact_too() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    assert!(!has_label(&h, "read-only"));

    expect_connection_controller().is_read_only.set(true);
    h.frame();

    assert!(has_label(&h, "read-only"));
}

#[test]
fn a_phone_card_tap_still_attaches_and_marks_this_device() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(PHONE, None);
    h.await_sessions();
    assert!(
        !has_label(&h, "Select a session"),
        "no detail pane on a phone"
    );
    assert_eq!(count_values(&h, "\u{25cf} this device"), 1);

    h.tap_value(SEEDED[0]);
    h.settle();

    assert_eq!(h.depth(), 2, "a phone tap attaches");
}

struct RecordingCreateRepo {
    created: Mutex<Vec<CreateSessionParams>>,
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for RecordingCreateRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        let mut all = vec![stub_session("alpha")];
        all.extend(
            self.created
                .lock()
                .unwrap()
                .iter()
                .map(|params| stub_session(&params.name)),
        );
        Ok(all)
    }

    async fn create_session(&self, params: CreateSessionParams) -> Result<Session, AppFailure> {
        let session = stub_session(&params.name);
        self.created.lock().unwrap().push(params);
        Ok(session)
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the backend-choice tests")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the backend-choice tests")
    }
}

fn offering(backends: Vec<ServerBackend>) -> (Harness, Arc<RecordingCreateRepo>) {
    let repo = Arc::new(RecordingCreateRepo {
        created: Mutex::new(Vec::new()),
    });
    let mut h = sized(
        FULL_TABLET,
        Some(Arc::clone(&repo) as SharedSessionsRepository),
    );
    let connection = expect_connection_controller();
    let mut info = connection
        .server_info
        .get_untracked()
        .expect("the mock connect published server info");
    info.available_backends = backends;
    connection.server_info.set(Some(info));
    h.pump_until_values("the seeded card", |values| {
        values.iter().any(|v| v == "alpha")
    });
    (h, repo)
}

fn create_named(h: &mut Harness, dialog: &Chrome, name: &str) {
    h.tap(center(dialog.content_child()));
    h.type_text(name);
    h.press(NamedKey::Enter);
    h.flush();
    h.pump_until_values("the created card", |values| {
        values.iter().any(|v| v == name)
    });
}

#[test]
fn two_offered_backends_show_the_choice_and_herdr_reaches_the_repository() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, repo) = offering(vec![ServerBackend::Zellij, ServerBackend::Herdr]);

    let dialog = open_dialog_by_value(&mut h, "new session");
    assert!(
        has_label(&h, "zellij") && has_label(&h, "herdr"),
        "the segmented control is mounted"
    );

    let (pill_origin, pill_size) = *dialog.rounded.last().expect("the selected segment's pill");
    let y = node_centers_by_label(&h.root, "herdr")[0].y;
    h.tap(Point::new(pill_origin.x + pill_size.width + 20.0, y));
    h.settle();
    create_named(&mut h, &dialog, "on-herdr");

    let created = repo.created.lock().unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].name, "on-herdr");
    assert_eq!(created[0].backend, SessionBackend::Herdr);
}

#[test]
fn two_offered_backends_default_to_zellij() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, repo) = offering(vec![ServerBackend::Herdr, ServerBackend::Zellij]);

    let dialog = open_dialog_by_value(&mut h, "new session");
    create_named(&mut h, &dialog, "untouched");

    assert_eq!(
        repo.created.lock().unwrap()[0].backend,
        SessionBackend::Zellij
    );
}

#[test]
fn a_lone_backend_mounts_no_choice_and_creates_on_it() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, repo) = offering(vec![ServerBackend::Zellij]);

    let dialog = open_dialog_by_value(&mut h, "new session");
    assert!(!has_label(&h, "herdr"), "no segmented control");
    create_named(&mut h, &dialog, "plain");

    assert_eq!(
        repo.created.lock().unwrap()[0].backend,
        SessionBackend::Zellij
    );
}

#[test]
fn a_blank_name_creates_nothing_with_or_without_the_choice() {
    let _g = serial();
    let _ambient = setup();
    for backends in [
        vec![ServerBackend::Zellij],
        vec![ServerBackend::Zellij, ServerBackend::Herdr],
    ] {
        let (mut h, repo) = offering(backends);
        let dialog = open_dialog_by_value(&mut h, "new session");
        h.tap(center(dialog.content_child()));
        h.type_text("   ");
        h.press(NamedKey::Enter);
        h.tap_label("Create");
        h.flush();
        assert_eq!(h.depth(), 2, "a blank name leaves the dialog open");
        assert!(repo.created.lock().unwrap().is_empty());
    }
}

#[test]
fn the_seeded_herdr_summary_is_the_rollup_of_the_terminal_mocks_default_agents() {
    use muxr::features::terminal::data::repositories::mock_terminal_repository::default_agents_for_preview;
    let rollup = AgentSummary::from_statuses(
        default_agents_for_preview()
            .iter()
            .map(|agent| (agent.status, agent.kind.as_str())),
    );
    let sessions = MockSessionsRepository::seeded().snapshot();
    let herdr = sessions
        .iter()
        .find(|session| session.id == SEEDED_HERDR_ID)
        .expect("herdr");
    assert_eq!(herdr.agents.as_ref(), Some(&rollup));
}

struct FastPollPreferences;

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl PreferencesRepository for FastPollPreferences {
    async fn load_preferences(&self) -> Result<Preferences, AppFailure> {
        let mut prefs = Preferences::defaults();
        prefs.poll.layout_refresh_secs = 1;
        Ok(prefs)
    }

    async fn save_appearance(
        &self,
        _prefs: AppearancePrefs,
    ) -> Result<AppearancePrefs, AppFailure> {
        unreachable!("not exercised by the pause test")
    }

    async fn save_poll_prefs(&self, _prefs: PollPrefs) -> Result<PollPrefs, AppFailure> {
        unreachable!("not exercised by the pause test")
    }
}

#[test]
fn the_poll_pauses_while_the_switcher_stage_is_open_and_resumes_on_close() {
    let _g = serial();
    let _ambient = setup();
    let repo = Arc::new(CountingSessionsRepo::new());
    let mut h = Harness::mount_with(
        Some(Arc::clone(&repo) as SharedSessionsRepository),
        Some(Arc::new(FastPollPreferences)),
        support::surface(),
    );
    expect_connection_controller().connect(mock_target(false));
    h.pump_until("the first load", |_| repo.calls() >= 1);

    let switcher = expect_servers_controller();
    switcher.open();
    h.frame();
    let mut stable = 0;
    let mut last = repo.calls();
    while stable < 5 {
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
        let now = repo.calls();
        stable = if now == last { stable + 1 } else { 0 };
        last = now;
    }
    let covered_at = last;
    let until = Instant::now() + Duration::from_millis(2600);
    while Instant::now() < until {
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    assert!(
        repo.calls() == covered_at,
        "no ListSessions while the stage is open: {} -> {}",
        covered_at,
        repo.calls()
    );
    let held = repo.calls();

    switcher.close();
    h.pump_until("the poll to resume", |_| repo.calls() > held);
}

const TABLET_FAB_INSET: f64 = 24.0;
const COMPACT_FAB_INSET: f64 = 16.0;
const TABLET_CARD_SCALE: f64 = 0.58;
const PHONE_CARD_SCALE: f64 = 0.62;
const FULL_SIDE_WIDTH: f64 = 300.0;
const COLLAPSED_SIDE_WIDTH: f64 = 240.0;
const SERVERS_FAB: &str = "Servers";
const SAVED_HOST: &str = "saved.example";
const SAVED_ENDPOINT: &str = "saved.example:50051";

struct Shell {
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    surface: Size,
}

impl Shell {
    fn mount(size: Size) -> Shell {
        static SPENT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        SPENT.get_or_init(|| {
            let mut throwaway = MuxrApp.init();
            let _ = MuxrApp.build(&mut throwaway);
        });
        provide_context(support::metrics_at(size.width, size.height));
        let mut shell = Shell {
            root: RenderRoot::new(),
            logic: Box::new(|state: &mut AppState| AnyView::new(MuxrApp.build(state))),
            state: MuxrApp.init(),
            tcx: TextContext::new(),
            surface: size,
        };
        shell.settle();
        expect_connection_controller().connect(mock_target(false));
        app_nav().push("/sessions");
        shell.settle();
        shell.pump_until("the seeded sessions", |s| s.shows_seeded());
        shell.settle();
        shell
    }

    fn shows_seeded(&self) -> bool {
        let values = node_values(&self.root);
        SEEDED.iter().all(|name| values.iter().any(|v| v == name))
    }

    fn settle(&mut self) -> RecScene {
        settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            self.surface,
        )
    }

    fn pump_until(&mut self, what: &str, done: impl Fn(&Shell) -> bool) {
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
            self.settle();
        }
    }

    fn bounds(&self, label: &str) -> frust::accesskit::Rect {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
            .and_then(|(_, node)| node.bounds())
            .unwrap_or_else(|| panic!("no bounds for {label:?}"))
    }

    fn open_stage(&mut self) -> RecScene {
        expect_servers_controller().open();
        let mut scene = RecScene::default();
        for step in 1..=240 {
            scene = frame_at_sized(
                &mut self.root,
                &mut self.logic,
                &mut self.state,
                &mut self.tcx,
                self.surface,
                support::ms(support::SETTLE_MS + f64::from(step) * 16.0),
            );
        }
        scene
    }
}

fn assert_fab_corner(shell: &Shell, size: Size, inset: f64) {
    let fab = shell.bounds(SERVERS_FAB);
    assert!(
        (size.width - fab.x1 - inset).abs() < EPSILON,
        "right inset {} != {inset}: {fab:?}",
        size.width - fab.x1
    );
    assert!(
        (size.height - fab.y1 - inset).abs() < EPSILON,
        "bottom inset {} != {inset}: {fab:?}",
        size.height - fab.y1
    );
}

#[test]
fn the_tablet_fab_sits_in_the_detail_panes_corner_at_24px() {
    let _g = serial();
    let _ambient = setup();
    let shell = Shell::mount(FULL_TABLET);
    assert_fab_corner(&shell, FULL_TABLET, TABLET_FAB_INSET);
    assert!(
        shell.bounds(SERVERS_FAB).x0 > FULL_SIDE_WIDTH,
        "right of the list column, over the detail pane"
    );
}

#[test]
fn the_collapsed_tablet_fab_sits_in_the_same_corner_clear_of_the_list_column() {
    let _g = serial();
    let _ambient = setup();
    let shell = Shell::mount(COLLAPSED_TABLET);
    assert_fab_corner(&shell, COLLAPSED_TABLET, TABLET_FAB_INSET);
    assert!(shell.bounds(SERVERS_FAB).x0 > COLLAPSED_SIDE_WIDTH);
}

#[test]
fn the_phone_fab_keeps_the_compact_16px_corner() {
    let _g = serial();
    let _ambient = setup();
    let shell = Shell::mount(PHONE);
    assert_fab_corner(&shell, PHONE, COMPACT_FAB_INSET);
}

fn scales(scene: &RecScene) -> Vec<f64> {
    scene
        .transforms
        .iter()
        .map(|t| t.as_coeffs())
        .filter(|c| (c[0] - c[3]).abs() < 1e-9)
        .map(|c| c[0])
        .collect()
}

fn paints_scale(scene: &RecScene, wanted: f64) -> bool {
    scales(scene).iter().any(|s| (s - wanted).abs() < 0.005)
}

fn open_card_scales(size: Size) -> (bool, bool) {
    let mut shell = Shell::mount(size);
    let closed = shell.settle();
    assert!(
        !paints_scale(&closed, TABLET_CARD_SCALE) && !paints_scale(&closed, PHONE_CARD_SCALE),
        "the closed page paints at full size: {:?}",
        scales(&closed)
    );
    let open = shell.open_stage();
    (
        paints_scale(&open, TABLET_CARD_SCALE),
        paints_scale(&open, PHONE_CARD_SCALE),
    )
}

#[test]
fn opening_the_switcher_on_a_full_tablet_scales_the_whole_page_to_0_58() {
    let _g = serial();
    let _ambient = setup();
    assert_eq!(open_card_scales(FULL_TABLET), (true, false));
}

#[test]
fn opening_the_switcher_on_a_collapsed_tablet_scales_the_whole_page_to_0_58() {
    let _g = serial();
    let _ambient = setup();
    assert_eq!(open_card_scales(COLLAPSED_TABLET), (true, false));
}

#[test]
fn opening_the_switcher_on_a_phone_keeps_the_0_62_scale() {
    let _g = serial();
    let _ambient = setup();
    assert_eq!(open_card_scales(PHONE), (false, true));
}

fn select_a_peer_at(size: Size) {
    let mut shell = Shell::mount(size);
    mock_entitlement()
        .expect("tests/support opted this process into the mock")
        .set_entitlement(Entitlement::Pro);
    let pro = expect_pro_controller();
    let refreshing = Arc::clone(&pro);
    spawn_local(async move { refreshing.refresh().await });
    shell.pump_until("pro", |_| pro.is_pro_untracked());
    expect_saved_servers()
        .save(SavedServer {
            host: SAVED_HOST.to_string(),
            port: 50051,
            trust_mode: TrustMode::System,
            label: "Saved rig".to_string(),
            last_used_ms: epoch_millis(),
            read_only: false,
            has_credential: true,
            token_name: None,
            fingerprint: None,
        })
        .expect("the mock store never fails");
    let mut keyring = Keyring::new();
    keyring.insert(
        SAVED_ENDPOINT,
        zeroize::Zeroizing::new("s3cr3t".to_string()),
    );
    expect_keyring().publish(keyring);
    shell.open_stage();

    let switcher = expect_servers_controller();
    let peer = peer_endpoints(&switcher.faces.get_untracked())
        .iter()
        .position(|e| e == SAVED_ENDPOINT)
        .expect("the saved row is a peer card");
    let gate = SwitchGate::standard(&shell.state.overlays, &pro, &app_nav());
    switcher.select(peer, &gate);
    shell.pump_until("the switch to land", |_| {
        !switcher.open.get_untracked()
            && expect_connection_controller()
                .server_info
                .get_untracked()
                .is_some_and(|info| info.endpoint == SAVED_ENDPOINT)
    });
    shell.pump_until("the new server's list", |s| s.shows_seeded());
    assert_fab_corner(&shell, size, TABLET_FAB_INSET);
    assert!(
        node_labels(&shell.root)
            .iter()
            .any(|l| l == "Select a session"),
        "{size:?}: the master-detail is up with nothing selected"
    );
}

#[test]
fn selecting_a_peer_on_a_full_tablet_lands_on_the_new_servers_master_detail() {
    let _g = serial();
    let _ambient = setup();
    select_a_peer_at(FULL_TABLET);
}

#[test]
fn selecting_a_peer_on_a_collapsed_tablet_lands_on_the_new_servers_master_detail() {
    let _g = serial();
    let _ambient = setup();
    select_a_peer_at(COLLAPSED_TABLET);
}

#[test]
fn the_poll_stays_paused_through_the_close_tail_and_resumes_after_it() {
    let _g = serial();
    let _ambient = setup();
    let repo = Arc::new(CountingSessionsRepo::new());
    let mut h = Harness::mount_with(
        Some(Arc::clone(&repo) as SharedSessionsRepository),
        Some(Arc::new(FastPollPreferences)),
        support::surface(),
    );
    expect_connection_controller().connect(mock_target(false));
    h.pump_until("the first load", |_| repo.calls() >= 1);

    let switcher = expect_servers_controller();
    switcher.open();
    h.frame();
    switcher.close();
    let clock = StageClock::closing();
    h.frame();
    let mut stable = 0;
    let mut last = repo.calls();
    while stable < 5 {
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
        let now = repo.calls();
        stable = if now == last { stable + 1 } else { 0 };
        last = now;
    }
    let held = last;
    let until = Instant::now() + Duration::from_millis(2600);
    while Instant::now() < until {
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    assert_eq!(repo.calls(), held, "no ListSessions through the close tail");

    clock.advance(PEER_TAIL);
    h.pump_until("the poll to resume after the tail", |_| repo.calls() > held);
}

struct ScriptedRepo {
    list_failure: Mutex<Option<AppFailure>>,
    kill_failure: Mutex<Option<AppFailure>>,
    list_calls: AtomicU32,
}

impl ScriptedRepo {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            list_failure: Mutex::new(None),
            kill_failure: Mutex::new(None),
            list_calls: AtomicU32::new(0),
        })
    }

    fn fail_list(&self, failure: Option<AppFailure>) {
        *self.list_failure.lock().expect("ok") = failure;
    }

    fn list_calls(&self) -> u32 {
        self.list_calls.load(Ordering::SeqCst)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for ScriptedRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        match self.list_failure.lock().expect("ok").clone() {
            Some(failure) => Err(failure),
            None => Ok(vec![stub_session("a")]),
        }
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the banner tests")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the banner tests")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        match self.kill_failure.lock().expect("ok").clone() {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }
}

fn counting_controller(repo: &Arc<ScriptedRepo>) -> (SessionsController, Arc<AtomicU32>) {
    let controller = SessionsController::new(Arc::clone(repo) as SharedSessionsRepository);
    let emitted = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&emitted);
    controller
        .core()
        .failures()
        .subscribe(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        })
        .forget();
    (controller, emitted)
}

#[tokio::test]
async fn background_refreshes_raise_no_failure_for_connection_class_errors() {
    let repo = ScriptedRepo::new();
    let (controller, emitted) = counting_controller(&repo);
    controller.refresh_background().await;
    for failure in [
        AppFailure::Network("not connected to a server".to_string()),
        AppFailure::Auth("expired".to_string()),
    ] {
        repo.fail_list(Some(failure));
        controller.refresh_background().await;
        controller.refresh_from_poll().await;
    }
    assert_eq!(emitted.load(Ordering::SeqCst), 0, "no toast-bound failure");

    repo.fail_list(Some(AppFailure::Internal("boom".to_string())));
    controller.refresh_background().await;
    assert_eq!(emitted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_user_action_failure_still_reaches_the_sink() {
    let repo = ScriptedRepo::new();
    *repo.kill_failure.lock().expect("ok") = Some(AppFailure::Network("dropped".to_string()));
    let (controller, emitted) = counting_controller(&repo);
    controller.kill("a".to_string()).await;
    assert_eq!(
        emitted.load(Ordering::SeqCst),
        1,
        "the kill's failure shows"
    );
}

fn network(message: &str) -> Option<AppFailure> {
    Some(AppFailure::Network(message.to_string()))
}

#[tokio::test]
async fn a_network_streak_raises_trouble_then_persistence_then_recovery_once_each() {
    let repo = ScriptedRepo::new();
    let (controller, emitted) = counting_controller(&repo);
    controller.refresh_background().await;
    repo.fail_list(network("down"));

    controller.refresh_from_poll().await;
    assert_eq!(
        controller.take_trouble(),
        None,
        "one failure is not a streak"
    );
    controller.refresh_from_poll().await;
    assert_eq!(controller.take_trouble(), Some(ConnectionTrouble::Started));
    assert_eq!(controller.take_trouble(), None, "taken once");
    controller.refresh_from_poll().await;
    assert_eq!(controller.take_trouble(), None);
    controller.refresh_from_poll().await;
    assert_eq!(
        controller.take_trouble(),
        Some(ConnectionTrouble::Persistent)
    );
    controller.refresh_from_poll().await;
    assert_eq!(
        controller.take_trouble(),
        None,
        "never again within the streak"
    );

    repo.fail_list(None);
    controller.refresh_from_poll().await;
    assert_eq!(
        controller.take_trouble(),
        Some(ConnectionTrouble::Recovered)
    );
    controller.refresh_from_poll().await;
    assert_eq!(controller.take_trouble(), None);

    repo.fail_list(network("blip"));
    controller.refresh_from_poll().await;
    repo.fail_list(None);
    controller.refresh_from_poll().await;
    assert_eq!(controller.take_trouble(), None);
    assert_eq!(emitted.load(Ordering::SeqCst), 0, "no toast-bound failure");
}

#[tokio::test]
async fn internal_and_auth_failures_never_count_and_internal_still_reaches_the_sink() {
    let repo = ScriptedRepo::new();
    let (controller, emitted) = counting_controller(&repo);
    controller.refresh_background().await;

    repo.fail_list(Some(AppFailure::Internal("boom".to_string())));
    for _ in 0..4 {
        controller.refresh_from_poll().await;
    }
    assert_eq!(
        controller.take_trouble(),
        None,
        "Internal is not connection trouble"
    );
    assert_eq!(
        emitted.load(Ordering::SeqCst),
        4,
        "each still emits, as before"
    );

    repo.fail_list(Some(AppFailure::Auth("expired".to_string())));
    for _ in 0..4 {
        controller.refresh_from_poll().await;
    }
    assert_eq!(
        controller.take_trouble(),
        None,
        "Auth is the renewal's, not the streak's"
    );
    assert_eq!(emitted.load(Ordering::SeqCst), 4, "and raises no toast");

    repo.fail_list(network("down"));
    controller.refresh_from_poll().await;
    repo.fail_list(Some(AppFailure::Internal("boom".to_string())));
    controller.refresh_from_poll().await;
    repo.fail_list(network("down"));
    controller.refresh_from_poll().await;
    assert_eq!(controller.take_trouble(), Some(ConnectionTrouble::Started));
}

fn poll_fast() -> Option<SharedPreferencesRepository> {
    Some(Arc::new(FastPollPreferences))
}

#[test]
fn a_network_streak_reports_trouble_under_the_banner_and_never_tears_the_connection_down() {
    use muxr::features::servers::presentation::controller::ReconnectBanner;

    let _g = serial();
    let _ambient = setup();
    let repo = ScriptedRepo::new();
    let mut h = Harness::mount_with(
        Some(Arc::clone(&repo) as SharedSessionsRepository),
        poll_fast(),
        support::surface(),
    );
    assert!(!expect_keyring().is_unlocked());
    assert!(expect_keyring().token_for("mock.example:7777").is_none());
    expect_connection_controller().connect(mock_target(false));
    h.pump_until("the first load", |_| repo.list_calls() >= 1);
    let toasts = muxr::ui::toasts::app_toasts();
    toasts.clear();
    let connection = expect_connection_controller();
    let dials = connection.dial_generation();
    let rebuilds = trouble_reconnects_for_tests();
    set_banner_connecting_delay_for_tests(50);
    set_banner_timings_for_tests(60, 100);

    repo.fail_list(network("deadline exceeded"));
    let banner = || expect_servers_controller().banner.get_untracked();
    h.pump_until("the trouble's Connecting banner", |_| {
        matches!(banner(), Some(ReconnectBanner::Connecting { .. }))
    });
    assert_eq!(
        banner(),
        Some(ReconnectBanner::Connecting {
            host: "mock.example".to_string()
        })
    );
    assert!(
        repo.list_calls() >= 3,
        "the first load plus two failed ticks"
    );
    assert!(
        trouble_reconnects_for_tests() > rebuilds,
        "the channel rebuild was asked for"
    );
    assert_eq!(connection.dial_generation(), dials, "no teardown, no dial");
    assert!(connection.connected.get_untracked(), "still connected");

    h.pump_until("the persistent trouble's Failed banner", |_| {
        matches!(banner(), Some(ReconnectBanner::Failed { .. }))
    });
    let until = Instant::now() + Duration::from_millis(400);
    while Instant::now() < until {
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
        assert!(matches!(banner(), Some(ReconnectBanner::Failed { .. })));
    }
    assert_eq!(connection.dial_generation(), dials, "still no dial");
    assert!(connection.connected.get_untracked());

    repo.fail_list(None);
    h.pump_until("the recovery's Connected flash", |_| {
        banner() == Some(ReconnectBanner::Connected)
    });
    h.pump_until("the banner to clear", |_| banner().is_none());
    assert_eq!(connection.dial_generation(), dials);
    set_banner_connecting_delay_for_tests(BANNER_CONNECTING_DELAY_MS);
    set_banner_timings_for_tests(1_500, 6_000);
    assert!(
        toasts.pending_untracked().is_empty(),
        "no toast: {:?}",
        toasts.pending_untracked()
    );
}

#[test]
fn a_disconnected_screen_polls_nothing_and_toasts_nothing() {
    let _g = serial();
    let _ambient = setup();
    let repo = ScriptedRepo::new();
    let mut h = Harness::mount_with(
        Some(Arc::clone(&repo) as SharedSessionsRepository),
        poll_fast(),
        support::surface(),
    );
    h.pump_until("the first load", |_| repo.list_calls() >= 1);
    let after_first = repo.list_calls();
    repo.fail_list(Some(AppFailure::Network(
        "not connected to a server".to_string(),
    )));
    let toasts = muxr::ui::toasts::app_toasts();
    toasts.clear();
    assert!(!expect_connection_controller().connected.get_untracked());

    let until = Instant::now() + Duration::from_millis(2600);
    while Instant::now() < until {
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    assert_eq!(repo.list_calls(), after_first, "no poll RPC while offline");
    assert!(toasts.pending_untracked().is_empty());
}
