//! [`ConnectionController`], the app's one connection handle, and its [`ConnectionBackend`] seam.
//! The login token rides `Zeroizing` until the `Login` request is built; that request's prost
//! `String` copy is the one unscrubbed residual. Disconnect clears the shared session token first.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use clean_signals::ControllerCore;
use clean_signals_frust::{expect_controller, provide_controller};
use frust::{GetUntracked, RwSignal, Set, Update, WithUntracked, spawn, spawn_local};
use tokio::sync::oneshot;
use zeroize::{Zeroize, Zeroizing};

use crate::failure::AppFailure;
use crate::net::proto::{Backend, Empty, LoginRequest, VersionInfo};
use crate::net::{MuxrClient, TrustConfig, error};

/// Server supports mouse frames in `AttachTerminal`; unknown flags carry through unchanged.
pub const CAP_MOUSE_INPUT: &str = "mouse-input";

/// `WatchAgents` delivers live agent changes; absent, a client polls `ListAgents`.
pub const CAP_AGENTS_STREAM: &str = "agents-stream";

const UNKNOWN_VERSION: &str = "unknown";

const MISSING_PIN: &str = "a pinned connection needs a certificate fingerprint";
const DIAL_ABANDONED: &str = "the connect task ended without an answer";
const SESSION_EXPIRED: &str = "the server rejected the session token";

#[derive(Clone, Debug, PartialEq)]
pub struct ServerInfo {
    pub endpoint: String,
    pub trust_mode: TrustMode,
    pub tls_label: String,
    pub version: String,
    pub latency_ms: u32,
    pub backend_versions: Vec<BackendVersion>,
    pub available_backends: Vec<ServerBackend>,
    pub capabilities: BTreeSet<String>,
    pub is_read_only: bool,
}

impl ServerInfo {
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServerBackend {
    Zellij,
    Herdr,
}

impl ServerBackend {
    pub fn label(self) -> &'static str {
        match self {
            ServerBackend::Zellij => "zellij",
            ServerBackend::Herdr => "herdr",
        }
    }

    fn from_wire(value: i32) -> Option<Self> {
        match Backend::try_from(value) {
            Ok(Backend::Zellij) => Some(ServerBackend::Zellij),
            Ok(Backend::Herdr) => Some(ServerBackend::Herdr),
            Ok(Backend::Unspecified) | Err(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendVersion {
    pub backend: ServerBackend,
    pub version: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustMode {
    System,
    NoVerification,
    Pinned,
}

impl TrustMode {
    fn label(self) -> &'static str {
        match self {
            TrustMode::System => "System trust",
            TrustMode::NoVerification => "No verification",
            TrustMode::Pinned => "Pinned certificate",
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct ConnectTarget {
    pub host: String,
    pub port: u16,
    pub trust_mode: TrustMode,
    pub label: String,
    /// Eligibility only: a successful connect may offer saving this target's credential.
    pub offer_credential: bool,
    /// The one secret that crosses the wire; dropping the target scrubs it.
    pub token: Zeroizing<String>,
    pub remember: bool,
    /// 64 lowercase hex characters; a [`TrustMode::Pinned`] target without one never dials.
    pub fingerprint: Option<String>,
}
impl fmt::Debug for ConnectTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectTarget")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("trust_mode", &self.trust_mode)
            .field("label", &self.label)
            .field("offer_credential", &self.offer_credential)
            .field("token", &"<redacted>")
            .field("remember", &self.remember)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

pub type ConnectOutcome = Result<ServerInfo, AppFailure>;

pub type ConnectFuture = Pin<Box<dyn Future<Output = ConnectOutcome> + Send>>;

pub enum ConnectRun {
    Ready(ConnectOutcome),
    Pending(ConnectFuture),
}

pub trait ConnectionBackend: Send + Sync + 'static {
    /// A dial superseded before it lands must drop what it opened rather than install it.
    fn begin_connect(&self, target: ConnectTarget) -> ConnectRun;

    /// Idempotent.
    fn disconnect(&self);

    /// `None` under the mock or while nothing is open.
    fn client(&self) -> Option<MuxrClient> {
        None
    }

    /// Single-flight; with nothing open it succeeds without dialing.
    fn reconnect(&self) -> ReconnectFuture {
        Box::pin(async { Ok(()) })
    }
}

pub type ReconnectFuture = Pin<Box<dyn Future<Output = Result<(), AppFailure>> + Send>>;

pub trait CredentialSaver: Send + Sync {
    /// `endpoint` is `host:port`; `on_done` runs once the save settles, however it settled.
    fn save_credential(&self, endpoint: String, label: String, on_done: Box<dyn FnOnce()>);
}

pub trait SessionRenewer: Send + Sync {
    /// `true` hands the renewer every user-visible outcome; further expiries are ignored until the
    /// next dial settles or a teardown. `false` (nothing saved for `endpoint`) logs out as usual.
    fn renew(&self, endpoint: &str) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Report {
    Emit,
    Quiet,
}

/// The `connect` feature's keyring, named by contract rather than type.
pub trait KeyringLock {
    fn lock_keyring(&self);
}

const MOCK_LATENCY_MS: u32 = 36;
const MOCK_SERVER_VERSION: &str = "zellij 0.44.3";
const MOCK_ZELLIJ_VERSION: &str = "0.44.3";

/// Never touches a socket; fails only when scripted by [`fail_next_mock_dials_for_tests`].
pub struct MockConnectionBackend;

impl ConnectionBackend for MockConnectionBackend {
    fn begin_connect(&self, target: ConnectTarget) -> ConnectRun {
        if take_mock_dial_failure() {
            return ConnectRun::Ready(Err(AppFailure::Network(MOCK_DIAL_REFUSED.to_string())));
        }
        let info = mock_server_info(&target);
        if let Some(held) = take_mock_dial_hold() {
            return ConnectRun::Pending(Box::pin(async move {
                if held.await.unwrap_or(false) {
                    Ok(info)
                } else {
                    Err(AppFailure::Network(MOCK_DIAL_REFUSED.to_string()))
                }
            }));
        }
        ConnectRun::Ready(Ok(info))
    }

    fn disconnect(&self) {}
}

#[cfg(test)]
struct LiveClientBackend {
    client: MuxrClient,
    live: AtomicBool,
}

#[cfg(test)]
impl ConnectionBackend for LiveClientBackend {
    fn begin_connect(&self, target: ConnectTarget) -> ConnectRun {
        let run = MockConnectionBackend.begin_connect(target);
        if matches!(run, ConnectRun::Ready(Ok(_))) {
            self.live.store(true, Ordering::SeqCst);
        }
        run
    }

    fn disconnect(&self) {
        self.live.store(false, Ordering::SeqCst);
    }

    fn client(&self) -> Option<MuxrClient> {
        self.live
            .load(Ordering::SeqCst)
            .then(|| self.client.clone())
    }
}

fn mock_server_info(target: &ConnectTarget) -> ServerInfo {
    ServerInfo {
        endpoint: endpoint_of(target),
        trust_mode: target.trust_mode,
        tls_label: target.trust_mode.label().to_string(),
        version: MOCK_SERVER_VERSION.to_string(),
        latency_ms: MOCK_LATENCY_MS,
        backend_versions: vec![BackendVersion {
            backend: ServerBackend::Zellij,
            version: MOCK_ZELLIJ_VERSION.to_string(),
        }],
        available_backends: vec![ServerBackend::Zellij],
        capabilities: BTreeSet::from([CAP_MOUSE_INPUT.to_string()]),
        is_read_only: false,
    }
}

pub struct NetConnectionBackend {
    live: Arc<Mutex<LiveSlot>>,
}

/// A dial installs its client only while its own ticket is still the newest.
#[derive(Default)]
struct LiveSlot {
    client: Option<MuxrClient>,
    ticket: u64,
}

impl NetConnectionBackend {
    pub fn new() -> Self {
        Self {
            live: Arc::new(Mutex::new(LiveSlot::default())),
        }
    }
    fn slot(&self) -> MutexGuard<'_, LiveSlot> {
        lock_slot(&self.live)
    }
}

fn lock_slot(live: &Mutex<LiveSlot>) -> MutexGuard<'_, LiveSlot> {
    live.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Default for NetConnectionBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionBackend for NetConnectionBackend {
    fn begin_connect(&self, target: ConnectTarget) -> ConnectRun {
        let trust = match trust_config_for(&target) {
            Ok(trust) => trust,
            Err(failure) => return ConnectRun::Ready(Err(failure)),
        };
        let ticket = {
            let mut slot = self.slot();
            slot.ticket += 1;
            slot.ticket
        };
        ConnectRun::Pending(Box::pin(open_session(
            Arc::clone(&self.live),
            ticket,
            target,
            trust,
        )))
    }

    fn disconnect(&self) {
        let taken = {
            let mut slot = self.slot();
            slot.ticket += 1;
            slot.client.take()
        };
        let Some(client) = taken else {
            return;
        };
        // Clearing the shared token closes the authenticated surface for every clone, not just this one.
        client.clear_session_token();
        drop(client);
    }

    fn client(&self) -> Option<MuxrClient> {
        self.slot().client.clone()
    }

    fn reconnect(&self) -> ReconnectFuture {
        match self.client() {
            Some(client) => Box::pin(async move { client.reconnect().await }),
            None => Box::pin(async { Ok(()) }),
        }
    }
}

fn trust_config_for(target: &ConnectTarget) -> Result<TrustConfig, AppFailure> {
    match target.trust_mode {
        TrustMode::System => Ok(TrustConfig::SystemCa),
        TrustMode::NoVerification => Ok(TrustConfig::InsecureAcceptAny),
        TrustMode::Pinned => {
            let fingerprint = target
                .fingerprint
                .as_deref()
                .ok_or_else(|| AppFailure::Network(MISSING_PIN.to_string()))?;
            Ok(TrustConfig::pinned_from_hex(fingerprint)?)
        }
    }
}

/// Keeps the client only while `ticket` is still the newest.
async fn open_session(
    live: Arc<Mutex<LiveSlot>>,
    ticket: u64,
    mut target: ConnectTarget,
    trust: TrustConfig,
) -> ConnectOutcome {
    let client = MuxrClient::connect(&target.host, target.port, trust).await?;

    let request = LoginRequest {
        auth_token: target.token.as_str().to_owned(),
        remember_me: target.remember,
    };
    // Drop the app's own token copy as soon as the request carries it.
    drop(std::mem::take(&mut target.token));

    let mut login = client
        .unauth()
        .login(request)
        .await
        .map_err(|status| error::from_status(&status))?
        .into_inner();
    client.set_session_token(&login.session_token);
    login.session_token.zeroize();

    let started = Instant::now();
    let version = client
        .unauth()
        .get_version(Empty {})
        .await
        .map_err(|status| error::from_status(&status))?
        .into_inner();
    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

    let info = server_info(&target, &version, login.is_read_only, latency_ms);
    let mut slot = lock_slot(&live);
    if slot.ticket == ticket {
        slot.client = Some(client);
    } else {
        // Superseded: installing would overwrite the newer connection's client.
        client.clear_session_token();
    }
    Ok(info)
}

fn server_info(
    target: &ConnectTarget,
    version: &VersionInfo,
    is_read_only: bool,
    latency_ms: u32,
) -> ServerInfo {
    let backend_versions: Vec<BackendVersion> = version
        .backends
        .iter()
        .filter_map(|entry| {
            ServerBackend::from_wire(entry.backend).map(|backend| BackendVersion {
                backend,
                version: entry.version.clone(),
            })
        })
        .collect();

    ServerInfo {
        endpoint: endpoint_of(target),
        trust_mode: target.trust_mode,
        tls_label: target.trust_mode.label().to_string(),
        version: multiplexer_version(version, &backend_versions),
        latency_ms,
        available_backends: version
            .available_backends
            .iter()
            .filter_map(|value| ServerBackend::from_wire(*value))
            .collect(),
        backend_versions,
        capabilities: version.capabilities.iter().cloned().collect(),
        is_read_only,
    }
}

/// The first detected backend, or the wire's back-compat zellij field when none is reported.
fn multiplexer_version(version: &VersionInfo, backends: &[BackendVersion]) -> String {
    if let Some(primary) = backends.first() {
        return format!("{} {}", primary.backend.label(), primary.version);
    }
    if version.zellij_version.is_empty() {
        return UNKNOWN_VERSION.to_string();
    }
    format!(
        "{} {}",
        ServerBackend::Zellij.label(),
        version.zellij_version
    )
}

/// `host:port`, the one spelling [`ServerInfo::endpoint`] uses.
fn endpoint_of(target: &ConnectTarget) -> String {
    format!("{}:{}", target.host, target.port)
}

pub struct ConnectionController {
    core: ControllerCore<AppFailure>,
    backend: Arc<dyn ConnectionBackend>,
    /// Bumped by every connect and teardown; a coordinator whose captured value differs drops its result.
    generation: AtomicU64,

    pub connected: RwSignal<bool>,
    /// A second connect while this is set is ignored.
    pub connecting: RwSignal<bool>,
    pub server_info: RwSignal<Option<ServerInfo>>,
    pub is_read_only: RwSignal<bool>,
    /// Bumped by every published connect and teardown; per-server screen state resets on it.
    pub connection_epoch: RwSignal<u64>,
    /// A tick mid-round-trip is dropped, not queued: a queued reading would already be stale.
    pinging: AtomicBool,
    /// Written by [`Self::settle`] before its `on_done`, so Connect can hold its redirect on it.
    pub credential_offer: RwSignal<Option<String>>,
    credential_saver: Mutex<Option<Arc<dyn CredentialSaver>>>,
    session_renewer: Mutex<Option<Arc<dyn SessionRenewer>>>,
    /// A burst of failing calls asks the renewer once; cleared by the next settled dial and teardown.
    renewing: AtomicBool,
}

impl ConnectionController {
    fn new(backend: Arc<dyn ConnectionBackend>) -> Self {
        Self {
            core: ControllerCore::new(),
            backend,
            generation: AtomicU64::new(0),
            connected: RwSignal::new(false),
            connecting: RwSignal::new(false),
            connection_epoch: RwSignal::new(0),
            server_info: RwSignal::new(None),
            is_read_only: RwSignal::new(false),
            pinging: AtomicBool::new(false),
            credential_offer: RwSignal::new(None),
            credential_saver: Mutex::new(None),
            session_renewer: Mutex::new(None),
            renewing: AtomicBool::new(false),
        }
    }

    /// `#[cfg(test)]`-only, so [`provide_connection_controller`] stays the one production constructor.
    #[cfg(test)]
    pub(crate) fn detached() -> Self {
        Self::new(Arc::new(MockConnectionBackend))
    }

    #[cfg(test)]
    pub(crate) fn with_backend(backend: Arc<dyn ConnectionBackend>) -> Self {
        Self::new(backend)
    }

    #[cfg(test)]
    pub(crate) fn with_live_client_for_tests(client: MuxrClient) -> Arc<Self> {
        let target = ConnectTarget {
            host: client.host().to_string(),
            port: client.port(),
            trust_mode: TrustMode::NoVerification,
            label: client.host().to_string(),
            offer_credential: false,
            token: Zeroizing::new(String::new()),
            remember: false,
            fingerprint: None,
        };
        let controller = Arc::new(Self::new(Arc::new(LiveClientBackend {
            client,
            live: AtomicBool::new(false),
        })));
        controller.connect(target);
        controller
    }

    /// Named rather than the [`AsRef`] impl: `Arc<ConnectionController>` has its own `as_ref`.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    pub fn client(&self) -> Option<MuxrClient> {
        self.backend.client()
    }

    /// An untracked read, for a caller deciding once rather than a build.
    pub fn supports(&self, capability: &str) -> bool {
        self.server_info
            .try_with_untracked(|info| info.as_ref().is_some_and(|info| info.supports(capability)))
            .unwrap_or(false)
    }

    /// Single-flight, so a concurrent caller collapses into the same dial; `Ok` while disconnected.
    pub async fn reconnect(&self) -> Result<(), AppFailure> {
        self.backend.reconnect().await
    }

    pub fn connect(self: &Arc<Self>, target: ConnectTarget) {
        self.connect_with(target, |_| {});
    }

    /// `on_done` runs on the UI thread (before return under the mock) and is not the failure path:
    /// failures go to [`Self::core`]'s sink. Ignored while connecting, and logged while connected.
    pub fn connect_with(
        self: &Arc<Self>,
        target: ConnectTarget,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        if self.connecting.get_untracked() {
            return;
        }
        if self.connected.get_untracked() {
            log::warn!("connect ignored: already connected (switch servers with switch_to)");
            return;
        }
        let generation = self.next_generation();
        self.dial(generation, target, Report::Emit, on_done);
    }

    /// Tears down first, then dials under a fresh generation so an in-flight dial cannot publish over it.
    pub fn switch_to(
        self: &Arc<Self>,
        target: ConnectTarget,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        self.switch(target, Report::Emit, on_done);
    }

    /// A failure goes only to `on_done`, for a caller that reports the dial itself.
    pub fn switch_to_quiet(
        self: &Arc<Self>,
        target: ConnectTarget,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        self.switch(target, Report::Quiet, on_done);
    }

    fn switch(
        self: &Arc<Self>,
        target: ConnectTarget,
        report: Report,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        if self.connecting.get_untracked() {
            log::warn!("switch ignored: a connect is already in flight");
            return;
        }
        let generation = self.next_generation();
        if self.connected.get_untracked() {
            self.tear_down();
        }
        self.dial(generation, target, report, on_done);
    }

    /// Compared against the value read after starting a dial to learn it was superseded, since a
    /// superseded dial's `on_done` is dropped.
    pub fn dial_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn next_generation(&self) -> u64 {
        self.generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
    }

    fn dial(
        self: &Arc<Self>,
        generation: u64,
        target: ConnectTarget,
        report: Report,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        // Taken before the target moves into the backend.
        let offer = target.offer_credential.then(|| target.label.clone());
        self.connecting.set(true);

        match self.backend.begin_connect(target) {
            ConnectRun::Ready(outcome) => self.settle(generation, outcome, offer, report, on_done),
            ConnectRun::Pending(work) => {
                // Only the UI-thread coordinator below writes a signal.
                let (tx, rx) = oneshot::channel();
                spawn(async move {
                    let _ = tx.send(work.await);
                });
                let controller = Arc::clone(self);
                spawn_local(async move {
                    let outcome = rx
                        .await
                        .unwrap_or_else(|_| Err(AppFailure::Internal(DIAL_ABANDONED.to_string())));
                    controller.settle(generation, outcome, offer, report, on_done);
                });
            }
        }
    }

    /// Runs on the UI thread, always.
    fn settle(
        &self,
        generation: u64,
        outcome: ConnectOutcome,
        offer: Option<String>,
        report: Report,
        on_done: impl FnOnce(Result<(), AppFailure>),
    ) {
        if self.generation.load(Ordering::SeqCst) != generation {
            // Superseded: the backend already declined to install it, and a teardown here would close the
            // newer connection.
            return;
        }
        self.connecting.set(false);
        self.renewing.store(false, Ordering::SeqCst);

        match outcome {
            Ok(info) => {
                let read_only = info.is_read_only;
                self.is_read_only.set(read_only);
                self.server_info.set(Some(info));
                self.connected.set(true);
                self.bump_epoch();
                if let Some(label) = offer
                    && !read_only
                {
                    self.credential_offer.set(Some(label));
                }
                on_done(Ok(()));
            }
            Err(failure) => {
                if report == Report::Emit {
                    self.core.failures().emit(&failure);
                }
                on_done(Err(failure));
            }
        }
    }

    /// Single-flight, released by an RAII guard so no early return wedges the heartbeat. `None` is
    /// silent: the session list's own poll surfaces a failing connection.
    pub async fn ping(&self) -> Option<u32> {
        if self.pinging.swap(true, Ordering::SeqCst) {
            return None;
        }
        struct ReleasePingClaim<'a>(&'a AtomicBool);
        impl Drop for ReleasePingClaim<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _release = ReleasePingClaim(&self.pinging);

        let client = self.client()?;
        let started = Instant::now();
        client.unauth().get_version(Empty {}).await.ok()?;
        let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

        // Only the measurement is this call's to replace.
        let info = self.server_info.get_untracked()?;
        self.server_info
            .set(Some(ServerInfo { latency_ms, ..info }));
        Some(latency_ms)
    }

    /// Never touches the router; callers navigate away themselves.
    pub fn disconnect(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.backend.disconnect();
        self.connecting.set(false);
        self.clear_server_state();
    }

    pub fn lock_and_disconnect(&self, keyring: &dyn KeyringLock) {
        keyring.lock_keyring();
        self.disconnect();
    }

    /// The client goes first, so no screen sees a live connection with a dead client.
    fn tear_down(&self) {
        self.backend.disconnect();
        self.clear_server_state();
    }

    fn clear_server_state(&self) {
        self.renewing.store(false, Ordering::SeqCst);
        self.server_info.set(None);
        self.is_read_only.set(false);
        self.credential_offer.set(None);
        self.connected.set(false);
        self.bump_epoch();
    }

    fn bump_epoch(&self) {
        self.connection_epoch.update(|epoch| *epoch += 1);
    }

    /// Fenced to the live connection: a stale `client`'s expiry is ignored. A live one goes to the
    /// [`SessionRenewer`] first, else the connection drops and the expiry is reported once, however
    /// many calls fail together.
    pub fn on_session_expired_for(&self, client: &MuxrClient) {
        if !self.connected.get_untracked()
            || !self.client().is_some_and(|live| live.is_same(client))
        {
            return;
        }
        if self.renewing.swap(true, Ordering::SeqCst) {
            return;
        }
        let renewer = self
            .session_renewer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let endpoint = self.server_info.get_untracked().map(|info| info.endpoint);
        // Raised before the renewer runs: one that settles a dial synchronously clears it, and that
        // clear must win.
        if let (Some(renewer), Some(endpoint)) = (renewer, endpoint)
            && renewer.renew(&endpoint)
        {
            return;
        }
        self.renewing.store(false, Ordering::SeqCst);
        self.expire();
    }

    /// Unfenced and never consults a [`SessionRenewer`]; prefer [`Self::on_session_expired_for`].
    pub fn on_session_expired(&self) {
        if !self.connected.get_untracked() {
            return;
        }
        self.expire();
    }

    fn expire(&self) {
        self.disconnect();
        self.core
            .failures()
            .emit(&AppFailure::Auth(SESSION_EXPIRED.to_string()));
    }

    pub fn set_session_renewer(&self, renewer: Arc<dyn SessionRenewer>) {
        *self
            .session_renewer
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(renewer);
    }

    pub fn set_credential_saver(&self, saver: Arc<dyn CredentialSaver>) {
        *self
            .credential_saver
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(saver);
    }

    pub fn accept_credential_offer(&self) {
        self.accept_credential_offer_with(|| {});
    }

    /// The offer is cleared first, so a second press cannot save twice. `on_done` runs on every
    /// path, no saver included: it is Connect's held navigation and must never park the user.
    pub fn accept_credential_offer_with(&self, on_done: impl FnOnce() + 'static) {
        let offer = self.credential_offer.get_untracked();
        self.credential_offer.set(None);
        let Some(label) = offer else {
            on_done();
            return;
        };
        if self.is_read_only.get_untracked() {
            on_done();
            return;
        }
        let Some(endpoint) = self
            .server_info
            .get_untracked()
            .map(|info| info.endpoint.clone())
        else {
            on_done();
            return;
        };
        let saver = self
            .credential_saver
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match saver {
            Some(saver) => saver.save_credential(endpoint, label, Box::new(on_done)),
            None => on_done(),
        }
    }

    pub fn dismiss_credential_offer(&self) {
        self.credential_offer.set(None);
    }
}

impl AsRef<ControllerCore<AppFailure>> for ConnectionController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

static USE_MOCK_CONNECTION_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Must run before `MuxrApp::init`; idempotent.
pub fn use_mock_connection_for_tests() {
    USE_MOCK_CONNECTION_FOR_TESTS.store(true, Ordering::Relaxed);
}

static MOCK_DIAL_FAILURES: AtomicUsize = AtomicUsize::new(0);

const MOCK_DIAL_REFUSED: &str = "mock dial refused";

/// Process-wide, so a suite using it holds the serial guard; reset leftovers with `0`.
pub fn fail_next_mock_dials_for_tests(count: usize) {
    MOCK_DIAL_FAILURES.store(count, Ordering::SeqCst);
}

pub struct HeldDial {
    release: oneshot::Sender<bool>,
}

impl HeldDial {
    /// Dropping a `HeldDial` unreleased fails it.
    pub fn release(self, succeed: bool) {
        let _ = self.release.send(succeed);
    }
}

static MOCK_DIAL_HOLDS: Mutex<Vec<oneshot::Receiver<bool>>> = Mutex::new(Vec::new());

/// One handle per dial, in dial order; `0` drops any leftovers.
pub fn hold_next_mock_dials_for_tests(count: usize) -> Vec<HeldDial> {
    let mut holds = MOCK_DIAL_HOLDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    holds.clear();
    (0..count)
        .map(|_| {
            let (release, held) = oneshot::channel();
            holds.push(held);
            HeldDial { release }
        })
        .collect()
}

fn take_mock_dial_hold() -> Option<oneshot::Receiver<bool>> {
    let mut holds = MOCK_DIAL_HOLDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    (!holds.is_empty()).then(|| holds.remove(0))
}

fn take_mock_dial_failure() -> bool {
    MOCK_DIAL_FAILURES
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
            left.checked_sub(1)
        })
        .is_ok()
}

fn platform_backend() -> Arc<dyn ConnectionBackend> {
    if USE_MOCK_CONNECTION_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockConnectionBackend);
    }
    Arc::new(NetConnectionBackend::new())
}

/// Called exactly once, from `MuxrApp::init`; the route guard captures the returned `Arc`
/// rather than reading context at navigation time.
pub fn provide_connection_controller() -> Arc<ConnectionController> {
    let controller = Arc::new(ConnectionController::new(platform_backend()));
    provide_controller(Arc::clone(&controller));
    controller
}

/// Panics if called before `MuxrApp::init`.
pub fn expect_connection_controller() -> Arc<ConnectionController> {
    expect_controller::<Arc<ConnectionController>>()
}

#[cfg(test)]
mod tests {
    use std::sync::Weak;
    use std::sync::atomic::AtomicUsize;

    use frust::GetUntracked;

    use super::*;

    fn target(offer_credential: bool) -> ConnectTarget {
        ConnectTarget {
            host: "mock.example".to_string(),
            port: 7777,
            trust_mode: TrustMode::NoVerification,
            label: "Mock rig".to_string(),
            offer_credential,
            token: Zeroizing::new("s3cr3t".to_string()),
            remember: true,
            fingerprint: None,
        }
    }

    fn controller() -> Arc<ConnectionController> {
        Arc::new(ConnectionController::new(Arc::new(MockConnectionBackend)))
    }

    #[test]
    fn connect_fills_server_info_and_flips_connected() {
        let controller = controller();
        controller.connect(target(false));

        assert!(controller.connected.get_untracked());
        assert!(
            !controller.connecting.get_untracked(),
            "a mock connect settles before `connect` returns"
        );
        let info = controller
            .server_info
            .get_untracked()
            .expect("connect fills server_info");
        assert_eq!(info.endpoint, "mock.example:7777");
        assert_eq!(
            info.trust_mode,
            TrustMode::NoVerification,
            "the target's posture rides through as a variant, not just a label"
        );
        assert_eq!(info.tls_label, "No verification");
        assert_eq!(info.latency_ms, MOCK_LATENCY_MS);
        assert_eq!(
            info.version, MOCK_SERVER_VERSION,
            "the strip's meta text reads the multiplexer version off here"
        );
    }

    #[test]
    fn the_mock_publishes_one_read_write_zellij_backend_with_mouse_input() {
        let controller = controller();
        controller.connect(target(false));

        let info = controller
            .server_info
            .get_untracked()
            .expect("connect fills server_info");
        assert_eq!(info.available_backends, vec![ServerBackend::Zellij]);
        assert_eq!(
            info.backend_versions,
            vec![BackendVersion {
                backend: ServerBackend::Zellij,
                version: MOCK_ZELLIJ_VERSION.to_string(),
            }]
        );
        assert!(info.supports(CAP_MOUSE_INPUT));
        assert!(controller.supports(CAP_MOUSE_INPUT));
        assert!(
            !controller.supports(CAP_AGENTS_STREAM),
            "the mock advertises no agents stream, so screens poll"
        );
        assert!(!info.is_read_only);
        assert!(!controller.is_read_only.get_untracked());
    }

    #[test]
    fn connect_without_offer_credential_leaves_the_offer_none() {
        let controller = controller();
        controller.connect(target(false));

        assert_eq!(controller.credential_offer.get_untracked(), None);
    }

    #[test]
    fn connect_with_offer_credential_records_the_targets_label() {
        let controller = controller();
        controller.connect(target(true));

        assert_eq!(
            controller.credential_offer.get_untracked(),
            Some("Mock rig".to_string())
        );
    }

    #[test]
    fn a_read_only_login_is_never_offered_for_saving() {
        let controller = Arc::new(ConnectionController::new(Arc::new(CannedBackend {
            outcome: Mutex::new(Some(Ok(read_only_info()))),
        })));

        controller.connect(target(true));

        assert!(controller.is_read_only.get_untracked());
        assert_eq!(
            controller.credential_offer.get_untracked(),
            None,
            "an eligible target still loses the offer when the credential is read-only"
        );
    }

    #[test]
    fn accept_and_dismiss_credential_offer_both_clear_it() {
        let accepted = controller();
        accepted.connect(target(true));
        accepted.accept_credential_offer();
        assert_eq!(accepted.credential_offer.get_untracked(), None);

        let dismissed = controller();
        dismissed.connect(target(true));
        dismissed.dismiss_credential_offer();
        assert_eq!(dismissed.credential_offer.get_untracked(), None);
    }

    thread_local! {
        /// A `thread_local!` because a boxed `FnOnce()` is not `Send + Sync`; savers run on the accepting thread.
        static PENDING_SAVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    fn finish_pending_save() {
        let done = PENDING_SAVE.with_borrow_mut(Option::take);
        if let Some(done) = done {
            done();
        }
    }

    #[derive(Default)]
    struct RecordingSaver {
        saved: Mutex<Vec<(String, String)>>,
    }

    impl CredentialSaver for RecordingSaver {
        fn save_credential(&self, endpoint: String, label: String, on_done: Box<dyn FnOnce()>) {
            self.saved
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((endpoint, label));
            PENDING_SAVE.with_borrow_mut(|pending| *pending = Some(on_done));
        }
    }

    fn flag() -> (Arc<AtomicBool>, impl FnOnce() + 'static) {
        let flag = Arc::new(AtomicBool::new(false));
        let write = Arc::clone(&flag);
        (flag, move || write.store(true, Ordering::SeqCst))
    }

    #[test]
    fn accepting_runs_its_completion_only_once_the_save_settles() {
        let controller = controller();
        let saver = Arc::new(RecordingSaver::default());
        controller.set_credential_saver(Arc::clone(&saver) as Arc<dyn CredentialSaver>);
        controller.connect(target(true));

        let (done, on_done) = flag();
        controller.accept_credential_offer_with(on_done);

        assert_eq!(
            saver
                .saved
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_slice(),
            [("mock.example:7777".to_string(), "Mock rig".to_string())],
            "the live endpoint and the offer's own label reach the saver"
        );
        assert!(
            !done.load(Ordering::SeqCst),
            "the completion is the *settled* save's, not the accept's — a \
             caller that navigates from it must still be on Connect here"
        );

        finish_pending_save();
        assert!(done.load(Ordering::SeqCst), "…and fires when it settles");
    }

    #[test]
    fn every_refused_accept_still_runs_its_completion() {
        let no_offer = controller();
        let (done, on_done) = flag();
        no_offer.accept_credential_offer_with(on_done);
        assert!(done.load(Ordering::SeqCst), "no offer");

        let read_only = Arc::new(ConnectionController::new(Arc::new(CannedBackend {
            outcome: Mutex::new(Some(Ok(read_only_info()))),
        })));
        read_only.connect(target(true));
        read_only.credential_offer.set(Some("Mock rig".to_string()));
        let (done, on_done) = flag();
        read_only.accept_credential_offer_with(on_done);
        assert!(done.load(Ordering::SeqCst), "read-only session");

        let no_saver = controller();
        no_saver.connect(target(true));
        let (done, on_done) = flag();
        no_saver.accept_credential_offer_with(on_done);
        assert!(done.load(Ordering::SeqCst), "no saver installed");
    }

    /// The single-flight claim must come back, or one no-op tick wedges the heartbeat.
    #[tokio::test]
    async fn a_ping_with_no_live_client_is_a_silent_no_op_that_releases_its_claim() {
        let controller = controller();
        controller.connect(target(false));
        let before = controller.server_info.get_untracked();

        assert_eq!(
            controller.ping().await,
            None,
            "the mock has no wire to time"
        );
        assert!(
            !controller.pinging.load(Ordering::SeqCst),
            "the claim is released even on the path that measured nothing"
        );

        assert_eq!(
            controller.ping().await,
            None,
            "…so a later tick still gets past the claim"
        );
        assert_eq!(
            controller.server_info.get_untracked(),
            before,
            "an unmeasured ping publishes nothing"
        );
    }

    #[tokio::test]
    async fn a_ping_over_a_live_connection_republishes_only_the_latency() {
        use crate::net::testing::{self, MuxrdFixture};

        let fixture = MuxrdFixture::start().await;
        let backend = Arc::new(NetConnectionBackend::new());
        let controller = Arc::new(ConnectionController::new(
            Arc::clone(&backend) as Arc<dyn ConnectionBackend>
        ));
        let mut connect = target(false);
        connect.host = "127.0.0.1".to_string();
        connect.port = fixture.port();
        connect.trust_mode = TrustMode::Pinned;
        connect.token = Zeroizing::new(testing::AUTH_TOKEN.to_string());
        connect.fingerprint = Some(fixture.fingerprint_hex().to_string());

        // Driven directly: `connect_with` spawns on `frust::spawn`, whose runtime a plain
        // `#[tokio::test]` lacks.
        let ConnectRun::Pending(work) = backend.begin_connect(connect) else {
            panic!("a real dial is never decided before it runs");
        };
        let info = work.await.expect("the fixture accepts its own pin + token");
        controller.server_info.set(Some(info.clone()));

        let measured = controller.ping().await.expect("a live connection is timed");
        let after = controller
            .server_info
            .get_untracked()
            .expect("the ping republished the facts");
        assert_eq!(after.latency_ms, measured);
        assert_eq!(
            after,
            ServerInfo {
                latency_ms: measured,
                ..info
            },
            "only the measurement changes — the handshake's facts are the connect's"
        );
    }

    #[test]
    fn disconnect_clears_every_connection_scoped_signal() {
        let controller = controller();
        controller.connect(target(true));

        controller.disconnect();

        assert!(!controller.connected.get_untracked());
        assert!(!controller.connecting.get_untracked());
        assert_eq!(controller.server_info.get_untracked(), None);
        assert_eq!(controller.credential_offer.get_untracked(), None);
        assert!(!controller.is_read_only.get_untracked());
    }

    #[test]
    fn the_backend_tears_down_before_the_signals_reset() {
        let backend = Arc::new(OrderedBackend::new());
        let controller = Arc::new(ConnectionController::new(backend.clone()));
        backend.watch(&controller);
        controller.connect(target(false));

        controller.disconnect();

        assert_eq!(
            backend.connected_at_teardown(),
            Some(true),
            "the client is let go first, while the app still reads as connected"
        );
    }

    #[test]
    fn a_session_expiry_logs_out_once_however_many_times_it_is_reported() {
        let controller = controller();
        controller.connect(target(false));

        let raised = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&raised);
        let _subscription = controller
            .core()
            .failures()
            .subscribe(move |_failure: &AppFailure| {
                counter.fetch_add(1, Ordering::SeqCst);
            });

        controller.on_session_expired();
        controller.on_session_expired();
        controller.on_session_expired();

        assert!(!controller.connected.get_untracked());
        assert_eq!(
            raised.load(Ordering::SeqCst),
            1,
            "several calls failing at once must not raise several toasts"
        );
    }

    fn emitted(
        controller: &ConnectionController,
    ) -> (Arc<AtomicUsize>, clean_signals::Subscription) {
        let raised = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&raised);
        let subscription = controller
            .core()
            .failures()
            .subscribe(move |_failure: &AppFailure| {
                counter.fetch_add(1, Ordering::SeqCst);
            });
        (raised, subscription)
    }

    struct ScriptedRenewer {
        accept: bool,
        asked: Mutex<Vec<String>>,
    }

    impl ScriptedRenewer {
        fn installed(controller: &ConnectionController, accept: bool) -> Arc<Self> {
            let renewer = Arc::new(Self {
                accept,
                asked: Mutex::new(Vec::new()),
            });
            controller.set_session_renewer(Arc::clone(&renewer) as Arc<dyn SessionRenewer>);
            renewer
        }

        fn asked(&self) -> Vec<String> {
            self.asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    impl SessionRenewer for ScriptedRenewer {
        fn renew(&self, endpoint: &str) -> bool {
            self.asked
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(endpoint.to_string());
            self.accept
        }
    }

    #[tokio::test]
    async fn an_expiry_reported_by_a_superseded_client_is_a_no_op() {
        use crate::net::testing::MuxrdFixture;

        let fixture = MuxrdFixture::start().await;
        let controller =
            ConnectionController::with_live_client_for_tests(fixture.connect_pinned().await);
        let stale = fixture.connect_pinned().await;
        let renewer = ScriptedRenewer::installed(&controller, true);
        let (raised, _subscription) = emitted(&controller);

        controller.on_session_expired_for(&stale);

        assert!(controller.connected.get_untracked());
        assert_eq!(raised.load(Ordering::SeqCst), 0);
        assert!(renewer.asked().is_empty(), "a stale expiry renews nothing");
    }

    #[tokio::test]
    async fn a_live_expiry_a_renewer_takes_over_stays_connected_and_reports_nothing() {
        use crate::net::testing::MuxrdFixture;

        let fixture = MuxrdFixture::start().await;
        let live = fixture.connect_pinned().await;
        let controller = ConnectionController::with_live_client_for_tests(live.clone());
        let renewer = ScriptedRenewer::installed(&controller, true);
        let (raised, _subscription) = emitted(&controller);

        for _ in 0..3 {
            controller.on_session_expired_for(&live);
        }

        assert!(controller.connected.get_untracked());
        assert_eq!(raised.load(Ordering::SeqCst), 0);
        assert_eq!(
            renewer.asked(),
            [format!("127.0.0.1:{}", fixture.port())],
            "a burst of failing calls asks for one renewal, of the live endpoint"
        );
    }

    #[tokio::test]
    async fn a_live_expiry_the_renewer_declines_logs_out_once() {
        use crate::net::testing::MuxrdFixture;

        let fixture = MuxrdFixture::start().await;
        let live = fixture.connect_pinned().await;
        let controller = ConnectionController::with_live_client_for_tests(live.clone());
        let renewer = ScriptedRenewer::installed(&controller, false);
        let (raised, _subscription) = emitted(&controller);

        for _ in 0..3 {
            controller.on_session_expired_for(&live);
        }

        assert!(!controller.connected.get_untracked());
        assert_eq!(raised.load(Ordering::SeqCst), 1);
        assert_eq!(renewer.asked().len(), 1);
    }

    #[tokio::test]
    async fn a_live_expiry_with_no_renewer_installed_logs_out_once() {
        use crate::net::testing::MuxrdFixture;

        let fixture = MuxrdFixture::start().await;
        let live = fixture.connect_pinned().await;
        let controller = ConnectionController::with_live_client_for_tests(live.clone());
        let (raised, _subscription) = emitted(&controller);

        for _ in 0..3 {
            controller.on_session_expired_for(&live.clone());
        }

        assert!(!controller.connected.get_untracked());
        assert!(controller.client().is_none());
        assert_eq!(
            raised.load(Ordering::SeqCst),
            1,
            "several calls failing at once must not raise several toasts"
        );
    }

    #[tokio::test]
    async fn a_settled_dial_ends_the_renewal_so_a_later_expiry_is_offered_again() {
        use crate::net::testing::MuxrdFixture;

        let fixture = MuxrdFixture::start().await;
        let live = fixture.connect_pinned().await;
        let controller = ConnectionController::with_live_client_for_tests(live.clone());
        let renewer = ScriptedRenewer::installed(&controller, true);

        controller.on_session_expired_for(&live);
        controller.switch_to(target_at("127.0.0.1"), |_| {});
        controller.on_session_expired_for(&live);

        assert_eq!(renewer.asked().len(), 2);
        assert!(controller.connected.get_untracked());
    }

    fn failing_dial() -> Arc<ConnectionController> {
        Arc::new(ConnectionController::new(Arc::new(CannedBackend {
            outcome: Mutex::new(Some(Err(AppFailure::Network("down".to_string())))),
        })))
    }

    type OutcomeSlot = Arc<Mutex<Option<Result<(), AppFailure>>>>;

    fn outcome_slot() -> (OutcomeSlot, impl FnOnce(Result<(), AppFailure>) + 'static) {
        let slot = Arc::new(Mutex::new(None));
        let write = Arc::clone(&slot);
        (slot, move |outcome| {
            *write.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
        })
    }

    #[test]
    fn a_quiet_dial_hands_its_failure_to_on_done_and_emits_nothing() {
        let controller = failing_dial();
        let (raised, _subscription) = emitted(&controller);
        let (outcome, on_done) = outcome_slot();

        controller.switch_to_quiet(target(false), on_done);

        assert!(matches!(
            *outcome.lock().unwrap_or_else(PoisonError::into_inner),
            Some(Err(AppFailure::Network(_)))
        ));
        assert_eq!(raised.load(Ordering::SeqCst), 0);
        assert!(!controller.connected.get_untracked());
        assert!(!controller.connecting.get_untracked());
    }

    #[test]
    fn a_failed_switch_to_still_emits_its_failure() {
        let controller = failing_dial();
        let (raised, _subscription) = emitted(&controller);
        let (outcome, on_done) = outcome_slot();

        controller.switch_to(target(false), on_done);

        assert!(matches!(
            *outcome.lock().unwrap_or_else(PoisonError::into_inner),
            Some(Err(AppFailure::Network(_)))
        ));
        assert_eq!(raised.load(Ordering::SeqCst), 1);
    }

    fn target_at(host: &str) -> ConnectTarget {
        ConnectTarget {
            host: host.to_string(),
            ..target(false)
        }
    }

    fn recorded() -> (Arc<RecordingBackend>, Arc<ConnectionController>) {
        let backend = Arc::new(RecordingBackend::default());
        let controller = Arc::new(ConnectionController::new(
            Arc::clone(&backend) as Arc<dyn ConnectionBackend>
        ));
        (backend, controller)
    }

    fn connected_endpoint(controller: &ConnectionController) -> Option<String> {
        controller
            .server_info
            .get_untracked()
            .map(|info| info.endpoint)
    }

    fn settled_flag() -> (
        Arc<AtomicBool>,
        impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        let (flag, set) = flag();
        (flag, move |_outcome| set())
    }

    #[test]
    fn switch_to_while_connected_disconnects_first_then_connects_the_new_host() {
        let (backend, controller) = recorded();
        controller.connect(target_at("a.example"));
        backend.clear();
        let epoch = controller.connection_epoch.get_untracked();

        let (done, on_done) = settled_flag();
        controller.switch_to(target_at("b.example"), on_done);

        assert_eq!(backend.events(), ["disconnect", "connect b.example:7777"]);
        assert!(controller.connected.get_untracked());
        assert_eq!(
            connected_endpoint(&controller).as_deref(),
            Some("b.example:7777")
        );
        assert_eq!(
            controller.connection_epoch.get_untracked(),
            epoch + 2,
            "one bump for the teardown, one for the new connection"
        );
        assert!(done.load(Ordering::SeqCst));
    }

    #[test]
    fn switch_to_while_disconnected_only_connects() {
        let (backend, controller) = recorded();
        controller.switch_to(target_at("b.example"), |_| {});

        assert_eq!(backend.events(), ["connect b.example:7777"]);
        assert_eq!(controller.connection_epoch.get_untracked(), 1);
    }

    #[test]
    fn a_dial_started_before_switch_to_settles_after_it_and_is_dropped() {
        let (backend, controller) = recorded();
        let stale = controller.next_generation();
        controller.connecting.set(true);
        controller.disconnect();
        controller.switch_to(target_at("b.example"), |_| {});
        let epoch = controller.connection_epoch.get_untracked();
        backend.clear();

        let (done, on_done) = settled_flag();
        let late = MockConnectionBackend.begin_connect(target_at("x.example"));
        let ConnectRun::Ready(outcome) = late else {
            unreachable!("the mock always answers on the spot");
        };
        controller.settle(stale, outcome, Some("X".to_string()), Report::Emit, on_done);

        assert_eq!(
            connected_endpoint(&controller).as_deref(),
            Some("b.example:7777"),
            "the late result never publishes over the switch"
        );
        assert!(controller.connected.get_untracked());
        assert_eq!(controller.connection_epoch.get_untracked(), epoch);
        assert_eq!(controller.credential_offer.get_untracked(), None);
        assert!(
            !done.load(Ordering::SeqCst),
            "a dropped dial reports nothing"
        );
        assert!(
            backend.events().is_empty(),
            "dropping it tears nothing down — the live connection is the switch's"
        );
    }

    #[test]
    fn connect_with_while_connected_is_ignored() {
        let (backend, controller) = recorded();
        controller.connect(target_at("a.example"));

        let (done, on_done) = settled_flag();
        controller.connect_with(target_at("b.example"), on_done);

        assert_eq!(backend.events(), ["connect a.example:7777"]);
        assert_eq!(
            connected_endpoint(&controller).as_deref(),
            Some("a.example:7777")
        );
        assert!(!done.load(Ordering::SeqCst));
    }

    #[test]
    fn switch_to_while_a_connect_is_in_flight_is_ignored() {
        let (backend, controller) = recorded();
        controller.connecting.set(true);

        controller.switch_to(target_at("b.example"), |_| {});

        assert!(backend.events().is_empty());
        assert!(!controller.connected.get_untracked());
    }

    #[test]
    fn the_epoch_moves_on_a_published_connect_and_on_every_disconnect() {
        let failing = Arc::new(ConnectionController::new(Arc::new(CannedBackend {
            outcome: Mutex::new(Some(Err(AppFailure::Network("down".to_string())))),
        })));
        failing.connect(target(false));
        assert_eq!(failing.connection_epoch.get_untracked(), 0, "a failure");

        let controller = controller();
        controller.connect(target(false));
        assert_eq!(controller.connection_epoch.get_untracked(), 1);
        controller.disconnect();
        assert_eq!(controller.connection_epoch.get_untracked(), 2);
    }

    #[test]
    fn lock_and_disconnect_locks_the_keyring_and_drops_the_connection() {
        use crate::features::connect::di::KeyringHandle;
        use crate::features::connect::domain::keyring::Keyring;

        let keyring = KeyringHandle::new();
        let mut unlocked = Keyring::new();
        unlocked.insert("a.example:7777", Zeroizing::new("tok".to_string()));
        keyring.publish(unlocked);
        let (backend, controller) = recorded();
        controller.connect(target_at("a.example"));

        controller.lock_and_disconnect(&keyring);

        assert!(!keyring.unlocked.get_untracked());
        assert!(keyring.endpoints().is_empty());
        assert!(!controller.connected.get_untracked());
        assert_eq!(
            backend.events().last().map(String::as_str),
            Some("disconnect")
        );
    }

    #[test]
    fn each_trust_mode_maps_onto_its_verifier() {
        let mut system = target(false);
        system.trust_mode = TrustMode::System;
        assert_eq!(trust_config_for(&system), Ok(TrustConfig::SystemCa));

        let unverified = target(false);
        assert_eq!(
            trust_config_for(&unverified),
            Ok(TrustConfig::InsecureAcceptAny),
            "no verification means no pin is ever checked"
        );

        let mut pinned = target(false);
        pinned.trust_mode = TrustMode::Pinned;
        pinned.fingerprint = Some("ab".repeat(32));
        assert_eq!(
            trust_config_for(&pinned),
            Ok(TrustConfig::PinnedSha256([0xAB; 32]))
        );
    }

    #[test]
    fn a_pinned_target_without_a_usable_pin_fails_before_dialing() {
        let backend = NetConnectionBackend::new();

        for fingerprint in [None, Some("not-hex".to_string()), Some("ab".repeat(31))] {
            let mut pinned = target(false);
            pinned.trust_mode = TrustMode::Pinned;
            pinned.fingerprint = fingerprint.clone();

            match backend.begin_connect(pinned) {
                ConnectRun::Ready(Err(AppFailure::Network(_))) => {}
                _ => panic!("a pin that cannot be parsed must never dial: {fingerprint:?}"),
            }
        }
        assert!(
            backend.client().is_none(),
            "nothing was opened, so nothing is held"
        );
    }

    #[tokio::test]
    async fn the_net_backend_logs_in_and_publishes_the_handshake() {
        use crate::net::testing::{self, MuxrdFixture};

        let fixture = MuxrdFixture::start().await;
        let backend = NetConnectionBackend::new();
        let target = ConnectTarget {
            host: "127.0.0.1".to_string(),
            port: fixture.port(),
            trust_mode: TrustMode::Pinned,
            label: "Fixture".to_string(),
            offer_credential: false,
            token: Zeroizing::new(testing::AUTH_TOKEN.to_string()),
            remember: false,
            fingerprint: Some(fixture.fingerprint_hex().to_string()),
        };

        let ConnectRun::Pending(work) = backend.begin_connect(target) else {
            panic!("a real dial is never decided before it runs");
        };
        let info = work.await.expect("the fixture accepts its own pin + token");

        assert_eq!(info.endpoint, format!("127.0.0.1:{}", fixture.port()));
        assert_eq!(info.trust_mode, TrustMode::Pinned);
        assert!(!info.is_read_only);
        assert_eq!(
            info.version, UNKNOWN_VERSION,
            "the fixture names no multiplexer, and a version is never invented"
        );
        assert!(
            backend
                .client()
                .expect("the connected client is held for the repositories")
                .has_session_token(),
            "the minted session token is installed by the flow, not by a caller"
        );

        backend.disconnect();
        assert!(backend.client().is_none());
    }

    #[tokio::test]
    async fn a_rejected_auth_token_fails_the_connect_as_an_auth_failure() {
        use clean_signals::Failure;

        use crate::net::testing::MuxrdFixture;

        let fixture = MuxrdFixture::start().await;
        let backend = NetConnectionBackend::new();
        let target = ConnectTarget {
            host: "127.0.0.1".to_string(),
            port: fixture.port(),
            trust_mode: TrustMode::Pinned,
            label: "Fixture".to_string(),
            offer_credential: false,
            token: Zeroizing::new("not-the-fixtures-token".to_string()),
            remember: false,
            fingerprint: Some(fixture.fingerprint_hex().to_string()),
        };

        let ConnectRun::Pending(work) = backend.begin_connect(target) else {
            panic!("a real dial is never decided before it runs");
        };
        let failure = work.await.expect_err("an unknown auth token is refused");

        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(!failure.is_retryable());
        assert!(
            backend.client().is_none(),
            "a failed login leaves no connection behind"
        );
    }

    fn fixture_target(fixture: &crate::net::testing::MuxrdFixture) -> ConnectTarget {
        ConnectTarget {
            host: "127.0.0.1".to_string(),
            port: fixture.port(),
            trust_mode: TrustMode::Pinned,
            token: Zeroizing::new(crate::net::testing::AUTH_TOKEN.to_string()),
            fingerprint: Some(fixture.fingerprint_hex().to_string()),
            ..target(false)
        }
    }

    fn pending(run: ConnectRun) -> ConnectFuture {
        match run {
            ConnectRun::Pending(work) => work,
            ConnectRun::Ready(_) => panic!("a real dial is never decided before it runs"),
        }
    }

    #[tokio::test]
    async fn a_superseded_dial_never_installs_its_client() {
        use crate::net::testing::MuxrdFixture;

        let fixture = MuxrdFixture::start().await;
        let backend = NetConnectionBackend::new();

        let orphan = pending(backend.begin_connect(fixture_target(&fixture)));
        backend.disconnect();
        orphan.await.expect("the dial itself succeeds");
        assert!(backend.client().is_none(), "a teardown outranks the dial");

        let older = pending(backend.begin_connect(fixture_target(&fixture)));
        let newer = pending(backend.begin_connect(fixture_target(&fixture)));
        newer.await.expect("the newer dial lands first");
        older.await.expect("the older dial lands second");
        assert!(
            backend
                .client()
                .expect("the newer dial's client stays installed")
                .has_session_token(),
            "the older dial scrubbed only its own client, never the installed one"
        );
    }

    #[derive(Default)]
    struct RecordingBackend {
        events: Mutex<Vec<String>>,
    }

    impl RecordingBackend {
        fn log(&self) -> MutexGuard<'_, Vec<String>> {
            self.events.lock().unwrap_or_else(PoisonError::into_inner)
        }

        fn events(&self) -> Vec<String> {
            self.log().clone()
        }

        fn clear(&self) {
            self.log().clear();
        }
    }

    impl ConnectionBackend for RecordingBackend {
        fn begin_connect(&self, target: ConnectTarget) -> ConnectRun {
            self.log().push(format!("connect {}", endpoint_of(&target)));
            MockConnectionBackend.begin_connect(target)
        }

        fn disconnect(&self) {
            self.log().push("disconnect".to_string());
        }
    }

    fn read_only_info() -> ServerInfo {
        ServerInfo {
            endpoint: "mock.example:7777".to_string(),
            trust_mode: TrustMode::NoVerification,
            tls_label: TrustMode::NoVerification.label().to_string(),
            version: MOCK_SERVER_VERSION.to_string(),
            latency_ms: MOCK_LATENCY_MS,
            backend_versions: Vec::new(),
            available_backends: Vec::new(),
            capabilities: BTreeSet::new(),
            is_read_only: true,
        }
    }

    /// For the facts the mock backend never produces.
    struct CannedBackend {
        outcome: Mutex<Option<ConnectOutcome>>,
    }

    impl ConnectionBackend for CannedBackend {
        fn begin_connect(&self, _target: ConnectTarget) -> ConnectRun {
            let outcome = self
                .outcome
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .expect("a canned backend answers one connect");
            ConnectRun::Ready(outcome)
        }

        fn disconnect(&self) {}
    }

    /// `Weak`, so the double cannot keep the controller that owns it alive.
    struct OrderedBackend {
        controller: Mutex<Option<Weak<ConnectionController>>>,
        connected_at_teardown: Mutex<Option<bool>>,
    }

    impl OrderedBackend {
        fn new() -> Self {
            Self {
                controller: Mutex::new(None),
                connected_at_teardown: Mutex::new(None),
            }
        }

        fn watch(&self, controller: &Arc<ConnectionController>) {
            *self
                .controller
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(controller));
        }

        fn connected_at_teardown(&self) -> Option<bool> {
            *self
                .connected_at_teardown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        }
    }

    impl ConnectionBackend for OrderedBackend {
        fn begin_connect(&self, target: ConnectTarget) -> ConnectRun {
            MockConnectionBackend.begin_connect(target)
        }

        fn disconnect(&self) {
            let observed = self
                .controller
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .and_then(Weak::upgrade)
                .map(|controller| controller.connected.get_untracked());
            *self
                .connected_at_teardown
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = observed;
        }
    }
}
