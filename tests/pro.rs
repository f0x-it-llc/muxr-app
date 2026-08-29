//! Headless behavior coverage for the `pro` slice (`src/features/pro/**`) — no
//! GPU, no window, and no store account.
//!
//! Three bands. The **free-tier rule** is driven as the pure verdict it is
//! (`domain::use_cases::check_server_limit`), with no controller and no mount.
//! The **controller** band drives `ProController` over the scriptable mock,
//! asserting both the signals it publishes and how many store calls it made —
//! the single-flight guard is only observable in that second number. The
//! **sheet** band mounts `show_upgrade_sheet` over a real navigator playing the
//! root overlay host and drives it by tapping labelled buttons in the
//! accessibility tree. Two tests mount the whole app, since the launch refresh
//! is `MuxrApp::init`'s to fire, not the controller's.
//!
//! Every sheet action is asynchronous *and* staged: a tap spawns a store call,
//! whose answer drives a staged close, whose follow-on only runs on the exit's
//! completion frame. [`Sheet::flush`] is what carries all three — pump the
//! UI-thread queue, settle the frames, then deliver the pop's result.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clean_signals::Failure;
use frust::{AnyView, Component, GetUntracked, NavigatorController, Set, SizedBox, any, navigator};
use frust_core::{PointerPhase, RenderRoot};
use frust_reactive::ReactiveRuntime;
use frust_text::TextContext;
use kurbo::Point;

use muxr::failure::AppFailure;
use muxr::features::connect::domain::entities::{SavedServer, TrustMode};
use muxr::features::pro::data::repositories::MockEntitlementRepository;
use muxr::features::pro::di::{
    SharedEntitlementRepository, expect_entitlement_repository, expect_pro_controller,
    mock_entitlement,
};
use muxr::features::pro::domain::entities::{Entitlement, PurchaseOutcome, ServerLimitVerdict};
use muxr::features::pro::domain::use_cases::check_server_limit;
use muxr::features::pro::presentation::controller::{ProController, RestoreOutcome};
use muxr::features::pro::presentation::widgets::{
    NOT_NOW_LABEL, NOTHING_TO_RESTORE_TOAST, PENDING_TOAST, RESTORE_LABEL, UNAVAILABLE_COPY,
    UPGRADE_SHEET_TITLE,
};
use muxr::features::pro::presentation::{ServerLimitContext, UpgradeContext, show_upgrade_sheet};
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    RecScene, node_center_by_label, node_labels, node_values, pointer, pump_until, serial, settle,
    setup, tap,
};

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

/// [`row`] carrying `label`, which is what the forget-and-connect action
/// prints when exactly one row stands in the way.
fn labelled_row(label: &str) -> SavedServer {
    SavedServer {
        label: label.to_string(),
        ..row(A)
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
fn a_restore_that_finds_the_purchase_answers_restored() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();
    mock.set_entitlement(Entitlement::Pro);

    let outcome = block_on(controller.restore());

    assert_eq!(outcome, RestoreOutcome::Restored);
    assert_eq!(controller.entitlement.get_untracked(), Entitlement::Pro);
    assert_eq!(mock.restore_calls(), 1);
    assert!(!controller.busy.get_untracked(), "the guard is released");
}

#[test]
fn a_restore_that_finds_nothing_answers_nothing_to_restore() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();
    let log = failure_log(&controller);

    let outcome = block_on(controller.restore());

    assert_eq!(outcome, RestoreOutcome::NothingToRestore);
    assert_eq!(controller.entitlement.get_untracked(), Entitlement::Free);
    assert_eq!(mock.restore_calls(), 1);
    assert!(
        raised(&log).is_empty(),
        "an empty account is an answer, not a failure"
    );
}

#[test]
fn a_failed_restore_answers_failed_and_never_nothing_to_restore() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();
    mock.set_entitlement(Entitlement::Pro);
    block_on(controller.restore());
    let log = failure_log(&controller);
    let failure = AppFailure::Network("store unreachable".to_string());
    mock.fail_next_restore(failure.clone());

    let outcome = block_on(controller.restore());

    assert_eq!(outcome, RestoreOutcome::Failed(failure.user_message()));
    assert_eq!(raised(&log).len(), 1, "reported once, on the sink");
    assert_eq!(
        controller.entitlement.get_untracked(),
        Entitlement::Pro,
        "an unreachable store must not downgrade a paying install"
    );
    assert!(!controller.busy.get_untracked(), "the guard is released");
}

#[test]
fn a_restore_raised_while_one_is_in_flight_never_reaches_the_store() {
    let _g = serial();
    let _ambient = setup();
    let (mock, controller) = controller();
    app_toasts().clear();

    block_on(controller.restore());
    // The mock resolves without ever yielding, so an overlapping call is
    // staged by holding the guard the first one would still be holding.
    controller.busy.set(true);
    let refused = block_on(controller.restore());

    assert_eq!(refused, RestoreOutcome::Busy);
    assert_eq!(mock.restore_calls(), 1, "the second call was refused");
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "a refused overlap says nothing at all"
    );
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
    assert!(
        !controller.offer_settled.get_untracked(),
        "nothing has been asked yet, which a paywall renders as loading"
    );

    block_on(controller.load_offer());

    let offer = controller.offer.get_untracked().expect("the seeded offer");
    assert_eq!(offer.display_price, "$4.99");
    assert!(
        controller.offer_settled.get_untracked(),
        "the lookup landed"
    );
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

// ---------------------------------------------------------------------------
// The upgrade sheet, over a real navigator
// ---------------------------------------------------------------------------

/// The seeded offer's Upgrade action, as the sheet labels it.
const SEEDED_UPGRADE: &str = "Upgrade \u{b7} $4.99";
/// …and the forget-and-connect action for a single row labelled `Home rig`.
const FORGET_HOME_RIG: &str = "Forget Home rig and connect";

/// A point in the top-left corner: scrim while the sheet is up, and where the
/// harmless `Move` that flushes a pop's result lands (`tests/settings.rs`'s
/// idiom).
const SCRIM_POINT: Point = Point::new(4.0, 4.0);

/// How many pump-and-settle rounds [`Sheet::flush`] runs — comfortably more
/// than the spawn → store answer → staged close → completion-frame chain needs.
const FLUSH_ROUNDS: usize = 4;

/// The sheet mounted over a navigator playing the root overlay host, beside
/// the store it reads and the two callback counters it fires.
struct Sheet {
    nav: NavigatorController<AppState>,
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    pro: Arc<ProController>,
    mock: Arc<MockEntitlementRepository>,
    unlocked: Rc<Cell<usize>>,
    forgotten: Rc<Cell<usize>>,
}

impl Sheet {
    /// Mount the whole app's `pro` seam plus an empty overlay host, and drain
    /// `MuxrApp::init`'s launch refresh so a later script cannot race it.
    fn mount() -> Self {
        let state = MuxrApp.init();
        let pro = expect_pro_controller();
        let mock = mock_entitlement().expect("the test seam publishes the scriptable store");
        let nav: NavigatorController<AppState> = NavigatorController::new();
        let logic_nav = nav.clone();
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            let overlay_nav = logic_nav.clone();
            any(navigator(&overlay_nav, || {
                any(SizedBox::<AppState>(
                    Some(f64::INFINITY),
                    Some(f64::INFINITY),
                ))
            }))
        });
        let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
        root.set_theme(Box::new(frust_glyph::baseline()));
        let mut sheet = Sheet {
            nav,
            root,
            logic,
            state,
            tcx: TextContext::new(),
            pro,
            mock,
            unlocked: Rc::new(Cell::new(0)),
            forgotten: Rc::new(Cell::new(0)),
        };
        let launch = Arc::clone(&sheet.mock);
        pump_until(
            &mut sheet.root,
            &mut sheet.logic,
            &mut sheet.state,
            &mut sheet.tcx,
            RecScene::default(),
            || launch.refresh_calls() >= 1,
        );
        sheet
    }

    /// Raise the paywall. A non-empty `to_forget` is what makes this the
    /// Connect gate's shape rather than the Settings row's.
    fn open(&mut self, to_forget: Vec<SavedServer>) {
        let unlocked = Rc::clone(&self.unlocked);
        let forgotten = Rc::clone(&self.forgotten);
        let server_limit = (!to_forget.is_empty()).then(|| ServerLimitContext {
            to_forget,
            on_forget_and_connect: Rc::new(move || forgotten.set(forgotten.get() + 1)),
        });
        show_upgrade_sheet(
            &self.nav,
            Arc::clone(&self.pro),
            UpgradeContext {
                on_unlocked: Rc::new(move || unlocked.set(unlocked.get() + 1)),
                server_limit,
            },
        );
        self.flush();
    }

    /// Pump the UI-thread queue and settle the frames, then deliver the pop's
    /// result — see the [module docs](self) for the chain this carries.
    fn flush(&mut self) {
        let runtime = ReactiveRuntime::get().expect("flush: ReactiveRuntime::init must have run");
        for _ in 0..FLUSH_ROUNDS {
            runtime.pump_local();
            std::thread::sleep(Duration::from_millis(1));
            settle(
                &mut self.root,
                &mut self.logic,
                &mut self.state,
                &mut self.tcx,
            );
        }
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, SCRIM_POINT));
    }

    /// Tap the button labelled `label`, then flush what it started.
    fn tap_label(&mut self, label: &str) {
        let point = node_center_by_label(&self.root, label).unwrap_or_else(|| {
            panic!(
                "nothing labelled {label:?}; labels on screen: {:?}",
                node_labels(&self.root)
            )
        });
        tap(&mut self.root, &mut self.state, point);
        self.flush();
    }

    fn labels(&self) -> Vec<String> {
        node_labels(&self.root)
    }

    fn values(&self) -> Vec<String> {
        node_values(&self.root)
    }

    /// Whether any action offers to buy — the one control an entitled or
    /// unsellable sheet must not carry.
    fn offers_upgrade(&self) -> bool {
        self.labels().iter().any(|l| l.starts_with("Upgrade"))
    }

    /// The overlay host's page depth: 1 is its own base page, 2 the sheet.
    fn depth(&self) -> usize {
        self.nav.depth()
    }
}

#[test]
fn the_sheet_prints_the_stores_own_price_and_the_free_tier_actions() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();

    sheet.open(Vec::new());

    assert_eq!(sheet.depth(), 2, "the sheet is a page of the overlay host");
    let values = sheet.values();
    assert!(
        values.iter().any(|v| v == UPGRADE_SHEET_TITLE),
        "{values:?}"
    );
    let labels = sheet.labels();
    for expected in [SEEDED_UPGRADE, RESTORE_LABEL, NOT_NOW_LABEL] {
        assert!(
            labels.iter().any(|l| l == expected),
            "{expected}: {labels:?}"
        );
    }
    assert!(
        !labels.iter().any(|l| l.starts_with("Forget")),
        "no server limit was passed, so nothing offers to forget: {labels:?}"
    );
}

#[test]
fn a_landed_purchase_closes_the_sheet_and_unlocks_exactly_once() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();
    sheet.open(Vec::new());

    sheet.tap_label(SEEDED_UPGRADE);

    assert_eq!(sheet.depth(), 1, "the sheet closed behind the purchase");
    assert_eq!(sheet.mock.purchase_calls(), 1);
    assert_eq!(
        sheet.unlocked.get(),
        1,
        "the follow-on runs once, on the exit's completion frame"
    );
}

#[test]
fn a_cancelled_purchase_leaves_the_sheet_up_and_unlocks_nothing() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();
    sheet.mock.script_purchase(PurchaseOutcome::Cancelled);
    sheet.open(Vec::new());

    sheet.tap_label(SEEDED_UPGRADE);

    assert_eq!(
        sheet.mock.purchase_calls(),
        1,
        "the tap did reach the store"
    );
    assert_eq!(
        sheet.depth(),
        2,
        "a dismissed store sheet is not an answer — the paywall stays"
    );
    assert_eq!(sheet.unlocked.get(), 0);
}

#[test]
fn an_unsettled_purchase_says_so_and_closes_without_unlocking() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();
    app_toasts().clear();
    sheet.mock.script_purchase(PurchaseOutcome::Pending);
    sheet.open(Vec::new());

    sheet.tap_label(SEEDED_UPGRADE);

    let toasts = app_toasts().pending();
    assert!(toasts.iter().any(|t| t == PENDING_TOAST), "{toasts:?}");
    assert_eq!(sheet.depth(), 1);
    assert_eq!(sheet.unlocked.get(), 0, "pending unlocks nothing");
}

#[test]
fn a_store_that_cannot_sell_here_says_so_instead_of_offering_a_price() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();
    sheet.mock.script_offer(None);

    sheet.open(Vec::new());

    let values = sheet.values();
    assert!(values.iter().any(|v| v == UNAVAILABLE_COPY), "{values:?}");
    assert!(!sheet.offers_upgrade(), "{:?}", sheet.labels());
    assert!(
        sheet.labels().iter().any(|l| l == RESTORE_LABEL),
        "a purchase made elsewhere can still be restored: {:?}",
        sheet.labels()
    );
}

#[test]
fn the_server_limit_context_names_the_one_row_it_would_forget() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();

    sheet.open(vec![labelled_row("Home rig")]);

    let labels = sheet.labels();
    assert!(labels.iter().any(|l| l == FORGET_HOME_RIG), "{labels:?}");
}

#[test]
fn forgetting_and_connecting_closes_the_sheet_and_retries_exactly_once() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();
    sheet.open(vec![labelled_row("Home rig")]);

    sheet.tap_label(FORGET_HOME_RIG);

    assert_eq!(sheet.depth(), 1, "the sheet closed behind the retry");
    assert_eq!(sheet.forgotten.get(), 1);
    assert_eq!(
        sheet.unlocked.get(),
        0,
        "forgetting a row is not a purchase"
    );
}

#[test]
fn a_restore_that_finds_the_purchase_closes_the_sheet_and_unlocks() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();
    sheet.open(Vec::new());
    // Scripted *after* the open: the sheet has to raise itself against a free
    // install, or it would render its entitled body and offer no Restore.
    sheet.mock.set_entitlement(Entitlement::Pro);

    sheet.tap_label(RESTORE_LABEL);

    assert_eq!(sheet.mock.restore_calls(), 1);
    assert_eq!(sheet.depth(), 1);
    assert_eq!(sheet.unlocked.get(), 1);
}

#[test]
fn a_restore_that_finds_nothing_says_so_and_leaves_the_sheet_up() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();
    app_toasts().clear();
    sheet.open(Vec::new());

    sheet.tap_label(RESTORE_LABEL);

    let toasts = app_toasts().pending();
    assert!(
        toasts.iter().any(|t| t == NOTHING_TO_RESTORE_TOAST),
        "{toasts:?}"
    );
    assert_eq!(sheet.depth(), 2, "the paywall stays up to buy from");
    assert_eq!(sheet.unlocked.get(), 0);
}

#[test]
fn a_restore_the_store_refused_toasts_the_failure_and_never_the_empty_account() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();
    app_toasts().clear();
    let log = failure_log(&sheet.pro);
    sheet.open(Vec::new());
    let failure = AppFailure::Network("store unreachable".to_string());
    sheet.mock.fail_next_restore(failure.clone());

    sheet.tap_label(RESTORE_LABEL);

    assert_eq!(raised(&log).len(), 1, "reported once, on the sink");
    assert_eq!(
        app_toasts().pending(),
        vec![failure.user_message()],
        "the app's failure bridge announced it, and nothing else did"
    );
    assert_eq!(sheet.depth(), 2, "the paywall stays up");
    assert_eq!(sheet.unlocked.get(), 0);
}

#[test]
fn an_entitled_install_is_told_it_is_entitled_and_offered_nothing_to_buy() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();
    sheet.mock.set_entitlement(Entitlement::Pro);
    block_on(sheet.pro.refresh());

    sheet.open(Vec::new());

    let labels = sheet.labels();
    assert!(!sheet.offers_upgrade(), "{labels:?}");
    assert!(
        !labels.iter().any(|l| l == RESTORE_LABEL),
        "nothing left to restore: {labels:?}"
    );
    assert!(labels.iter().any(|l| l == NOT_NOW_LABEL), "{labels:?}");
}

#[test]
fn not_now_closes_the_sheet_and_runs_no_callback() {
    let _g = serial();
    let _ambient = setup();
    let mut sheet = Sheet::mount();
    sheet.open(vec![labelled_row("Home rig")]);

    sheet.tap_label(NOT_NOW_LABEL);

    assert_eq!(sheet.depth(), 1);
    assert_eq!(sheet.unlocked.get(), 0);
    assert_eq!(sheet.forgotten.get(), 0);
}
