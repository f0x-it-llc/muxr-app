//! [`ServersSwitcherController`] — the server switcher's view model.
//!
//! **App-scoped**, built once by [`crate::features::servers::di`]: the stage
//! is raised over the shell by the shell host, and this controller owns what
//! that host reads — whether the stage is [`open`](Self::open), the strip's
//! [`faces`](Self::faces), which peer card is [`expanding`](Self::expanding)
//! into the live page, and which endpoint a switch is [`busy`](Self::busy)
//! dialing.
//!
//! **Strip order.** `faces` is published active-first; the live card is the
//! switcher component's real page, so the component's *peers* are the faces
//! that are not [`FaceKind::Live`], in order. [`peer_endpoints`] is that
//! mapping and [`select`](ServersSwitcherController::select) takes the
//! component's peer index through it.
//!
//! **A switch** ([`select`](ServersSwitcherController::select)) sets
//! `expanding` first, so the skin grows the card while the work runs, then:
//! the free-tier verdict (blocked raises the paywall through the caller's
//! [`SwitchGate`], clears `expanding`, dials nothing); a row that claims no
//! credential is handed to the add-server page instead of being dialed; a
//! locked keyring gets `connect`'s page-agnostic [`KeyringUnlock`] — one
//! prompt — and the dial then goes through
//! [`KeyringUnlock::switch_to_saved`] with the keyring's token. When the
//! connection settles, `open` and `expanding` are cleared together (two
//! writes with nothing between them and the next frame), so the shell host
//! rebuilds around the new server. A failure clears `expanding` (the ramp
//! reverses, the stage stays open) and marks that face
//! [`FaceKind::Failed`]; a second tap retries.
//!
//! No `Nav`, overlay or other `Rc` handle is stored: the caller lends them per
//! call ([`SwitchGate`], [`lock`](ServersSwitcherController::lock)), which
//! keeps this controller `Send + Sync`.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use frust::{GetUntracked, NavigatorController, RwSignal, Set, spawn_local};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::connection::TrustMode;
use crate::features::connect::di::{Credentials, KeyringHandle, SavedServers};
use crate::features::connect::domain::entities::SavedServer;
use crate::features::connect::presentation::controller::ConfirmPrompt;
use crate::features::connect::presentation::widgets::credential_dialogs::unverified_confirm_prompt;
use crate::features::connect::presentation::{KeyringUnlock, KeyringUnlockOutcome};
use crate::features::pro::domain::entities::ServerLimitVerdict;
use crate::features::pro::presentation::controller::ProController;
use crate::features::pro::presentation::{UpgradeContext, show_upgrade_sheet};
use crate::features::servers::di::LastSeenHandle;
use crate::features::servers::domain::entities::{FaceKind, ServerFace, faces_for};
use crate::routes::{ADD_SERVER_LOCATION, Nav, add_server_location};
use crate::ui::toasts::app_toasts;

/// Shown on a face whose credential could not be found once unlocked.
const NO_CREDENTIAL_MESSAGE: &str = "No saved credential found.";
/// Where a lock sends the app: Connect.
const LOCKED_LOCATION: &str = "/";

/// A follow-on the paywall runs once Pro is unlocked.
pub type Retry = Rc<dyn Fn()>;

/// The two pieces of UI a switch can need, lent by the caller per
/// [`select`](ServersSwitcherController::select).
#[derive(Clone)]
pub struct SwitchGate {
    /// Raise the paywall; run the given retry once Pro is unlocked.
    pub paywall: Rc<dyn Fn(Retry)>,
    /// Open the add-server page prefilled with this endpoint.
    pub add_server: Rc<dyn Fn(String)>,
    /// Ask before a stored token is replayed to a
    /// [`TrustMode::NoVerification`] row; `on_confirm` dials. A decline simply
    /// never calls it.
    pub confirm: ConfirmPrompt,
}

impl SwitchGate {
    /// The gate wired to the real paywall sheet over `overlays` and the
    /// add-server page behind `nav`.
    pub fn standard(
        overlays: &NavigatorController<AppState>,
        pro: &Arc<ProController>,
        nav: &Nav,
    ) -> Self {
        let overlays = overlays.clone();
        let pro = Arc::clone(pro);
        let nav = nav.clone();
        let confirm = unverified_confirm_prompt(&overlays);
        Self {
            paywall: Rc::new(move |retry| {
                show_upgrade_sheet(
                    &overlays,
                    Arc::clone(&pro),
                    UpgradeContext {
                        on_unlocked: retry,
                        server_limit: None,
                    },
                );
            }),
            add_server: Rc::new(move |endpoint| nav.push(&add_server_location(Some(&endpoint)))),
            confirm,
        }
    }
}

/// The endpoints of the component's peer cards, in peer order: every face but
/// the live one.
pub fn peer_endpoints(faces: &[ServerFace]) -> Vec<String> {
    faces
        .iter()
        .filter(|face| face.kind != FaceKind::Live)
        .map(|face| face.endpoint.clone())
        .collect()
}

/// The server switcher's view model. See the [module docs](self).
pub struct ServersSwitcherController {
    saved_servers: SavedServers,
    connection: Arc<ConnectionController>,
    pro: Arc<ProController>,
    keyring_unlock: KeyringUnlock,
    last_seen: LastSeenHandle,
    /// Whether the stage is up.
    pub open: RwSignal<bool>,
    /// The strip, active first (see the [module docs](self)).
    pub faces: RwSignal<Vec<ServerFace>>,
    /// The peer index growing into the live page, while a switch runs.
    pub expanding: RwSignal<Option<usize>>,
    /// The endpoint a switch is working on — its single-flight claim.
    pub busy: RwSignal<Option<String>>,
    /// Endpoint to the message of its last failed switch.
    pub failures: RwSignal<BTreeMap<String, String>>,
    /// Bumped by every [`close`](Self::close) and lock: a switch captures it
    /// and abandons silently when it moved while an unlock or confirm was
    /// pending, so a dismissed stage never dials.
    stage_epoch: AtomicU64,
}

impl ServersSwitcherController {
    pub fn new(
        saved_servers: SavedServers,
        credentials: Credentials,
        connection: Arc<ConnectionController>,
        pro: Arc<ProController>,
        keyring: KeyringHandle,
        last_seen: LastSeenHandle,
    ) -> Self {
        let keyring_unlock = KeyringUnlock::new(
            Arc::clone(&saved_servers),
            credentials,
            Arc::clone(&connection),
            keyring,
        );
        Self {
            saved_servers,
            connection,
            pro,
            keyring_unlock,
            last_seen,
            open: RwSignal::new(false),
            faces: RwSignal::new(Vec::new()),
            expanding: RwSignal::new(None),
            busy: RwSignal::new(None),
            failures: RwSignal::new(BTreeMap::new()),
            stage_epoch: AtomicU64::new(0),
        }
    }

    /// Re-read the strip from the saved list, the live connection, the
    /// keyring, the last-seen map and the recorded failures. A saved list that
    /// cannot be read leaves the previous strip in place.
    pub fn refresh_faces(&self) {
        let Ok(saved) = self.saved_servers.list() else {
            return;
        };
        let active = self.active_endpoint();
        let faces = faces_for(
            &saved,
            active.as_deref(),
            self.keyring_unlock.keyring().is_unlocked(),
            &self.last_seen.snapshot(),
            &self.failures.get_untracked(),
        );
        self.faces.set(faces);
    }

    /// The connected server's endpoint, if any.
    fn active_endpoint(&self) -> Option<String> {
        if !self.connection.connected.get_untracked() {
            return None;
        }
        self.connection
            .server_info
            .get_untracked()
            .map(|info| info.endpoint)
    }

    /// Raise the stage over a fresh strip.
    pub fn open(&self) {
        self.refresh_faces();
        self.open.set(true);
    }

    /// Dismiss the stage; a half-grown card shrinks back with it.
    pub fn close(&self) {
        self.stage_epoch.fetch_add(1, Ordering::SeqCst);
        self.expanding.set(None);
        self.open.set(false);
    }

    /// Switch to the peer card at `peer_index` (resolved to its endpoint now,
    /// at the tap). Ignored while a switch is in flight.
    pub fn select(self: &Arc<Self>, peer_index: usize, gate: &SwitchGate) {
        if self.busy.get_untracked().is_some() {
            return;
        }
        if let Some(endpoint) = self.endpoint_of_peer(peer_index) {
            self.select_endpoint(endpoint, gate);
        }
    }

    /// [`Self::select`] by endpoint — what the paywall's retry carries. Marks
    /// the card `expanding` at once; the rest runs on the UI executor (an
    /// unlock prompt may be pending). See the [module docs](self).
    pub fn select_endpoint(self: &Arc<Self>, endpoint: String, gate: &SwitchGate) {
        if self.busy.get_untracked().is_some() {
            return;
        }
        self.expanding.set(self.peer_index_of(&endpoint));
        let controller = Arc::clone(self);
        let gate = gate.clone();
        spawn_local(async move { controller.run_select_endpoint(endpoint, gate).await });
    }

    fn endpoint_of_peer(&self, peer_index: usize) -> Option<String> {
        peer_endpoints(&self.faces.get_untracked())
            .get(peer_index)
            .cloned()
    }

    fn peer_index_of(&self, endpoint: &str) -> Option<usize> {
        peer_endpoints(&self.faces.get_untracked())
            .iter()
            .position(|peer| peer == endpoint)
    }

    /// [`Self::select`]'s work for peer `peer_index`, awaitable.
    pub async fn run_select(self: &Arc<Self>, peer_index: usize, gate: SwitchGate) {
        if let Some(endpoint) = self.endpoint_of_peer(peer_index) {
            self.run_select_endpoint(endpoint, gate).await;
        }
    }

    /// The switch itself, awaitable.
    pub async fn run_select_endpoint(self: &Arc<Self>, endpoint: String, gate: SwitchGate) {
        if self.busy.get_untracked().is_some() {
            return;
        }
        let epoch = self.stage_epoch.load(Ordering::SeqCst);
        self.expanding.set(self.peer_index_of(&endpoint));
        let Some(row) = self.saved_row(&endpoint) else {
            self.expanding.set(None);
            self.refresh_faces();
            return;
        };
        if !row.has_credential {
            self.expanding.set(None);
            (gate.add_server)(endpoint);
            return;
        }
        let saved = self.saved_servers.list().unwrap_or_default();
        if let ServerLimitVerdict::Blocked { .. } = self.pro.verdict_for(&saved, &endpoint) {
            self.expanding.set(None);
            let controller = Arc::clone(self);
            let retry_gate = gate.clone();
            let retry_endpoint = endpoint.clone();
            (gate.paywall)(Rc::new(move || {
                controller.select_endpoint(retry_endpoint.clone(), &retry_gate);
            }));
            return;
        }
        if self.connection.connecting.get_untracked() {
            self.expanding.set(None);
            return;
        }
        self.busy.set(Some(endpoint.clone()));
        if !self.keyring_unlock.keyring().is_unlocked() {
            let unlocked = self.unlock(&endpoint).await;
            if !unlocked {
                return;
            }
            if self.stage_epoch.load(Ordering::SeqCst) != epoch {
                self.abandon();
                return;
            }
        }
        if row.trust_mode == TrustMode::NoVerification {
            // The dialog is outside the in-flight state: a decline never calls
            // back, so nothing may stay claimed while it is up.
            self.busy.set(None);
            self.expanding.set(None);
            let controller = Arc::clone(self);
            (gate.confirm)(
                row,
                Box::new(move || controller.dial_confirmed(&endpoint, epoch)),
            );
            return;
        }
        self.dial(&endpoint);
    }

    /// The confirm's accept: dial only if the stage was not dismissed since.
    fn dial_confirmed(self: &Arc<Self>, endpoint: &str, epoch: u64) {
        if self.stage_epoch.load(Ordering::SeqCst) != epoch
            || self.busy.get_untracked().is_some()
            || self.connection.connecting.get_untracked()
        {
            return;
        }
        self.busy.set(Some(endpoint.to_string()));
        self.expanding.set(self.peer_index_of(endpoint));
        self.dial(endpoint);
    }

    /// The saved row for `endpoint`, read fresh.
    fn saved_row(&self, endpoint: &str) -> Option<SavedServer> {
        self.saved_servers
            .list()
            .ok()?
            .into_iter()
            .find(|row| row.endpoint() == endpoint)
    }

    /// Dial `endpoint` with the keyring's token, the row read fresh and only
    /// while it still claims a credential the keyring holds. `busy` is held.
    fn dial(self: &Arc<Self>, endpoint: &str) {
        let found = self
            .saved_row(endpoint)
            .filter(|row| row.has_credential)
            .and_then(|row| Some((self.keyring_unlock.keyring().token_for(endpoint)?, row)));
        let Some((token, row)) = found else {
            self.fail(endpoint, NO_CREDENTIAL_MESSAGE.to_string());
            return;
        };
        let controller = Arc::clone(self);
        let dialed = endpoint.to_string();
        self.keyring_unlock
            .switch_to_saved(&row, token, move |outcome| match outcome {
                Ok(()) => controller.settle(&dialed),
                Err(failure) => {
                    use clean_signals::Failure;
                    controller.fail(&dialed, failure.user_message());
                }
            });
    }

    /// One `unlock_all` prompt; `false` (state already reset) when the dial
    /// must not go on. A cancel or a superseding lock is silent.
    async fn unlock(&self, endpoint: &str) -> bool {
        match self.keyring_unlock.unlock().await {
            KeyringUnlockOutcome::Unlocked { .. } => true,
            KeyringUnlockOutcome::Refused(status) => {
                if let Some(message) = status.message() {
                    app_toasts().show(message);
                }
                self.abandon();
                false
            }
            KeyringUnlockOutcome::ListUnavailable => {
                app_toasts().show(
                    crate::features::connect::presentation::controller::SAVED_LIST_UNAVAILABLE_MESSAGE,
                );
                self.abandon();
                false
            }
            KeyringUnlockOutcome::Busy | KeyringUnlockOutcome::Superseded => {
                log::debug!("servers: unlock for {endpoint} abandoned silently");
                self.abandon();
                false
            }
        }
    }

    /// Give up a switch with nothing marked failed.
    fn abandon(&self) {
        self.busy.set(None);
        self.expanding.set(None);
        self.refresh_faces();
    }

    /// The connection settled on `endpoint`: stage down, card grown, together
    /// (both writes are synchronous, so no frame sits between them).
    fn settle(&self, endpoint: &str) {
        let mut failures = self.failures.get_untracked();
        failures.remove(endpoint);
        self.failures.set(failures);
        self.busy.set(None);
        self.expanding.set(None);
        self.open.set(false);
    }

    /// The dial to `endpoint` failed: reverse the ramp, mark the face.
    fn fail(&self, endpoint: &str, message: String) {
        let mut failures = self.failures.get_untracked();
        failures.insert(endpoint.to_string(), message);
        self.failures.set(failures);
        self.busy.set(None);
        self.expanding.set(None);
        self.refresh_faces();
    }

    /// Lock the keyring, drop the connection and return to Connect.
    pub fn lock(&self, nav: &Nav) {
        self.lock_with(|location| nav.go(location));
    }

    /// [`Self::lock`] with the navigation lent as a closure.
    pub fn lock_with(&self, go: impl FnOnce(&str)) {
        self.stage_epoch.fetch_add(1, Ordering::SeqCst);
        self.failures.set(BTreeMap::new());
        self.connection
            .lock_and_disconnect(self.keyring_unlock.keyring());
        self.busy.set(None);
        self.close();
        go(LOCKED_LOCATION);
    }

    /// Open the add-server page; the page runs its own free-tier verdict.
    pub fn add(&self, nav: &Nav) {
        self.add_with(|location| nav.push(location));
    }

    /// [`Self::add`] with the navigation lent as a closure.
    pub fn add_with(&self, push: impl FnOnce(&str)) {
        push(ADD_SERVER_LOCATION);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::Mutex;

    use super::*;
    use crate::connection::{
        ConnectRun, ConnectTarget, ConnectionBackend, MockConnectionBackend, TrustMode,
    };
    use crate::failure::AppFailure;
    use crate::features::connect::data::repositories::{
        MockCredentialService, MockSavedServersRepository,
    };
    use crate::features::connect::domain::entities::SavedServer;
    use crate::features::pro::data::repositories::MockEntitlementRepository;
    use crate::features::servers::domain::entities::SessionGlimpse;

    /// A backend whose every dial fails, until told to succeed.
    struct FlakyBackend {
        fail: Mutex<bool>,
    }

    impl ConnectionBackend for FlakyBackend {
        fn begin_connect(&self, target: ConnectTarget) -> ConnectRun {
            if *self.fail.lock().unwrap() {
                return ConnectRun::Ready(Err(AppFailure::Network("refused".to_string())));
            }
            MockConnectionBackend.begin_connect(target)
        }

        fn disconnect(&self) {}
    }

    fn server(host: &str, last_used_ms: i64, has_credential: bool) -> SavedServer {
        SavedServer {
            host: host.to_string(),
            port: 7777,
            trust_mode: TrustMode::System,
            label: host.to_string(),
            last_used_ms,
            read_only: false,
            has_credential,
            token_name: None,
            fingerprint: None,
        }
    }

    struct Rig {
        controller: Arc<ServersSwitcherController>,
        connection: Arc<ConnectionController>,
        credentials: Arc<MockCredentialService>,
        keyring: KeyringHandle,
        last_seen: LastSeenHandle,
    }

    fn rig_with(backend: Arc<dyn ConnectionBackend>, rows: Vec<SavedServer>, pro: bool) -> Rig {
        rig_over(backend, rows, pro, |mock| mock as Credentials)
    }

    /// A credential store whose `unlock_all` waits for the test to release it,
    /// so a prompt can be pending while the stage is dismissed.
    struct GatedCredentials {
        inner: Arc<MockCredentialService>,
        release: Arc<tokio::sync::Notify>,
    }

    #[clean_signals::async_trait]
    impl crate::features::connect::domain::repositories::CredentialService for GatedCredentials {
        async fn store(
            &self,
            request: crate::features::connect::domain::entities::StoreRequest,
        ) -> crate::features::connect::domain::entities::CredentialStatus {
            self.inner.store(request).await
        }

        async fn unlock(
            &self,
            request: crate::features::connect::domain::entities::UnlockRequest,
        ) -> crate::features::connect::domain::entities::UnlockedCredential {
            self.inner.unlock(request).await
        }

        async fn unlock_all(
            &self,
            reason: &str,
            endpoints: &[String],
        ) -> crate::features::connect::domain::entities::UnlockedKeyring {
            self.release.notified().await;
            self.inner.unlock_all(reason, endpoints).await
        }

        async fn store_in_vault(
            &self,
            request: crate::features::connect::domain::entities::StoreRequest,
            keyring: &mut crate::features::connect::domain::keyring::Keyring,
        ) -> crate::features::connect::domain::entities::CredentialStatus {
            self.inner.store_in_vault(request, keyring).await
        }

        async fn delete_from_vault(
            &self,
            server_key: String,
            keyring: &mut crate::features::connect::domain::keyring::Keyring,
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

    fn rig_over(
        backend: Arc<dyn ConnectionBackend>,
        rows: Vec<SavedServer>,
        pro: bool,
        over: impl FnOnce(Arc<MockCredentialService>) -> Credentials,
    ) -> Rig {
        let credentials = Arc::new(MockCredentialService::new());
        let connection = Arc::new(ConnectionController::with_backend(backend));
        let entitlement = Arc::new(if pro {
            MockEntitlementRepository::pro()
        } else {
            MockEntitlementRepository::free()
        });
        let keyring = KeyringHandle::new();
        let last_seen = LastSeenHandle::new();
        let controller = Arc::new(ServersSwitcherController::new(
            Arc::new(MockSavedServersRepository::with_servers(rows)),
            over(Arc::clone(&credentials)),
            Arc::clone(&connection),
            Arc::new(ProController::new(entitlement)),
            keyring.clone(),
            last_seen.clone(),
        ));
        Rig {
            controller,
            connection,
            credentials,
            keyring,
            last_seen,
        }
    }

    fn rig(rows: Vec<SavedServer>, pro: bool) -> Rig {
        rig_with(Arc::new(MockConnectionBackend), rows, pro)
    }

    /// Connect to `host` first, so the strip has a live card.
    fn connect_to(rig: &Rig, host: &str) {
        rig.connection.connect(ConnectTarget {
            host: host.to_string(),
            port: 7777,
            trust_mode: TrustMode::NoVerification,
            label: host.to_string(),
            offer_credential: false,
            token: zeroize::Zeroizing::new("t".to_string()),
            remember: false,
            fingerprint: None,
        });
    }

    fn unlocked(rig: &Rig, entries: &[(&str, &str)]) {
        let mut keyring = crate::features::connect::domain::keyring::Keyring::new();
        for (endpoint, token) in entries {
            keyring.insert(*endpoint, zeroize::Zeroizing::new((*token).to_string()));
        }
        rig.keyring.publish(keyring);
    }

    /// A gate recording what it was asked, raising nothing. The confirm
    /// answers `accept` at once.
    struct Probe {
        gate: SwitchGate,
        paywalls: Rc<Cell<usize>>,
        confirms: Rc<Cell<usize>>,
        added: Rc<RefCell<Vec<String>>>,
    }

    fn probe_answering(accept: bool) -> Probe {
        let paywalls = Rc::new(Cell::new(0));
        let confirms = Rc::new(Cell::new(0));
        let added = Rc::new(RefCell::new(Vec::new()));
        let gate = SwitchGate {
            paywall: {
                let paywalls = Rc::clone(&paywalls);
                Rc::new(move |_retry| paywalls.set(paywalls.get() + 1))
            },
            add_server: {
                let added = Rc::clone(&added);
                Rc::new(move |endpoint| added.borrow_mut().push(endpoint))
            },
            confirm: {
                let confirms = Rc::clone(&confirms);
                Rc::new(move |_row, on_confirm| {
                    confirms.set(confirms.get() + 1);
                    if accept {
                        on_confirm();
                    }
                })
            },
        };
        Probe {
            gate,
            paywalls,
            confirms,
            added,
        }
    }

    fn probe() -> Probe {
        probe_answering(true)
    }

    #[tokio::test]
    async fn select_on_pro_switches_and_settles_open_and_expanding() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        assert!(rig.controller.open.get_untracked());
        let epoch = rig.connection.connection_epoch.get_untracked();
        assert_eq!(
            peer_endpoints(&rig.controller.faces.get_untracked()),
            ["b:7777"]
        );

        rig.controller.run_select(0, probe().gate).await;

        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "b:7777");
        assert!(rig.connection.connection_epoch.get_untracked() > epoch);
        assert!(!rig.controller.open.get_untracked());
        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert_eq!(rig.controller.busy.get_untracked(), None);
    }

    #[tokio::test]
    async fn select_on_a_free_install_with_two_rows_raises_the_paywall_and_never_dials() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], false);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        let epoch = rig.connection.connection_epoch.get_untracked();
        let probe = probe();

        rig.controller.run_select(0, probe.gate.clone()).await;

        assert_eq!(probe.paywalls.get(), 1);
        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert!(rig.controller.open.get_untracked(), "the stage stays up");
        assert_eq!(rig.connection.connection_epoch.get_untracked(), epoch);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "a:7777");
    }

    #[tokio::test]
    async fn a_locked_face_prompts_once_then_switches() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        rig.credentials
            .seed_vault(&[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        assert_eq!(
            rig.controller.faces.get_untracked()[1].kind,
            FaceKind::Locked
        );

        rig.controller.run_select(0, probe().gate).await;

        assert_eq!(rig.credentials.prompts().len(), 1);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "b:7777");
        assert!(rig.keyring.is_unlocked());
        assert!(!rig.controller.open.get_untracked());
    }

    #[tokio::test]
    async fn a_failed_dial_marks_the_face_failed_and_a_second_tap_retries() {
        let backend = Arc::new(FlakyBackend {
            fail: Mutex::new(false),
        });
        let rig = rig_with(
            Arc::clone(&backend) as Arc<dyn ConnectionBackend>,
            vec![server("a", 20, true), server("b", 10, true)],
            true,
        );
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        *backend.fail.lock().unwrap() = true;

        rig.controller.run_select(0, probe().gate).await;

        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert_eq!(rig.controller.busy.get_untracked(), None);
        assert!(
            rig.controller.open.get_untracked(),
            "the stage stays open on failure"
        );
        assert!(matches!(
            rig.controller.faces.get_untracked().last().map(|f| &f.kind),
            Some(FaceKind::Failed { .. })
        ));

        // `switch_to` tears the old connection down before it dials, so after
        // the failure no face is live and the strip's peers are every row.
        assert!(!rig.connection.connected.get_untracked());
        let peers = peer_endpoints(&rig.controller.faces.get_untracked());
        let again = peers
            .iter()
            .position(|e| e == "b:7777")
            .expect("b is a peer");

        *backend.fail.lock().unwrap() = false;
        rig.controller.run_select(again, probe().gate).await;

        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "b:7777");
        assert!(!rig.controller.open.get_untracked());
        assert!(rig.controller.failures.get_untracked().is_empty());
    }

    #[tokio::test]
    async fn a_row_without_a_credential_goes_to_the_add_page_not_the_keyring() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, false)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta")]);
        rig.controller.open();
        let probe = probe();

        rig.controller.run_select(0, probe.gate.clone()).await;

        assert_eq!(*probe.added.borrow(), ["b:7777"]);
        assert_eq!(rig.credentials.prompts().len(), 0);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "a:7777");
        assert_eq!(rig.controller.expanding.get_untracked(), None);
    }

    #[test]
    fn close_clears_expanding_and_lock_drops_the_connection() {
        let rig = rig(vec![server("a", 20, true)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta")]);
        rig.controller.open();
        rig.controller.expanding.set(Some(0));
        rig.controller.close();
        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert!(!rig.controller.open.get_untracked());

        let went = RefCell::new(None);
        rig.controller
            .lock_with(|to| *went.borrow_mut() = Some(to.to_string()));
        assert_eq!(went.borrow().as_deref(), Some("/"));
        assert!(!rig.keyring.is_unlocked());
        assert!(!rig.connection.connected.get_untracked());

        let pushed = RefCell::new(None);
        rig.controller
            .add_with(|to| *pushed.borrow_mut() = Some(to.to_string()));
        assert_eq!(pushed.borrow().as_deref(), Some("/?mode=add"));
    }

    #[test]
    fn a_cached_face_reads_the_last_seen_handle() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.last_seen.record_at(
            "b:7777",
            vec![SessionGlimpse {
                name: "main".to_string(),
                backend: "zellij".to_string(),
                running: true,
                meta_line: "1h".to_string(),
            }],
            7,
        );
        rig.controller.open();
        assert!(matches!(
            rig.controller.faces.get_untracked()[1].kind,
            FaceKind::Cached {
                last_seen_ms: 7,
                ..
            }
        ));
    }

    /// A `NoVerification` credentialed peer `b` under a live `a`, keyring open.
    fn unverified_rig() -> Rig {
        let mut b = server("b", 10, true);
        b.trust_mode = TrustMode::NoVerification;
        let rig = rig(vec![server("a", 20, true), b], true);
        connect_to(&rig, "a");
        unlocked(&rig, &[("a:7777", "ta"), ("b:7777", "tb")]);
        rig.controller.open();
        rig
    }

    #[tokio::test]
    async fn an_unverified_row_asks_once_and_a_decline_dials_nothing() {
        let rig = unverified_rig();
        let declined = probe_answering(false);
        let epoch = rig.connection.connection_epoch.get_untracked();

        rig.controller.run_select(0, declined.gate.clone()).await;

        assert_eq!(declined.confirms.get(), 1);
        assert_eq!(rig.connection.connection_epoch.get_untracked(), epoch);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "a:7777");
        assert_eq!(rig.controller.busy.get_untracked(), None);
        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert!(rig.controller.open.get_untracked());
    }

    #[tokio::test]
    async fn an_unverified_row_dials_once_the_confirm_is_accepted() {
        let rig = unverified_rig();
        let accepted = probe_answering(true);

        rig.controller.run_select(0, accepted.gate.clone()).await;

        assert_eq!(accepted.confirms.get(), 1);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "b:7777");
        assert!(!rig.controller.open.get_untracked());
    }

    #[tokio::test]
    async fn closing_the_stage_while_the_unlock_is_pending_dials_nothing() {
        let release = Arc::new(tokio::sync::Notify::new());
        let gate_release = Arc::clone(&release);
        let rig = rig_over(
            Arc::new(MockConnectionBackend),
            vec![server("a", 20, true), server("b", 10, true)],
            true,
            move |inner| {
                inner.seed_vault(&[("a:7777", "ta"), ("b:7777", "tb")]);
                Arc::new(GatedCredentials {
                    inner,
                    release: gate_release,
                }) as Credentials
            },
        );
        connect_to(&rig, "a");
        rig.controller.open();
        let epoch = rig.connection.connection_epoch.get_untracked();
        let probe = probe();

        let select = rig.controller.run_select(0, probe.gate.clone());
        let dismiss = async {
            tokio::task::yield_now().await;
            rig.controller.close();
            release.notify_one();
        };
        tokio::join!(select, dismiss);

        assert_eq!(rig.connection.connection_epoch.get_untracked(), epoch);
        let info = rig.connection.server_info.get_untracked().expect("live");
        assert_eq!(info.endpoint, "a:7777");
        assert_eq!(rig.controller.busy.get_untracked(), None);
        assert_eq!(rig.controller.expanding.get_untracked(), None);
        assert_eq!(probe.confirms.get(), 0);
    }

    #[test]
    fn a_tap_during_an_in_flight_switch_leaves_expanding_alone() {
        let rig = rig(vec![server("a", 20, true), server("b", 10, true)], true);
        connect_to(&rig, "a");
        rig.controller.open();
        rig.controller.busy.set(Some("b:7777".to_string()));
        rig.controller.expanding.set(Some(3));

        rig.controller.select(0, &probe().gate);

        assert_eq!(rig.controller.expanding.get_untracked(), Some(3));
    }

    #[test]
    fn lock_forgets_recorded_failures() {
        let rig = rig(vec![server("a", 20, true)], true);
        rig.controller
            .failures
            .set(BTreeMap::from([("a:7777".to_string(), "down".to_string())]));

        rig.controller.lock_with(|_| {});

        assert!(rig.controller.failures.get_untracked().is_empty());
    }
}
