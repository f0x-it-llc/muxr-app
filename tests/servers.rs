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
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use frust::{AnyView, Component, GetUntracked, spawn_local};
use frust_core::{InputEvent, PointerPhase, RenderRoot};
use frust_reactive::{ReactiveRuntime, TrackedScope};
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{
    ConnectTarget, TrustMode, expect_connection_controller, fail_next_mock_dials_for_tests,
};
use muxr::features::connect::di::{expect_keyring, expect_saved_servers, mock_credentials};
use muxr::features::connect::domain::entities::{SavedServer, epoch_millis};
use muxr::features::connect::domain::keyring::Keyring;
use muxr::features::pro::di::{expect_pro_controller, mock_entitlement};
use muxr::features::pro::domain::entities::Entitlement;
use muxr::features::servers::di::{expect_servers_controller, use_last_seen};
use muxr::features::servers::domain::entities::FaceKind;
use muxr::features::servers::presentation::controller::{
    ServersSwitcherController, peer_endpoints,
};
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
/// The unverified-trust confirm's title and its two actions, transcribed.
const UNVERIFIED_TITLE: &str = "Connect without verification?";
const UNVERIFIED_CONFIRM: &str = "Connect anyway";
const CANCEL: &str = "Cancel";
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

/// Strip index of `endpoint` (live page 0, then the peers).
fn strip_index(endpoint: &str) -> usize {
    1 + peer_endpoints(&switcher().faces.get_untracked())
        .iter()
        .position(|peer| peer == endpoint)
        .unwrap_or_else(|| panic!("{endpoint} is not a peer card"))
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
        "A is live, then the peers newest first"
    );
    let before = epoch();

    h.tap_card(strip_index(B_ENDPOINT), 0);
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

    h.tap_card(strip_index(B_ENDPOINT), 0);
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

    h.tap_card(b, 0);
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

    // Retry: B is the centred card now, so the second tap lands on the middle of the stage.
    let faces = switcher().faces.get_untracked();
    let b_now = faces
        .iter()
        .position(|face| face.endpoint == B_ENDPOINT)
        .expect("B still has a face")
        + 1;
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

    h.tap_card(strip_index(B_ENDPOINT), 0);
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
fn an_unverified_peer_asks_first_and_declining_dials_nothing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = open_pro_stage(TrustMode::NoVerification, true);
    let before = epoch();

    h.tap_card(strip_index(B_ENDPOINT), 0);
    h.pump_until(|h| h.overlay_depth() > REST_DEPTH);
    h.settle();
    h.park();

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the confirm is above the stage"
    );
    assert!(
        h.values().iter().any(|v| v == UNVERIFIED_TITLE),
        "the confirm names itself: {:?}",
        h.values()
    );
    assert!(h.labels().iter().any(|l| l == UNVERIFIED_CONFIRM));
    assert!(stage_is_open());
    assert_eq!(epoch(), before, "nothing dialed while it is up");

    let cancel = node_centers_by_label(&h.root, CANCEL)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no Cancel: {:?}", h.labels()));
    h.tap_parked(cancel);
    h.settle();
    h.park();
    h.settle();
    h.park();

    assert_eq!(h.overlay_depth(), REST_DEPTH, "the confirm closed");
    assert_eq!(connected_endpoint().as_deref(), Some(A_ENDPOINT));
    assert_eq!(epoch(), before, "declining dialed nothing");
    assert!(switcher().busy.get_untracked().is_none());
    assert!(stage_is_open(), "the stage is still up");
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

    h.tap_card(strip_index(B_ENDPOINT), 0);
    settle_on(&mut h, B_ENDPOINT);
    let last_seen = use_last_seen().expect("the app provides the last-seen handle");
    h.pump_until(|_| last_seen.snapshot().contains_key(B_ENDPOINT));

    h.open_stage();
    h.tap_card(strip_index(A_ENDPOINT), 0);
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
