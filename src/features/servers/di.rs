//! `servers`'s composition-root wiring seam (see `docs/FEATURES_ARCHITECTURE.md`'s
//! DI pattern). Two app-scoped singletons, both published with
//! [`frust::provide_context`] and both meant to be called from
//! `crate::MuxrApp::init` (`MuxrApp::init` calls `provide_last_seen` and
//! `provide_controller`; this slice does not edit `lib.rs`; see
//! `APP_FEATURES_ARCHITECTURE.md`):
//!
//! - [`provide_last_seen`] — the [`LastSeenHandle`], what each server last
//!   listed in this process. Plain data, shared and interior-mutable, no
//!   secret. [`use_last_seen`] is the **optional** lookup: `sessions` writes
//!   through it when present and does nothing when absent, so a process or
//!   test that never provided it is unaffected.
//! - [`provide_controller`] — the one [`ServersSwitcherController`], built
//!   over the other features' own seams and the handle above. It also
//!   publishes the app's one [`SwitcherBack`] (the component's back
//!   handle, closing the stage through that controller). In the shell it is
//!   wired but not the live back path: the shell's dismiss page outranks it
//!   and Sessions' dismiss consumer closes the stage.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use frust::{provide_context, use_context};
use frust_glyph::SwitcherBack;

use super::domain::entities::{LastSeen, SeenServer, SessionGlimpse};
use super::presentation::controller::{ServersSwitcherController, install_landing};
use crate::connection::expect_connection_controller;
use crate::features::connect::di::{
    expect_credential_service, expect_keyring, expect_saved_servers,
};
use crate::features::connect::domain::entities::epoch_millis;
use crate::features::pro::di::expect_pro_controller;
use crate::routes::app_nav;

/// How long after the stage closes its peer cards are still built: the close
/// ramp (320 ms, 120 ms under reduced motion) keeps painting them, and the
/// component tears down any peer the next view omits at once.
pub const PEER_TAIL: Duration = Duration::from_millis(500);

/// How long the `Connected` banner shows, and how long a `Failed` one is kept.
static BANNER_SUCCESS_MS: AtomicU64 = AtomicU64::new(1_500);
static BANNER_FAILURE_MS: AtomicU64 = AtomicU64::new(6_000);

/// How long a background dial may stay pending before `Connecting` shows, ms.
pub const BANNER_CONNECTING_DELAY_MS: u64 = 400;
static BANNER_CONNECTING_MS: AtomicU64 = AtomicU64::new(BANNER_CONNECTING_DELAY_MS);

/// The `Connecting` banner's show delay, ms.
pub fn banner_connecting_delay_ms() -> u64 {
    BANNER_CONNECTING_MS.load(Ordering::Relaxed)
}

/// Overrides the `Connecting` show delay (process-wide).
pub fn set_banner_connecting_delay_for_tests(ms: u64) {
    BANNER_CONNECTING_MS.store(ms, Ordering::Relaxed);
}

/// The `Connected` banner's lifetime, ms.
pub fn banner_success_ms() -> u64 {
    BANNER_SUCCESS_MS.load(Ordering::Relaxed)
}

/// The `Failed` banner's lifetime, ms.
pub fn banner_failure_ms() -> u64 {
    BANNER_FAILURE_MS.load(Ordering::Relaxed)
}

/// Overrides the banner lifetimes (process-wide), so a suite need not wait them out.
pub fn set_banner_timings_for_tests(success_ms: u64, failure_ms: u64) {
    BANNER_SUCCESS_MS.store(success_ms, Ordering::Relaxed);
    BANNER_FAILURE_MS.store(failure_ms, Ordering::Relaxed);
}

thread_local! {
    static SWITCHER_BACK: RefCell<Option<SwitcherBack>> = const { RefCell::new(None) };
    static STAGE_WAS_OPEN: Cell<bool> = const { Cell::new(false) };
    static STAGE_CLOSED_AT: Cell<Option<Instant>> = const { Cell::new(None) };
    static CLOCK_OVERRIDE: Cell<Option<Instant>> = const { Cell::new(None) };
}

fn stage_now() -> Instant {
    CLOCK_OVERRIDE.with(Cell::get).unwrap_or_else(Instant::now)
}

/// Pins (`Some`) or releases (`None`) the clock [`keep_peers`] reads, so a suite
/// can step past [`PEER_TAIL`] without sleeping.
pub fn set_stage_clock_for_tests(at: Option<Instant>) {
    CLOCK_OVERRIDE.with(|cell| cell.set(at));
}

/// Whether the shell should build the stage's peer cards this build: while the
/// stage is `open`, and for [`PEER_TAIL`] after it was seen to close. Nothing
/// rebuilds at the tail's end; the peers drop at the first build after it.
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

/// Whether the stage is fully at rest: not open, not expanding (both are
/// `open` to [`keep_peers`]), not mid-close, and past its [`PEER_TAIL`]. Reads
/// the same clock and state as [`keep_peers`]; a stage never seen open is
/// settled.
pub fn stage_settled() -> bool {
    if STAGE_WAS_OPEN.with(Cell::get) {
        return false;
    }
    let now = stage_now();
    STAGE_CLOSED_AT
        .with(Cell::get)
        .is_none_or(|closed| now.saturating_duration_since(closed) >= PEER_TAIL)
}

/// The per-process last-seen map, shared. Cloning shares the same map.
#[derive(Clone, Default)]
pub struct LastSeenHandle {
    seen: Arc<Mutex<LastSeen>>,
}

impl LastSeenHandle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember `sessions` as what `endpoint` showed just now.
    pub fn record(&self, endpoint: &str, sessions: Vec<SessionGlimpse>) {
        self.record_at(endpoint, sessions, epoch_millis());
    }

    /// [`Self::record`] with an explicit stamp. Glimpses only: the face falls
    /// back to the glimpse list, there being no page state to re-render.
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

    /// Remember `seen` as everything `endpoint` last showed, replacing the
    /// previous entry.
    pub fn record_seen(&self, endpoint: &str, seen: SeenServer) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(endpoint.to_string(), seen);
    }

    /// Drop the entries whose endpoint `keep` rejects (a forgotten row).
    pub fn retain(&self, keep: impl Fn(&str) -> bool) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|endpoint, _| keep(endpoint));
    }

    /// Forget every glimpse (a keyring lock invalidates what was cached).
    pub fn clear(&self) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// A copy of the map as it stands.
    pub fn snapshot(&self) -> LastSeen {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Publishes the process's one [`LastSeenHandle`] and hands it back.
pub fn provide_last_seen() -> LastSeenHandle {
    let handle = LastSeenHandle::new();
    provide_context(handle.clone());
    handle
}

/// The handle [`provide_last_seen`] published, if it ran.
pub fn use_last_seen() -> Option<LastSeenHandle> {
    use_context::<LastSeenHandle>()
}

/// Builds and publishes the app's one switcher controller. Needs
/// [`provide_last_seen`] and the `connect`, `pro` and connection singletons
/// published first.
pub fn provide_controller() -> Arc<ServersSwitcherController> {
    let last_seen = use_last_seen().unwrap_or_else(provide_last_seen);
    let controller = Arc::new(ServersSwitcherController::new(
        expect_saved_servers(),
        expect_credential_service(),
        expect_connection_controller(),
        expect_pro_controller(),
        expect_keyring(),
        last_seen,
    ));
    provide_context(Arc::clone(&controller));
    install_landing(Rc::new(|location| app_nav().go(location)));
    // One per app, created under the reactive runtime (`MuxrApp::init`). The
    // handle is `Rc`-based, so it cannot go through `provide_context` (which
    // wants `Send + Sync`): it is this thread's, like `routes::app_nav`.
    let closing = Arc::clone(&controller);
    SWITCHER_BACK
        .with(|cell| *cell.borrow_mut() = Some(SwitcherBack::new(move || closing.close())));
    controller
}

/// The app's one [`SwitcherBack`], passed to the stage on every build.
pub fn expect_switcher_back() -> SwitcherBack {
    SWITCHER_BACK.with(|cell| cell.borrow().clone()).expect(
        "expect_switcher_back: features::servers::di::provide_controller() must run first \
         (the shell wiring calls it from MuxrApp::init)",
    )
}

/// The switcher controller if [`provide_controller`] ran — the optional lookup
/// for screens (Sessions) that also mount in a process without the shell.
pub fn use_servers_controller() -> Option<Arc<ServersSwitcherController>> {
    use_context::<Arc<ServersSwitcherController>>()
}

pub fn expect_servers_controller() -> Arc<ServersSwitcherController> {
    use_context::<Arc<ServersSwitcherController>>().expect(
        "expect_servers_controller: features::servers::di::provide_controller() must run first \
         (the shell wiring calls it from MuxrApp::init)",
    )
}
