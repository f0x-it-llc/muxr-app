use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// What a settled connect's credential offer tells the screen's dialog.
pub struct CredentialOffer {
    pub label: String,
    pub replacing: bool,
}

/// Callback [`ConnectController::connect`] invokes when a settled connect raises
/// an offer instead of navigating; supplied by the screen, which holds the
/// overlay host this controller does not.
pub type OfferPrompt = Rc<dyn Fn(CredentialOffer)>;

/// Which connect a blocked dial re-runs once the paywall's answer clears it.
/// The entry point rather than the target: each one re-reads its own source
/// (the form, the stashed pairing, the row that was tapped).
#[derive(Clone)]
pub enum ConnectRetry {
    Manual,
    Pairing,
    Saved(SavedServer),
    /// The unlock dock, which re-reads the most recent row.
    Keyring,
}

/// Raised by the dock's unlock before it dials a [`TrustMode::NoVerification`]
/// row: the screen confirms, and `on_confirm` dials. Lent per call.
pub type ConfirmPrompt = Rc<dyn Fn(SavedServer, Box<dyn FnOnce()>)>;

/// The dock copy after re-enrollment made the vault unreadable and it was reset.
pub const VAULT_RESET_MESSAGE: &str =
    "Biometrics changed — saved credentials were reset. Connect again with a token.";
/// The dock copy after the user reset saved credentials from a failed unlock.
pub const MANUAL_RESET_MESSAGE: &str = "Saved credentials were reset. Connect again with a token.";

/// What the unlock dock shows, derived from the controller's signals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockState {
    Idle,
    Unlocking,
    Failed(DockFailure),
    /// No biometric gate on this device: the rows keep their own tap-to-unlock.
    Unavailable,
}

/// A failed unlock's copy, and whether it offers the credential reset.
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

/// What the free-tier server rule refused: the remembered rows standing in the
/// way, and the connect to re-run once they no longer do.
pub struct ServerLimitBlock {
    pub to_forget: Vec<SavedServer>,
    pub retry: ConnectRetry,
}

/// Callback a connect entry point invokes *instead of dialing* when the
/// free-tier rule blocks it; supplied by the screen, which owns the paywall
/// this controller cannot raise. Lent per call, exactly like [`OfferPrompt`].
pub type GatePrompt = Rc<dyn Fn(ServerLimitBlock)>;

/// Which Connect page a controller drives: the `/` home screen, or the
/// add-server form pushed over a live shell (`/?mode=add`), which dials
/// through [`ConnectionController::switch_to`] and lands by popping itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectMode {
    Home,
    Add,
}

/// Where a settled connect goes next: Home pushes Sessions; Add pops back to
/// the shell it covers (whose Sessions screen restarts on the new epoch) —
/// but only its own page, marked when the dial started, so a dial settling
/// after the user already backed out never pops the shell — and latches the
/// page shut against a second dial.
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

/// The trust posture a fresh form starts on — the safe one.
const DEFAULT_TRUST_MODE: TrustMode = TrustMode::System;

/// The port a bare host dials under [`TrustMode::System`]: a publicly trusted
/// certificate implies a TLS-terminating edge in front of `muxrd`, which this
/// app's own demo deployment fronts on the standard HTTPS port.
const PUBLIC_CA_DEFAULT_PORT: u16 = 443;

/// The port a bare host dials under [`TrustMode::NoVerification`] or
/// [`TrustMode::Pinned`]: `muxrd`'s own default bind port
/// (`DEFAULT_BIND = "127.0.0.1:50051"` in `muxr-core/muxrd/src/config.rs`),
/// dialed directly with no TLS-terminating edge in front of it.
const MUXRD_DEFAULT_PORT: u16 = 50051;

/// Whether a fresh form offers to remember the server. `true`, matching the mock.
const DEFAULT_REMEMBER: bool = true;

/// What a host:port parse reject says under the SERVER URL field.
pub const URL_ERROR_MESSAGE: &str = concat!(
    "Enter host:port, for example dev.home.local:50051. A bare host uses :443 ",
    "under Public domain (CA) and :50051 under Trust anyway.",
);

/// The prefix every token this device mints for itself carries, so a server's
/// token list says which device asked for each one.
const TOKEN_NAME_PREFIX: &str = "muxr-mobile";

/// How much of a server's label rides in a minted token's name. Long enough to
/// tell two rigs apart in the settings list, short enough to stay one line.
const TOKEN_LABEL_MAX: usize = 24;

pub struct ConnectController {
    core: ControllerCore<AppFailure>,
    repository: SavedServers,
    credentials: Credentials,
    tokens: SharedAuthTokensRepository,
    save_credential: SaveCredential,
    unlock_credential: UnlockCredential,
    connection: Arc<ConnectionController>,
    /// The app-scoped entitlement view model every dial's free-tier verdict is
    /// asked of. `Arc<ProController>`, so this controller stays `Send + Sync`.
    pro: Arc<ProController>,
    /// The app-scoped unlocked keyring the dock publishes into.
    keyring: KeyringHandle,
    /// The dock's one-prompt unlock, shared with Settings.
    keyring_unlock: KeyringUnlock,
    mode: ConnectMode,
    /// Add mode: set once a settled connect lands — the page is leaving, so
    /// every later dial is refused (see [`Self::busy_untracked`]).
    landed: Arc<AtomicBool>,
    /// Add mode: the page mark the last dial started on, for an offer
    /// answered after it settled.
    landing_mark: std::sync::Mutex<Option<PageMark>>,
    /// Probed once per mount: enrollment can change while the app is away.
    biometrics_available: bool,
    saving_credential: AtomicBool,
    pub url: RwSignal<String>,
    pub token: RwSignal<Zeroizing<String>>,
    pub trust_mode: RwSignal<TrustMode>,
    pub remember: RwSignal<bool>,
    pub saved: RwSignal<Vec<SavedServer>>,
    pub invalid: RwSignal<bool>,
    /// The inline line under the URL field: set by a validation reject (which
    /// raises no toast), cleared by a URL edit, an applied scanned payload and
    /// the start of a dial.
    pub url_error: RwSignal<Option<String>>,
    pub pending_pairing: RwSignal<Option<PairingPayload>>,
    pub unlocking: RwSignal<bool>,
    pub resolving_offer: RwSignal<bool>,
    /// The dock's last failed unlock, cleared by the next attempt.
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
        controller.reload_saved();
        controller
    }

    /// This controller, driving the add-server page instead of `/`.
    pub fn in_add_mode(mut self) -> Self {
        self.mode = ConnectMode::Add;
        self
    }

    pub fn mode(&self) -> ConnectMode {
        self.mode
    }

    /// Where a dial starting now lands once it settles: in Add mode, marks
    /// the page on top now and keeps that mark for the offer's answer.
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

    /// [`Self::landing`] for an answer to the offer the last dial raised:
    /// the dial's own mark, else (no dial recorded one) the page on top now.
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

    /// Dial `target` the way this page dials: `/` opens the first
    /// connection, the add-server page moves the live one.
    fn dial(&self, target: ConnectTarget, on_done: impl FnOnce(Result<(), AppFailure>) + 'static) {
        match self.mode {
            ConnectMode::Home => self.connection.connect_with(target, on_done),
            ConnectMode::Add => self.connection.switch_to(target, on_done),
        }
    }

    /// The [`CredentialSaver`] `crate::connection` calls on an accepted offer,
    /// held weakly since the connection handle outlives every screen; a
    /// gone-away screen makes the accept a no-op.
    pub fn credential_saver(self: &Arc<Self>) -> Arc<dyn CredentialSaver> {
        Arc::new(ControllerCredentialSaver {
            controller: Arc::downgrade(self),
        })
    }

    /// Re-read the saved-server list into [`Self::saved`]. A repository failure
    /// raises a toast and leaves the previous list in place.
    pub fn reload_saved(&self) {
        match self.repository.list() {
            Ok(servers) => self.saved.set(servers),
            Err(failure) => app_toasts().raise(&failure),
        }
    }

    /// Sets the URL field and clears stale validation. Also the manual-edit
    /// seam: an address that no longer matches a live pairing's host/port drops
    /// it (unparseable counts as mismatch) — why [`Self::apply_scanned_payload`]
    /// writes [`Self::url`] directly instead.
    pub fn set_url(&self, url: String) {
        self.invalidate_pairing_unless_covering(&url);
        self.url.set(url);
        if self.invalid.get_untracked() || self.url_error.get_untracked().is_some() {
            self.clear_url_validation();
        }
    }

    /// The one writer that rejects the URL: lights [`Self::invalid`] and sets
    /// the inline line together, so the two signals never disagree.
    fn reject_url(&self, message: &str) {
        self.invalid.set(true);
        self.url_error.set(Some(message.to_owned()));
    }

    /// The counterpart of [`Self::reject_url`]: clears both signals.
    fn clear_url_validation(&self) {
        self.invalid.set(false);
        self.url_error.set(None);
    }

    /// The install's current entitlement, tracked — what the saved rows' pro
    /// tags are computed from.
    pub fn entitlement(&self) -> Entitlement {
        self.pro.entitlement.get()
    }

    /// Sets the token field. A hand-typed token drops a live pairing — the
    /// pinned CTA sends the payload's token, so the two must never disagree.
    pub fn set_token(&self, token: String) {
        self.token.set(Zeroizing::new(token));
        self.clear_pending_pairing();
    }

    pub fn clear_token(&self) {
        self.token.set(Zeroizing::new(String::new()));
    }

    /// Sets the trust mode. A manual choice that disagrees with a stashed
    /// pairing's own posture ([`pairing_trust_mode`]) drops the pairing first
    /// (`clear_pending_pairing`, which also resets the mode to the default)
    /// and only then applies the chosen one — mirroring [`Self::set_token`]/
    /// [`Self::set_url`]: without this, the selector's radio would move while
    /// both CTAs kept dialing the pairing's own posture underneath it.
    /// Re-selecting the pairing's own posture is not an override, so the
    /// pairing survives it.
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

    /// Reconnects through the row's stored credential if it has one, else
    /// refills the form (no credential means no gate to raise).
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

    /// The row at `index`, if a tap must confirm before unlocking: a stored
    /// credential under [`TrustMode::NoVerification`] would replay a real secret
    /// over an unauthenticated channel. Predicate only — the dialog itself
    /// belongs to the screen.
    pub fn saved_row_needs_confirmation(&self, index: usize) -> Option<SavedServer> {
        let server = self.saved_row(index)?;
        (server.has_credential && server.trust_mode == TrustMode::NoVerification).then_some(server)
    }

    /// Whether a dial is in flight (the connection controller's own signal, not
    /// a copy) — the single-flight guard both CTAs share.
    pub fn connecting(&self) -> RwSignal<bool> {
        self.connection.connecting
    }

    /// Whether a dial, an unlock, or an unanswered offer is in flight — tracked,
    /// for rendering busy state.
    pub fn busy(&self) -> bool {
        self.connection.connecting.get() || self.unlocking.get() || self.resolving_offer.get()
    }

    /// [`Self::busy`] without subscribing, for press handlers with no owner to
    /// subscribe — plus an add-server page that has already landed, whose
    /// second dial (a double tap) would re-dial with a scrubbed form.
    /// `pub(crate)` for the QR affordance's own use.
    pub(crate) fn busy_untracked(&self) -> bool {
        self.landed.load(Ordering::SeqCst)
            || self.connection.connecting.get_untracked()
            || self.unlocking.get_untracked()
            || self.resolving_offer.get_untracked()
    }

    // -----------------------------------------------------------------------
    // The free-tier server gate (`features::pro`'s rule, ahead of every dial)
    // -----------------------------------------------------------------------

    /// The free-tier verdict for `endpoint`, raised on `gate_prompt` when it
    /// refuses. `true` means the caller must return without dialing: a blocked
    /// connect saves, unlocks, writes and toasts nothing, since the only place
    /// a new saved row is written is a successful login.
    fn blocked(&self, endpoint: &str, retry: ConnectRetry, gate_prompt: &GatePrompt) -> bool {
        match self.pro.verdict_for(&self.saved.get_untracked(), endpoint) {
            ServerLimitVerdict::Allowed => false,
            ServerLimitVerdict::Blocked { to_forget } => {
                gate_prompt(ServerLimitBlock { to_forget, retry });
                true
            }
        }
    }

    /// [`Self::blocked`] for a tapped saved row, taken by the screen *ahead* of
    /// [`Self::recover_and_connect`] rather than inside it: the row's own
    /// no-verification confirmation sits between the two and dials through the
    /// ungated method, so the verdict has to be taken before that dialog is
    /// raised (`widgets::pro_gate` owns the ordering). A row with no stored
    /// credential only refills the form, so there is nothing to judge.
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

    /// Where `endpoint` sits in [`Self::saved`] now. A retry holds the row it
    /// was blocked on across a forget, which moves every later row's index.
    pub fn saved_row_index(&self, endpoint: &str) -> Option<usize> {
        self.saved
            .get_untracked()
            .iter()
            .position(|server| server.endpoint() == endpoint)
    }

    // -----------------------------------------------------------------------
    // The pairing path (see the module docs)
    // -----------------------------------------------------------------------

    /// Prefills the form from a scanned payload and stashes it; never connects.
    /// Writes [`Self::url`]/[`Self::token`] directly — their setters would clear
    /// the payload being stashed.
    pub fn apply_scanned_payload(&self, payload: PairingPayload) {
        self.url.set(pairing_endpoint(&payload));
        self.token.set(payload.token.clone());
        self.trust_mode.set(pairing_trust_mode(&payload.trust));
        self.clear_url_validation();
        self.pending_pairing.set(Some(payload));
    }

    /// Drops the stashed pairing and resets trust mode to the manual default — a
    /// leftover [`TrustMode::Pinned`] would fail closed at dial time. No-op when
    /// empty.
    pub fn clear_pending_pairing(&self) {
        if self.pending_pairing.with(Option::is_none) {
            return;
        }
        self.pending_pairing.set(None);
        self.trust_mode.set(DEFAULT_TRUST_MODE);
    }

    /// Drop the stashed pairing unless `url` still names its host and port.
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

    /// The pinned CTA: connects with the stashed pairing's own credentials.
    /// No-op with nothing stashed, or while [`Self::busy`]. On success, scrubs
    /// the token and raises the paired credential's one save prompt (session
    /// already live, so a denial changes nothing); on failure, leaves the form
    /// untouched for a one-tap retry.
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
            // The payload stays stashed, so the retry dials this same code.
            return;
        }
        self.clear_url_validation();
        let landing = self.landing(nav);
        // Both `Copy`; the closure may outlive this borrow (see `Self::connect`).
        let token = self.token;
        let pending = self.pending_pairing;
        let upsert = self.login_upsert(&target);
        let controller = Arc::clone(self);
        // Stored as scanned — never re-minted; the QR already is the credential.
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

    /// The [`ConnectTarget`] this form describes, or why it doesn't; split out
    /// so validation + mapping is unit-testable without a live navigator.
    ///
    /// `offer_credential` is set whenever remember is checked and this isn't a
    /// pairing — including an already-saved endpoint, since that's exactly where
    /// a revoked credential needs replacing. A hand-picked [`TrustMode::Pinned`]
    /// carries no fingerprint and fails closed at dial time, except when a
    /// matching stashed pairing supplies its own posture and fingerprint (never
    /// offerable).
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
            // A known endpoint keeps its stored label; a new one is labelled by its host.
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

    /// Whether a stored credential for `host:port` already exists — what makes an
    /// offer a *replacement*.
    fn stores_credential_for(&self, host: &str, port: u16) -> bool {
        self.saved
            .get_untracked()
            .iter()
            .any(|server| server.host == host && server.port == port && server.has_credential)
    }

    /// Validates the form and, once the dial actually settles, pushes
    /// [`SESSIONS_LOCATION`] through the app's guarded router.
    ///
    /// `nav`/`offer_prompt` are lent, not held: both wrap `Rc`, so are neither
    /// `Send` nor `Sync`, while this controller must be both.
    ///
    /// A rejection sets [`Self::url_error`] and lights [`Self::invalid`], with
    /// no toast; a refused dial toasts and leaves the form as typed, for a
    /// one-tap retry. Neither navigates.
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
                    // Before `invalid` too: a blocked connect writes nothing at
                    // all, and the form is left exactly as it was typed.
                    return;
                }
                self.clear_url_validation();
                let landing = self.landing(nav);
                // Token's job ends once the connection opens with it; captured by
                // value (`RwSignal` is `Copy`) since the closure may outlive this borrow.
                let token = self.token;
                // The settled connect reads the offer back off the connection's own signal.
                let offer = self.connection.credential_offer;
                let resolving = self.resolving_offer;
                // Whether this offer would be a *replacement* — read before the dial.
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
                // Intentional: every validation reject takes the inline line, not a
                // toast (the plan's Phase 2 decision), superseding "Compact
                // byte-for-byte" for this behaviour. `validated_target`'s only
                // failure today is the URL parse, so the fixed message is honest.
                // `parse_endpoint` only ever fails as `Network`; a second source
                // (any other kind) must not silently reuse the fixed message.
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

    /// Runs the accepted offer's save, then lands ([`Landing`]) once it
    /// settles (however it settled) — the prompt is answered on Connect, not
    /// over Sessions.
    ///
    /// The add-server page saves through itself rather than the connection's
    /// installed [`CredentialSaver`]: that saver is `/`'s, still retained
    /// beneath the shell, and this short-lived page must not replace it. The
    /// save is the same [`Self::begin_credential_save`] either way, with the
    /// connection's own guards (no offer, no endpoint) mirrored here.
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

    /// Declined or dismissed (same outcome): drops the offer and lands, as
    /// the connect would have done unprompted.
    pub fn decline_offer_then_navigate(&self, nav: &Nav) {
        self.connection.dismiss_credential_offer();
        self.resolving_offer.set(false);
        self.offer_landing(nav).land();
    }

    // -----------------------------------------------------------------------
    // Saved-server persistence (the module docs' flow 5)
    // -----------------------------------------------------------------------

    /// The metadata write a successful connect performs; built before `target`
    /// moves into the connection controller, run from the completion.
    fn login_upsert(&self, target: &ConnectTarget) -> LoginUpsert {
        LoginUpsert::new(&self.repository, &self.connection, Some(self.saved), target)
    }

    /// [`Self::reload_saved`] by another name, callable from the completion
    /// closures that hold no `&self`.
    fn reload_into(repository: &SavedServers, saved: RwSignal<Vec<SavedServer>>) {
        match repository.list() {
            Ok(servers) => saved.set(servers),
            Err(failure) => app_toasts().raise(&failure),
        }
    }

    /// Rewrites the saved row for `endpoint` via `edit`, returning its previous
    /// token name. No-op if no row exists (every credential flow runs after a
    /// login that already wrote one).
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

    // -----------------------------------------------------------------------
    // Flow 1 — the manual save (the offer's accept)
    // -----------------------------------------------------------------------

    /// Mints a token, stores it behind the gate, and records it on the saved
    /// row. Returns immediately; `on_done` fires once settled, on every path
    /// out — including the two early refusals (read-only session, save already
    /// running).
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

    /// [`Self::begin_credential_save`]'s body: mint, gate + store, record,
    /// rotate. Ordering contract: a denied gate revokes the just-minted token
    /// (else it sits unrevoked forever); the previous token is revoked only
    /// after the new one persists, so a mid-flow failure leaves a working old
    /// token, not none.
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
                // Minting isn't run through `core`, so its failure is emitted here explicitly.
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

    // -----------------------------------------------------------------------
    // Flow 2 — the QR save
    // -----------------------------------------------------------------------

    /// Stores the scanned token for a just-paired server: one prompt, no mint,
    /// silent regardless of the gate's answer (session already live). Only
    /// flips the credential fields — the row itself was already written by the
    /// login upsert.
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
            // No token name: `muxrctl` minted it, this device never learns the name.
            controller.update_saved(&endpoint, |row| {
                row.has_credential = true;
                row.token_name = None;
            });
            // A pairing target is never offerable; dismiss any pending offer regardless.
            controller.connection.dismiss_credential_offer();
        });
    }

    // -----------------------------------------------------------------------
    // Flow 3 — logging in with a saved credential
    // -----------------------------------------------------------------------

    /// Unlocks `server`'s stored credential and connects under its own posture
    /// and pin, offering no save. Any other outcome reports its message and
    /// prefills the form (a cancel is silent); an invalidated key also forgets
    /// the credential. Ignored while busy.
    ///
    /// Callers own the no-verification confirmation — by the time this runs it
    /// was either unnecessary or already given — and, for the same reason, the
    /// free-tier verdict: `widgets::pro_gate` takes it through
    /// [`Self::saved_row_blocked`] before the confirmation it sits ahead of.
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

    /// [`Self::recover_and_connect`]'s body.
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
            // A passed gate recovering nothing is always the not-found branch.
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

    /// Connect with a token just recovered from `server`'s stored credential.
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

    /// [`KeyringUnlock::forget_unreadable`], then the list re-read.
    async fn forget_unreadable(&self, endpoint: &str) {
        self.keyring_unlock.forget_unreadable(endpoint).await;
        self.reload_saved();
    }

    // -----------------------------------------------------------------------
    // The unlock dock — one prompt for the whole keyring, then the MRU row
    // -----------------------------------------------------------------------

    pub fn biometrics_available(&self) -> bool {
        self.biometrics_available
    }

    /// What the dock shows — tracked.
    pub fn dock_state(&self) -> DockState {
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

    /// The dock's press: the free-tier verdict for the most recent row first
    /// (a block raises `gate_prompt` and no biometric prompt), then one
    /// `unlock_all` prompt, the keyring published, and that row dialed with
    /// its token. Single-flight on [`Self::unlocking`].
    pub fn unlock_keyring_and_connect(
        self: &Arc<Self>,
        nav: &Nav,
        gate_prompt: GatePrompt,
        confirm: ConfirmPrompt,
    ) {
        if self.busy_untracked() {
            return;
        }
        let saved = self.saved.get_untracked();
        let Some(target) = saved.first().cloned() else {
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
            controller
                .unlock_all_then_connect(saved, target, nav, confirm)
                .await;
        });
    }

    /// [`Self::unlock_keyring_and_connect`]'s body. A cancel is silent; a
    /// vault re-enrollment can never be read again, so it is reset; `Failed`
    /// offers that reset behind a confirm instead (it also covers a failed
    /// authentication); every other refusal fails the dock and toasts.
    async fn unlock_all_then_connect(
        self: &Arc<Self>,
        saved: Vec<SavedServer>,
        target: SavedServer,
        nav: Nav,
        confirm: ConfirmPrompt,
    ) {
        let unreadable = match self.keyring_unlock.unlock(&saved).await {
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
        };
        if !unreadable.is_empty() {
            self.reload_saved();
        }
        let endpoint = target.endpoint();
        let token = self.keyring.token_for(&endpoint);
        self.unlocking.set(false);
        let Some(token) = token else {
            self.fail_dock(if unreadable.contains(&endpoint) {
                CredentialStatus::KeyInvalidated
            } else {
                CredentialStatus::NotFound
            });
            self.prefill(&target);
            return;
        };
        if target.trust_mode != TrustMode::NoVerification {
            self.connect_recovered(&target, token, &nav);
            return;
        }
        let controller = Arc::clone(self);
        let server = target.clone();
        confirm(
            target,
            Box::new(move || controller.connect_recovered(&server, token, &nav)),
        );
    }

    /// Fails the dock with `status`'s message and toasts it, as every flow does.
    fn fail_dock(&self, status: CredentialStatus) {
        self.dock_failure
            .set(status.message().map(|message| DockFailure {
                message: message.to_string(),
                offers_reset: status == CredentialStatus::Failed,
            }));
        self.report(status);
    }

    /// The failed dock's confirmed reset: the vault goes, ungated, and every
    /// saved row stops claiming a credential. Ignored while busy.
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

    /// Removes the vault (ungated), clears every row's credential fields — the
    /// next unlock re-migrates any legacy leftovers — and locks the handle.
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

    // -----------------------------------------------------------------------
    // Flow 4 — forgetting a saved server (this device only)
    // -----------------------------------------------------------------------

    /// Drops everything this device holds for `server`: credential, then row.
    /// Strictly local (no RPC) — the token keeps working until revoked
    /// server-side, and removing the currently-connected row is harmless.
    /// Returns immediately.
    pub fn forget_saved_server(self: &Arc<Self>, server: SavedServer) {
        let controller = Arc::clone(self);
        spawn_local(async move {
            controller.forget_saved(server).await;
        });
    }

    /// Forgets `rows` in turn, then runs `then` on the UI thread — the paywall's
    /// forget-and-connect, whose retry must not start until the last row is
    /// actually gone (the verdict it re-takes reads the same list). Each row
    /// goes through [`Self::forget_saved`], so the semantics are
    /// [`Self::forget_saved_server`]'s exactly, failures included: a row whose
    /// secret refuses to delete toasts and keeps its row, and the retry still
    /// runs — where the gate simply refuses again.
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

    /// [`Self::forget_saved_server`]'s body. Ordering contract: secret first,
    /// row second, and only if the secret's delete succeeded — the row is the
    /// secret's own key, so removing it first could strand the secret
    /// unreachable.
    async fn forget_saved(&self, server: SavedServer) {
        let endpoint = server.endpoint();
        if let Err(failure) = self.credentials.delete(endpoint.clone()).await {
            app_toasts().raise(&failure);
            return;
        }
        if let Err(failure) = self.repository.remove(&endpoint) {
            app_toasts().raise(&failure);
            // The secret is gone regardless; the surviving row must stop claiming one.
            self.update_saved(&endpoint, |row| {
                row.has_credential = false;
                row.token_name = None;
            });
            return;
        }
        self.reload_saved();
    }

    // -----------------------------------------------------------------------
    // Shared credential plumbing
    // -----------------------------------------------------------------------

    /// Gates and stores a secret. Unlocked, a snapshot of the keyring takes
    /// the one gated vault write; on success the handle gains the token iff it
    /// is still unlocked once the prompt settles. A lock during the prompt is
    /// the one state the token must stay out of (the vault keeps it); any
    /// other change meanwhile — another endpoint inserted or removed, a
    /// re-unlock — still takes it, so the handle never lacks a token the vault
    /// holds (the next snapshot-based vault write would erase it). Locked, the
    /// read-modify-write save runs through `ControllerCore::run` (whose
    /// impossible failure reads as `Failed`) and the handle is left alone.
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

    /// Say what `status` means, if it means anything to say (a success and a cancel
    /// do not — see [`CredentialStatus::message`]).
    fn report(&self, status: CredentialStatus) {
        if let Some(message) = status.message() {
            app_toasts().show(message);
        }
    }

    /// Revokes a server-side token, best effort — every caller is past the
    /// point of no return.
    async fn revoke_quietly(&self, name: String) {
        if let Err(failure) = self.tokens.revoke_token(name).await {
            log::warn!("credentials: revoking a replaced token failed: {failure:?}");
        }
    }
}

/// What a [`KeyringUnlock::unlock`] settled as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyringUnlockOutcome {
    /// The keyring is published on the handle. `unreadable` names the rows
    /// whose legacy credential could not be read and was dropped.
    Unlocked { unreadable: Vec<String> },
    /// Nothing was published. Never [`CredentialStatus::Success`].
    Refused(CredentialStatus),
    /// Another unlock is already in flight app-wide: no prompt was raised.
    /// Callers treat it silently.
    Busy,
    /// A lock landed while the prompt was pending, so the unlocked keyring
    /// was dropped unpublished. Callers treat it as a silent cancel: no dial,
    /// no toast.
    Superseded,
}

/// The page-agnostic keyring seam the Connect dock and Settings share: one
/// `unlock_all` prompt over the saved list, its keyring published on the
/// app-scoped [`KeyringHandle`], and a switch to a saved row with a token
/// taken from that keyring. Owns no signals and raises no dialog, so any
/// screen's controller can hold one and keep its own failure surface.
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

    /// One gated read of the whole keyring for `saved`'s endpoints. On
    /// success, rows whose legacy entry was unreadable lose their credential
    /// claim first, then the keyring is published.
    pub async fn unlock(&self, saved: &[SavedServer]) -> KeyringUnlockOutcome {
        let Some(_ticket) = self.keyring.begin_unlock() else {
            return KeyringUnlockOutcome::Busy;
        };
        let epoch = self.keyring.lock_epoch();
        let endpoints: Vec<String> = saved.iter().map(SavedServer::endpoint).collect();
        let UnlockedKeyring {
            status,
            keyring,
            unreadable,
            ..
        } = self
            .unlock_credential
            .run_all(&unlock_reason(saved), &endpoints)
            .await;
        let keyring = match (status, keyring) {
            (CredentialStatus::Success, Some(keyring)) => keyring,
            (CredentialStatus::Success, None) => {
                return KeyringUnlockOutcome::Refused(CredentialStatus::Failed);
            }
            (status, _) => return KeyringUnlockOutcome::Refused(status),
        };
        if self.keyring.lock_epoch() != epoch {
            // Lock & disconnect won: the keyring zeroizes on drop.
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

    /// Drops a credential the device can no longer read (key invalidated by
    /// re-enrollment) and clears its row's credential fields. Best-effort,
    /// ungated — the user must be able to get rid of it either way.
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

    /// Move the live connection to `server` with `token` (one of the
    /// keyring's), writing the row's login back once it opens — the switch
    /// every saved-row entry point outside `/` takes.
    pub fn switch_to_saved(
        &self,
        server: &SavedServer,
        token: Zeroizing<String>,
        on_done: impl FnOnce(Result<(), AppFailure>) + 'static,
    ) {
        let target = saved_target(server, token);
        let upsert = LoginUpsert::new(&self.repository, &self.connection, None, &target);
        self.connection.switch_to(target, move |outcome| {
            if outcome.is_ok() {
                upsert.run();
            }
            on_done(outcome);
        });
    }
}

/// The target a stored credential dials: the row's own posture and pin,
/// never offerable (it is stored already), and remembered by definition.
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

/// The [`CredentialSaver`] a [`ConnectController`] hands to `crate::connection`.
struct ControllerCredentialSaver {
    controller: std::sync::Weak<ConnectController>,
}

impl CredentialSaver for ControllerCredentialSaver {
    fn save_credential(&self, endpoint: String, label: String, on_done: Box<dyn FnOnce()>) {
        match self.controller.upgrade() {
            Some(controller) => controller.begin_credential_save(endpoint, label, on_done),
            // A gone-away screen saves nothing, but `on_done` still runs — dropping
            // it would strand the caller.
            None => on_done(),
        }
    }
}

/// The saved-server row a successful connect writes back — run from a completion
/// closure that holds no `&self`, hence the cloned handles.
struct LoginUpsert {
    repository: SavedServers,
    connection: Arc<ConnectionController>,
    /// The list signal to re-read into, when the caller renders one.
    saved: Option<RwSignal<Vec<SavedServer>>>,
    host: String,
    port: u16,
    trust_mode: TrustMode,
    label: String,
    fingerprint: Option<String>,
}

impl LoginUpsert {
    fn new(
        repository: &SavedServers,
        connection: &Arc<ConnectionController>,
        saved: Option<RwSignal<Vec<SavedServer>>>,
        target: &ConnectTarget,
    ) -> Self {
        Self {
            repository: Arc::clone(repository),
            connection: Arc::clone(connection),
            saved,
            host: target.host.clone(),
            port: target.port,
            trust_mode: target.trust_mode,
            label: target.label.clone(),
            fingerprint: target.fingerprint.clone(),
        }
    }

    /// The `host:port` this write is for.
    fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Writes the row from a settled connect. Credential fields are carried
    /// forward from any existing row — rebuilding them would wipe
    /// `has_credential` on every login, including the recover path's own.
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
        if let Err(failure) = self.repository.save(server) {
            app_toasts().raise(&failure);
            return;
        }
        if let Some(saved) = self.saved {
            ConnectController::reload_into(&self.repository, saved);
        }
    }
}

/// The `host:port` a target dials — the same key a saved row is written under
/// ([`SavedServer::endpoint`]) and the free-tier rule is judged in.
fn target_endpoint(target: &ConnectTarget) -> String {
    format!("{}:{}", target.host, target.port)
}

/// The keyring prompt's rationale: the one row by label, else the count.
fn unlock_reason(saved: &[SavedServer]) -> String {
    match saved {
        [only] => format!("Unlock {}", only.label),
        rows => format!("Unlock {} servers", rows.len()),
    }
}

/// `muxr-mobile-<label>-<base36 timestamp>` — the stamp keeps names unique
/// since the daemon addresses tokens by name.
fn minted_token_name(label: &str) -> String {
    format!(
        "{TOKEN_NAME_PREFIX}-{}-{}",
        slug(label),
        base36(epoch_millis())
    )
}

/// `label` as a token-name-safe fragment: lowercase, `[a-z0-9-]` only, no
/// leading/trailing or repeated dashes, and capped at [`TOKEN_LABEL_MAX`].
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
        // Needs a fallback before the stamp, or the name reads as a stray double dash.
        return "device".to_string();
    }
    trimmed.to_string()
}

/// `value` in base 36 — the compact stamp a minted token's name ends with. A
/// non-positive clock reads as `0`.
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

/// The port a bare (no `:port`) host dials under `mode`: a publicly trusted
/// certificate implies a TLS-terminating edge on the HTTPS port
/// ([`PUBLIC_CA_DEFAULT_PORT`]); a self-verified or pinned connection dials
/// `muxrd` directly on its own default ([`MUXRD_DEFAULT_PORT`]). A scanned
/// pairing always prefills `host:port` explicitly, so this default never
/// applies to it.
fn default_port_for(mode: TrustMode) -> u16 {
    match mode {
        TrustMode::System => PUBLIC_CA_DEFAULT_PORT,
        TrustMode::NoVerification | TrustMode::Pinned => MUXRD_DEFAULT_PORT,
    }
}

// ---------------------------------------------------------------------------
// Pairing → form / target mapping
// ---------------------------------------------------------------------------

/// Endpoint text a scanned payload prefills: `host:port`, IPv6 bracketed.
/// `parse_endpoint` is mock-grade and can't take that apart again — why
/// [`pairing_covers`] strips brackets instead of round-tripping.
fn pairing_endpoint(payload: &PairingPayload) -> String {
    if payload.host.contains(':') {
        return format!("[{}]:{}", payload.host, payload.port);
    }
    format!("{}:{}", payload.host, payload.port)
}

/// The connect-form posture a pairing's trust hint maps onto — the one mapping
/// between two deliberately separate types, with a pin's fingerprint threaded
/// separately ([`pinned_fingerprint`]).
fn pairing_trust_mode(trust: &PairingTrust) -> TrustMode {
    match trust {
        PairingTrust::Pin { .. } => TrustMode::Pinned,
        PairingTrust::Ca => TrustMode::System,
    }
}

/// The pinned certificate's fingerprint, or `None` for a CA pairing.
fn pinned_fingerprint(payload: &PairingPayload) -> Option<String> {
    match &payload.trust {
        PairingTrust::Pin { fp_hex } => Some(fp_hex.clone()),
        PairingTrust::Ca => None,
    }
}

/// Whether `endpoint` still names the host/port `payload` was scanned for. Host
/// comparison accepts the bracketed IPv6 form [`pairing_endpoint`] writes, since
/// that's what sits in the field after a prefill.
fn pairing_covers(payload: &PairingPayload, endpoint: &Endpoint) -> bool {
    endpoint.port == payload.port && unbracketed(&endpoint.host) == payload.host
}

/// `host` with one layer of IPv6 brackets removed, if it has them.
fn unbracketed(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

/// The [`ConnectTarget`] a stashed pairing dials under: every field from the
/// payload, endpoint as fallback label. Always `remember: true`,
/// `offer_credential: false` (flow 2 owns that save).
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

    /// A `tm=pin` payload for `host:port`, with `fp` as its fingerprint.
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
        // Every prefill below always carries an explicit port, so the default
        // passed here never applies — any [`TrustMode`] would do.
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
        // Explicit port again — the default passed here is never reached.
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
