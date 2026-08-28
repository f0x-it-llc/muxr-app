//! Headless behavior coverage for the `pro` slice (`src/features/pro/**`) — no
//! GPU, no window, and no store account.
//!
//! Three bands. The **free-tier rule** is driven as the pure verdict it is
//! (`domain::use_cases::check_server_limit`), with no controller and no mount.
//! The **controller** band drives `ProController` over the scriptable mock,
//! asserting both the signals it publishes and how many store calls it made —
//! the single-flight guard is only observable in that second number. The last
//! test mounts the whole app, since the launch refresh is `MuxrApp::init`'s to
//! fire, not the controller's.

use std::sync::{Arc, Mutex};

use frust::{AnyView, Component, GetUntracked, Set};
use frust_core::RenderRoot;
use frust_text::TextContext;

use muxr::failure::AppFailure;
use muxr::features::connect::domain::entities::{SavedServer, TrustMode};
use muxr::features::pro::data::repositories::MockEntitlementRepository;
use muxr::features::pro::di::{
    SharedEntitlementRepository, expect_entitlement_repository, expect_pro_controller,
    mock_entitlement,
};
use muxr::features::pro::domain::entities::{Entitlement, PurchaseOutcome, ServerLimitVerdict};
use muxr::features::pro::domain::use_cases::check_server_limit;
use muxr::features::pro::presentation::controller::ProController;
use muxr::{AppState, MuxrApp};

mod support;
use support::{RecScene, pump_until, serial, setup};

const A: &str = "a.example:50051";
const B: &str = "b.example:50051";

/// The app-logic closure `RenderRoot::rebuild` drives.
type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

fn row(endpoint: &str) -> SavedServer {
    let (host, port) = endpoint.rsplit_once(':').expect("host:port");
    SavedServer {
        host: host.to_string(),
        port: port.parse().expect("a port"),
        trust_mode: TrustMode::System,
        label: host.to_string(),
        last_used_ms: 1_700_000_000_000,
        read_only: false,
        has_credential: false,
        token_name: None,
        fingerprint: None,
    }
}

/// The endpoints a verdict asks to forget; empty for an allowed one.
fn forgets(verdict: &ServerLimitVerdict) -> Vec<String> {
    match verdict {
        ServerLimitVerdict::Allowed => Vec::new(),
        ServerLimitVerdict::Blocked { to_forget } => {
            to_forget.iter().map(SavedServer::endpoint).collect()
        }
    }
}

/// A controller over a fresh mock, handed back beside it so a test can script
/// the store and read its call counters.
fn controller() -> (Arc<MockEntitlementRepository>, ProController) {
    let mock = Arc::new(MockEntitlementRepository::free());
    let repository = Arc::clone(&mock) as SharedEntitlementRepository;
    (mock, ProController::new(repository))
}

/// Every failure `controller` raises from here on, in order.
fn failure_log(controller: &ProController) -> Arc<Mutex<Vec<AppFailure>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    controller
        .core()
        .failures()
        .subscribe(move |failure: &AppFailure| {
            sink.lock().expect("the failure log").push(failure.clone());
        })
        .forget();
    seen
}

fn raised(log: &Arc<Mutex<Vec<AppFailure>>>) -> Vec<AppFailure> {
    log.lock().expect("the failure log").clone()
}

fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("a current-thread runtime")
        .block_on(fut)
}

// ---------------------------------------------------------------------------
// The free-tier server rule
// ---------------------------------------------------------------------------

#[test]
fn pro_may_keep_every_server_it_has_saved() {
    let saved = vec![row(A), row(B)];
    assert_eq!(
        check_server_limit(Entitlement::Pro, &saved, "c.example:1"),
        ServerLimitVerdict::Allowed
    );
}

#[test]
fn a_free_install_with_nothing_saved_may_connect() {
    assert_eq!(
        check_server_limit(Entitlement::Free, &[], A),
        ServerLimitVerdict::Allowed
    );
}

#[test]
fn a_free_install_may_reconnect_to_the_one_server_it_remembers() {
    assert_eq!(
        check_server_limit(Entitlement::Free, &[row(A)], A),
        ServerLimitVerdict::Allowed
    );
}

#[test]
fn a_free_install_reaching_for_a_second_server_is_blocked() {
    let verdict = check_server_limit(Entitlement::Free, &[row(A)], B);
    assert_eq!(forgets(&verdict), vec![A.to_string()]);
}

#[test]
fn a_blocked_verdict_never_asks_to_forget_the_server_being_connected_to() {
    let verdict = check_server_limit(Entitlement::Free, &[row(A), row(B)], A);
    assert_eq!(forgets(&verdict), vec![B.to_string()]);
}

#[test]
fn an_unsettled_purchase_is_judged_exactly_as_free() {
    assert_eq!(
        check_server_limit(Entitlement::Pending, &[row(A)], B),
        check_server_limit(Entitlement::Free, &[row(A)], B)
    );
    assert_eq!(
        check_server_limit(Entitlement::Pending, &[row(A)], A),
        ServerLimitVerdict::Allowed
    );
}

// ---------------------------------------------------------------------------
// The controller, over the scriptable store
// ---------------------------------------------------------------------------

#[test]
fn refresh_publishes_what_the_store_answers_in_one_call() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();
    mock.set_entitlement(Entitlement::Pro);

    block_on(controller.refresh());

    assert_eq!(controller.entitlement.get_untracked(), Entitlement::Pro);
    assert!(controller.is_pro_untracked());
    assert_eq!(mock.refresh_calls(), 1);
}

#[test]
fn a_landed_purchase_publishes_pro_and_records_its_outcome() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();

    let outcome = block_on(controller.purchase());

    assert_eq!(outcome, PurchaseOutcome::Purchased);
    assert_eq!(controller.entitlement.get_untracked(), Entitlement::Pro);
    assert_eq!(
        controller.last_outcome.get_untracked(),
        Some(PurchaseOutcome::Purchased)
    );
    assert_eq!(mock.purchase_calls(), 1);
    assert!(!controller.busy.get_untracked(), "the guard is released");
}

#[test]
fn a_cancelled_purchase_leaves_the_install_free_and_raises_nothing() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();
    let log = failure_log(&controller);
    mock.script_purchase(PurchaseOutcome::Cancelled);

    let outcome = block_on(controller.purchase());

    assert_eq!(outcome, PurchaseOutcome::Cancelled);
    assert_eq!(controller.entitlement.get_untracked(), Entitlement::Free);
    assert!(
        raised(&log).is_empty(),
        "a dismissed sheet is not a failure"
    );
}

#[test]
fn an_unsettled_purchase_publishes_pending_rather_than_pro() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();
    mock.script_purchase(PurchaseOutcome::Pending);

    let outcome = block_on(controller.purchase());

    assert_eq!(outcome, PurchaseOutcome::Pending);
    assert_eq!(controller.entitlement.get_untracked(), Entitlement::Pending);
    assert!(!controller.is_pro_untracked(), "pending unlocks nothing");
}

#[test]
fn a_failed_refresh_reports_once_and_leaves_the_entitlement_alone() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();
    mock.set_entitlement(Entitlement::Pro);
    block_on(controller.refresh());
    let log = failure_log(&controller);
    mock.fail_next_refresh(AppFailure::Network("store unreachable".to_string()));

    block_on(controller.refresh());

    assert_eq!(raised(&log).len(), 1);
    assert_eq!(
        controller.entitlement.get_untracked(),
        Entitlement::Pro,
        "an unreachable store must not downgrade a paying install"
    );
}

#[test]
fn restore_reaches_the_store_once_and_publishes_what_it_found() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();
    mock.set_entitlement(Entitlement::Pro);

    block_on(controller.restore());

    assert_eq!(mock.restore_calls(), 1);
    assert_eq!(controller.entitlement.get_untracked(), Entitlement::Pro);
}

#[test]
fn a_purchase_raised_while_one_is_in_flight_never_reaches_the_store() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();

    block_on(controller.purchase());
    // The mock resolves without ever yielding, so an overlapping call is
    // staged by holding the guard the first one would still be holding.
    controller.busy.set(true);
    let refused = block_on(controller.purchase());

    assert_eq!(refused, PurchaseOutcome::Cancelled);
    assert_eq!(mock.purchase_calls(), 1, "the second call was refused");
}

#[test]
fn the_offer_is_published_only_once_looked_up() {
    let _g = serial();
    let _ambient = setup();
    let (_mock, controller) = controller();
    assert_eq!(controller.offer.get_untracked(), None);

    block_on(controller.load_offer());

    let offer = controller.offer.get_untracked().expect("the seeded offer");
    assert_eq!(offer.display_price, "$4.99");
}

#[test]
fn the_controllers_verdict_reads_its_own_live_entitlement() {
    let _g = serial();
    let _ambient = setup();
    let (_mock, controller) = controller();
    let saved = vec![row(A)];

    assert_eq!(
        forgets(&controller.verdict_for(&saved, B)),
        vec![A.to_string()]
    );

    block_on(controller.purchase());

    assert_eq!(
        controller.verdict_for(&saved, B),
        ServerLimitVerdict::Allowed,
        "buying Pro lifts the limit without the caller re-reading anything"
    );
}

// ---------------------------------------------------------------------------
// The composition root
// ---------------------------------------------------------------------------

#[test]
fn app_init_publishes_the_pro_seam_and_fires_one_launch_refresh() {
    let _g = serial();
    let _ambient = setup();
    let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
    root.set_theme(Box::new(frust_glyph::baseline()));
    let mut logic: Logic = Box::new(|state: &mut AppState| MuxrApp.build(state));
    let mut state = MuxrApp.init();
    let mut tcx = TextContext::new();

    let controller = expect_pro_controller();
    let mock = mock_entitlement().expect("the test seam publishes the scriptable store");
    assert_eq!(expect_entitlement_repository().cached(), Entitlement::Free);
    assert_eq!(controller.entitlement.get_untracked(), Entitlement::Free);

    pump_until(
        &mut root,
        &mut logic,
        &mut state,
        &mut tcx,
        RecScene::default(),
        || mock.refresh_calls() >= 1,
    );

    assert_eq!(mock.refresh_calls(), 1, "the launch refresh runs once");
    assert_eq!(controller.entitlement.get_untracked(), Entitlement::Free);
}
