//! `servers`'s composition-root wiring seam (see `docs/FEATURES_ARCHITECTURE.md`'s
//! DI pattern). Two app-scoped singletons, both published with
//! [`frust::provide_context`] and both meant to be called from
//! `crate::MuxrApp::init` (the shell wiring card does; this slice does not edit
//! `lib.rs`):
//!
//! - [`provide_last_seen`] — the [`LastSeenHandle`], what each server last
//!   listed in this process. Plain data, shared and interior-mutable, no
//!   secret. [`use_last_seen`] is the **optional** lookup: `sessions` writes
//!   through it when present and does nothing when absent, so a process or
//!   test that never provided it is unaffected.
//! - [`provide_controller`] — the one [`ServersSwitcherController`], built
//!   over the other features' own seams and the handle above. It also
//!   publishes the app's one [`SwitcherBack`] (the stage's Android back
//!   claim), closing the stage through that controller.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use frust::{provide_context, use_context};
use frust_glyph::SwitcherBack;

use super::domain::entities::{LastSeen, SessionGlimpse};
use super::presentation::controller::ServersSwitcherController;
use crate::connection::expect_connection_controller;
use crate::features::connect::di::{
    expect_credential_service, expect_keyring, expect_saved_servers,
};
use crate::features::pro::di::expect_pro_controller;

thread_local! {
    static SWITCHER_BACK: RefCell<Option<SwitcherBack>> = const { RefCell::new(None) };
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
        self.record_at(endpoint, sessions, now_ms());
    }

    /// [`Self::record`] with an explicit stamp.
    pub fn record_at(&self, endpoint: &str, sessions: Vec<SessionGlimpse>, seen_at_ms: i64) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(endpoint.to_string(), (sessions, seen_at_ms));
    }

    /// A copy of the map as it stands.
    pub fn snapshot(&self) -> LastSeen {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(0))
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
