//! Whole-app coverage of the **server switcher**: the stage raised over the placed shell, driven
//! by the pointer the way a thumb drives it, over the mocks and three saved servers.
//!
//! Only the real `MuxrApp` places the shell and the switcher host, so this suite mounts all of
//! it. `tests/navigation_motion.rs` already proves the host's chrome (FAB, dots, bar chips, back,
//! reduce-motion) and select through the controller seam; `tests/sessions.rs` the tablet card.
//! This suite owns what those do not: a tap on a peer card's on-screen rect, the inert page, the
//! locked-keyring prompt count, the failed-dial face and its retry, the paywall and the
//! unverified-trust confirm gates, and the cached face after a round trip.
//!
//! **Peer cards have no accessibility nodes** (frust 0.6.5), so what a peer face shows is read
//! off the controller's `faces`, and a peer is *tapped* at a rect computed from the strip
//! geometry ([`support::strip_card_rect`]). The surface is a phone (400x800) so the shell paints
//! the 0.62 card scale; the first peer then hangs off the right edge, and the tap lands on the
//! part of it that is on screen.

use std::any::Any;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use frust::{AnyView, Component, GetUntracked, spawn_local};
use frust_core::{InputEvent, PointerPhase, RenderRoot};
use frust_reactive::{ReactiveRuntime, TrackedScope};
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
    PEER_TAIL, expect_servers_controller, keep_peers, set_banner_timings_for_tests,
    set_stage_clock_for_tests, stage_settled, use_last_seen,
};
use muxr::features::servers::domain::entities::FaceKind;
use muxr::features::servers::presentation::controller::{
    ReconnectBanner, ServersSwitcherController, SwitchGate, peer_endpoints,
};
use muxr::features::sessions::presentation::controllers::snapshots_built_for_tests;
use muxr::routes::app_nav;
use muxr::{AppState, MuxrApp};
use zeroize::Zeroizing;

mod support;
use support::{
    RecScene, frame_sized, ms, node_centers_by_label, node_centers_by_value, node_labels,
    node_values, on_screen_centre, pointer, serial, settle_sized, setup, strip_card_rect,
};

/// The phone surface every test lays out at.
const WINDOW: Size = Size::new(400.0, 800.0);
/// The shell's phone card scale, transcribed (`shell.rs`'s private constant).
const CARD_SCALE: f64 = 0.62;
/// The overlay host's depth with nothing open.
const REST_DEPTH: usize = 1;
/// One 60Hz frame of the parking loop, and its cap (ten seconds of frames).
const FRAME_MS: f64 = 16.0;
const PARK_FRAME_CAP: usize = 600;

/// The connected server `A` (the live card) and the two peers.
const A_HOST: &str = "mock.example";
const A_ENDPOINT: &str = "mock.example:7777";
const B_HOST: &str = "bravo.example";
const B_ENDPOINT: &str = "bravo.example:50051";
const C_HOST: &str = "charlie.example";
const C_ENDPOINT: &str = "charlie.example:50051";
const TOKEN: &str = "t0ken-s3cr3t";
/// The retired unverified-trust confirm's title, kept to assert it never shows.
const UNVERIFIED_TITLE: &str = "Connect without verification?";
/// A session name the mock sessions repository always lists.
const SESSION_NAME: &str = "dev";

fn spend_cold_start() {
    static SPENT: OnceLock<()> = OnceLock::new();
    SPENT.get_or_init(|| {
        let mut throwaway = MuxrApp.init();
        let _ = MuxrApp.build(&mut throwaway);
    });
}

type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

/// The whole app on a phone surface.
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

    /// A full tap at `point`, each half through the production loop.
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

    /// Raise the stage the way the FAB does and run the loop to its parking point.
    fn open_stage(&mut self) {
        switcher().open();
        self.park();
    }

    /// Tap strip card `card` (live page = 0, then the peers in `faces` order) while strip card
    /// `centred` is in the middle, at the centre of its on-screen part.
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

/// A saved row at `host:port`, `age_h` hours since last used.
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

/// The three-server fixture: the app connected to A (mock), on `/sessions`, the saved list
/// replaced by A (newest), B, C. `b_trust` is B's posture; every row claims a credential, whose
/// token is in the vault, and in the unlocked keyring when `unlocked`. The stage is closed.
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

/// [`three_servers`] with Pro unlocked and the stage open.
fn open_pro_stage(b_trust: TrustMode, unlocked: bool) -> Harness {
    let mut h = three_servers(b_trust, unlocked);
    set_entitlement(&mut h, Entitlement::Pro);
    h.open_stage();
    h
}

/// Strip index of `endpoint`: its position among the faces (the live page sits at its own slot).
fn strip_index(endpoint: &str) -> usize {
    switcher()
        .faces
        .get_untracked()
        .iter()
        .position(|face| face.endpoint == endpoint)
        .unwrap_or_else(|| panic!("{endpoint} has no card"))
}

/// Strip index of the live card.
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
    // Where a session card sits in the full-size page, before the stage shrinks it.
    h.pump_until(|h| !node_centers_by_value(&h.root, SESSION_NAME).is_empty());
    h.park();
    let row_at = node_centers_by_value(&h.root, SESSION_NAME)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no `{SESSION_NAME}` session on screen: {:?}", h.values()));
    h.open_stage();
    let (before, depth, overlays) = (epoch(), h.depth(), h.overlay_depth());

    // The same card, now inside the shrunken live card.
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

    // Retry: B is the centred card now (no face is live after the failure, so the strip is the
    // empty live page and then every face), so the second tap lands on the middle of the stage.
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

    // The right-hand peer (the card beside the live one).
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

/// Make the live server A a saved row with no stored credential.
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

/// Visit B (its list loaded and filed), then come back to A, leaving the stage closed.
fn visit_b_and_return_to_a(h: &mut Harness) {
    h.tap_card(strip_index(B_ENDPOINT), live_strip_index());
    settle_on(h, B_ENDPOINT);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    h.pump_until(|_| last_seen.snapshot().contains_key(B_ENDPOINT));
    h.open_stage();
    h.tap_card(strip_index(A_ENDPOINT), live_strip_index());
    settle_on(h, A_ENDPOINT);
}

/// Close the open stage and run to the parking point.
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

    // The same card without a page falls back to the dimmed glimpse list; the
    // snapshot paints the whole screen (bar, strip, cards, create row) instead.
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

/// Pin the stage clock, so the close tail is stepped rather than slept through.
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

/// Tap a strip card and stop at the moment the select is in flight (the press
/// ramp before it is still the steady open stage, which may build snapshots).
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
fn a_select_in_flight_builds_no_peer_snapshot_and_keeps_the_peers() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);
    h.open_stage();
    h.park();
    let peers = peer_count();
    tap_without_settling(&mut h, strip_index(B_ENDPOINT));
    assert!(
        switcher().expanding.get_untracked().is_some(),
        "select in flight"
    );
    let before = snapshots_built_for_tests();
    h.park();
    assert_eq!(
        snapshots_built_for_tests(),
        before,
        "no snapshot mid-select"
    );
    assert_eq!(peer_count(), peers);
}

#[test]
fn the_repro_sequence_builds_no_snapshot_during_the_handshake_or_the_tail() {
    let _g = serial();
    let _ambient = setup();
    let t0 = pin_stage_clock();
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);
    // A -> B -> A again with B cached: the switch back is the freezing one.
    h.open_stage();
    h.park();
    tap_without_settling(&mut h, strip_index(B_ENDPOINT));
    let before = snapshots_built_for_tests();
    settle_on(&mut h, B_ENDPOINT);
    set_stage_clock_for_tests(Some(t0 + PEER_TAIL / 2));
    h.park();
    assert_eq!(snapshots_built_for_tests(), before);
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
    assert_eq!(
        snapshots_built_for_tests(),
        opened,
        "the close tail builds light faces, never snapshots"
    );
    assert_eq!(peer_count(), peers_open, "the peers are still present");
    assert_eq!(
        live_strip_index(),
        live_open,
        "the strip keeps its live slot"
    );

    // Within the tail a rebuild still carries them; reopening is a normal open.
    set_stage_clock_for_tests(Some(t0 + PEER_TAIL / 2));
    let mid = snapshots_built_for_tests();
    switcher().refresh_faces();
    h.park();
    assert_eq!(snapshots_built_for_tests(), mid, "still inside the tail");
    h.open_stage();
    assert!(stage_is_open());
    assert!(matches!(
        face_kind(B_ENDPOINT),
        FaceKind::Cached { page: Some(_), .. }
    ));
    close_stage(&mut h);

    // Past the tail the next rebuild drops them.
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

/// Tap B with its dial held open, and wait until the claim shows.
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

/// The stage bar's add chip label and the paywall sheet's two actions, transcribed.
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
fn a_settle_close_builds_no_peers_and_the_faces_name_the_new_live_server() {
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
    // The clock is still at the close instant, deep inside what would be the
    // tail: a settle-close keeps no peers.
    assert!(!keep_peers(false), "no tail after a settle-close");
    // A plain close of the next stage keeps its tail.
    h.open_stage();
    h.park();
    close_stage(&mut h);
    assert!(keep_peers(false), "a plain close keeps the tail");
    set_stage_clock_for_tests(None);
}

// ---------------------------------------------------------------------------
// Background reconnect: a cached target goes live from its cache at once.
// ---------------------------------------------------------------------------

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

/// Visit B, taint its cache, return to A (stage closed): B is a cached target.
fn cached_b() -> Harness {
    set_banner_timings_for_tests(60, 60);
    let mut h = open_pro_stage(TrustMode::System, true);
    visit_b_and_return_to_a(&mut h);
    taint_cached_page(B_ENDPOINT);
    h
}

fn reset_reconnect_rig() {
    hold_next_mock_dials_for_tests(0);
    fail_next_mock_dials_for_tests(0);
    set_banner_timings_for_tests(1_500, 6_000);
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

/// The bar names `host` once its title transition has run.
fn assert_title(h: &mut Harness, host: &str) {
    h.park();
    assert!(h.labels().iter().any(|v| v == host), "{:?}", h.labels());
}

/// The app bar's banner strip reads `text`.
fn assert_banner_text(h: &Harness, text: &str) {
    assert!(h.labels().iter().any(|v| v == text), "{:?}", h.labels());
}

/// Open the stage and select `endpoint` through the controller (a card two slots
/// from the centre is off screen), then wait for the connection to land there.
fn switch_via_controller(h: &mut Harness, endpoint: &str) {
    h.open_stage();
    let gate = SwitchGate {
        paywall: Rc::new(|_| {}),
        add_server: Rc::new(|_| {}),
    };
    switcher().select_endpoint(endpoint.to_string(), &gate);
    h.pump_until(|_| connected_endpoint().as_deref() == Some(endpoint) && !stage_is_open());
}

/// Open the stage and tap the cached B with its dial held open.
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
    assert!(!stage_is_open());
    for _ in 0..3 {
        h.frame();
        assert_page_shown(&h, &[CACHED_ONLY]);
    }
    assert_title(&mut h, B_HOST);
    assert_banner_text(&h, "Couldn't reach bravo.example");
    assert!(matches!(face_kind(B_ENDPOINT), FaceKind::Failed { .. }));

    // The failure banner clears on its own; a tap on the failed face retries.
    h.pump_until(|_| banner().is_none());
    switch_via_controller(&mut h, B_ENDPOINT);
    h.pump_until(|_| switcher().failures.get_untracked().is_empty());
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
    // C visited too, so both targets are cached.
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

    // The superseded dial lands late: nothing follows it.
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
fn a_commit_close_builds_no_peers() {
    let _g = serial();
    let _ambient = setup();
    let _t0 = pin_stage_clock();
    let mut h = cached_b();

    let held = tap_cached_b_held(&mut h);
    h.park();

    assert!(!keep_peers(false), "a commit-close keeps no tail of peers");
    held.release(true);
    h.pump_until(|_| connected_endpoint().as_deref() == Some(B_ENDPOINT));
    assert!(!keep_peers(false));
    set_stage_clock_for_tests(None);
    reset_reconnect_rig();
}
