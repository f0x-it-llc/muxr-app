use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use clean_signals::{ControllerCore, Failure, RunOptions};
use frust::{Get, GetUntracked, RwSignal, Set, With, spawn_local};
use frust_secure_storage::{CanAuthenticate, SecureStorage};
use zeroize::Zeroizing;

use crate::connection::{ConnectTarget, ConnectionController, CredentialSaver, TrustMode};
use crate::failure::AppFailure;
use crate::features::connect::di::{Credentials, KeyringHandle, SavedServers};
use crate::features::connect::domain::entities::{
    CredentialStatus, PairingPayload, PairingTrust, SavedServer, StoreRequest, UnlockRequest,
    UnlockedCredential, UnlockedKeyring, epoch_millis,
};
use crate::features::connect::domain::use_cases::parse_endpoint::{Endpoint, parse_endpoint};
use crate::features::connect::domain::use_cases::{SaveCredential, UnlockCredential};
use crate::features::pro::domain::entities::{Entitlement, ServerLimitVerdict};
use crate::features::pro::presentation::controller::ProController;
use crate::features::settings::di::SharedAuthTokensRepository;
use crate::features::settings::domain::entities::CreateTokenRequest;
use crate::routes::{Nav, PageMark};
use crate::ui::toasts::app_toasts;

pub const SESSIONS_LOCATION: &str = "/sessions";

pub struct CredentialOffer {
    pub label: String,
    pub replacing: bool,
}

pub type OfferPrompt = Rc<dyn Fn(CredentialOffer)>;

#[derive(Clone)]
pub enum ConnectRetry {
    Manual,
    Pairing,
    Saved(SavedServer),
    Keyring,
}

pub const VAULT_RESET_MESSAGE: &str =
    "Biometrics changed — saved credentials were reset. Connect again with a token.";
pub const MANUAL_RESET_MESSAGE: &str = "Saved credentials were reset. Connect again with a token.";
pub const SAVED_LIST_UNAVAILABLE_MESSAGE: &str =
    "Saved servers aren't available right now. Connect with a token instead.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockState {
    Idle,
    Unlocking,
    Failed(DockFailure),
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockFailure {
    pub message: String,
    pub offers_reset: bool,
}

thread_local! {
    static BIOMETRICS_FOR_TESTS: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Overrides this thread's biometric probe (`None` restores the real one).
pub fn set_biometrics_available_for_tests(available: Option<bool>) {
    BIOMETRICS_FOR_TESTS.with(|cell| cell.set(available));
}

/// Whether the device can show a biometric prompt now — never prompts itself.
fn probe_biometrics() -> bool {
    if let Some(available) = BIOMETRICS_FOR_TESTS.with(Cell::get) {
        return available;
    }
    matches!(
        SecureStorage::can_authenticate(),
        CanAuthenticate::Available
    )
}

pub struct ServerLimitBlock {
    pub to_forget: Vec<SavedServer>,
    pub retry: ConnectRetry,
}

pub type GatePrompt = Rc<dyn Fn(ServerLimitBlock)>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectMode {
    Home,
    Add,
}

/// Pops only the page marked when the dial started, so a dial settling after the user
/// backed out never pops the shell; a landed Add page refuses a second dial.
#[derive(Clone)]
struct Landing {
    nav: Nav,
    /// `Some` exactly in [`ConnectMode::Add`].
    mark: Option<PageMark>,
    landed: Arc<AtomicBool>,
}

impl Landing {
    fn land(&self) {
        match &self.mark {
            None => self.nav.push(SESSIONS_LOCATION),
            Some(mark) => {
                self.landed.store(true, Ordering::SeqCst);
                self.nav.pop_marked(mark);
            }
        }
    }
}

const DEFAULT_TRUST_MODE: TrustMode = TrustMode::System;

/// A publicly trusted certificate implies a TLS-terminating edge on the HTTPS port.
const PUBLIC_CA_DEFAULT_PORT: u16 = 443;

/// `muxrd`'s own default bind port, dialed with no TLS-terminating edge in front.
const MUXRD_DEFAULT_PORT: u16 = 50051;

const DEFAULT_REMEMBER: bool = true;

pub const URL_ERROR_MESSAGE: &str = concat!(
    "Enter host:port, for example dev.home.local:50051. A bare host uses :443 ",
    "under Public domain (CA) and :50051 under Trust anyway.",
);

const TOKEN_NAME_PREFIX: &str = "muxr-mobile";

const TOKEN_LABEL_MAX: usize = 24;

pub struct ConnectController {
    core: ControllerCore<AppFailure>,
    repository: SavedServers,
    credentials: Credentials,
    tokens: SharedAuthTokensRepository,
    save_credential: SaveCredential,
    unlock_credential: UnlockCredential,
    connection: Arc<ConnectionController>,
    pro: Arc<ProController>,
    keyring: KeyringHandle,
    keyring_unlock: KeyringUnlock,
    mode: ConnectMode,
    /// Add mode: set once a connect lands; every later dial is refused.
    landed: Arc<AtomicBool>,
    landing_mark: std::sync::Mutex<Option<PageMark>>,
    biometrics_available: bool,
    seen_marks: Mutex<(u64, u64)>,
    saving_credential: AtomicBool,
    pub url: RwSignal<String>,
    pub token: RwSignal<Zeroizing<String>>,
    pub trust_mode: RwSignal<TrustMode>,
    pub remember: RwSignal<bool>,
    pub saved: RwSignal<Vec<SavedServer>>,
    pub invalid: RwSignal<bool>,
    pub url_error: RwSignal<Option<String>>,
    pub pending_pairing: RwSignal<Option<PairingPayload>>,
    pub unlocking: RwSignal<bool>,
    pub resolving_offer: RwSignal<bool>,
    pub dock_failure: RwSignal<Option<DockFailure>>,
}

impl ConnectController {
    pub fn new(
        repository: SavedServers,
        credentials: Credentials,
        tokens: SharedAuthTokensRepository,
        connection: Arc<ConnectionController>,
        pro: Arc<ProController>,
        keyring: KeyringHandle,
    ) -> Self {
        let keyring_unlock = KeyringUnlock::new(
            Arc::clone(&repository),
            Arc::clone(&credentials),
            Arc::clone(&connection),
            keyring.clone(),
        );
        let controller = Self {
            core: ControllerCore::new(),
            repository,
            save_credential: SaveCredential::new(Arc::clone(&credentials)),
            unlock_credential: UnlockCredential::new(Arc::clone(&credentials)),
            credentials,
            tokens,
            connection,
            pro,
            keyring,
            keyring_unlock,
            mode: ConnectMode::Home,
            landed: Arc::new(AtomicBool::new(false)),
            landing_mark: std::sync::Mutex::new(None),
            biometrics_available: probe_biometrics(),
            seen_marks: Mutex::new((0, 0)),
            saving_credential: AtomicBool::new(false),
            url: RwSignal::new(String::new()),
            token: RwSignal::new(Zeroizing::new(String::new())),
            trust_mode: RwSignal::new(DEFAULT_TRUST_MODE),
            remember: RwSignal::new(DEFAULT_REMEMBER),
            saved: RwSignal::new(Vec::new()),
            invalid: RwSignal::new(false),
            url_error: RwSignal::new(None),
            pending_pairing: RwSignal::new(None),
            unlocking: RwSignal::new(false),
            resolving_offer: RwSignal::new(false),
            dock_failure: RwSignal::new(None),
        };
        controller.refresh_saved_if_moved();
        controller.reload_saved();
        controller
    }

    pub fn in_add_mode(mut self) -> Self {
        self.mode = ConnectMode::Add;
        self
    }

    pub fn mode(&self) -> ConnectMode {
        self.mode
    }

    fn landing(&self, nav: &Nav) -> Landing {
        let mark = (self.mode == ConnectMode::Add).then(|| nav.mark_top());
        if mark.is_some() {
            *self
                .landing_mark
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = mark;
        }
        Landing {
            nav: nav.clone(),
            mark,
            landed: Arc::clone(&self.landed),
        }
    }

    fn offer_landing(&self, nav: &Nav) -> Landing {
        let stored = self
            .landing_mark
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let mark =
            (self.mode == ConnectMode::Add).then(|| stored.unwrap_or_else(|| nav.mark_top()));
        Landing {
            nav: nav.clone(),
            mark,
            landed: Arc::clone(&self.landed),
        }
    }

    fn dial(&self, target: ConnectTarget, on_done: impl FnOnce(Result<(), AppFailure>) + 'static) {
        match self.mode {
            ConnectMode::Home => self.connection.connect_with(target, on_done),
            ConnectMode::Add => self.connection.switch_to(target, on_done),
        }
    }

    /// Held weakly: the connection handle outlives every screen.
    pub fn credential_saver(self: &Arc<Self>) -> Arc<dyn CredentialSaver> {
        Arc::new(ControllerCredentialSaver {
            controller: Arc::downgrade(self),
        })
    }

    pub fn reload_saved(&self) {
        match self.repository.list() {
            Ok(servers) => self.saved.set(servers),
            Err(failure) => app_toasts().raise(&failure),
        }
    }

    /// `/` stays retained under the shell while the add page and Settings edit the same
    /// list. Untracked, so a build may call it every frame.
    fn refresh_saved_if_moved(&self) {
        let marks = (
            self.connection.connection_epoch.get_untracked(),
            self.keyring.generation.get_untracked(),
        );
        {
            let mut seen = self
                .seen_marks
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if *seen == marks {
                return;
            }
            *seen = marks;
        }
        if let Ok(servers) = self.repository.list()
            && self.saved.get_untracked() != servers
        {
            self.saved.set(servers);
        }
    }

    /// An address that stops matching a live pairing's host/port drops it, which is why
    /// [`Self::apply_scanned_payload`] writes [`Self::url`] directly.
    pub fn set_url(&self, url: String) {
        self.invalidate_pairing_unless_covering(&url);
        self.url.set(url);
        if self.invalid.get_untracked() || self.url_error.get_untracked().is_some() {
            self.clear_url_validation();
        }
    }

    fn reject_url(&self, message: &str) {
        self.invalid.set(true);
        self.url_error.set(Some(message.to_owned()));
    }

    fn clear_url_validation(&self) {
        self.invalid.set(false);
        self.url_error.set(None);
    }

    pub fn entitlement(&self) -> Entitlement {
        self.pro.entitlement.get()
    }

    /// A hand-typed token drops a live pairing: the pinned CTA sends the payload's token.
    pub fn set_token(&self, token: String) {
        self.token.set(Zeroizing::new(token));
        self.clear_pending_pairing();
    }

    pub fn clear_token(&self) {
        self.token.set(Zeroizing::new(String::new()));
    }

    /// A choice that disagrees with a stashed pairing's posture drops the pairing first (which
    /// resets the mode), then applies; re-selecting the pairing's own posture keeps it.
    pub fn select_trust_mode(&self, mode: TrustMode) {
        let overrides_pairing = self.pending_pairing.with(|pending| {
            pending
                .as_ref()
                .is_some_and(|payload| pairing_trust_mode(&payload.trust) != mode)
        });
        if overrides_pairing {
            self.clear_pending_pairing();
        }
        self.trust_mode.set(mode);
    }

    pub fn toggle_remember(&self) {
        self.remember.set(!self.remember.get_untracked());
    }

    pub fn saved_row(&self, index: usize) -> Option<SavedServer> {
        self.saved.get_untracked().get(index).cloned()
    }

    pub fn select_saved(&self, index: usize) {
        let Some(server) = self.saved_row(index) else {
            return;
        };
        self.prefill(&server);
    }

    pub fn prefill(&self, server: &SavedServer) {
        self.set_url(server.endpoint());
        self.trust_mode.set(server.trust_mode);
    }

    pub fn saved_row_tapped(self: &Arc<Self>, index: usize, nav: &Nav) {
        let Some(server) = self.saved_row(index) else {
            return;
        };
        if server.has_credential {
            self.recover_and_connect(server, nav);
        } else {
            self.prefill(&server);
        }
    }

    pub fn connecting(&self) -> RwSignal<bool> {
        self.connection.connecting
    }

    pub fn busy(&self) -> bool {
        self.connection.connecting.get() || self.unlocking.get() || self.resolving_offer.get()
    }

    /// Untracked. Also refuses once an add page has landed: its form is already scrubbed.
    pub(crate) fn busy_untracked(&self) -> bool {
        self.landed.load(Ordering::SeqCst)
            || self.connection.connecting.get_untracked()
            || self.unlocking.get_untracked()
            || self.resolving_offer.get_untracked()
    }

    /// `true` means return without dialing: a blocked connect writes and toasts nothing.
    fn blocked(&self, endpoint: &str, retry: ConnectRetry, gate_prompt: &GatePrompt) -> bool {
        match self.pro.verdict_for(&self.saved.get_untracked(), endpoint) {
            ServerLimitVerdict::Allowed => false,
            ServerLimitVerdict::Blocked { to_forget } => {
                gate_prompt(ServerLimitBlock { to_forget, retry });
                true
            }
        }
    }

    /// Taken ahead of [`Self::recover_and_connect`]: the row's no-verification confirmation
    /// sits between the two and dials ungated.
    pub fn saved_row_blocked(&self, server: &SavedServer, gate_prompt: &GatePrompt) -> bool {
        if !server.has_credential {
            return false;
        }
        self.blocked(
            &server.endpoint(),
            ConnectRetry::Saved(server.clone()),
            gate_prompt,
        )
    }

    pub fn saved_row_index(&self, endpoint: &str) -> Option<usize> {
        self.saved
            .get_untracked()
            .iter()
            .position(|server| server.endpoint() == endpoint)
    }

    /// Never connects. Writes the fields directly: their setters would clear the stashed payload.
    pub fn apply_scanned_payload(&self, payload: PairingPayload) {
        self.url.set(pairing_endpoint(&payload));
        self.token.set(payload.token.clone());
        self.trust_mode.set(pairing_trust_mode(&payload.trust));
        self.clear_url_validation();
        self.pending_pairing.set(Some(payload));
    }

    /// Resets trust to the default: a leftover [`TrustMode::Pinned`] fails closed at dial time.
    pub fn clear_pending_pairing(&self) {
        if self.pending_pairing.with(Option::is_none) {
            return;
        }
        self.pending_pairing.set(None);
        self.trust_mode.set(DEFAULT_TRUST_MODE);
    }

    fn invalidate_pairing_unless_covering(&self, url: &str) {
        let default_port = default_port_for(self.trust_mode.get_untracked());
        let covered = self.pending_pairing.with(|pending| match pending {
            None => true,
            Some(payload) => parse_endpoint(url, default_port)
                .is_ok_and(|endpoint| pairing_covers(payload, &endpoint)),
        });
        if !covered {
            self.clear_pending_pairing();
        }
    }

    pub fn connect_from_pairing(self: &Arc<Self>, nav: &Nav, gate_prompt: GatePrompt) {
        if self.busy_untracked() {
            return;
        }
        let Some(payload) = self.pending_pairing.get_untracked() else {
            return;
        };
        let target = pairing_target(&payload);
        if self.blocked(
            &target_endpoint(&target),
            ConnectRetry::Pairing,
            &gate_prompt,
        ) {
            return;
        }
        self.clear_url_validation();
        let landing = self.landing(nav);
        let token = self.token;
        let pending = self.pending_pairing;
        let upsert = self.login_upsert(&target);
        let controller = Arc::clone(self);
        // Stored as scanned, never re-minted: the QR already is the credential.
        let scanned = payload.token.clone();
        let label = target.label.clone();
        let endpoint = upsert.endpoint();
        self.dial(target, move |outcome| {
            if outcome.is_ok() {
                upsert.run();
                token.set(Zeroizing::new(String::new()));
                pending.set(None);
                controller.save_paired_credential(endpoint, label, scanned);
                landing.land();
            }
        });
    }

    /// `offer_credential` is set whenever remember is checked outside a pairing, saved endpoints
    /// included: that is where a revoked credential is replaced. A hand-picked `Pinned` carries
    /// no fingerprint and fails closed unless a matching stashed pairing supplies one.
    pub fn validated_target(&self) -> Result<ConnectTarget, AppFailure> {
        let default_port = default_port_for(self.trust_mode.get_untracked());
        let endpoint = parse_endpoint(&self.url.get_untracked(), default_port)?;
        let saved = self.saved.get_untracked();
        let known = saved
            .iter()
            .find(|server| server.host == endpoint.host && server.port == endpoint.port);
        let remember = self.remember.get_untracked();
        let pairing = self.pending_pairing.with(|pending| {
            pending
                .as_ref()
                .filter(|payload| pairing_covers(payload, &endpoint))
                .map(|payload| {
                    (
                        pairing_trust_mode(&payload.trust),
                        pinned_fingerprint(payload),
                    )
                })
        });
        let paired = pairing.is_some();
        let (trust_mode, fingerprint) =
            pairing.unwrap_or_else(|| (self.trust_mode.get_untracked(), None));
        Ok(ConnectTarget {
            label: known.map_or_else(|| endpoint.host.clone(), |server| server.label.clone()),
            offer_credential: !paired && remember,
            host: endpoint.host,
            port: endpoint.port,
            trust_mode,
            token: self.token.get_untracked(),
            remember,
            fingerprint,
        })
    }

    fn stores_credential_for(&self, host: &str, port: u16) -> bool {
        self.saved
            .get_untracked()
            .iter()
            .any(|server| server.host == host && server.port == port && server.has_credential)
    }

    /// `nav`/`offer_prompt` are lent, not held: both wrap `Rc`, and this must be `Send + Sync`.
    pub fn connect(&self, nav: &Nav, offer_prompt: OfferPrompt, gate_prompt: GatePrompt) {
        if self.busy_untracked() {
            return;
        }
        match self.validated_target() {
            Ok(target) => {
                if self.blocked(
                    &target_endpoint(&target),
                    ConnectRetry::Manual,
                    &gate_prompt,
                ) {
                    return;
                }
                self.clear_url_validation();
                let landing = self.landing(nav);
                let token = self.token;
                let offer = self.connection.credential_offer;
                let resolving = self.resolving_offer;
                let replacing = self.stores_credential_for(&target.host, target.port);
                let upsert = self.login_upsert(&target);
                self.dial(target, move |outcome| {
                    if outcome.is_err() {
                        return;
                    }
                    upsert.run();
                    token.set(Zeroizing::new(String::new()));
                    match offer.get_untracked() {
                        Some(label) => {
                            resolving.set(true);
                            offer_prompt(CredentialOffer { label, replacing });
                        }
                        None => landing.land(),
                    }
                });
            }
            Err(failure) => {
                debug_assert!(
                    matches!(failure, AppFailure::Network(_)),
                    "validated_target gained a failure source besides the URL parse"
                );
                // Kind only: `AppFailure`'s `Debug` echoes the typed URL.
                log::debug!("connect: form rejected: {}", failure_kind(&failure));
                self.reject_url(URL_ERROR_MESSAGE);
            }
        }
    }

    /// The add page saves through itself, not the connection's installed saver: that saver is
    /// `/`'s, still retained beneath the shell.
    pub fn accept_offer_then_navigate(self: &Arc<Self>, nav: &Nav) {
        let landing = self.offer_landing(nav);
        let resolving = self.resolving_offer;
        let finish = move || {
            resolving.set(false);
            landing.land();
        };
        if self.mode == ConnectMode::Home {
            self.connection.accept_credential_offer_with(finish);
            return;
        }
        let label = self.connection.credential_offer.get_untracked();
        self.connection.dismiss_credential_offer();
        let endpoint = self
            .connection
            .server_info
            .get_untracked()
            .map(|info| info.endpoint);
        match (label, endpoint) {
            (Some(label), Some(endpoint)) => self.begin_credential_save(endpoint, label, finish),
            _ => finish(),
        }
    }

    pub fn decline_offer_then_navigate(&self, nav: &Nav) {
        self.connection.dismiss_credential_offer();
        self.resolving_offer.set(false);
        self.offer_landing(nav).land();
    }

    fn login_upsert(&self, target: &ConnectTarget) -> LoginUpsert {
        LoginUpsert::new(
            &self.repository,
            &self.connection,
            &self.keyring,
            Some(self.saved),
            target,
        )
    }

    fn reload_into(repository: &SavedServers, saved: RwSignal<Vec<SavedServer>>) {
        match repository.list() {
            Ok(servers) => saved.set(servers),
            Err(failure) => app_toasts().raise(&failure),
        }
    }

    fn update_saved(&self, endpoint: &str, edit: impl FnOnce(&mut SavedServer)) -> Option<String> {
        let Ok(servers) = self.repository.list() else {
            return None;
        };
        let mut row = servers
            .into_iter()
            .find(|server| server.endpoint() == endpoint)?;
        let previous = row.token_name.clone();
        edit(&mut row);
        if let Err(failure) = self.repository.save(row) {
            app_toasts().raise(&failure);
            return previous;
        }
        Self::reload_into(&self.repository, self.saved);
        previous
    }

    pub fn begin_credential_save(
        self: &Arc<Self>,
        endpoint: String,
        label: String,
        on_done: impl FnOnce() + 'static,
    ) {
        if self.connection.is_read_only.get_untracked() {
            on_done();
            return;
        }
        if self.saving_credential.swap(true, Ordering::SeqCst) {
            on_done();
            return;
        }
        let controller = Arc::clone(self);
        spawn_local(async move {
            controller.save_session_credential(endpoint, label).await;
            controller.saving_credential.store(false, Ordering::SeqCst);
            on_done();
        });
    }

    /// Ordering: a denied gate revokes the just-minted token; the previous token is revoked
    /// only after the new one persists, so a mid-flow failure leaves the old one working.
    async fn save_session_credential(&self, endpoint: String, label: String) {
        let token_name = minted_token_name(&label);
        let minted = match self
            .tokens
            .create_token(CreateTokenRequest {
                name: token_name.clone(),
                read_only: false,
            })
            .await
        {
            Ok(minted) => minted,
            Err(failure) => {
                self.core.failures().emit(&failure);
                return;
            }
        };

        let status = self
            .gate_store(
                endpoint.clone(),
                minted.token.clone(),
                format!("Save credentials for {label}"),
            )
            .await;
        if status != CredentialStatus::Success {
            self.revoke_quietly(token_name).await;
            self.report(status);
            return;
        }

        let previous = self.update_saved(&endpoint, |row| {
            row.has_credential = true;
            row.token_name = Some(token_name);
        });
        if let Some(previous) = previous {
            self.revoke_quietly(previous).await;
        }
    }

    /// One prompt, no mint, silent whatever the gate answers: the session is already live.
    fn save_paired_credential(
        self: &Arc<Self>,
        endpoint: String,
        label: String,
        token: Zeroizing<String>,
    ) {
        let controller = Arc::clone(self);
        spawn_local(async move {
            let status = controller
                .gate_store(
                    endpoint.clone(),
                    token,
                    format!("Save paired credentials for {label}"),
                )
                .await;
            if status != CredentialStatus::Success {
                return;
            }
            controller.update_saved(&endpoint, |row| {
                row.has_credential = true;
                row.token_name = None;
            });
            controller.connection.dismiss_credential_offer();
        });
    }

    /// Callers own the no-verification confirmation and, ahead of it, the free-tier verdict
    /// ([`Self::saved_row_blocked`]). Ignored while busy.
    pub fn recover_and_connect(self: &Arc<Self>, server: SavedServer, nav: &Nav) {
        if self.busy_untracked() {
            return;
        }
        self.unlocking.set(true);
        let controller = Arc::clone(self);
        let nav = nav.clone();
        spawn_local(async move {
            controller.recover(server, nav).await;
        });
    }

    async fn recover(self: &Arc<Self>, server: SavedServer, nav: Nav) {
        let endpoint = server.endpoint();
        let unlocked = self
            .core
            .run(
                &self.unlock_credential,
                UnlockRequest {
                    server_key: endpoint.clone(),
                    reason: format!("Unlock {}", server.label),
                },
                RunOptions::default(),
            )
            .await
            .unwrap_or_else(|_| UnlockedCredential::refused(CredentialStatus::Failed));
        self.unlocking.set(false);

        let UnlockedCredential { status, token } = unlocked;
        match token {
            Some(token) if status == CredentialStatus::Success => {
                self.connect_recovered(&server, token, &nav);
            }
            _ => {
                let status = if status == CredentialStatus::Success {
                    CredentialStatus::NotFound
                } else {
                    status
                };
                if status == CredentialStatus::KeyInvalidated {
                    self.forget_unreadable(&endpoint).await;
                }
                self.report(status);
                if !status.is_silent() {
                    self.prefill(&server);
                }
            }
        }
    }

    fn connect_recovered(&self, server: &SavedServer, token: Zeroizing<String>, nav: &Nav) {
        let target = saved_target(server, token);
        let upsert = self.login_upsert(&target);
        let landing = self.landing(nav);
        self.dial(target, move |outcome| {
            if outcome.is_ok() {
                upsert.run();
                landing.land();
            }
        });
    }

    async fn forget_unreadable(&self, endpoint: &str) {
        self.keyring_unlock.forget_unreadable(endpoint).await;
        self.reload_saved();
    }

    pub fn biometrics_available(&self) -> bool {
        self.biometrics_available
    }

    pub fn dock_state(&self) -> DockState {
        self.refresh_saved_if_moved();
        if !self.biometrics_available {
            return DockState::Unavailable;
        }
        if self.unlocking.get() {
            return DockState::Unlocking;
        }
        self.dock_failure
            .get()
            .map_or(DockState::Idle, DockState::Failed)
    }

    /// Reads the saved list fresh, never the retained page's copy. The free-tier verdict comes
    /// first, so a block raises no biometric prompt.
    pub fn unlock_keyring_and_connect(self: &Arc<Self>, nav: &Nav, gate_prompt: GatePrompt) {
        if self.busy_untracked() {
            return;
        }
        self.reload_saved();
        let Some(target) = self
            .saved
            .get_untracked()
            .into_iter()
            .find(|row| row.has_credential)
        else {
            return;
        };
        if self.blocked(&target.endpoint(), ConnectRetry::Keyring, &gate_prompt) {
            return;
        }
        self.dock_failure.set(None);
        self.unlocking.set(true);
        let controller = Arc::clone(self);
        let nav = nav.clone();
        spawn_local(async move {
            controller.unlock_all_then_connect(target, nav).await;
        });
    }

    /// A vault re-enrollment can never be read again, so it is reset; `Failed` offers that reset
    /// behind a confirm. The target dials only if it still claims a held credential afterwards.
    async fn unlock_all_then_connect(self: &Arc<Self>, target: SavedServer, nav: Nav) {
        let unreadable = match self.keyring_unlock.unlock().await {
            KeyringUnlockOutcome::Unlocked { unreadable } => unreadable,
            KeyringUnlockOutcome::Refused(CredentialStatus::Canceled)
            | KeyringUnlockOutcome::Busy
            | KeyringUnlockOutcome::Superseded => {
                self.unlocking.set(false);
                return;
            }
            KeyringUnlockOutcome::Refused(CredentialStatus::KeyInvalidated) => {
                self.reset_credentials(VAULT_RESET_MESSAGE).await;
                self.unlocking.set(false);
                return;
            }
            KeyringUnlockOutcome::Refused(status) => {
                self.unlocking.set(false);
                self.fail_dock(status);
                return;
            }
            KeyringUnlockOutcome::ListUnavailable => {
                self.unlocking.set(false);
                self.dock_failure.set(Some(DockFailure {
                    message: SAVED_LIST_UNAVAILABLE_MESSAGE.to_string(),
                    offers_reset: false,
                }));
                app_toasts().show(SAVED_LIST_UNAVAILABLE_MESSAGE);
                return;
            }
        };
        self.reload_saved();
        let endpoint = target.endpoint();
        let row = self
            .saved
            .get_untracked()
            .into_iter()
            .find(|row| row.endpoint() == endpoint && row.has_credential);
        let token = row.as_ref().and_then(|_| self.keyring.token_for(&endpoint));
        self.unlocking.set(false);
        let (Some(target), Some(token)) = (row, token) else {
            self.fail_dock(if unreadable.contains(&endpoint) {
                CredentialStatus::KeyInvalidated
            } else {
                CredentialStatus::NotFound
            });
            self.prefill(&target);
            return;
        };
        self.connect_recovered(&target, token, &nav);
    }

    fn fail_dock(&self, status: CredentialStatus) {
        self.dock_failure
            .set(status.message().map(|message| DockFailure {
                message: message.to_string(),
                offers_reset: status == CredentialStatus::Failed,
            }));
        self.report(status);
    }

    pub fn reset_saved_credentials(self: &Arc<Self>) {
        if self.busy_untracked() {
            return;
        }
        self.unlocking.set(true);
        let controller = Arc::clone(self);
        spawn_local(async move {
            controller.reset_credentials(MANUAL_RESET_MESSAGE).await;
            controller.unlocking.set(false);
        });
    }

    async fn reset_credentials(&self, message: &str) {
        if let Err(failure) = self.credentials.reset_vault().await {
            app_toasts().raise(&failure);
            self.dock_failure.set(Some(DockFailure {
                message: failure.user_message(),
                offers_reset: true,
            }));
            return;
        }
        if let Ok(rows) = self.repository.list() {
            for mut row in rows {
                if !row.has_credential && row.token_name.is_none() {
                    continue;
                }
                row.has_credential = false;
                row.token_name = None;
                if let Err(failure) = self.repository.save(row) {
                    app_toasts().raise(&failure);
                }
            }
        }
        self.reload_saved();
        self.keyring.lock();
        self.dock_failure.set(Some(DockFailure {
            message: message.to_string(),
            offers_reset: false,
        }));
    }

    pub fn forget_saved_server(self: &Arc<Self>, server: SavedServer) {
        let controller = Arc::clone(self);
        spawn_local(async move {
            controller.forget_saved(server).await;
        });
    }

    /// `then` runs only once the last row is gone: the retry's verdict reads the same list.
    pub fn forget_saved_servers_then(
        self: &Arc<Self>,
        rows: Vec<SavedServer>,
        then: Box<dyn FnOnce()>,
    ) {
        let controller = Arc::clone(self);
        spawn_local(async move {
            for server in rows {
                controller.forget_saved(server).await;
            }
            then();
        });
    }

    /// Ordering: secret first, row second. Locked, the row goes only if the legacy delete
    /// succeeded: the row is the secret's own key.
    async fn forget_saved(&self, server: SavedServer) {
        let endpoint = server.endpoint();
        if self.keyring.is_unlocked() {
            self.evict_forgotten(&endpoint).await;
        } else if let Err(failure) = self.credentials.delete(endpoint.clone()).await {
            app_toasts().raise(&failure);
            return;
        }
        if let Err(failure) = self.repository.remove(&endpoint) {
            app_toasts().raise(&failure);
            self.update_saved(&endpoint, |row| {
                row.has_credential = false;
                row.token_name = None;
            });
            return;
        }
        self.reload_saved();
    }

    /// The handle drops the token whatever the rewrite said: a token left there would ride the
    /// next snapshot-based vault write back in.
    async fn evict_forgotten(&self, endpoint: &str) {
        let mut snapshot = self.keyring.snapshot();
        let evicted = self
            .credentials
            .delete_from_vault(endpoint.to_string(), &mut snapshot)
            .await;
        self.keyring.remove(endpoint);
        if let Err(failure) = evicted {
            app_toasts().raise(&failure);
            if let Err(failure) = self.credentials.delete(endpoint.to_string()).await {
                log::warn!("credentials: legacy delete after a refused eviction failed: {failure}");
            }
        }
    }

    /// Unlocked, the handle gains the token iff it is still unlocked once the prompt settles: a
    /// lock during the prompt keeps it out, any other change still takes it, so the handle
    /// never lacks a token the vault holds.
    async fn gate_store(
        &self,
        server_key: String,
        token: Zeroizing<String>,
        reason: String,
    ) -> CredentialStatus {
        let request = StoreRequest {
            server_key: server_key.clone(),
            token: token.clone(),
            reason,
        };
        if self.keyring.is_unlocked() {
            let mut snapshot = self.keyring.snapshot();
            let status = self
                .save_credential
                .run_in_vault(request, &mut snapshot)
                .await;
            if status == CredentialStatus::Success && self.keyring.is_unlocked() {
                self.keyring.insert(server_key, token);
            }
            return status;
        }
        self.core
            .run(&self.save_credential, request, RunOptions::default())
            .await
            .unwrap_or(CredentialStatus::Failed)
    }

    fn report(&self, status: CredentialStatus) {
        if let Some(message) = status.message() {
            app_toasts().show(message);
        }
    }

    async fn revoke_quietly(&self, name: String) {
        if let Err(failure) = self.tokens.revoke_token(name).await {
            log::warn!("credentials: revoking a replaced token failed: {failure:?}");
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyringUnlockOutcome {
    Unlocked {
        unreadable: Vec<String>,
    },
    Refused(CredentialStatus),
    Busy,
    Superseded,
    /// No prompt was raised: a prune over an unpersisted list could drop every real entry.
    ListUnavailable,
}

pub struct KeyringUnlock {
    repository: SavedServers,
    credentials: Credentials,
    unlock_credential: UnlockCredential,
    connection: Arc<ConnectionController>,
    keyring: KeyringHandle,
}

impl KeyringUnlock {
    pub fn new(
        repository: SavedServers,
        credentials: Credentials,
        connection: Arc<ConnectionController>,
        keyring: KeyringHandle,
    ) -> Self {
        Self {
            unlock_credential: UnlockCredential::new(Arc::clone(&credentials)),
            repository,
            credentials,
            connection,
            keyring,
        }
    }

    pub fn keyring(&self) -> &KeyringHandle {
        &self.keyring
    }

    /// Only rows claiming a credential reach `unlock_all`; every other vault entry is pruned.
    pub async fn unlock(&self) -> KeyringUnlockOutcome {
        let Some(_ticket) = self.keyring.begin_unlock() else {
            return KeyringUnlockOutcome::Busy;
        };
        let Some(credentialed) = self.credentialed_rows() else {
            return KeyringUnlockOutcome::ListUnavailable;
        };
        let epoch = self.keyring.lock_epoch();
        let endpoints: Vec<String> = credentialed.iter().map(SavedServer::endpoint).collect();
        let UnlockedKeyring {
            status,
            keyring,
            unreadable,
            ..
        } = self
            .unlock_credential
            .run_all(&unlock_reason(&credentialed), &endpoints)
            .await;
        let keyring = match (status, keyring) {
            (CredentialStatus::Success, Some(keyring)) => keyring,
            (CredentialStatus::Success, None) => {
                return KeyringUnlockOutcome::Refused(CredentialStatus::Failed);
            }
            (status, _) => return KeyringUnlockOutcome::Refused(status),
        };
        if self.keyring.lock_epoch() != epoch {
            return KeyringUnlockOutcome::Superseded;
        }
        for endpoint in &unreadable {
            self.forget_unreadable(endpoint).await;
        }
        if self.keyring.lock_epoch() != epoch {
            return KeyringUnlockOutcome::Superseded;
        }
        self.keyring.publish(keyring);
        KeyringUnlockOutcome::Unlocked { unreadable }
    }

    fn credentialed_rows(&self) -> Option<Vec<SavedServer>> {
        if !self.repository.is_persistent() {
            log::warn!("keyring: refusing to unlock over a saved list that is not persisted");
            return None;
        }
        match self.repository.list() {
            Ok(rows) => Some(rows.into_iter().filter(|row| row.has_credential).collect()),
            Err(failure) => {
                log::warn!("keyring: the saved list could not be read: {failure}");
                None
            }
        }
    }

    pub async fn forget_unreadable(&self, endpoint: &str) {
        if let Err(failure) = self.credentials.delete(endpoint.to_string()).await {
            log::warn!("credentials: dropping an invalidated entry failed: {failure:?}");
        }
        let Ok(rows) = self.repository.list() else {
            return;
        };
        let Some(mut row) = rows.into_iter().find(|row| row.endpoint() == endpoint) else {
            return;
        };
        row.has_credential = false;
        row.token_name = None;
        if let Err(failure) = self.repository.save(row) {
            app_toasts().raise(&failure);
        }
    }

    pub fn switch_to_saved(
        &self,
        server: &SavedServer,
        token: Zeroizing<String>,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        self.switch_saved(server, token, false, on_done);
    }

    pub fn switch_to_saved_quiet(
        &self,
        server: &SavedServer,
        token: Zeroizing<String>,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        self.switch_saved(server, token, true, on_done);
    }

    fn switch_saved(
        &self,
        server: &SavedServer,
        token: Zeroizing<String>,
        quiet: bool,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        let target = saved_target(server, token);
        let upsert = LoginUpsert::new(
            &self.repository,
            &self.connection,
            &self.keyring,
            None,
            &target,
        );
        let on_done = move |outcome: Result<(), AppFailure>| {
            if outcome.is_ok() {
                upsert.run();
            }
            on_done(outcome);
        };
        if quiet {
            self.connection.switch_to_quiet(target, on_done);
        } else {
            self.connection.switch_to(target, on_done);
        }
    }
}

fn saved_target(server: &SavedServer, token: Zeroizing<String>) -> ConnectTarget {
    ConnectTarget {
        host: server.host.clone(),
        port: server.port,
        trust_mode: server.trust_mode,
        label: server.label.clone(),
        offer_credential: false,
        token,
        remember: true,
        fingerprint: server.fingerprint.clone(),
    }
}

struct ControllerCredentialSaver {
    controller: std::sync::Weak<ConnectController>,
}

impl CredentialSaver for ControllerCredentialSaver {
    fn save_credential(&self, endpoint: String, label: String, on_done: Box<dyn FnOnce()>) {
        match self.controller.upgrade() {
            Some(controller) => controller.begin_credential_save(endpoint, label, on_done),
            None => on_done(),
        }
    }
}

struct LoginUpsert {
    repository: SavedServers,
    connection: Arc<ConnectionController>,
    keyring: KeyringHandle,
    saved: Option<RwSignal<Vec<SavedServer>>>,
    host: String,
    port: u16,
    trust_mode: TrustMode,
    label: String,
    fingerprint: Option<String>,
    previous: Option<String>,
}

impl LoginUpsert {
    fn new(
        repository: &SavedServers,
        connection: &Arc<ConnectionController>,
        keyring: &KeyringHandle,
        saved: Option<RwSignal<Vec<SavedServer>>>,
        target: &ConnectTarget,
    ) -> Self {
        Self {
            repository: Arc::clone(repository),
            connection: Arc::clone(connection),
            keyring: keyring.clone(),
            saved,
            host: target.host.clone(),
            port: target.port,
            trust_mode: target.trust_mode,
            label: target.label.clone(),
            fingerprint: target.fingerprint.clone(),
            previous: connection
                .connected
                .get_untracked()
                .then(|| connection.server_info.get_untracked())
                .flatten()
                .map(|info| info.endpoint),
        }
    }

    /// A row holding no credential can only reopen the add page, so switching away from it
    /// forgets it. Runs only after a successful switch.
    fn forget_previous_without_credential(&self) {
        let Some(previous) = self.previous.as_deref() else {
            return;
        };
        if previous == self.endpoint() {
            return;
        }
        let Ok(rows) = self.repository.list() else {
            return;
        };
        let forgettable = rows
            .iter()
            .any(|row| row.endpoint() == previous && !row.has_credential);
        if forgettable && let Err(failure) = self.repository.remove(previous) {
            app_toasts().raise(&failure);
        }
    }

    fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Credential fields carry forward from any existing row. A row written with no credential
    /// claim takes the handle's token out, so no later vault write resurrects it.
    fn run(&self) {
        let existing = self
            .repository
            .list()
            .unwrap_or_default()
            .into_iter()
            .find(|server| server.endpoint() == self.endpoint());
        let server = SavedServer {
            host: self.host.clone(),
            port: self.port,
            trust_mode: self.trust_mode,
            label: self.label.clone(),
            last_used_ms: epoch_millis(),
            read_only: self.connection.is_read_only.get_untracked(),
            has_credential: existing.as_ref().is_some_and(|row| row.has_credential),
            token_name: existing.and_then(|row| row.token_name),
            // Only `Pinned` carries a fingerprint — a downgraded row must not keep a stale one.
            fingerprint: match self.trust_mode {
                TrustMode::Pinned => self.fingerprint.clone(),
                _ => None,
            },
        };
        let claims_credential = server.has_credential;
        if let Err(failure) = self.repository.save(server) {
            app_toasts().raise(&failure);
            return;
        }
        if !claims_credential {
            self.keyring.remove(&self.endpoint());
        }
        self.forget_previous_without_credential();
        if let Some(saved) = self.saved {
            ConnectController::reload_into(&self.repository, saved);
        }
    }
}

fn target_endpoint(target: &ConnectTarget) -> String {
    format!("{}:{}", target.host, target.port)
}

fn unlock_reason(saved: &[SavedServer]) -> String {
    match saved {
        [] => "Unlock saved servers".to_string(),
        [only] => format!("Unlock {}", only.label),
        rows => format!("Unlock {} servers", rows.len()),
    }
}

fn minted_token_name(label: &str) -> String {
    format!(
        "{TOKEN_NAME_PREFIX}-{}-{}",
        slug(label),
        base36(epoch_millis())
    )
}

fn slug(label: &str) -> String {
    let mut out = String::new();
    for ch in label.chars() {
        if out.len() >= TOKEN_LABEL_MAX {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let trimmed = out.trim_end_matches('-');
    if trimmed.is_empty() {
        return "device".to_string();
    }
    trimmed.to_string()
}

fn base36(value: i64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value <= 0 {
        return "0".to_string();
    }
    let mut value = value;
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("every digit is ASCII")
}

impl AsRef<ControllerCore<AppFailure>> for ConnectController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

fn default_port_for(mode: TrustMode) -> u16 {
    match mode {
        TrustMode::System => PUBLIC_CA_DEFAULT_PORT,
        TrustMode::NoVerification | TrustMode::Pinned => MUXRD_DEFAULT_PORT,
    }
}

/// `parse_endpoint` cannot split the bracketed form, so [`pairing_covers`] strips brackets.
fn pairing_endpoint(payload: &PairingPayload) -> String {
    if payload.host.contains(':') {
        return format!("[{}]:{}", payload.host, payload.port);
    }
    format!("{}:{}", payload.host, payload.port)
}

fn pairing_trust_mode(trust: &PairingTrust) -> TrustMode {
    match trust {
        PairingTrust::Pin { .. } => TrustMode::Pinned,
        PairingTrust::Ca => TrustMode::System,
    }
}

fn pinned_fingerprint(payload: &PairingPayload) -> Option<String> {
    match &payload.trust {
        PairingTrust::Pin { fp_hex } => Some(fp_hex.clone()),
        PairingTrust::Ca => None,
    }
}

fn pairing_covers(payload: &PairingPayload, endpoint: &Endpoint) -> bool {
    endpoint.port == payload.port && unbracketed(&endpoint.host) == payload.host
}

fn unbracketed(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

fn pairing_target(payload: &PairingPayload) -> ConnectTarget {
    let endpoint = pairing_endpoint(payload);
    ConnectTarget {
        label: if payload.label.is_empty() {
            endpoint
        } else {
            payload.label.clone()
        },
        host: payload.host.clone(),
        port: payload.port,
        trust_mode: pairing_trust_mode(&payload.trust),
        offer_credential: false,
        token: payload.token.clone(),
        remember: true,
        fingerprint: pinned_fingerprint(payload),
    }
}

/// The failure's variant name alone — safe to log where its payload is not.
fn failure_kind(failure: &AppFailure) -> &'static str {
    match failure {
        AppFailure::Network(_) => "Network",
        AppFailure::Auth(_) => "Auth",
        AppFailure::Storage(_) => "Storage",
        AppFailure::Internal(_) => "Internal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned(host: &str, port: u16, fp: &str) -> PairingPayload {
        PairingPayload {
            host: host.to_string(),
            port,
            token: Zeroizing::new("scanned-token".to_string()),
            read_only: false,
            label: "Home rig".to_string(),
            trust: PairingTrust::Pin {
                fp_hex: fp.to_string(),
            },
        }
    }

    #[test]
    fn a_scanned_endpoint_brackets_an_ipv6_literal_and_nothing_else() {
        assert_eq!(
            pairing_endpoint(&pinned("192.168.1.100", 50051, "ab")),
            "192.168.1.100:50051"
        );
        assert_eq!(
            pairing_endpoint(&pinned("fd00::1", 50051, "ab")),
            "[fd00::1]:50051",
            "an unbracketed IPv6 literal would read as a host with a port of \
             its own last colon"
        );
    }

    #[test]
    fn a_pairing_covers_exactly_the_address_it_was_scanned_for() {
        let payload = pinned("192.168.1.100", 50051, "ab");
        let at = |raw: &str| {
            parse_endpoint(raw, default_port_for(TrustMode::System))
                .is_ok_and(|endpoint| pairing_covers(&payload, &endpoint))
        };
        assert!(at("192.168.1.100:50051"), "the prefilled text itself");
        assert!(!at("192.168.1.100:50052"), "another port");
        assert!(!at("192.168.1.101:50051"), "another host");
    }

    #[test]
    fn a_bracketed_ipv6_prefill_still_covers_its_own_payload() {
        let payload = pinned("fd00::1", 50051, "ab");
        let endpoint = parse_endpoint(
            &pairing_endpoint(&payload),
            default_port_for(TrustMode::System),
        )
        .expect("host:port parses");
        assert!(pairing_covers(&payload, &endpoint));
    }

    #[test]
    fn a_pinned_payload_dials_pinned_with_its_fingerprint_and_offers_nothing() {
        let fp = "ab".repeat(32);
        let target = pairing_target(&pinned("192.168.1.100", 50051, &fp));
        assert_eq!(target.trust_mode, TrustMode::Pinned);
        assert_eq!(target.fingerprint.as_deref(), Some(fp.as_str()));
        assert_eq!(target.token.as_str(), "scanned-token");
        assert!(target.remember, "a paired login is a keeper");
        assert!(
            !target.offer_credential,
            "the manual save offer must never fire for a scanned credential"
        );
        assert_eq!(target.label, "Home rig");
    }

    #[test]
    fn a_ca_payload_dials_system_trust_with_no_pin() {
        let mut payload = pinned("rig.example.com", 50051, "ab");
        payload.trust = PairingTrust::Ca;
        payload.label = String::new();
        let target = pairing_target(&payload);
        assert_eq!(target.trust_mode, TrustMode::System);
        assert_eq!(target.fingerprint, None);
        assert_eq!(
            target.label, "rig.example.com:50051",
            "a QR that named no label still labels its saved row with something"
        );
    }

    #[test]
    fn the_keyring_prompt_names_one_row_and_counts_several() {
        let row = |label: &str| SavedServer {
            host: "rig".to_string(),
            port: 1,
            trust_mode: TrustMode::System,
            label: label.to_string(),
            last_used_ms: 0,
            read_only: false,
            has_credential: true,
            token_name: None,
            fingerprint: None,
        };
        assert_eq!(unlock_reason(&[row("devbox")]), "Unlock devbox");
        assert_eq!(unlock_reason(&[row("a"), row("b")]), "Unlock 2 servers");
    }

    #[test]
    fn default_port_for_every_trust_mode() {
        assert_eq!(default_port_for(TrustMode::System), PUBLIC_CA_DEFAULT_PORT);
        assert_eq!(
            default_port_for(TrustMode::NoVerification),
            MUXRD_DEFAULT_PORT
        );
        assert_eq!(default_port_for(TrustMode::Pinned), MUXRD_DEFAULT_PORT);
    }

    #[test]
    fn a_bare_host_validates_to_each_trust_modes_own_default_port() {
        assert_eq!(
            parse_endpoint("demo.muxr.app", default_port_for(TrustMode::System)),
            Ok(Endpoint {
                host: "demo.muxr.app".to_string(),
                port: PUBLIC_CA_DEFAULT_PORT,
            })
        );
        assert_eq!(
            parse_endpoint("demo.muxr.app", default_port_for(TrustMode::NoVerification)),
            Ok(Endpoint {
                host: "demo.muxr.app".to_string(),
                port: MUXRD_DEFAULT_PORT,
            })
        );
    }
}
