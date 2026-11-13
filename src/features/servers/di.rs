//! `servers` composition-root seam: the app-scoped [`LastSeenHandle`] ([`provide_last_seen`], optional [`use_last_seen`]) and the one [`ServersSwitcherController`] ([`provide_controller`]).
//! Both are called from `crate::MuxrApp::init`; the controller also installs a [`SwitcherRenewer`] as the connection's [`SessionRenewer`].
//! Cycle-free: switcher, connection and renewer are `Arc`s and the renewer holds the switcher as a [`Weak`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use frust::{provide_context, use_context};
use frust_glyph::SwitcherBack;

use super::domain::entities::{LastSeen, SeenServer, SessionGlimpse};
use super::presentation::controller::{ServersSwitcherController, install_landing};
use crate::connection::{SessionRenewer, expect_connection_controller};
use crate::features::connect::di::{
    expect_credential_service, expect_keyring, expect_saved_servers,
};
use crate::features::connect::domain::entities::epoch_millis;
use crate::features::pro::di::expect_pro_controller;
use crate::routes::app_nav;

/// The close ramp (320 ms, 120 ms under reduced motion) keeps painting peer cards after close; the component drops any peer the next view omits at once.
pub const PEER_TAIL: Duration = Duration::from_millis(500);

static BANNER_SUCCESS_MS: AtomicU64 = AtomicU64::new(1_500);
static BANNER_FAILURE_MS: AtomicU64 = AtomicU64::new(6_000);

pub const BANNER_CONNECTING_DELAY_MS: u64 = 400;
static BANNER_CONNECTING_MS: AtomicU64 = AtomicU64::new(BANNER_CONNECTING_DELAY_MS);

pub fn banner_connecting_delay_ms() -> u64 {
    BANNER_CONNECTING_MS.load(Ordering::Relaxed)
}

pub fn set_banner_connecting_delay_for_tests(ms: u64) {
    BANNER_CONNECTING_MS.store(ms, Ordering::Relaxed);
}

pub fn banner_success_ms() -> u64 {
    BANNER_SUCCESS_MS.load(Ordering::Relaxed)
}

pub fn banner_failure_ms() -> u64 {
    BANNER_FAILURE_MS.load(Ordering::Relaxed)
}

pub fn set_banner_timings_for_tests(success_ms: u64, failure_ms: u64) {
    BANNER_SUCCESS_MS.store(success_ms, Ordering::Relaxed);
    BANNER_FAILURE_MS.store(failure_ms, Ordering::Relaxed);
}

thread_local! {
    static SWITCHER_BACK: RefCell<Option<SwitcherBack>> = const { RefCell::new(None) };
    static INSTALLED_RENEWER: RefCell<Option<Arc<dyn SessionRenewer>>> = const { RefCell::new(None) };
    static STAGE_WAS_OPEN: Cell<bool> = const { Cell::new(false) };
    static STAGE_CLOSED_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    static CLOCK_OVERRIDE: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn stage_now() -> Instant {
    CLOCK_OVERRIDE.with(Cell::get).unwrap_or_else(Instant::now)
}

/// Pins the clock so a suite can step past [`PEER_TAIL`] without sleeping.
pub fn set_stage_clock_for_tests(at: Option<Instant>) {
    CLOCK_OVERRIDE.with(|cell| cell.set(at));
}

/// Nothing rebuilds at the tail's end; the peers drop at the first build after it.
pub fn keep_peers(open: bool) -> bool {
    let now = stage_now();
    if open {
        STAGE_WAS_OPEN.with(|cell| cell.set(true));
        STAGE_CLOSED_AT.with(|cell| cell.set(None));
        return true;
    }
    if STAGE_WAS_OPEN.with(|cell| cell.replace(false)) {
        STAGE_CLOSED_AT.with(|cell| cell.set(Some(now)));
    }
    STAGE_CLOSED_AT
        .with(Cell::get)
        .is_some_and(|closed| now.saturating_duration_since(closed) < PEER_TAIL)
}

/// A stage never seen open is settled.
pub fn stage_settled() -> bool {
    if STAGE_WAS_OPEN.with(Cell::get) {
        return false;
    }
    let now = stage_now();
    STAGE_CLOSED_AT
        .with(Cell::get)
        .is_none_or(|closed| now.saturating_duration_since(closed) >= PEER_TAIL)
}

#[derive(Clone, Default)]
pub struct LastSeenHandle {
    seen: Arc<Mutex<LastSeen>>,
}

impl LastSeenHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, endpoint: &str, sessions: Vec<SessionGlimpse>) {
        self.record_at(endpoint, sessions, epoch_millis());
    }

    /// Glimpses only: the face falls back to the glimpse list, there being no page state to re-render.
    pub fn record_at(&self, endpoint: &str, sessions: Vec<SessionGlimpse>, seen_at_ms: i64) {
        self.record_seen(
            endpoint,
            SeenServer {
                glimpses: sessions,
                seen_at_ms,
                page: None,
            },
        );
    }

    pub fn record_seen(&self, endpoint: &str, seen: SeenServer) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(endpoint.to_string(), seen);
    }

    pub fn retain(&self, keep: impl Fn(&str) -> bool) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|endpoint, _| keep(endpoint));
    }

    pub fn clear(&self) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    pub fn snapshot(&self) -> LastSeen {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

pub fn provide_last_seen() -> LastSeenHandle {
    let handle = LastSeenHandle::new();
    provide_context(handle.clone());
    handle
}

pub fn use_last_seen() -> Option<LastSeenHandle> {
    use_context::<LastSeenHandle>()
}

/// Needs [`provide_last_seen`] and the `connect`, `pro` and connection singletons published first.
pub fn provide_controller() -> Arc<ServersSwitcherController> {
    let last_seen = use_last_seen().unwrap_or_else(provide_last_seen);
    let connection = expect_connection_controller();
    let controller = Arc::new(ServersSwitcherController::new(
        expect_saved_servers(),
        expect_credential_service(),
        Arc::clone(&connection),
        expect_pro_controller(),
        expect_keyring(),
        last_seen,
    ));
    provide_context(Arc::clone(&controller));
    let renewer: Arc<dyn SessionRenewer> = Arc::new(SwitcherRenewer::new(&controller));
    connection.set_session_renewer(Arc::clone(&renewer));
    INSTALLED_RENEWER.with(|cell| *cell.borrow_mut() = Some(renewer));
    install_landing(Rc::new(|location| app_nav().go(location)));
    // `Rc`-based, so it cannot go through `provide_context` (`Send + Sync`); kept thread-local like `routes::app_nav`.
    let closing = Arc::clone(&controller);
    SWITCHER_BACK
        .with(|cell| *cell.borrow_mut() = Some(SwitcherBack::new(move || closing.close())));
    controller
}

pub struct SwitcherRenewer {
    switcher: Weak<ServersSwitcherController>,
}

impl SwitcherRenewer {
    pub fn new(switcher: &Arc<ServersSwitcherController>) -> Self {
        Self {
            switcher: Arc::downgrade(switcher),
        }
    }
}

impl SessionRenewer for SwitcherRenewer {
    fn renew(&self, endpoint: &str) -> bool {
        let Some(switcher) = self.switcher.upgrade() else {
            return false;
        };
        switcher.live_endpoint().as_deref() == Some(endpoint) && switcher.reconnect_live()
    }
}

/// A whole-app test's seam: the mock backend's missing client keeps `on_session_expired_for` off it.
pub fn installed_session_renewer_for_tests() -> Option<Arc<dyn SessionRenewer>> {
    INSTALLED_RENEWER.with(|cell| cell.borrow().clone())
}

pub fn expect_switcher_back() -> SwitcherBack {
    SWITCHER_BACK.with(|cell| cell.borrow().clone()).expect(
        "expect_switcher_back: features::servers::di::provide_controller() must run first \
         (the shell wiring calls it from MuxrApp::init)",
    )
}

pub fn use_servers_controller() -> Option<Arc<ServersSwitcherController>> {
    use_context::<Arc<ServersSwitcherController>>()
}

pub fn expect_servers_controller() -> Arc<ServersSwitcherController> {
    use_context::<Arc<ServersSwitcherController>>().expect(
        "expect_servers_controller: features::servers::di::provide_controller() must run first \
         (the shell wiring calls it from MuxrApp::init)",
    )
}
