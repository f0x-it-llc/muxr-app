//! [`ConnectionController`] — muxr's one app-scoped connection handle — and
//! the [`ConnectionBackend`] seam it drives.
//!
//! ## Token Hygiene
//!
//! The bearer token crossing this module lives only as long as it has a job:
//! [`ConnectTarget::token`] rides `Zeroizing`-wrapped from Connect's form to
//! [`open_session`], which drops (and so scrubs) the app's own copy the instant
//! the `Login` request is built — leaving one **un-zeroized residual**: that
//! request's `auth_token` is a plain prost `String`, not `Zeroizing`, so its
//! plaintext copy survives unscrubbed in the message until the call it rides on
//! is dropped. The session token `Login` mints is installed onto the client via
//! [`crate::net::client::MuxrClient::set_session_token`] and the wire
//! response's own copy is `zeroize`d immediately after. From there it lives
//! behind [`crate::net::auth::SessionToken`]'s `ArcSwapOption<Zeroizing<_>>`,
//! scrubbed on every `set`/`clear` and read fresh per call, never cached
//! longer. [`NetConnectionBackend::disconnect`] clears the session token
//! before dropping its client handle — since every [`MuxrClient`] clone
//! shares one token store, that clear closes the authenticated surface for
//! every clone at once, not just the one being torn down.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// Capability flag: server supports mouse frames in `AttachTerminal`. Unknown
/// flags carry through unchanged, as the wire requires.
pub const CAP_MOUSE_INPUT: &str = "mouse-input";

/// Capability flag: `WatchAgents` delivers live agent changes. Absent, a client polls
/// `ListAgents` instead.
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
    /// Whether the server advertised `capability` (e.g. [`CAP_MOUSE_INPUT`]).
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }
}

/// A multiplexer a `muxrd` server can front.
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

/// A connect request's target, credential and options.
#[derive(Clone, PartialEq)]
pub struct ConnectTarget {
    pub host: String,
    pub port: u16,
    pub trust_mode: TrustMode,
    pub label: String,
    /// Whether a successful connect is *eligible* to offer saving this
    /// target's credential.
    pub offer_credential: bool,
    /// The login token this connect authenticates with — the one secret that
    /// crosses the wire, [`Zeroizing`]-wrapped so dropping this target scrubs
    /// it rather than leaving plaintext in a freed allocation.
    pub token: Zeroizing<String>,
    pub remember: bool,
    /// The pinned certificate's SHA-256 as 64 lowercase hex characters —
    /// [`TrustMode::Pinned`]'s input, and required by it: a pinned target
    /// without one never dials.
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

/// What a completed connect produces: the connected server's facts, or why the
/// connection did not open.
pub type ConnectOutcome = Result<ServerInfo, AppFailure>;

/// The work a [`ConnectRun::Pending`] still has to do.
pub type ConnectFuture = Pin<Box<dyn Future<Output = ConnectOutcome> + Send>>;

pub enum ConnectRun {
    Ready(ConnectOutcome),
    Pending(ConnectFuture),
}

/// The work behind [`ConnectionController`]: opening a connection, tearing one
/// down, and holding the live client in between.
pub trait ConnectionBackend: Send + Sync + 'static {
    /// Start a dial. One superseded by a later `begin_connect` or
    /// `disconnect` before it lands must drop what it opened rather than
    /// install it — the controller discards such a stale outcome unseen.
    fn begin_connect(&self, target: ConnectTarget) -> ConnectRun;

    /// Tear the live connection down. Idempotent: a backend with nothing open
    /// does nothing.
    fn disconnect(&self);

    /// The live `muxrd` client, for the repositories built over the same
    /// connection. `None` when this backend has no wire behind it (the mock)
    /// or nothing is open.
    fn client(&self) -> Option<MuxrClient> {
        None
    }

    /// Rebuild the live client's channel (single-flight, see
    /// [`MuxrClient::reconnect`]). Nothing open — or no wire behind the
    /// backend — is nothing to rebuild, and succeeds.
    fn reconnect(&self) -> ReconnectFuture {
        Box::pin(async { Ok(()) })
    }
}

/// The work behind [`ConnectionController::reconnect`].
pub type ReconnectFuture = Pin<Box<dyn Future<Output = Result<(), AppFailure>> + Send>>;

/// The work behind an accepted credential offer — installed by the `connect`
/// feature, called by [`ConnectionController::accept_credential_offer`].
pub trait CredentialSaver: Send + Sync {
    /// Save a credential for the connected server at `endpoint` (`host:port`),
    /// which the offer showed the user as `label`, and run `on_done` once that
    /// save has settled — **however** it settled.
    fn save_credential(&self, endpoint: String, label: String, on_done: Box<dyn FnOnce()>);
}

/// What [`ConnectionController::lock_and_disconnect`] locks — the `connect`
/// feature's keyring, named here by contract rather than by type.
pub trait KeyringLock {
    /// Zeroize every held token and mark the keyring locked.
    fn lock_keyring(&self);
}

const MOCK_LATENCY_MS: u32 = 36;
const MOCK_SERVER_VERSION: &str = "zellij 0.44.3";
const MOCK_ZELLIJ_VERSION: &str = "0.44.3";

/// The in-memory backend: derives a [`ServerInfo`] from the target and answers
/// on the spot. Never fails, and never touches a socket.
pub struct MockConnectionBackend;

impl ConnectionBackend for MockConnectionBackend {
    fn begin_connect(&self, target: ConnectTarget) -> ConnectRun {
        ConnectRun::Ready(Ok(ServerInfo {
            endpoint: endpoint_of(&target),
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
        }))
    }

    fn disconnect(&self) {}
}

/// The `muxrd` backend: a real dial + `Login` + `GetVersion` over
/// [`crate::net`], holding the connected [`MuxrClient`] until a teardown drops
/// it.
pub struct NetConnectionBackend {
    live: Arc<Mutex<LiveSlot>>,
}

/// The one live client, and the ticket of the newest dial or teardown: a dial
/// installs its client only while its own ticket is still the newest.
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
        // Scrub the credential before letting go: dropping this handle closes
        // the transport only if it was the last clone, while clearing the token
        // closes the authenticated surface for every clone there is.
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

/// The TLS posture `target` dials under.
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

/// The whole wire flow, in order: dial, log in, install the session token, time
/// a `GetVersion`, keep the client (unless `ticket` was superseded meanwhile),
/// and hand back what the screens render.
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
    // The token's job ends with the request that carries it: drop this
    // app's own copy now rather than at the end of the flow.
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

/// The handshake, as the screens read it.
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

/// The strip's `"zellij 0.44.3"` half: the first detected backend, or the
/// wire's back-compat zellij field when a server reported no backends at all.
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

// ---------------------------------------------------------------------------
// The controller
// ---------------------------------------------------------------------------

pub struct ConnectionController {
    core: ControllerCore<AppFailure>,
    backend: Arc<dyn ConnectionBackend>,
    /// Bumped by every connect and every teardown — a coordinator whose
    /// captured value no longer matches drops its result instead of
    /// publishing it.
    generation: AtomicU64,

    pub connected: RwSignal<bool>,
    /// Whether a connect is in flight — the Connect screen's busy state, and
    /// the single-flight guard: a second connect while this is set is ignored.
    pub connecting: RwSignal<bool>,
    pub server_info: RwSignal<Option<ServerInfo>>,
    pub is_read_only: RwSignal<bool>,
    /// Bumped by every published connect and every teardown, so a screen
    /// holding per-server state resets it when this moves.
    pub connection_epoch: RwSignal<u64>,
    /// Whether a [`ping`](Self::ping) is in flight — its single-flight claim.
    /// A tick arriving mid-round-trip is dropped, not queued: a queued
    /// reading would already be stale, and queuing would grow one RPC per
    /// tick against a slow server.
    pinging: AtomicBool,
    /// The server label whose credential the just-settled connect should
    /// offer to save, or `None` when there's nothing to offer. Written by
    /// [`Self::settle`] before its `on_done`, so the Connect flow can read
    /// it back and hold its own redirect on the answer.
    pub credential_offer: RwSignal<Option<String>>,
    /// What an accepted offer actually saves through, or `None` while nothing
    /// has installed one — see [`CredentialSaver`].
    credential_saver: Mutex<Option<Arc<dyn CredentialSaver>>>,
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
        }
    }

    /// A fresh, **unpublished** mock-backed controller — `#[cfg(test)]`-only,
    /// so [`provide_connection_controller`] stays the single production
    /// construction site. `crate::routes`' guard test needs one it can hold
    /// without any reactive owner or context in play.
    #[cfg(test)]
    pub(crate) fn detached() -> Self {
        Self::new(Arc::new(MockConnectionBackend))
    }

    /// A fresh, unpublished controller over a test's own backend — the seam
    /// for observing what a screen asks of the connection.
    #[cfg(test)]
    pub(crate) fn with_backend(backend: Arc<dyn ConnectionBackend>) -> Self {
        Self::new(backend)
    }

    /// This controller's [`ControllerCore`] — the failure sink `MuxrApp::init`
    /// bridges to the app's toasts. A named accessor rather than the [`AsRef`]
    /// impl below because `Arc<ConnectionController>` has its own inherent
    /// `as_ref`, which would make the bare call ambiguous at every site.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    /// The live `muxrd` client, or `None` while disconnected (always `None`
    /// under the mock backend).
    pub fn client(&self) -> Option<MuxrClient> {
        self.backend.client()
    }

    /// Whether the connected server advertised `capability` — `false` while disconnected. An
    /// untracked read, for a caller deciding once rather than a build.
    pub fn supports(&self, capability: &str) -> bool {
        self.server_info
            .try_with_untracked(|info| info.as_ref().is_some_and(|info| info.supports(capability)))
            .unwrap_or(false)
    }

    /// Rebuild the shared client's channel now — the shared client's
    /// single-flight [`MuxrClient::reconnect`], so a concurrent caller (the
    /// terminal feed) collapses into the same dial. A no-op `Ok` while nothing
    /// is connected. For a screen that must not trust a half-open channel
    /// after a network change before it sends its next RPC.
    pub async fn reconnect(&self) -> Result<(), AppFailure> {
        self.backend.reconnect().await
    }

    /// Connect to `target`, ignoring the outcome — the fire-and-forget form for
    /// callers with nothing to do afterwards. See [`Self::connect_with`].
    pub fn connect(self: &Arc<Self>, target: ConnectTarget) {
        self.connect_with(target, |_| {});
    }

    /// Connect to `target` and run `on_done` on the UI thread once the attempt
    /// settles — with the mock backend, before this call even returns.
    ///
    /// Ignored while another connect is in flight, and — logged — while
    /// already connected: one app, one connection. Moving to another server
    /// goes through [`Self::switch_to`], which tears the live one down first.
    ///
    /// `on_done` is for what only the caller can do (navigate, scrub its
    /// form) — not the failure path, which is emitted on [`Self::core`]'s
    /// sink for the app's toast bridge.
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
        self.dial(generation, target, on_done);
    }

    /// Move to `target`: tear the live connection down first, then dial under
    /// one fresh generation, so a dial still in flight can never publish over
    /// the new one. Ignored (logged) while a connect is in flight. `on_done`
    /// as for [`Self::connect_with`].
    pub fn switch_to(
        self: &Arc<Self>,
        target: ConnectTarget,
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
        self.dial(generation, target, on_done);
    }

    fn next_generation(&self) -> u64 {
        self.generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
    }

    /// Start a dial under `generation` and settle it on the UI thread.
    fn dial(
        self: &Arc<Self>,
        generation: u64,
        target: ConnectTarget,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        // The caller's half of the credential-offer decision, taken before the
        // target moves into the backend.
        let offer = target.offer_credential.then(|| target.label.clone());
        self.connecting.set(true);

        match self.backend.begin_connect(target) {
            ConnectRun::Ready(outcome) => self.settle(generation, outcome, offer, on_done),
            ConnectRun::Pending(work) => {
                // The dial runs on the background runtime; the UI-thread
                // coordinator below is the only thing that writes a signal.
                let (tx, rx) = oneshot::channel();
                spawn(async move {
                    let _ = tx.send(work.await);
                });
                let controller = Arc::clone(self);
                spawn_local(async move {
                    let outcome = rx
                        .await
                        .unwrap_or_else(|_| Err(AppFailure::Internal(DIAL_ABANDONED.to_string())));
                    controller.settle(generation, outcome, offer, on_done);
                });
            }
        }
    }

    /// Publish a settled connect. Runs on the UI thread, always.
    fn settle(
        &self,
        generation: u64,
        outcome: ConnectOutcome,
        offer: Option<String>,
        on_done: impl FnOnce(Result<(), AppFailure>),
    ) {
        if self.generation.load(Ordering::SeqCst) != generation {
            // A teardown or a newer dial landed while this one was in flight,
            // so its outcome belongs to nobody. The backend already declined
            // to install what it opened (see `begin_connect`); a teardown here
            // would close the newer connection instead.
            return;
        }
        self.connecting.set(false);

        match outcome {
            Ok(info) => {
                let read_only = info.is_read_only;
                self.is_read_only.set(read_only);
                self.server_info.set(Some(info));
                self.connected.set(true);
                self.bump_epoch();
                // A read-only credential is never offered for saving
                if let Some(label) = offer
                    && !read_only
                {
                    self.credential_offer.set(Some(label));
                }
                on_done(Ok(()));
            }
            Err(failure) => {
                self.core.failures().emit(&failure);
                on_done(Err(failure));
            }
        }
    }

    /// Re-time the connection: one unauthenticated `GetVersion` round trip,
    /// replacing [`ServerInfo::latency_ms`]. The Sessions strip's heartbeat is
    /// the one caller.
    ///
    /// Single-flight via [`pinging`](Self::pinging), released through an RAII
    /// guard so no early return wedges the heartbeat shut.
    ///
    /// `None` means nothing was measured, and is deliberately silent: the
    /// session list's own 3s poll is what surfaces a failing connection, so a
    /// heartbeat toasting its own failure would double up on it.
    ///
    /// Unauthenticated, like the connect-time `GetVersion`: no credential is
    /// read, installed or scrubbed here.
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

        // The rest of the handshake's facts are the connect's; only the
        // measurement is this call's to replace.
        let info = self.server_info.get_untracked()?;
        self.server_info
            .set(Some(ServerInfo { latency_ms, ..info }));
        Some(latency_ms)
    }

    /// Close the connection and clear every connection-scoped signal. Callers
    /// navigate away themselves — this never touches the router.
    pub fn disconnect(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.backend.disconnect();
        self.connecting.set(false);
        self.clear_server_state();
    }

    /// Lock `keyring` (zeroizing every token), then [`Self::disconnect`].
    pub fn lock_and_disconnect(&self, keyring: &dyn KeyringLock) {
        keyring.lock_keyring();
        self.disconnect();
    }

    /// Let the backend's client go, then clear the server-scoped signals —
    /// in that order, so no screen sees a live connection with a dead client.
    fn tear_down(&self) {
        self.backend.disconnect();
        self.clear_server_state();
    }

    /// Clear the server-scoped signals and move the epoch.
    fn clear_server_state(&self) {
        self.server_info.set(None);
        self.is_read_only.set(false);
        self.credential_offer.set(None);
        self.connected.set(false);
        self.bump_epoch();
    }

    fn bump_epoch(&self) {
        self.connection_epoch.update(|epoch| *epoch += 1);
    }

    /// The server rejected this session's credential — every
    /// [`AppFailure::Auth`] a repository raises funnels here.
    ///
    /// Idempotent: several in-flight calls failing at once is the normal shape
    /// of an expiry, and the user gets one teardown and one toast, not one per
    /// call.
    pub fn on_session_expired(&self) {
        if !self.connected.get_untracked() {
            return;
        }
        self.disconnect();
        self.core
            .failures()
            .emit(&AppFailure::Auth(SESSION_EXPIRED.to_string()));
    }

    /// Install what an accepted offer saves through, replacing any previous
    /// one — the `connect` feature's own [`CredentialSaver`].
    ///
    /// Called from the Connect screen's `Component::init`, where every
    /// dependency the save needs is already resolved.
    pub fn set_credential_saver(&self, saver: Arc<dyn CredentialSaver>) {
        *self
            .credential_saver
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(saver);
    }

    /// Accept the pending credential offer: clear it, and hand the save to the
    /// installed [`CredentialSaver`] — the fire-and-forget form, for a caller
    /// with nothing to resume. See [`Self::accept_credential_offer_with`].
    pub fn accept_credential_offer(&self) {
        self.accept_credential_offer_with(|| {});
    }

    /// Accept the pending credential offer and run `on_done` once the save has
    /// settled, whatever it settled as.
    ///
    /// The offer is cleared first and unconditionally, so a second press (or
    /// a slow saver) cannot start the save twice. The save itself is also a
    /// no-op on three more paths: no offer pending, no live connection to
    /// name an endpoint, or a read-only session (mints no token to save) —
    /// the same fact [`Self::settle`] refuses to raise the offer on.
    ///
    /// `on_done` runs on every one of those paths, including no
    /// [`CredentialSaver`] installed at all: it is the Connect screen's held
    /// navigation, so a save that could not run may never leave the user
    /// parked on the form.
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

    /// Dismiss the pending credential offer without saving it.
    pub fn dismiss_credential_offer(&self) {
        self.credential_offer.set(None);
    }
}

impl AsRef<ControllerCore<AppFailure>> for ConnectionController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

// ---------------------------------------------------------------------------
// Composition-root wiring
// ---------------------------------------------------------------------------

/// Set by [`use_mock_connection_for_tests`], read by [`platform_backend`]:
/// `false` (the real `muxrd` client) for every app run, flipped to `true`
/// once per test process before [`provide_connection_controller`] ever runs.
static USE_MOCK_CONNECTION_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real `muxrd` client for the rest of
/// its life: every later [`provide_connection_controller`] builds a
/// [`MockConnectionBackend`] instead. Idempotent. Must run before
/// `MuxrApp::init` — `tests/support::setup` is the one call site outside
/// this module.
pub fn use_mock_connection_for_tests() {
    USE_MOCK_CONNECTION_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// The backend [`provide_connection_controller`] drives.
fn platform_backend() -> Arc<dyn ConnectionBackend> {
    if USE_MOCK_CONNECTION_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockConnectionBackend);
    }
    Arc::new(NetConnectionBackend::new())
}

/// Construct the app's one [`ConnectionController`], publish it under the
/// root owner, and hand it straight back. Called exactly once, from
/// `MuxrApp::init`.
///
/// The return value is not a convenience: `crate::routes`' session guard is
/// closed over this `Arc` at construction rather than reading it from
/// context at navigation time, and the composition root is the one place
/// that already holds it.
pub fn provide_connection_controller() -> Arc<ConnectionController> {
    let controller = Arc::new(ConnectionController::new(platform_backend()));
    provide_controller(Arc::clone(&controller));
    controller
}

/// Recover the app-scoped [`ConnectionController`]
/// [`provide_connection_controller`] published — reachable from any screen,
/// and from `crate::routes`'s session guard, which has no `&mut AppState` to
/// reach one any other way. Panics if called before `MuxrApp::init`, which
/// every call site in this app runs after.
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

    /// A mock-backed controller, as every suite's `MuxrApp::init` builds one.
    fn controller() -> Arc<ConnectionController> {
        Arc::new(ConnectionController::new(Arc::new(MockConnectionBackend)))
    }

    // -----------------------------------------------------------------------
    // The mock backend's published facts
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Credential-offer eligibility
    // -----------------------------------------------------------------------

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
        /// The completion [`RecordingSaver`] was last handed, parked until the
        /// test settles it. A `thread_local!` rather than a field because
        /// [`CredentialSaver`] is `Send + Sync` and a boxed `FnOnce()` is
        /// neither — which is honest here: this saver, like the real one, is
        /// only ever called on the thread that accepted the offer.
        static PENDING_SAVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Run the completion the last `save_credential` parked, as a finished save
    /// would.
    fn finish_pending_save() {
        let done = PENDING_SAVE.with_borrow_mut(Option::take);
        if let Some(done) = done {
            done();
        }
    }

    /// A [`CredentialSaver`] that records what it was asked to save and parks
    /// the completion, so a test can settle it on its own schedule — the
    /// held-navigation handshake's stand-in for `connect`'s real saver.
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

    /// Flip a shared flag — the completion every test below hands in.
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
        // Nothing pending: an accept with no offer up.
        let no_offer = controller();
        let (done, on_done) = flag();
        no_offer.accept_credential_offer_with(on_done);
        assert!(done.load(Ordering::SeqCst), "no offer");

        // A read-only session, which cannot mint at all.
        let read_only = Arc::new(ConnectionController::new(Arc::new(CannedBackend {
            outcome: Mutex::new(Some(Ok(read_only_info()))),
        })));
        read_only.connect(target(true));
        read_only.credential_offer.set(Some("Mock rig".to_string()));
        let (done, on_done) = flag();
        read_only.accept_credential_offer_with(on_done);
        assert!(done.load(Ordering::SeqCst), "read-only session");

        // A connected session with no saver installed — every headless suite
        // that never mounts Connect.
        let no_saver = controller();
        no_saver.connect(target(true));
        let (done, on_done) = flag();
        no_saver.accept_credential_offer_with(on_done);
        assert!(done.load(Ordering::SeqCst), "no saver installed");
    }

    // -----------------------------------------------------------------------
    // The heartbeat ping
    // -----------------------------------------------------------------------

    /// A ping with no wire behind it measures nothing, says so quietly, and —
    /// the part that matters — hands its single-flight claim back, so the
    /// Sessions strip's every-3s heartbeat is not wedged shut for the rest of
    /// the mount by one no-op tick.
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

    /// The real flow, against the in-process fixture: a live connection's ping
    /// re-times `GetVersion` and republishes the handshake with the new
    /// measurement and nothing else touched.
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

        // The dial itself is driven here rather than through `connect_with`:
        // that path hands the work to `frust::spawn`, whose runtime no plain
        // `#[tokio::test]` stands up (the net-backend tests above take the
        // same route). The controller holds the same backend either way, so
        // its `client()` — all a ping needs — is the production one.
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

    // -----------------------------------------------------------------------
    // Teardown
    // -----------------------------------------------------------------------

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

    /// The teardown order, observed from inside the backend: the client is
    /// let go while the app still reads as connected, so no screen can see a
    /// live connection with a dead client behind it.
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

    // -----------------------------------------------------------------------
    // Switching servers, the epoch, and locking
    // -----------------------------------------------------------------------

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
        // A pending dial's prologue, as `dial` runs it for a real backend.
        let stale = controller.next_generation();
        controller.connecting.set(true);
        // Backed out of (an app lock, say), then a switch to another server.
        controller.disconnect();
        controller.switch_to(target_at("b.example"), |_| {});
        let epoch = controller.connection_epoch.get_untracked();
        backend.clear();

        let (done, on_done) = settled_flag();
        let late = MockConnectionBackend.begin_connect(target_at("x.example"));
        let ConnectRun::Ready(outcome) = late else {
            unreachable!("the mock always answers on the spot");
        };
        controller.settle(stale, outcome, Some("X".to_string()), on_done);

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

    // -----------------------------------------------------------------------
    // Trust mapping
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // The real backend, against the in-process fixture
    // -----------------------------------------------------------------------

    /// The whole wire flow end to end: a pinned dial, a login, an installed
    /// session token, a timed `GetVersion`, and a teardown that lets the client
    /// go.
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

    /// A rejected credential is an auth failure, not a retryable network one —
    /// the mapping `crate::net::error` owns, exercised through the flow.
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

    /// A dial overtaken by a teardown or a newer dial keeps nothing: it can
    /// neither resurrect a closed connection nor overwrite the newer client.
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

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    /// The mock's answers, plus an ordered log of every backend call.
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

    /// A read-only server's facts, for the offer-eligibility test.
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

    /// A backend answering one canned outcome — for the facts the mock backend
    /// deliberately never produces.
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

    /// A mock backend that reads the controller's own `connected` signal back
    /// as it is torn down — the only way to observe the teardown *order* from
    /// outside the controller. `Weak` so the test double cannot keep the
    /// controller that owns it alive.
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

        /// Point the double at the controller it was handed to.
        fn watch(&self, controller: &Arc<ConnectionController>) {
            *self
                .controller
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(Arc::downgrade(controller));
        }

        /// What `connected` read while the teardown ran, or `None` if none has.
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
