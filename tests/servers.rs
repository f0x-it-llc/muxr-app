//! Whole-app coverage of the server switcher over the mocks and three saved servers, driven by pointer.
//! Peer cards have no accessibility nodes: faces are read from the controller and taps land at computed rects.

use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use frust::{AnyView, Component, GetUntracked, spawn_local};
use frust_core::{InputEvent, PointerPhase, RenderRoot};
use frust_reactive::{Owner, ReactiveRuntime, TrackedScope};
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{
    ConnectTarget, HeldDial, TrustMode, expect_connection_controller,
    fail_next_mock_dials_for_tests, hold_next_mock_dials_for_tests,
};
use muxr::features::connect::di::{expect_keyring, expect_saved_servers, mock_credentials};
use muxr::features::connect::domain::entities::{SavedServer, epoch_millis};
use muxr::features::connect::domain::keyring::Keyring;
use muxr::features::connect::presentation::pages::ADD_SERVER_HEADING;
use muxr::features::pro::di::{expect_pro_controller, mock_entitlement};
use muxr::features::pro::domain::entities::Entitlement;
use muxr::features::servers::di::{
    BANNER_CONNECTING_DELAY_MS, PEER_TAIL, expect_servers_controller,
    installed_session_renewer_for_tests, keep_peers, set_banner_connecting_delay_for_tests,
    set_banner_timings_for_tests, set_stage_clock_for_tests, stage_settled, use_last_seen,
};
use muxr::features::servers::domain::entities::FaceKind;
use muxr::features::servers::presentation::controller::{
    ReconnectBanner, ServersSwitcherController, SwitchGate, peer_endpoints,
    set_retry_backoff_for_tests,
};
use muxr::features::sessions::presentation::controllers::snapshots_built_for_tests;
use muxr::routes::app_nav;
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};
use zeroize::Zeroizing;

mod support;
use support::{
    RecScene, frame_sized, ms, node_centers_by_label, node_centers_by_value, node_labels,
    node_values, on_screen_centre, pointer, serial, settle_sized, setup, strip_card_rect,
};

const WINDOW: Size = Size::new(400.0, 800.0);
const CARD_SCALE: f64 = 0.62;
const REST_DEPTH: usize = 1;
/// One 60Hz frame of the parking loop, and its cap (ten seconds of frames).
const FRAME_MS: f64 = 16.0;
const PARK_FRAME_CAP: usize = 600;

const A_HOST: &str = "mock.example";
const A_ENDPOINT: &str = "mock.example:7777";
const B_HOST: &str = "bravo.example";
const B_ENDPOINT: &str = "bravo.example:50051";
const C_HOST: &str = "charlie.example";
const C_ENDPOINT: &str = "charlie.example:50051";
const TOKEN: &str = "t0ken-s3cr3t";
/// The retired unverified-trust confirm's title, kept to assert it never shows.
const UNVERIFIED_TITLE: &str = "Connect without verification?";
const SESSION_NAME: &str = "dev";

fn spend_cold_start() {
    static SPENT: OnceLock<()> = OnceLock::new();
    SPENT.get_or_init(|| {
        let mut throwaway = MuxrApp.init();
        let _ = MuxrApp.build(&mut throwaway);
    });
}

type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

struct Harness {
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    clock: f64,
    scope: TrackedScope,
}

impl Harness {
    fn mount() -> Self {
        spend_cold_start();
        support::reprovide_metrics(support::metrics_at(WINDOW.width, WINDOW.height));
        let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
        root.set_theme(Box::new(frust_glyph::baseline()));
        let mut harness = Harness {
            root,
            logic: Box::new(|state: &mut AppState| AnyView::new(MuxrApp.build(state))),
            state: MuxrApp.init(),
            tcx: TextContext::new(),
            clock: support::SETTLE_MS,
            scope: TrackedScope::new(),
        };
        support::reprovide_metrics(support::metrics_at(WINDOW.width, WINDOW.height));
        harness.frame();
        harness
    }

    fn frame(&mut self) -> RecScene {
        frame_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            WINDOW,
        )
    }

    fn settle(&mut self) -> RecScene {
        settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            WINDOW,
        )
    }

    /// One frame the way the desktop loop paints one: report whether another is wanted.
    fn requested_frame(&mut self) -> (RecScene, bool) {
        self.clock += FRAME_MS;
        let (root, logic, state, scope) = (
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &self.scope,
        );
        scope.track(|| root.rebuild(logic, state));
        let tcx: &mut dyn Any = &mut self.tcx;
        self.root.layout_with_text(WINDOW, tcx);
        let mut scene = RecScene::default();
        let outcome = self.root.paint(&mut scene, ms(self.clock));
        let converging = outcome.needs_frame && !outcome.needs_frame_paced_only;
        (scene, converging || self.scope.is_dirty())
    }

    /// Run frames for exactly as long as the app asks for them.
    fn park(&mut self) -> RecScene {
        for _ in 0..PARK_FRAME_CAP {
            let (scene, more) = self.requested_frame();
            if !more {
                return scene;
            }
        }
        panic!("the app never stopped asking for frames ({PARK_FRAME_CAP} of them)");
    }

    fn dispatch(&mut self, event: &InputEvent) {
        if self.root.event(&mut self.state, event).needs_redraw {
            self.park();
        }
    }

    fn tap_parked(&mut self, point: Point) {
        self.dispatch(&pointer(PointerPhase::Down, point));
        self.dispatch(&pointer(PointerPhase::Up, point));
    }

    fn labels(&self) -> Vec<String> {
        node_labels(&self.root)
    }

    fn values(&self) -> Vec<String> {
        node_values(&self.root)
    }

    fn depth(&self) -> usize {
        self.state.nav.controller().depth()
    }

    fn overlay_depth(&self) -> usize {
        self.state.overlays.depth()
    }

    /// Pump the UI-thread queue and frames until `done` (which may read the harness), failing
    /// after five seconds.
    fn pump_until(&mut self, mut done: impl FnMut(&Harness) -> bool) {
        let runtime = ReactiveRuntime::get().expect("setup() installs the reactive runtime");
        let deadline = Instant::now() + Duration::from_secs(5);
        self.frame();
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the condition"
            );
            runtime.pump_local();
            std::thread::sleep(Duration::from_millis(1));
            self.frame();
        }
    }

    fn open_stage(&mut self) {
        switcher().open();
        self.park();
    }

    fn tap_card(&mut self, card: usize, centred: usize) {
        let rect = strip_card_rect(WINDOW, CARD_SCALE, card, centred);
        let point = on_screen_centre(rect, WINDOW)
            .unwrap_or_else(|| panic!("strip card {card} is wholly off screen: {rect:?}"));
        self.tap_parked(point);
    }
}

fn switcher() -> Arc<ServersSwitcherController> {
    expect_servers_controller()
}

fn stage_is_open() -> bool {
    switcher().open.get_untracked()
}

fn connected_endpoint() -> Option<String> {
    expect_connection_controller()
        .server_info
        .get_untracked()
        .map(|info| info.endpoint)
}

fn epoch() -> u64 {
    expect_connection_controller()
        .connection_epoch
        .get_untracked()
}

fn row(
    host: &str,
    port: u16,
    age_h: i64,
    trust_mode: TrustMode,
    credentialed: bool,
) -> SavedServer {
    SavedServer {
        host: host.to_string(),
        port,
        trust_mode,
        label: host.to_string(),
        last_used_ms: epoch_millis() - age_h * 3_600_000,
        read_only: false,
        has_credential: credentialed,
        token_name: None,
        fingerprint: None,
    }
}

fn mock_target() -> ConnectTarget {
    ConnectTarget {
        host: A_HOST.to_string(),
        port: 7777,
        trust_mode: TrustMode::System,
        label: "Mock rig".to_string(),
        offer_credential: false,
        token: Zeroizing::new(TOKEN.to_string()),
        remember: false,
        fingerprint: None,
    }
}

fn set_entitlement(h: &mut Harness, entitlement: Entitlement) {
    mock_entitlement()
        .expect("tests/support opted this process into the mock")
        .set_entitlement(entitlement);
    let pro = expect_pro_controller();
    let refreshing = Arc::clone(&pro);
    spawn_local(async move { refreshing.refresh().await });
    h.pump_until(|_| pro.is_pro_untracked() == (entitlement == Entitlement::Pro));
}

/// A keyring holding a token for each of `endpoints`, as an unlock would publish.
fn keyring_of(endpoints: &[&str]) -> Keyring {
    let mut keyring = Keyring::new();
    for endpoint in endpoints {
        keyring.insert(*endpoint, Zeroizing::new(TOKEN.to_string()));
    }
    keyring
}

fn three_servers(b_trust: TrustMode, unlocked: bool) -> Harness {
    fail_next_mock_dials_for_tests(0);
    let mut h = Harness::mount();
    let saved = expect_saved_servers();
    for seeded in saved.list().expect("the mock store never fails") {
        saved
            .remove(&seeded.endpoint())
            .expect("the mock store never fails");
    }
    for server in [
        row(A_HOST, 7777, 0, TrustMode::System, true),
        row(B_HOST, 50051, 1, b_trust, true),
        row(C_HOST, 50051, 2, TrustMode::System, true),
    ] {
        saved.save(server).expect("the mock store never fails");
    }
    mock_credentials()
        .expect("tests/support opted this process into the mock")
        .seed_vault(&[
            (A_ENDPOINT, TOKEN),
            (B_ENDPOINT, TOKEN),
            (C_ENDPOINT, TOKEN),
        ]);
    if unlocked {
        expect_keyring().publish(keyring_of(&[A_ENDPOINT, B_ENDPOINT, C_ENDPOINT]));
    }
    expect_connection_controller().connect(mock_target());
    app_nav().push("/sessions");
    h.settle();
    h.park();
    h
}

fn open_pro_stage(b_trust: TrustMode, unlocked: bool) -> Harness {
    let mut h = three_servers(b_trust, unlocked);
    set_entitlement(&mut h, Entitlement::Pro);
    h.open_stage();
    h
}

fn strip_index(endpoint: &str) -> usize {
    switcher()
        .faces
        .get_untracked()
        .iter()
        .position(|face| face.endpoint == endpoint)
        .unwrap_or_else(|| panic!("{endpoint} has no card"))
}

fn live_strip_index() -> usize {
    switcher()
        .faces
        .get_untracked()
        .iter()
        .position(|face| face.kind == FaceKind::Live)
        .expect("a live card")
}

fn face_kind(endpoint: &str) -> FaceKind {
    switcher()
        .faces
        .get_untracked()
        .into_iter()
        .find(|face| face.endpoint == endpoint)
        .unwrap_or_else(|| panic!("no face for {endpoint}"))
        .kind
}

/// Wait for the switch to `endpoint` to settle: connected there, stage down.
fn settle_on(h: &mut Harness, endpoint: &str) {
    h.pump_until(|_| connected_endpoint().as_deref() == Some(endpoint) && !stage_is_open());
    h.settle();
    h.park();
}

#[test]
fn a_tap_on_a_peer_cards_rect_switches_the_server_and_the_stage_comes_down_on_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    assert_eq!(
        peer_endpoints(&switcher().faces.get_untracked()),
        [B_ENDPOINT, C_ENDPOINT],
        "A is live, then the peers in the strip order"
    );
    let before = epoch();

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(&mut h, B_ENDPOINT);

    assert!(epoch() > before, "the connection epoch advanced");
    assert!(!stage_is_open());
    assert!(switcher().expanding.get_untracked().is_none());
    assert!(switcher().busy.get_untracked().is_none());
    assert_eq!(h.depth(), 2, "no navigation: still on the placed shell");
    assert!(app_nav().on_sessions());
    assert!(
        h.labels().iter().any(|l| l == "Servers"),
        "the new server's Sessions is up with its FAB: {:?}",
        h.labels()
    );
}

#[test]
fn the_inner_page_is_inert_while_the_stage_is_open() {
    let _g = serial();
    let _ambient = setup();
    let mut h = three_servers(TrustMode::System, true);
    h.pump_until(|h| !node_centers_by_value(&h.root, SESSION_NAME).is_empty());
    h.park();
    let row_at = node_centers_by_value(&h.root, SESSION_NAME)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no `{SESSION_NAME}` session on screen: {:?}", h.values()));
    h.open_stage();
    let (before, depth, overlays) = (epoch(), h.depth(), h.overlay_depth());

    let stage_centre = Point::new(WINDOW.width / 2.0, WINDOW.height / 2.0);
    let on_card = stage_centre + (row_at - stage_centre) * CARD_SCALE;
    h.tap_parked(on_card);
    h.settle();
    h.park();

    assert_eq!(h.depth(), depth, "no terminal was pushed");
    assert!(app_nav().on_sessions(), "still on Sessions");
    assert_eq!(h.overlay_depth(), overlays, "no menu or sheet opened");
    assert_eq!(epoch(), before, "no reconnect");
    assert_eq!(connected_endpoint().as_deref(), Some(A_ENDPOINT));
    assert!(
        !stage_is_open(),
        "the stage's own rule: a tap on the centred live card closes it, and nothing more"
    );
}

#[test]
fn a_locked_keyring_costs_one_prompt_then_switches() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, false);
    assert!(
        !expect_keyring().is_unlocked(),
        "connected without unlocking"
    );
    assert!(matches!(face_kind(B_ENDPOINT), FaceKind::Locked));
    let credentials = mock_credentials().expect("mock credential service");
    assert!(credentials.prompts().is_empty(), "nothing prompted yet");

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(&mut h, B_ENDPOINT);

    assert_eq!(
        credentials.prompts().len(),
        1,
        "exactly one prompt: {:?}",
        credentials.prompts()
    );
    assert!(
        expect_keyring().is_unlocked(),
        "the one prompt unlocked the vault"
    );
    assert!(!stage_is_open());
}

#[test]
fn a_failed_dial_marks_the_face_keeps_the_stage_and_a_second_tap_retries() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    let b = strip_index(B_ENDPOINT);
    fail_next_mock_dials_for_tests(1);

    h.tap_card(b, live_strip_index());
    h.pump_until(|_| matches!(face_kind(B_ENDPOINT), FaceKind::Failed { .. }));
    h.park();

    match face_kind(B_ENDPOINT) {
        FaceKind::Failed { message } => {
            assert_eq!(message, "Connection problem — please try again.");
        }
        other => panic!("B should read Failed, got {other:?}"),
    }
    assert!(stage_is_open(), "the stage stays up on a failure");
    assert!(switcher().busy.get_untracked().is_none());
    assert!(switcher().expanding.get_untracked().is_none());

    let b_now = strip_index(B_ENDPOINT) + 1;
    h.tap_card(b_now, b);
    settle_on(&mut h, B_ENDPOINT);

    assert!(
        !stage_is_open(),
        "the retry switched and took the stage down"
    );
    assert!(
        switcher().failures.get_untracked().is_empty(),
        "the failure mark cleared"
    );
}

#[test]
fn a_free_install_with_saved_rows_meets_the_paywall_and_dials_nothing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = three_servers(TrustMode::System, true);
    set_entitlement(&mut h, Entitlement::Free);
    h.open_stage();
    let before = epoch();

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|h| h.overlay_depth() > REST_DEPTH);
    h.settle();
    h.park();

    assert_eq!(h.overlay_depth(), REST_DEPTH + 1, "the paywall is up");
    assert_eq!(connected_endpoint().as_deref(), Some(A_ENDPOINT));
    assert_eq!(epoch(), before, "nothing was dialed");
    assert!(switcher().busy.get_untracked().is_none());
    assert!(switcher().expanding.get_untracked().is_none());
    assert!(stage_is_open(), "the stage waits under the paywall");
}

#[test]
fn a_saved_unverified_peer_dials_with_no_confirm() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::NoVerification, true);

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(&mut h, B_ENDPOINT);

    assert_eq!(h.overlay_depth(), REST_DEPTH, "no confirm was raised");
    assert!(
        !h.values().iter().any(|v| v == UNVERIFIED_TITLE),
        "no confirm names itself: {:?}",
        h.values()
    );
    assert_eq!(connected_endpoint().as_deref(), Some(B_ENDPOINT));
    assert!(switcher().busy.get_untracked().is_none());
}

#[test]
fn the_strip_keeps_its_order_after_a_switch_and_the_live_card_keeps_its_slot() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    let order = || {
        switcher()
            .faces
            .get_untracked()
            .into_iter()
            .map(|f| f.endpoint)
            .collect::<Vec<_>>()
    };
    let before = order();
    assert_eq!(before, [A_ENDPOINT, B_ENDPOINT, C_ENDPOINT]);

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(&mut h, B_ENDPOINT);
    h.open_stage();

    assert_eq!(order(), before, "connecting B did not reorder the cards");
    assert_eq!(face_kind(B_ENDPOINT), FaceKind::Live);
    assert_eq!(live_strip_index(), 1, "B's live card sits where B was");
    assert_eq!(
        peer_endpoints(&switcher().faces.get_untracked()),
        [A_ENDPOINT, C_ENDPOINT],
        "the peers are the others in strip order"
    );
}

fn strip_a_of_its_credential() {
    expect_saved_servers()
        .save(row(A_HOST, 7777, 0, TrustMode::System, false))
        .expect("the mock store never fails");
}

fn saved_has(endpoint: &str) -> bool {
    expect_saved_servers()
        .list()
        .expect("the mock store never fails")
        .iter()
        .any(|saved| saved.endpoint() == endpoint)
}

fn strip_has(endpoint: &str) -> bool {
    switcher()
        .faces
        .get_untracked()
        .iter()
        .any(|face| face.endpoint == endpoint)
}

#[test]
fn switching_away_from_a_credentialless_server_forgets_its_row_and_card() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    strip_a_of_its_credential();

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(&mut h, B_ENDPOINT);
    h.open_stage();

    assert!(!saved_has(A_ENDPOINT), "A's row is gone from the list");
    assert!(!strip_has(A_ENDPOINT), "and from the strip");
    assert!(saved_has(B_ENDPOINT) && saved_has(C_ENDPOINT));
}

#[test]
fn switching_away_from_a_credentialed_server_keeps_its_row() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(&mut h, B_ENDPOINT);
    h.open_stage();

    assert!(saved_has(A_ENDPOINT));
    assert!(strip_has(A_ENDPOINT));
}

#[test]
fn a_failed_switch_away_from_a_credentialless_server_keeps_its_row() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    strip_a_of_its_credential();
    fail_next_mock_dials_for_tests(1);

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| matches!(face_kind(B_ENDPOINT), FaceKind::Failed { .. }));
    h.park();

    assert!(saved_has(A_ENDPOINT), "the failed switch forgot nothing");
    assert!(strip_has(A_ENDPOINT));
}

#[test]
fn a_visited_server_reads_cached_with_its_last_seen_sessions_after_switching_back() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    assert!(
        matches!(face_kind(B_ENDPOINT), FaceKind::Never),
        "B has never been seen"
    );

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(&mut h, B_ENDPOINT);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    h.pump_until(|_| last_seen.snapshot().contains_key(B_ENDPOINT));

    h.open_stage();
    h.tap_card(strip_index(A_ENDPOINT), live_strip_index());
    settle_on(&mut h, A_ENDPOINT);
    h.open_stage();

    match face_kind(B_ENDPOINT) {
        FaceKind::Cached { sessions, .. } => {
            let names: Vec<_> = sessions.iter().map(|s| s.name.as_str()).collect();
            assert!(
                names.contains(&SESSION_NAME),
                "B's cached face lists what B's Sessions showed: {names:?}"
            );
        }
        other => panic!("B should read Cached, got {other:?}"),
    }
}

fn visit_b_and_return_to_a(h: &mut Harness) {
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(h, B_ENDPOINT);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    h.pump_until(|_| last_seen.snapshot().contains_key(B_ENDPOINT));
    h.open_stage();
    h.tap_card(strip_index(A_ENDPOINT), live_strip_index());
    settle_on(h, A_ENDPOINT);
}

fn close_stage(h: &mut Harness) {
    switcher().close_with(|_| {});
    h.park();
}

#[test]
fn a_cached_peer_card_renders_the_servers_last_sessions_page_through_the_snapshot_widget() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);

    let before = snapshots_built_for_tests();
    h.open_stage();
    let with_snapshot = h.park().glyph_runs;
    assert!(
        snapshots_built_for_tests() > before,
        "opening the stage built B's snapshot card"
    );
    match face_kind(B_ENDPOINT) {
        FaceKind::Cached {
            page: Some(page), ..
        } => {
            assert!(page.sessions.iter().any(|s| s.name == SESSION_NAME));
            assert_eq!(
                page.info.as_ref().map(|info| info.endpoint.as_str()),
                Some(B_ENDPOINT),
                "the header facts are B's"
            );
        }
        other => panic!("B should read Cached with its page, got {other:?}"),
    }

    close_stage(&mut h);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    last_seen.record_at(B_ENDPOINT, Vec::new(), 1);
    h.open_stage();
    let without_snapshot = h.park().glyph_runs;
    assert!(
        with_snapshot > without_snapshot + 5,
        "the snapshot card paints the real screen: {with_snapshot} vs {without_snapshot}"
    );
}

fn pin_stage_clock() -> Instant {
    let at = Instant::now();
    set_stage_clock_for_tests(Some(at));
    at
}

#[test]
fn the_stage_is_settled_only_once_closed_and_past_its_tail() {
    let at = pin_stage_clock();
    assert!(stage_settled(), "a stage never opened is at rest");
    keep_peers(true);
    assert!(!stage_settled(), "open (or expanding) is not at rest");
    keep_peers(false);
    assert!(!stage_settled(), "the close tail is not at rest");
    set_stage_clock_for_tests(Some(at + PEER_TAIL - Duration::from_millis(1)));
    assert!(!stage_settled());
    set_stage_clock_for_tests(Some(at + PEER_TAIL));
    assert!(stage_settled(), "closed and past the tail");
    keep_peers(true);
    assert!(!stage_settled(), "reopening cancels the rest");
    set_stage_clock_for_tests(None);
}

fn tap_without_settling(h: &mut Harness, card: usize) {
    let rect = strip_card_rect(WINDOW, CARD_SCALE, card, live_strip_index());
    let point = on_screen_centre(rect, WINDOW).expect("card on screen");
    h.dispatch(&pointer(PointerPhase::Down, point));
    h.dispatch(&pointer(PointerPhase::Up, point));
}

fn peer_count() -> usize {
    switcher()
        .faces
        .get_untracked()
        .iter()
        .filter(|face| face.kind != FaceKind::Live)
        .count()
}

#[test]
fn a_select_in_flight_keeps_the_peer_snapshots_and_the_peers() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);
    h.open_stage();
    h.park();
    let peers = peer_count();
    let before = snapshots_built_for_tests();
    tap_without_settling(&mut h, strip_index(B_ENDPOINT));
    h.park();
    assert!(
        switcher().expanding.get_untracked().is_some(),
        "select in flight"
    );
    assert!(
        snapshots_built_for_tests() > before,
        "the growing card stays the cached page, never the light face"
    );
    assert_eq!(peer_count(), peers);
}

#[test]
fn the_repro_sequence_keeps_snapshot_peers_through_the_handshake_and_the_tail() {
    let _g = serial();
    let _ambient = setup();
    let t0 = pin_stage_clock();
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);
    h.open_stage();
    h.park();
    tap_without_settling(&mut h, strip_index(B_ENDPOINT));
    let before = snapshots_built_for_tests();
    settle_on(&mut h, B_ENDPOINT);
    set_stage_clock_for_tests(Some(t0 + PEER_TAIL / 2));
    switcher().refresh_faces();
    h.park();
    assert!(snapshots_built_for_tests() > before);
    set_stage_clock_for_tests(None);
}

#[test]
fn a_stage_closed_past_its_tail_builds_no_peer_snapshots() {
    let _g = serial();
    let _ambient = setup();
    let t0 = pin_stage_clock();
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);
    h.park();

    set_stage_clock_for_tests(Some(t0 + PEER_TAIL + Duration::from_millis(100)));
    let before = snapshots_built_for_tests();
    switcher().refresh_faces();
    h.park();
    h.frame();
    assert_eq!(
        snapshots_built_for_tests(),
        before,
        "the first build after the tail builds no peer face"
    );
    set_stage_clock_for_tests(None);
}

#[test]
fn closing_the_stage_keeps_building_the_peers_through_the_close_tail() {
    let _g = serial();
    let _ambient = setup();
    let t0 = pin_stage_clock();
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);

    h.open_stage();
    let live_open = live_strip_index();
    let opened = snapshots_built_for_tests();
    let peers_open = peer_count();
    close_stage(&mut h);
    assert!(
        snapshots_built_for_tests() > opened,
        "the close tail keeps the snapshot faces"
    );
    assert_eq!(peer_count(), peers_open, "the peers are still present");
    assert_eq!(
        live_strip_index(),
        live_open,
        "the strip keeps its live slot"
    );

    set_stage_clock_for_tests(Some(t0 + PEER_TAIL / 2));
    let mid = snapshots_built_for_tests();
    switcher().refresh_faces();
    h.park();
    assert!(snapshots_built_for_tests() > mid, "still inside the tail");
    h.open_stage();
    assert!(stage_is_open());
    assert!(matches!(
        face_kind(B_ENDPOINT),
        FaceKind::Cached { page: Some(_), .. }
    ));
    close_stage(&mut h);

    set_stage_clock_for_tests(Some(t0 + PEER_TAIL * 3));
    let late = snapshots_built_for_tests();
    switcher().refresh_faces();
    h.park();
    assert_eq!(snapshots_built_for_tests(), late);
    set_stage_clock_for_tests(None);
}

#[test]
fn a_tap_on_a_snapshot_card_selects_the_server_and_never_opens_a_session() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);
    h.open_stage();
    let depth = h.depth();

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(&mut h, B_ENDPOINT);

    assert_eq!(h.depth(), depth, "no terminal route was pushed");
    assert_eq!(connected_endpoint().as_deref(), Some(B_ENDPOINT));
}

#[test]
fn a_keyring_lock_drops_every_snapshot() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    assert!(last_seen.snapshot().contains_key(B_ENDPOINT));

    switcher().lock_with(|_| {});
    h.park();

    assert!(
        last_seen.snapshot().is_empty(),
        "the lock cleared the cache"
    );
}

#[test]
fn forgetting_a_row_drops_its_snapshot() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    assert!(last_seen.snapshot().contains_key(B_ENDPOINT));

    expect_saved_servers()
        .remove(B_ENDPOINT)
        .expect("the mock store never fails");
    h.open_stage();
    h.park();

    assert!(!strip_has(B_ENDPOINT));
    assert!(
        !last_seen.snapshot().contains_key(B_ENDPOINT),
        "B's remembered page went with its row"
    );
}

fn tap_b_with_a_held_dial(h: &mut Harness) -> HeldDial {
    let held = hold_next_mock_dials_for_tests(1).remove(0);
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| switcher().busy.get_untracked().is_some());
    held
}

#[test]
fn a_dial_superseded_by_a_disconnect_does_not_leave_the_switcher_busy_for_good() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    let held = tap_b_with_a_held_dial(&mut h);

    switcher().close();
    h.park();
    expect_connection_controller().disconnect();
    expect_connection_controller().connect(mock_target());
    held.release(true);
    h.pump_until(|_| connected_endpoint().as_deref() == Some(A_ENDPOINT));
    h.open_stage();
    assert_eq!(connected_endpoint().as_deref(), Some(A_ENDPOINT));

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(&mut h, B_ENDPOINT);

    assert!(!stage_is_open(), "the tap on a peer switched");
    assert!(switcher().busy.get_untracked().is_none());
    hold_next_mock_dials_for_tests(0);
}

#[test]
fn a_late_settle_does_not_take_down_a_stage_reopened_meanwhile() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    let held = tap_b_with_a_held_dial(&mut h);

    switcher().close();
    h.park();
    h.open_stage();
    held.release(true);
    h.pump_until(|_| {
        switcher().busy.get_untracked().is_none()
            && connected_endpoint().as_deref() == Some(B_ENDPOINT)
    });

    assert!(
        stage_is_open(),
        "the reopened stage is not the dial's to close"
    );
    hold_next_mock_dials_for_tests(0);
}

#[test]
fn closing_the_stage_after_a_failed_switch_lands_on_connect() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    fail_next_mock_dials_for_tests(1);
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| matches!(face_kind(B_ENDPOINT), FaceKind::Failed { .. }));
    h.park();
    assert!(
        connected_endpoint().is_none(),
        "the failed switch tore A down"
    );
    assert_eq!(h.depth(), 2);

    switcher().close();
    h.settle();
    h.park();

    assert!(!stage_is_open());
    assert_eq!(h.depth(), 1, "landing on Connect, not a dead Sessions");
}

const ADD_CHIP: &str = "add server";
const NOT_NOW: &str = "Not now";
const RESTORE: &str = "Restore purchases";

/// A free install with saved rows, the stage open and the add chip tapped: the paywall is up.
/// Also returns the route depth before the tap.
fn free_stage_with_the_paywall_raised() -> (Harness, usize) {
    let mut h = three_servers(TrustMode::System, true);
    set_entitlement(&mut h, Entitlement::Free);
    h.open_stage();
    let depth = h.depth();
    let add = node_centers_by_label(&h.root, ADD_CHIP)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no add chip: {:?}", h.labels()));
    h.tap_parked(add);
    h.pump_until(|h| h.overlay_depth() > REST_DEPTH);
    h.settle();
    h.park();
    (h, depth)
}

#[test]
fn the_add_chip_on_a_free_install_raises_the_paywall_in_place_and_dismissing_it_returns_to_the_stage()
 {
    let _g = serial();
    let _ambient = setup();
    let (mut h, depth) = free_stage_with_the_paywall_raised();

    assert_eq!(h.overlay_depth(), REST_DEPTH + 1, "the paywall is up");
    assert_eq!(h.depth(), depth, "no add page was pushed");
    assert!(stage_is_open(), "the stage waits under the paywall");

    let not_now = node_centers_by_label(&h.root, NOT_NOW)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no Not now: {:?}", h.labels()));
    h.tap_parked(not_now);
    h.settle();
    h.park();
    h.settle();
    h.park();

    assert_eq!(h.overlay_depth(), REST_DEPTH, "the paywall closed");
    assert_eq!(h.depth(), depth, "still on Sessions");
    assert!(stage_is_open(), "dismissing returns to the open stage");
}

#[test]
fn unlocking_pro_from_the_add_chips_paywall_closes_the_stage_and_opens_the_add_page() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, depth) = free_stage_with_the_paywall_raised();
    mock_entitlement()
        .expect("tests/support opted this process into the mock")
        .set_entitlement(Entitlement::Pro);

    let restore = node_centers_by_label(&h.root, RESTORE)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no Restore: {:?}", h.labels()));
    h.tap_parked(restore);
    let pro = expect_pro_controller();
    h.pump_until(|_| pro.is_pro_untracked());
    for _ in 0..4 {
        h.settle();
        h.park();
    }

    assert!(!stage_is_open(), "the unlocked add took the stage down");
    assert_eq!(h.depth(), depth + 1, "the add page is pushed");
    assert!(
        h.values().iter().any(|v| v == ADD_SERVER_HEADING),
        "the add form is up: {:?}",
        h.values()
    );
}

#[test]
fn a_settle_close_keeps_its_peer_tail_and_the_faces_name_the_new_live_server() {
    let _g = serial();
    let _ambient = setup();
    let _t0 = pin_stage_clock();
    let mut h = open_pro_stage(TrustMode::System, true);
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(&mut h, B_ENDPOINT);
    assert_eq!(
        face_kind(B_ENDPOINT),
        FaceKind::Live,
        "settle refreshed the faces: B is live"
    );
    assert_ne!(face_kind(A_ENDPOINT), FaceKind::Live);
    assert!(keep_peers(false), "a settle-close keeps the tail");
    h.open_stage();
    h.park();
    close_stage(&mut h);
    assert!(keep_peers(false), "a plain close keeps the tail");
    set_stage_clock_for_tests(None);
}

/// A session name only the cache knows, so the cached rows are told from the live ones.
const CACHED_ONLY: &str = "cachedonly";
/// The strip's disconnected line: it must never paint on a cached switch.
const NOT_CONNECTED: &str = "not connected";

/// Rewrite `endpoint`'s filed page so its first session reads [`CACHED_ONLY`].
fn taint_cached_page(endpoint: &str) {
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    let mut seen = last_seen.snapshot().remove(endpoint).expect("filed");
    let mut page = (**seen.page.as_ref().expect("a page was filed")).clone();
    page.sessions[0].name = CACHED_ONLY.to_string();
    seen.page = Some(Arc::new(page));
    last_seen.record_seen(endpoint, seen);
}

fn cached_b() -> Harness {
    set_banner_timings_for_tests(60, 60);
    set_banner_connecting_delay_for_tests(60);
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);
    taint_cached_page(B_ENDPOINT);
    h
}

fn reset_reconnect_rig() {
    hold_next_mock_dials_for_tests(0);
    fail_next_mock_dials_for_tests(0);
    set_banner_timings_for_tests(1_500, 6_000);
    set_banner_connecting_delay_for_tests(BANNER_CONNECTING_DELAY_MS);
    set_retry_backoff_for_tests(None);
}

/// Pump frames for `ms` of wall time, running `each` after every frame.
fn pump_for(h: &mut Harness, ms: u64, mut each: impl FnMut()) {
    let runtime = ReactiveRuntime::get().expect("setup() installs the reactive runtime");
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < end {
        runtime.pump_local();
        h.frame();
        each();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn a_dial_that_lands_before_the_delay_shows_no_banner_in_any_frame() {
    let _g = serial();
    let _ambient = setup();
    let mut h = cached_b();
    set_banner_connecting_delay_for_tests(250);
    h.open_stage();
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| {
        assert_eq!(banner(), None, "never Connecting, never Connected");
        connected_endpoint().as_deref() == Some(B_ENDPOINT)
    });
    pump_for(&mut h, 500, || {
        assert_eq!(banner(), None, "never Connecting, never Connected");
    });
    reset_reconnect_rig();
}

#[test]
fn a_dial_pending_past_the_delay_shows_connecting_then_flashes_connected() {
    let _g = serial();
    let _ambient = setup();
    let mut h = cached_b();
    set_banner_connecting_delay_for_tests(150);
    h.open_stage();
    let held = hold_next_mock_dials_for_tests(1).remove(0);
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| switcher().seed.get_untracked().is_some());
    assert_eq!(banner(), None, "not before the delay");
    h.pump_until(|_| banner() == connecting_to(B_HOST));
    held.release(true);
    h.pump_until(|_| connected_endpoint().as_deref() == Some(B_ENDPOINT));
    assert_eq!(banner(), Some(ReconnectBanner::Connected));
    h.pump_until(|_| banner().is_none());
    reset_reconnect_rig();
}

#[test]
fn a_failure_before_the_delay_still_shows_the_failure_banner() {
    let _g = serial();
    let _ambient = setup();
    let mut h = cached_b();
    set_banner_connecting_delay_for_tests(2_000);
    set_banner_timings_for_tests(60, 600);
    h.open_stage();
    fail_next_mock_dials_for_tests(1);
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| matches!(banner(), Some(ReconnectBanner::Failed { .. })));
    assert_eq!(
        banner(),
        Some(ReconnectBanner::Failed {
            host: B_HOST.to_string()
        })
    );
    reset_reconnect_rig();
}

#[test]
fn a_second_switch_during_the_delay_leaves_no_stale_banner() {
    let _g = serial();
    let _ambient = setup();
    let mut h = cached_b();
    switch_via_controller(&mut h, C_ENDPOINT);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    h.pump_until(|_| last_seen.snapshot().contains_key(C_ENDPOINT));
    switch_via_controller(&mut h, A_ENDPOINT);
    h.pump_until(|_| banner().is_none());

    set_banner_connecting_delay_for_tests(250);
    let mut held = hold_next_mock_dials_for_tests(2);
    let second = held.remove(1);
    let first = held.remove(0);
    h.open_stage();
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| switcher().seed.get_untracked().is_some() && !stage_is_open());
    pump_for(&mut h, 100, || {
        assert_eq!(banner(), None, "still inside B's delay")
    });
    h.open_stage();
    let gate = SwitchGate {
        paywall: Rc::new(|_| {}),
        add_server: Rc::new(|_| {}),
    };
    switcher().select_endpoint(C_ENDPOINT.to_string(), &gate);
    // Only C's own delay may bring a banner, and it names C, never B.
    let mut seen = Vec::new();
    pump_for(&mut h, 600, || {
        assert_ne!(banner(), connecting_to(B_HOST), "no stale banner");
        if seen.last() != Some(&banner()) {
            seen.push(banner());
        }
    });
    assert_eq!(
        banner(),
        connecting_to(C_HOST),
        "{seen:?} {} {:?}",
        stage_is_open(),
        switcher().seed.get_untracked().map(|s| s.host.clone())
    );
    first.release(true);
    second.release(true);
    h.pump_until(|_| connected_endpoint().as_deref() == Some(C_ENDPOINT));
    reset_reconnect_rig();
}

fn banner() -> Option<ReconnectBanner> {
    switcher().banner.get_untracked()
}

fn connecting_to(host: &str) -> Option<ReconnectBanner> {
    Some(ReconnectBanner::Connecting {
        host: host.to_string(),
    })
}

/// The page is on screen with its rows and its header: never empty, never "not connected".
fn assert_page_shown(h: &Harness, rows: &[&str]) {
    let values = h.values();
    assert!(!values.iter().any(|v| v == NOT_CONNECTED), "{values:?}");
    assert!(
        rows.iter().any(|row| values.iter().any(|v| v == row)),
        "no row of {rows:?} on screen: {values:?}"
    );
}

fn assert_title(h: &mut Harness, host: &str) {
    h.park();
    assert!(h.labels().iter().any(|v| v == host), "{:?}", h.labels());
}

fn assert_banner_text(h: &Harness, text: &str) {
    assert!(h.labels().iter().any(|v| v == text), "{:?}", h.labels());
}

fn switch_via_controller(h: &mut Harness, endpoint: &str) {
    h.open_stage();
    let gate = SwitchGate {
        paywall: Rc::new(|_| {}),
        add_server: Rc::new(|_| {}),
    };
    switcher().select_endpoint(endpoint.to_string(), &gate);
    h.pump_until(|_| connected_endpoint().as_deref() == Some(endpoint) && !stage_is_open());
}

fn tap_cached_b_held(h: &mut Harness) -> HeldDial {
    h.open_stage();
    let held = hold_next_mock_dials_for_tests(1).remove(0);
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| banner() == connecting_to(B_HOST));
    held
}

#[test]
fn a_cached_switch_closes_the_stage_at_once_and_refreshes_in_place_when_the_dial_lands() {
    let _g = serial();
    let _ambient = setup();
    let mut h = cached_b();

    let held = tap_cached_b_held(&mut h);

    // (a) the very next frames: stage down, cached rows and header up, banner
    // Connecting, nothing needing the server works.
    assert!(!stage_is_open(), "the stage closed at once");
    assert_eq!(banner(), connecting_to(B_HOST));
    assert!(
        !expect_connection_controller().connected.get_untracked(),
        "the dial is still pending"
    );
    let depth = h.depth();
    for _ in 0..5 {
        h.frame();
        assert_page_shown(&h, &[CACHED_ONLY]);
    }
    assert_title(&mut h, B_HOST);
    assert_banner_text(&h, "Connecting to bravo.example\u{2026}");
    let row = node_centers_by_value(&h.root, CACHED_ONLY)[0];
    h.tap_parked(row);
    assert_eq!(h.depth(), depth, "attach is off until connected");
    assert_page_shown(&h, &[CACHED_ONLY]);

    // The dial lands: banner Success, then cleared; rows refresh in place with
    // no frame that shows an empty list or the disconnected strip.
    held.release(true);
    h.pump_until(|h| {
        assert_page_shown(h, &[CACHED_ONLY, SESSION_NAME]);
        connected_endpoint().as_deref() == Some(B_ENDPOINT)
    });
    assert_eq!(banner(), Some(ReconnectBanner::Connected));
    h.pump_until(|h| {
        assert_page_shown(h, &[CACHED_ONLY, SESSION_NAME]);
        h.values().iter().any(|v| v == SESSION_NAME) && banner().is_none()
    });
    assert!(!h.values().iter().any(|v| v == CACHED_ONLY), "replaced");
    assert_eq!(face_kind(B_ENDPOINT), FaceKind::Live);
    reset_reconnect_rig();
}

#[test]
fn a_cached_switch_keeps_the_page_continuous_through_the_disconnect_and_dial_window() {
    let _g = serial();
    let _ambient = setup();
    let mut h = cached_b();

    let held = tap_cached_b_held(&mut h);
    // Frame by frame across the pending dial: the torn-down connection never
    // leaks into the page.
    for _ in 0..20 {
        h.frame();
        assert_page_shown(&h, &[CACHED_ONLY]);
    }
    held.release(true);
    h.pump_until(|h| {
        assert_page_shown(h, &[CACHED_ONLY, SESSION_NAME]);
        connected_endpoint().as_deref() == Some(B_ENDPOINT)
    });
    reset_reconnect_rig();
}

#[test]
fn a_failed_background_dial_keeps_the_cached_page_and_marks_the_face_failed() {
    let _g = serial();
    let _ambient = setup();
    let mut h = cached_b();
    set_banner_timings_for_tests(60, 60);
    set_retry_backoff_for_tests(Some(60_000));
    h.open_stage();
    fail_next_mock_dials_for_tests(1);
    app_toasts().clear();
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| matches!(banner(), Some(ReconnectBanner::Failed { .. })));

    assert_eq!(
        banner(),
        Some(ReconnectBanner::Failed {
            host: B_HOST.to_string()
        })
    );
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "the banner and the face report it, never a toast too"
    );
    assert!(!stage_is_open());
    for _ in 0..3 {
        h.frame();
        assert_page_shown(&h, &[CACHED_ONLY]);
    }
    assert_title(&mut h, B_HOST);
    assert_banner_text(&h, "Couldn't reach bravo.example");
    assert!(matches!(face_kind(B_ENDPOINT), FaceKind::Failed { .. }));

    assert_eq!(switcher().pending_reconnect().as_deref(), Some(B_ENDPOINT));
    pump_for(&mut h, 200, || {
        assert!(matches!(banner(), Some(ReconnectBanner::Failed { .. })));
    });
    switch_via_controller(&mut h, B_ENDPOINT);
    h.pump_until(|_| switcher().failures.get_untracked().is_empty());
    assert_eq!(switcher().pending_reconnect(), None);
    reset_reconnect_rig();
}

#[test]
fn a_never_visited_server_keeps_the_stage_held_path() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    let held = hold_next_mock_dials_for_tests(1).remove(0);

    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| switcher().busy.get_untracked().is_some());

    assert!(stage_is_open(), "the stage holds the growing card");
    assert!(switcher().seed.get_untracked().is_none());
    assert_eq!(banner(), None);
    held.release(true);
    settle_on(&mut h, B_ENDPOINT);
    assert_eq!(banner(), None, "no banner on the plain path");
    reset_reconnect_rig();
}

#[test]
fn a_second_switch_during_a_background_dial_supersedes_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = cached_b();
    switch_via_controller(&mut h, C_ENDPOINT);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    h.pump_until(|_| last_seen.snapshot().contains_key(C_ENDPOINT));
    switch_via_controller(&mut h, A_ENDPOINT);
    h.pump_until(|_| banner().is_none());

    let mut held = hold_next_mock_dials_for_tests(2);
    let second = held.remove(1);
    let first = held.remove(0);
    h.open_stage();
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    h.pump_until(|_| banner() == connecting_to(B_HOST));

    h.open_stage();
    h.tap_card(strip_index(C_ENDPOINT), live_strip_index());
    h.pump_until(|_| banner() == connecting_to(C_HOST));
    assert!(!stage_is_open());

    first.release(true);
    for _ in 0..5 {
        h.frame();
    }
    assert_ne!(connected_endpoint().as_deref(), Some(B_ENDPOINT));
    assert_eq!(banner(), connecting_to(C_HOST), "no stale banner");

    second.release(true);
    h.pump_until(|_| connected_endpoint().as_deref() == Some(C_ENDPOINT));
    assert_eq!(banner(), Some(ReconnectBanner::Connected));
    h.pump_until(|_| banner().is_none());
    assert_eq!(face_kind(C_ENDPOINT), FaceKind::Live);
    reset_reconnect_rig();
}

#[test]
fn a_commit_close_keeps_its_peer_tail() {
    let _g = serial();
    let _ambient = setup();
    let _t0 = pin_stage_clock();
    let mut h = cached_b();

    let held = tap_cached_b_held(&mut h);
    h.park();

    assert!(keep_peers(false), "a commit-close keeps its tail of peers");
    held.release(true);
    h.pump_until(|_| connected_endpoint().as_deref() == Some(B_ENDPOINT));
    assert!(keep_peers(false));
    set_stage_clock_for_tests(None);
    reset_reconnect_rig();
}

fn expire_live_session() -> bool {
    let endpoint = connected_endpoint().expect("a live session to expire");
    let renewed = renew(&endpoint);
    if !renewed {
        expect_connection_controller().on_session_expired();
    }
    renewed
}

fn renew(endpoint: &str) -> bool {
    installed_session_renewer_for_tests()
        .expect("provide_controller installs the switcher's renewer")
        .renew(endpoint)
}

fn dial_generation() -> u64 {
    expect_connection_controller().dial_generation()
}

fn listed_on_a(unlocked: bool) -> Harness {
    set_banner_timings_for_tests(60, 600);
    set_banner_connecting_delay_for_tests(60);
    let mut h = three_servers(TrustMode::System, unlocked);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    h.pump_until(|h| {
        last_seen
            .snapshot()
            .get(A_ENDPOINT)
            .is_some_and(|seen| seen.page.is_some())
            && h.values().iter().any(|v| v == SESSION_NAME)
    });
    h.park();
    app_toasts().clear();
    h
}

#[test]
fn a_live_expiry_with_an_unlocked_keyring_renews_in_place_under_the_banner() {
    let _g = serial();
    let _ambient = setup();
    let mut h = listed_on_a(true);
    let (depth, before) = (h.depth(), dial_generation());
    let held = hold_next_mock_dials_for_tests(1).remove(0);

    assert!(expire_live_session(), "the switcher takes the renewal over");

    // While the dial is pending: still on Sessions, the list on screen and inert, the stage
    // untouched, `Connecting` only past the delay.
    assert!(!stage_is_open(), "the stage is never raised");
    assert_eq!(
        switcher()
            .seed
            .get_untracked()
            .map(|seed| seed.endpoint.clone()),
        Some(A_ENDPOINT.to_string()),
        "the page is seeded with the live server's own"
    );
    h.pump_until(|h| {
        assert_page_shown(h, &[SESSION_NAME]);
        banner() == connecting_to(A_HOST)
    });
    for _ in 0..5 {
        h.frame();
        assert_page_shown(&h, &[SESSION_NAME]);
    }
    assert!(app_nav().on_sessions(), "still on Sessions");
    let row = node_centers_by_value(&h.root, SESSION_NAME)[0];
    h.tap_parked(row);
    assert_eq!(h.depth(), depth, "attach is off until the renewal lands");

    // Landing: `Connected` flashes, then clears; the list never empties; nothing toasts.
    held.release(true);
    h.pump_until(|h| {
        assert_page_shown(h, &[SESSION_NAME]);
        connected_endpoint().as_deref() == Some(A_ENDPOINT)
    });
    assert_eq!(banner(), Some(ReconnectBanner::Connected));
    h.pump_until(|h| {
        assert_page_shown(h, &[SESSION_NAME]);
        banner().is_none()
    });
    assert!(!stage_is_open());
    assert_eq!(dial_generation(), before + 1, "one dial");
    assert_eq!(face_kind(A_ENDPOINT), FaceKind::Live);
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "{:?}",
        app_toasts().pending_untracked()
    );
    reset_reconnect_rig();
}

#[test]
fn a_live_expiry_with_the_keyring_locked_falls_back_to_one_auth_toast() {
    let _g = serial();
    let _ambient = setup();
    let mut h = listed_on_a(false);
    assert!(!expect_keyring().is_unlocked());
    let before = dial_generation();

    assert!(!expire_live_session(), "no token to renew with");
    h.settle();

    assert!(!expect_connection_controller().connected.get_untracked());
    assert_eq!(
        app_toasts().pending_untracked().len(),
        1,
        "{:?}",
        app_toasts().pending_untracked()
    );
    assert!(switcher().seed.get_untracked().is_none(), "nothing seeded");
    assert_eq!(banner(), None);
    assert_eq!(
        dial_generation(),
        before + 1,
        "only the logout's teardown, no dial"
    );
    reset_reconnect_rig();
}

/// [`listed_on_a`] (unlocked), then a renewal whose dial fails with a network failure: the
/// `Failed` banner up and A pending. `backoff_ms` pins the retry timer. Also answers the dial
/// generation from before the renewal. `pro` unlocks Pro first, so a switch can follow.
fn failed_renewal_on_a(backoff_ms: u64, pro: bool) -> (Harness, u64) {
    let mut h = listed_on_a(true);
    if pro {
        set_entitlement(&mut h, Entitlement::Pro);
    }
    set_banner_timings_for_tests(60, 60);
    set_retry_backoff_for_tests(Some(backoff_ms));
    let before = dial_generation();
    fail_next_mock_dials_for_tests(1);
    assert!(expire_live_session());
    h.pump_until(|_| matches!(banner(), Some(ReconnectBanner::Failed { .. })));
    assert_eq!(switcher().pending_reconnect().as_deref(), Some(A_ENDPOINT));
    (h, before)
}

#[test]
fn a_failed_renewal_keeps_a_pending_reconnect_that_the_backoff_redials() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, before) = failed_renewal_on_a(400, false);

    assert_eq!(
        banner(),
        Some(ReconnectBanner::Failed {
            host: A_HOST.to_string()
        })
    );
    assert!(matches!(face_kind(A_ENDPOINT), FaceKind::Failed { .. }));
    for _ in 0..3 {
        h.frame();
        assert_page_shown(&h, &[SESSION_NAME]);
    }
    assert!(app_nav().on_sessions(), "still on Sessions");
    assert_banner_text(&h, "Couldn't reach mock.example");

    // Kept past its own 60 ms timeout while pending; nothing redials before the backoff.
    pump_for(&mut h, 200, || {
        assert!(matches!(banner(), Some(ReconnectBanner::Failed { .. })));
    });
    assert_eq!(dial_generation(), before + 1, "only the failed renewal");
    let filed = use_last_seen()
        .and_then(|seen| seen.snapshot().remove(A_ENDPOINT)?.page)
        .expect("A's page was filed");

    h.pump_until(|_| connected_endpoint().as_deref() == Some(A_ENDPOINT));
    assert_eq!(dial_generation(), before + 2, "one retry dial");
    assert_eq!(banner(), Some(ReconnectBanner::Connected));
    h.pump_until(|h| {
        assert_page_shown(h, &[SESSION_NAME]);
        banner().is_none()
    });
    assert_eq!(switcher().pending_reconnect(), None);
    assert_eq!(face_kind(A_ENDPOINT), FaceKind::Live);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    h.pump_until(|_| {
        last_seen
            .snapshot()
            .get(A_ENDPOINT)
            .and_then(|seen| seen.page.clone())
            .is_some_and(|page| !Arc::ptr_eq(&page, &filed))
    });
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "{:?}",
        app_toasts().pending_untracked()
    );
    reset_reconnect_rig();
}

#[test]
fn forgetting_the_pending_reconnects_server_invalidates_its_retry() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, before) = failed_renewal_on_a(300, false);
    let generation = switcher().retry_generation_for_tests();

    expect_saved_servers()
        .remove(A_ENDPOINT)
        .expect("the mock store never fails");
    switcher().refresh_faces();

    assert!(
        switcher().retry_generation_for_tests() > generation,
        "the refresh invalidated the armed backoff itself"
    );
    assert_eq!(
        switcher().pending_reconnect(),
        None,
        "the forget dropped it"
    );
    assert!(
        !matches!(banner(), Some(ReconnectBanner::Failed { .. })),
        "and the banner it held"
    );
    pump_for(&mut h, 500, || {});
    assert_eq!(dial_generation(), before + 1, "only the failed renewal");
    reset_reconnect_rig();
}

#[test]
fn a_retry_armed_under_a_disposed_owner_still_fires() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, before) = failed_renewal_on_a(300, false);

    // With the stage open the retry defers one backoff step: it re-arms the
    // timer from inside a child owner, which is then disposed before it fires.
    h.open_stage();
    let child = Owner::new();
    child.with(|| assert!(!switcher().retry_pending(), "deferred, not dialed"));
    switcher().close();
    child.cleanup();
    drop(child);

    h.pump_until(|_| dial_generation() > before + 1);
    reset_reconnect_rig();
}

#[test]
fn a_banner_timer_armed_under_a_disposed_owner_still_fires() {
    let _g = serial();
    let _ambient = setup();
    let mut h = listed_on_a(true);
    set_banner_connecting_delay_for_tests(100);

    let child = Owner::new();
    child.with(|| switcher().report_trouble(false));
    child.cleanup();
    drop(child);

    assert_eq!(banner(), None, "not before the delay");
    h.pump_until(|_| banner() == connecting_to(A_HOST));
    reset_reconnect_rig();
}

#[test]
fn a_retry_request_redials_the_pending_reconnect_at_once_one_dial_at_a_time() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, _) = failed_renewal_on_a(60_000, false);
    let before = dial_generation();
    let held = hold_next_mock_dials_for_tests(1).remove(0);

    assert!(switcher().retry_pending());
    assert!(!switcher().retry_pending(), "one dial in flight");
    assert_eq!(dial_generation(), before + 1);
    h.pump_until(|_| banner() == connecting_to(A_HOST));
    held.release(true);
    h.pump_until(|_| connected_endpoint().as_deref() == Some(A_ENDPOINT));
    assert_eq!(banner(), Some(ReconnectBanner::Connected));
    h.pump_until(|_| banner().is_none());
    assert_eq!(switcher().pending_reconnect(), None);
    assert!(!switcher().retry_pending(), "nothing pending any more");
    assert_eq!(dial_generation(), before + 1);
    reset_reconnect_rig();
}

#[test]
fn a_user_switch_cancels_the_pending_reconnect() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, _) = failed_renewal_on_a(300, true);

    switch_via_controller(&mut h, C_ENDPOINT);
    assert_eq!(switcher().pending_reconnect(), None);
    assert_eq!(banner(), None, "the switch took the banner down");
    let after = dial_generation();
    pump_for(&mut h, 600, || {});
    assert_eq!(connected_endpoint().as_deref(), Some(C_ENDPOINT));
    assert_eq!(
        dial_generation(),
        after,
        "the cancelled retry never dials A"
    );
    reset_reconnect_rig();
}

#[test]
fn a_keyring_locked_meanwhile_drops_the_pending_reconnect() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, _) = failed_renewal_on_a(200, false);
    let after = dial_generation();

    expect_keyring().lock();
    h.pump_until(|_| switcher().pending_reconnect().is_none());
    assert_eq!(dial_generation(), after, "no token, no dial");
    h.pump_until(|_| banner().is_none());
    assert!(!expect_connection_controller().connected.get_untracked());
    reset_reconnect_rig();
}

#[test]
fn concurrent_expiries_cost_one_renewal_and_a_peer_endpoint_is_never_renewed() {
    let _g = serial();
    let _ambient = setup();
    let mut h = listed_on_a(true);
    assert!(!renew(B_ENDPOINT), "B is not the live server");
    let before = dial_generation();
    let held = hold_next_mock_dials_for_tests(1).remove(0);

    for _ in 0..3 {
        assert!(renew(A_ENDPOINT), "each expiry is the renewal's to report");
    }
    assert!(switcher().reconnect_live(), "a direct request joins it too");
    assert_eq!(dial_generation(), before + 1, "one dial for the burst");

    held.release(true);
    h.pump_until(|_| connected_endpoint().as_deref() == Some(A_ENDPOINT));
    h.pump_until(|_| banner().is_none());
    assert_eq!(dial_generation(), before + 1, "still one dial");
    assert!(app_toasts().pending_untracked().is_empty());
    reset_reconnect_rig();
}
