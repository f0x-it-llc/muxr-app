//! [`ConnectionController`] — muxr's one app-scoped connection handle — and
//! the [`ConnectionBackend`] seam it drives.
//!
//! Lives beside [`crate::failure`] rather than inside any one feature's
//! `{domain, data, presentation}` slice: whether the app is connected gates
//! *routing* itself (`crate::routes`'s session guard) and is read by at least
//! two features (Sessions renders [`ConnectionController::server_info`]'s
//! latency, Settings renders the rest), so it is composition-root state, not
//! `connect`'s alone.
//!
//! Constructed exactly once, in `MuxrApp::init`, via
//! [`provide_connection_controller`] — published under the root owner
//! (`clean_signals_frust::provide_controller`) so any screen recovers the
//! *same* handle via [`expect_connection_controller`] (itself a thin
//! `use_context` wrapper), and so does `crate::routes`'s top-level session
//! guard, which has no `&mut AppState` to reach a controller through any
//! other way.
//!
//! # The backend seam
//!
//! The controller owns the *state machine* (which signals a connect writes, in
//! what order, and what a teardown resets); a [`ConnectionBackend`] owns the
//! *work*. Two implementations live here:
//!
//! - [`NetConnectionBackend`] — the real flow over [`crate::net`]: dial under
//!   the target's [`TrustConfig`], `Login`, install the minted session token,
//!   `GetVersion` (timed, which is what [`ServerInfo::latency_ms`] reports).
//! - [`MockConnectionBackend`] — the in-memory facts the app ran on before a
//!   wire existed, unchanged and **synchronous**.
//!
//! That synchrony is a contract, not an accident: every headless suite in
//! `tests/` calls `connect` and asserts the connected state on the next line,
//! with no frame pumped in between. [`ConnectRun`] is what lets one controller
//! serve both — a backend either hands back an outcome that is already
//! decided, or the future that will decide it. A `tonic` dial cannot be polled
//! outside the runtime that owns its reactor, so "just make it async and poll
//! it once" is not an option; the two arms name the difference instead of
//! hiding it.
//!
//! A suite opts the process into the mock through
//! [`use_mock_connection_for_tests`] — the same seam shape
//! `crate::features::settings::di::use_mock_preferences_for_tests` uses, and
//! for the same reason: an integration test binary links the *normal* build of
//! this crate (never one compiled with `--cfg test`), so there is no
//! `cfg(test)` for [`provide_connection_controller`] to branch on. The one call
//! site is `tests/support::setup()`, which every suite already runs before
//! mounting anything.
//!
//! # The connect flow
//!
//! [`ConnectTarget::trust_mode`] maps onto a [`TrustConfig`] *before* a socket
//! is opened, and [`TrustMode::Pinned`] with a missing or malformed fingerprint
//! fails right there — a pin that cannot be parsed never degrades into a
//! weaker posture (`crate::net::tls`'s fail-closed contract). Then: dial,
//! `Login` (the only place [`ConnectTarget::token`] crosses the wire),
//! `set_session_token`, timed `GetVersion`, publish. Teardown runs the other
//! way: scrub the session token, drop the client (which is how a `muxrd`
//! connection closes — see [`crate::net::client`]), then reset the signals,
//! `connected` last, since that write is what makes the session guard bounce.
//!
//! **Token hygiene.** The plaintext auth token is dropped — and so zeroized —
//! as soon as the `Login` request is built, and the minted session token is
//! scrubbed out of the response message once the client holds it. What this
//! cannot reach is the encoded request frame `tonic` owns; that buffer is not
//! addressable from this layer.
//!
//! # Threading
//!
//! A real dial runs on the background runtime through [`frust::spawn`], and a
//! UI-thread coordinator ([`frust::spawn_local`]) awaits its result and is the
//! **sole signal writer** — the `use_task` split `docs/CODE_STANDARDS.md`
//! mandates for heavy work, hand-rolled because a connect is imperative (a
//! press) rather than a component's first load. No signal is written off the UI
//! thread. Polling socket futures on the UI queue would couple the handshake to
//! the frame clock and is deliberately not what happens here.
//!
//! # Failure seam
//!
//! [`ConnectionController`] embeds a [`ControllerCore`] by composition (the
//! `examples/huddle` controller shape) and exposes it via
//! [`core`](ConnectionController::core) plus `AsRef`. Every failure this
//! controller raises — a refused dial, a rejected token, an expired session —
//! is emitted there, and `MuxrApp::init` subscribes the app's toast bridge to
//! it once. That is why nothing in this module names `crate::ui`.
//!
//! One vocabulary gap rides along, recorded in `crate::net::error`: a
//! server-side `PERMISSION_DENIED` (a read-only credential refusing a mutating
//! RPC) collapses into the retryable [`AppFailure::Network`] bucket. Splitting
//! it would mean a new [`AppFailure`] variant and a new toast string for every
//! caller; [`ConnectionController::is_read_only`] is the primary defence
//! instead — the screens disable what a read-only credential cannot do, and the
//! server's refusal is the fallback nobody should reach.
//!
//! # Credential offer placement
//!
//! [`ConnectionController::credential_offer`] is hosted here (not on `connect`)
//! for the reason everything else in this file is: a settled connect writes it,
//! a teardown clears it, and it has to survive a navigation. What *answers* it
//! is the **Connect screen**, before the redirect to Sessions — the user who
//! just typed a server is asked about that server while still looking at the
//! form they typed it into.
//!
//! Sessions used to render it as a banner instead, because the Connect screen
//! unmounts on the success redirect before its own offer UI could show. The way
//! out of that lifecycle trap is not a second screen: it is **not redirecting
//! yet**. `connect`'s completion closure reads this signal and, when it holds an
//! offer, raises the save dialog on Connect and holds the push to `/sessions`
//! until the answer resolves — a save that finished either way, a decline, or a
//! dismissal. Connect therefore stays mounted for exactly as long as the
//! question is on screen, and the redirect that used to race it is the thing
//! waiting on it. [`ConnectionController::accept_credential_offer_with`] is that
//! handshake's completion seam, and it is total by contract: every refusal path
//! below still runs the callback, so a save that fails — or one no
//! [`CredentialSaver`] is installed to run at all — can never strand the user on
//! Connect.
//!
//! Eligibility is split deliberately: the **caller** decides whether a target
//! is offerable at all ([`ConnectTarget::offer_credential`] — a manual,
//! non-pairing connect the user asked to be remembered, for an endpoint no
//! stored credential already covers), because only the caller has the saved
//! server list, and this module must not depend on a feature's repository. The
//! controller ANDs that with the one fact only a completed login knows: a
//! read-only credential is never offered for saving.
//!
//! Accepting the offer has to *do* something, and what it does — mint a token,
//! raise a biometric prompt, write a secure store, update a saved-server row —
//! belongs to the `connect` feature, not here. [`CredentialSaver`] is the seam
//! that keeps it there: this module names a one-method trait and calls it,
//! `connect` installs an implementation through
//! [`set_credential_saver`](ConnectionController::set_credential_saver), and no
//! feature type crosses into this file. With none installed (a headless suite
//! that never mounts Connect), accepting still clears the offer, saves nothing,
//! and runs the completion callback described above.

use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use clean_signals::ControllerCore;
use clean_signals_frust::{expect_controller, provide_controller};
use frust::{GetUntracked, RwSignal, Set, spawn, spawn_local};
use tokio::sync::oneshot;
use zeroize::{Zeroize, Zeroizing};

use crate::failure::AppFailure;
use crate::net::proto::{Backend, Empty, LoginRequest, VersionInfo};
use crate::net::{MuxrClient, TrustConfig, error};

/// Mock round-trip latency [`MockConnectionBackend`] reports — Sessions'
/// `conn-row` renders it, and a mock connection times nothing.
/// `docs/redesign.html`'s sessions strip prints `36ms`, so the mock says 36.
const MOCK_LATENCY_MS: u32 = 36;
/// Mock multiplexer version string [`MockConnectionBackend`] reports — the
/// value `docs/redesign.html`'s sessions strip prints (`zellij 0.44.3 · 36ms`).
/// It names the **multiplexer** the daemon fronts rather than the daemon build,
/// because that is what the strip's meta text says.
const MOCK_SERVER_VERSION: &str = "zellij 0.44.3";
/// The version [`MockConnectionBackend`] reports for its one backend — the
/// numeric half of [`MOCK_SERVER_VERSION`].
const MOCK_ZELLIJ_VERSION: &str = "0.44.3";

/// The capability flag a server sets when `AttachTerminal` understands mouse
/// frames. The one documented value of `VersionInfo.capabilities` today;
/// unknown flags are carried verbatim and ignored, as the wire requires.
pub const CAP_MOUSE_INPUT: &str = "mouse-input";

/// What [`ServerInfo::version`] reads when a server reported neither a backend
/// nor the back-compat `zellij_version` field.
const UNKNOWN_VERSION: &str = "unknown";

/// The failure a [`TrustMode::Pinned`] target with no fingerprint fails closed
/// with — before any socket is opened.
const MISSING_PIN: &str = "a pinned connection needs a certificate fingerprint";
/// The failure a connect settles with when its background task vanished
/// without answering (an aborted runtime — never a server behaviour).
const DIAL_ABANDONED: &str = "the connect task ended without an answer";
/// The detail [`ConnectionController::on_session_expired`] logs. The user sees
/// [`AppFailure::user_message`](clean_signals::Failure::user_message), never
/// this.
const SESSION_EXPIRED: &str = "the server rejected the session token";

/// A connected server's facts — Sessions' `conn-row` strip renders
/// [`Self::endpoint`], [`Self::version`], [`Self::latency_ms`] and (for the one
/// posture that skips certificate validation) [`Self::trust_mode`]; Settings
/// renders the TLS posture and the version again.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerInfo {
    /// `host:port`, as dialed.
    pub endpoint: String,
    /// The connection's TLS verification posture, carried through from
    /// [`ConnectTarget::trust_mode`]. **This** is what a screen branches on
    /// (a warning-hued badge for the posture that skips validation);
    /// [`Self::tls_label`] is display text only — never match on it.
    pub trust_mode: TrustMode,
    /// A human label for [`Self::trust_mode`] (e.g. "System trust"), derived
    /// once here so every screen prints the same words.
    pub tls_label: String,
    /// The multiplexer version behind the connection (`"zellij 0.44.3"`) —
    /// the first half of the sessions strip's dim meta text
    /// (`zellij 0.44.3 · 36ms`; [`Self::latency_ms`] is the other half).
    /// Derived from the first detected backend, falling back to the wire's
    /// back-compat zellij field and then to [`UNKNOWN_VERSION`].
    pub version: String,
    /// The measured `GetVersion` round trip, milliseconds.
    pub latency_ms: u32,
    /// A version per detected backend — [`Self::version`] is the first of
    /// these, formatted for the strip.
    pub backend_versions: Vec<BackendVersion>,
    /// The backends this server can actually front. A wire value this build
    /// does not know is dropped rather than guessed at.
    pub available_backends: Vec<ServerBackend>,
    /// The server's capability flags, e.g. [`CAP_MOUSE_INPUT`]. Additive on the
    /// wire and unknown values are kept, so this is a set of raw strings rather
    /// than a closed enum — ask through [`Self::supports`].
    pub capabilities: BTreeSet<String>,
    /// Whether the credential this connection logged in with is read-only.
    /// Mirrored onto [`ConnectionController::is_read_only`], which is what a
    /// screen reads to disable the controls a read-only token would be refused
    /// for.
    pub is_read_only: bool,
}

impl ServerInfo {
    /// Whether the server advertised `capability` (e.g. [`CAP_MOUSE_INPUT`]).
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }
}

/// A multiplexer a `muxrd` server can front.
///
/// The app's own closed enum over the wire's `Backend`: the generated type is
/// an `i32` with an `UNSPECIFIED` arm and room for values this build has never
/// heard of, neither of which a screen should have to match on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ServerBackend {
    /// zellij.
    Zellij,
    /// herdr.
    Herdr,
}

impl ServerBackend {
    /// This backend's lowercase name, as the sessions strip and the settings
    /// rows print it.
    pub fn label(self) -> &'static str {
        match self {
            ServerBackend::Zellij => "zellij",
            ServerBackend::Herdr => "herdr",
        }
    }

    /// The backend a wire enum value names, or `None` for `UNSPECIFIED` and for
    /// any value this build does not know.
    fn from_wire(value: i32) -> Option<Self> {
        match Backend::try_from(value) {
            Ok(Backend::Zellij) => Some(ServerBackend::Zellij),
            Ok(Backend::Herdr) => Some(ServerBackend::Herdr),
            Ok(Backend::Unspecified) | Err(_) => None,
        }
    }
}

/// One backend's version, as the handshake reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendVersion {
    /// Which backend.
    pub backend: ServerBackend,
    /// Its version string, verbatim from the server.
    pub version: String,
}

/// A connection's TLS verification posture — carried on [`ConnectTarget`],
/// mapped onto a [`TrustConfig`] at dial time, and rendered back as
/// [`ServerInfo::tls_label`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustMode {
    /// The platform's system trust store validates the certificate normally.
    System,
    /// No certificate verification at all: any certificate the server
    /// presents is accepted. Named for what actually happens rather than for
    /// the trust-on-first-use scheme this variant used to promise — nothing
    /// ever records a first-seen fingerprint, so this posture authenticates
    /// the server on no connection, first or otherwise.
    NoVerification,
    /// A certificate pinned ahead of time (the QR-pairing flow), whose
    /// fingerprint rides on [`ConnectTarget::fingerprint`].
    Pinned,
}

impl TrustMode {
    /// The label [`ServerInfo::tls_label`] renders for this mode.
    fn label(self) -> &'static str {
        match self {
            TrustMode::System => "System trust",
            TrustMode::NoVerification => "No verification",
            TrustMode::Pinned => "Pinned certificate",
        }
    }
}

/// A connect request's target, credential and options.
///
/// Built by whoever starts a connect — the Connect form
/// (`features::connect`'s `ConnectController::validated_target`), and later a
/// scanned pairing payload.
#[derive(Clone, PartialEq)]
pub struct ConnectTarget {
    /// The host to dial.
    pub host: String,
    /// The port to dial.
    pub port: u16,
    /// This target's TLS verification posture.
    pub trust_mode: TrustMode,
    /// A user-facing label for this server (e.g. "Home rig") — carried
    /// through to [`ConnectionController::credential_offer`] when
    /// [`Self::offer_credential`] is set.
    pub label: String,
    /// Whether a successful connect is *eligible* to offer saving this
    /// target's credential. The caller's half of the split the module docs'
    /// credential-offer section describes: a read-only login is refused the
    /// offer regardless.
    pub offer_credential: bool,
    /// The login token this connect authenticates with — the one secret that
    /// crosses the wire, [`Zeroizing`]-wrapped so dropping this target scrubs
    /// it rather than leaving plaintext in a freed allocation.
    pub token: Zeroizing<String>,
    /// The `remember_me` flag `Login` carries: a remembered session token lives
    /// 28 days, an unremembered one about five minutes. Either way the login
    /// happens — this is the token's lifetime, not whether to log in.
    pub remember: bool,
    /// The pinned certificate's SHA-256 as 64 lowercase hex characters —
    /// [`TrustMode::Pinned`]'s input, and required by it: a pinned target
    /// without one never dials.
    pub fingerprint: Option<String>,
}

impl fmt::Debug for ConnectTarget {
    /// Hand-written so [`Self::token`] never prints: `Zeroizing`'s own `Debug`
    /// forwards straight to the wrapped `String`, so a derived impl would put
    /// the login token in the clear in any log line that formats a target.
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

// ---------------------------------------------------------------------------
// The backend seam
// ---------------------------------------------------------------------------

/// What a completed connect produces: the connected server's facts, or why the
/// connection did not open.
pub type ConnectOutcome = Result<ServerInfo, AppFailure>;

/// The work a [`ConnectRun::Pending`] still has to do.
pub type ConnectFuture = Pin<Box<dyn Future<Output = ConnectOutcome> + Send>>;

/// How far a [`ConnectionBackend::begin_connect`] got before it had to hand
/// control back. See the module docs' backend-seam section for why this is a
/// sum type rather than a single future.
pub enum ConnectRun {
    /// Already decided, with no IO left to drive — the mock backend's every
    /// connect, and any real connect rejected before a socket was opened (a
    /// pin that fails to parse).
    Ready(ConnectOutcome),
    /// A dial in progress. Must be driven on the background runtime, never on
    /// the UI thread's frame-coupled queue.
    Pending(ConnectFuture),
}

/// The work behind [`ConnectionController`]: opening a connection, tearing one
/// down, and holding the live client in between.
///
/// Deliberately narrow. Everything observable — which signals a connect writes,
/// what a teardown resets, whether a credential offer is raised — belongs to
/// the controller, so both implementations of this trait produce the same
/// behaviour from the same state machine.
pub trait ConnectionBackend: Send + Sync + 'static {
    /// Begin connecting to `target`. See [`ConnectRun`].
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
}

// ---------------------------------------------------------------------------
// The credential-save seam
// ---------------------------------------------------------------------------

/// The work behind an accepted credential offer — installed by the `connect`
/// feature, called by [`ConnectionController::accept_credential_offer`].
///
/// One method, and only plain data crosses it, so this module stays free of any
/// feature's repositories (see the module docs' credential-offer section). The
/// implementor owns everything the save actually involves: minting the token,
/// raising the biometric prompt, writing the secure store, and updating the
/// saved-server row.
pub trait CredentialSaver: Send + Sync {
    /// Save a credential for the connected server at `endpoint` (`host:port`),
    /// which the offer showed the user as `label`, and run `on_done` once that
    /// save has settled — **however** it settled.
    ///
    /// Called on the UI thread, from the press that accepted the offer, and
    /// expected to return immediately — the prompt and the stores behind it are
    /// the implementor's own asynchronous work. `on_done` is what the caller
    /// resumes on (the Connect screen's held navigation, see the module docs'
    /// credential-offer section), so an implementation that drops it strands
    /// that caller: it must run on the success path, on every failure path, and
    /// on any early return of its own. It is boxed rather than generic to keep
    /// this trait object-safe, and carries no `Send` bound because it is
    /// consumed on the same UI thread the call arrived on.
    fn save_credential(&self, endpoint: String, label: String, on_done: Box<dyn FnOnce()>);
}

// ---------------------------------------------------------------------------
// The mock backend
// ---------------------------------------------------------------------------

/// The in-memory backend: derives a [`ServerInfo`] from the target and answers
/// on the spot. Never fails, and never touches a socket.
///
/// Its facts are the ones `docs/redesign.html` prints, and every headless suite
/// asserts against them — see the module docs' backend-seam section.
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

// ---------------------------------------------------------------------------
// The real backend
// ---------------------------------------------------------------------------

/// The `muxrd` backend: a real dial + `Login` + `GetVersion` over
/// [`crate::net`], holding the connected [`MuxrClient`] until a teardown drops
/// it.
pub struct NetConnectionBackend {
    /// The live client. Shared with the in-flight dial (which installs it on
    /// success) rather than held behind `&self`, since that future outlives the
    /// `begin_connect` call that built it.
    live: Arc<Mutex<Option<MuxrClient>>>,
}

impl NetConnectionBackend {
    /// A backend with nothing open.
    pub fn new() -> Self {
        Self {
            live: Arc::new(Mutex::new(None)),
        }
    }

    /// The live-client slot. Poisoning is recovered from rather than propagated
    /// — a panic elsewhere must not make the app permanently unable to connect.
    fn slot(&self) -> MutexGuard<'_, Option<MuxrClient>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
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
            // Fail-closed: a pin that cannot be parsed never dials at all.
            Err(failure) => return ConnectRun::Ready(Err(failure)),
        };
        ConnectRun::Pending(Box::pin(open_session(
            Arc::clone(&self.live),
            target,
            trust,
        )))
    }

    fn disconnect(&self) {
        let Some(client) = self.slot().take() else {
            return;
        };
        // Scrub the credential before letting go: dropping this handle closes
        // the transport only if it was the last clone, while clearing the token
        // closes the authenticated surface for every clone there is.
        client.clear_session_token();
        drop(client);
    }

    fn client(&self) -> Option<MuxrClient> {
        self.slot().clone()
    }
}

/// The TLS posture `target` dials under.
///
/// [`TrustMode::NoVerification`] maps onto the accept-any verifier: it
/// authenticates nothing — which is exactly why it is the one the sessions
/// strip flags with a warning chip.
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
/// a `GetVersion`, keep the client, and hand back what the screens render.
async fn open_session(
    live: Arc<Mutex<Option<MuxrClient>>>,
    mut target: ConnectTarget,
    trust: TrustConfig,
) -> ConnectOutcome {
    let client = MuxrClient::connect(&target.host, target.port, trust).await?;

    let request = LoginRequest {
        auth_token: target.token.as_str().to_owned(),
        remember_me: target.remember,
    };
    // The token's job ends with the request that carries it (see the module
    // docs' token-hygiene note): drop this app's own copy now rather than at
    // the end of the flow.
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
    *live.lock().unwrap_or_else(PoisonError::into_inner) = Some(client);
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

/// Muxr's one app-scoped connection handle. See the [module docs](self).
pub struct ConnectionController {
    core: ControllerCore<AppFailure>,
    backend: Arc<dyn ConnectionBackend>,
    /// Bumped by every connect and every teardown. A coordinator whose captured
    /// value no longer matches is holding the result of a connection nobody
    /// asked for any more, and drops it instead of publishing it.
    generation: AtomicU64,
    /// Whether the app currently holds a live connection. Read by
    /// `crate::routes`'s session guard and by any screen.
    pub connected: RwSignal<bool>,
    /// Whether a connect is in flight — the Connect screen's busy state, and
    /// the single-flight guard: a second connect while this is set is ignored.
    pub connecting: RwSignal<bool>,
    /// The connected server's facts, or `None` while disconnected.
    pub server_info: RwSignal<Option<ServerInfo>>,
    /// Whether the credential this connection logged in with is read-only —
    /// [`ServerInfo::is_read_only`], hoisted into its own signal because it
    /// gates controls on screens that never render the rest of the facts.
    pub is_read_only: RwSignal<bool>,
    /// The server label whose credential the just-settled connect should offer
    /// to save, or `None` when there is nothing to offer. Written by
    /// [`Self::settle`] **before** its `on_done`, which is what lets the Connect
    /// flow read it back and hold its own redirect on the answer (see the module
    /// docs' credential-offer section).
    pub credential_offer: RwSignal<Option<String>>,
    /// What an accepted offer actually saves through, or `None` while nothing
    /// has installed one — see [`CredentialSaver`].
    credential_saver: Mutex<Option<Arc<dyn CredentialSaver>>>,
}

impl ConnectionController {
    /// A controller over `backend`. Private: [`provide_connection_controller`]
    /// is the one production construction site.
    fn new(backend: Arc<dyn ConnectionBackend>) -> Self {
        Self {
            core: ControllerCore::new(),
            backend,
            generation: AtomicU64::new(0),
            connected: RwSignal::new(false),
            connecting: RwSignal::new(false),
            server_info: RwSignal::new(None),
            is_read_only: RwSignal::new(false),
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

    /// Connect to `target`, ignoring the outcome — the fire-and-forget form for
    /// callers with nothing to do afterwards. See [`Self::connect_with`].
    pub fn connect(self: &Arc<Self>, target: ConnectTarget) {
        self.connect_with(target, |_| {});
    }

    /// Connect to `target` and run `on_done` on the UI thread once the attempt
    /// settles — with the mock backend, before this call even returns.
    ///
    /// Ignored outright while another connect is in flight: one app, one
    /// connection, and a second press must not open a second socket.
    ///
    /// `on_done` is for what only the caller can do (navigate, scrub its own
    /// form); it is not the failure path. A failed connect is emitted on
    /// [`Self::core`]'s sink, which is where the app's toast bridge listens.
    pub fn connect_with(
        self: &Arc<Self>,
        target: ConnectTarget,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        if self.connecting.get_untracked() {
            return;
        }
        // The caller's half of the credential-offer decision, taken before the
        // target moves into the backend.
        let offer = target.offer_credential.then(|| target.label.clone());
        let generation = self
            .generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);
        self.connecting.set(true);

        match self.backend.begin_connect(target) {
            ConnectRun::Ready(outcome) => self.settle(generation, outcome, offer, on_done),
            ConnectRun::Pending(work) => {
                // The heavy-work split: the dial runs on the background runtime,
                // and the UI-thread coordinator below is the only thing that
                // writes a signal (see the module docs' threading note).
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
            // A teardown landed while this dial was in flight, so whatever it
            // opened belongs to nobody: drop it rather than publishing a
            // connection the user already walked away from.
            if outcome.is_ok() {
                self.backend.disconnect();
            }
            return;
        }
        self.connecting.set(false);

        match outcome {
            Ok(info) => {
                let read_only = info.is_read_only;
                self.is_read_only.set(read_only);
                self.server_info.set(Some(info));
                self.connected.set(true);
                // A read-only credential is never offered for saving — see the
                // module docs' credential-offer section.
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

    /// Close the connection and clear every connection-scoped signal. Callers
    /// navigate away themselves — this never touches the router.
    ///
    /// Order is the contract: the generation bump abandons any dial still in
    /// flight, the backend scrubs and drops the client, and `connected` is
    /// written **last**, since that write is what the session guard bounces on.
    pub fn disconnect(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.backend.disconnect();
        self.connecting.set(false);
        self.server_info.set(None);
        self.is_read_only.set(false);
        self.credential_offer.set(None);
        self.connected.set(false);
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

    /// Install what an accepted offer saves through, replacing any previous one
    /// — the `connect` feature's own [`CredentialSaver`] (see the module docs).
    ///
    /// Called from the Connect screen's `Component::init`, which is where every
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
    /// The offer is cleared **first** and unconditionally, so a second press
    /// (or a saver that answers slowly) cannot start the save twice. Three
    /// things make the save itself a no-op beyond that clear: no offer pending,
    /// no live connection to name an endpoint, and a read-only session — which
    /// cannot mint the token a saved credential is made of, the same fact
    /// [`Self::settle`] refuses to raise the offer on in the first place.
    ///
    /// `on_done` runs on **every** one of those paths — and on the fourth, no
    /// [`CredentialSaver`] installed at all. It is the Connect screen's held
    /// navigation (see the module docs' credential-offer section): nothing about
    /// a save that could not run may leave the user parked on the form.
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
/// `false` (the real `muxrd` client) for every app run, flipped to `true` once
/// per test process before [`provide_connection_controller`] ever runs — see
/// the [module docs](self)' backend-seam section.
static USE_MOCK_CONNECTION_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real `muxrd` client for the rest of its
/// life: every later [`provide_connection_controller`] builds a
/// [`MockConnectionBackend`] instead of a [`NetConnectionBackend`]. Idempotent,
/// and a no-op once already set. Must run **before** `MuxrApp::init` to have
/// any effect — `tests/support::setup` is the one call site outside this
/// module.
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

/// Construct the app's one [`ConnectionController`], publish it under the root
/// owner, and hand it straight back. Called exactly once, from `MuxrApp::init`
/// — see the [module docs](self).
///
/// The return value is not a convenience: `crate::routes`' session guard is
/// closed over this `Arc` at construction rather than reading it from context
/// at navigation time (see `routes::session_guard_with`), and the composition
/// root is the one place that already holds it.
pub fn provide_connection_controller() -> Arc<ConnectionController> {
    let controller = Arc::new(ConnectionController::new(platform_backend()));
    provide_controller(Arc::clone(&controller));
    controller
}

/// Recover the app-scoped [`ConnectionController`] [`provide_connection_controller`]
/// published — reachable from any screen, and from `crate::routes`'s
/// top-level session guard, which has no `&mut AppState` to reach one through
/// any other way. Panics if called before `MuxrApp::init` has run (see
/// `clean_signals_frust::expect_controller`'s contract); every call site in
/// this app runs after it, so this is never hit in practice.
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

    /// The teardown order the module docs promise, observed from inside the
    /// backend: the client is let go while the app still reads as connected, so
    /// no screen can see a live connection with a dead client behind it.
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

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

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
