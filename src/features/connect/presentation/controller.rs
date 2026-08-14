//! [`ConnectController`] — the Connect screen's view model.
//!
//! Hosted through `use_controller`, so it is built once on route mount and
//! disposed on unmount; it owns every **observed** piece of the screen as
//! `RwSignal`s, while non-reactive UI state lives in the component's own `State`.
//!
//! **Injected, never resolved here.** Every dependency arrives through the
//! constructor, the one cross-feature reach included (`settings`' auth-token
//! store, since a saved credential *is* a server-side token). The screen resolves
//! them with `use_context` where a reactive owner is live — **not** from a press
//! handler, which runs outside the shell's `with_owner` scope and would silently
//! see `None`.
//!
//! # The five credential flows
//!
//! 1. **Manual save** — [`ConnectController::begin_credential_save`]: mint, gate
//!    and store *that* secret, then rotate. The previous token is revoked only
//!    once the new credential is persisted, and a gate denial revokes the token it
//!    just minted rather than orphaning it.
//! 2. **QR save** — [`ConnectController::connect_from_pairing`]'s success branch:
//!    connect first, then **one** prompt to store the scanned token (nothing is
//!    minted). A denial leaves the session live and silently unsaved.
//! 3. **Login with a saved credential** —
//!    [`ConnectController::recover_and_connect`]: unlock, then connect under the
//!    row's own stored posture. A [`TrustMode::NoVerification`] row is gated a
//!    step earlier still, by the screen
//!    ([`ConnectController::saved_row_needs_confirmation`]).
//! 4. **Delete** — [`ConnectController::forget_saved_server`], ungated and
//!    **strictly local**: no RPC is made at all.
//! 5. **Upsert on login** — every successful connect writes the metadata row back
//!    (`LoginUpsert`), carrying the credential fields forward.
//!
//! No gated call happens on the UI thread: both go through `ControllerCore::run`
//! on async use cases, whose implementation puts the blocking call on
//! `frust::spawn_blocking`. Every failure raised here is an [`AppFailure`] on
//! [`crate::ui::toasts::app_toasts`]; a *refused dial* is the deliberate exception,
//! raised on [`ConnectionController`]'s own sink, so re-raising would show two
//! toasts for one failure.
//!
//! **The pairing path.** A scanned code never connects by itself:
//! [`ConnectController::apply_scanned_payload`] prefills and stashes, and the
//! pinned CTA calls [`ConnectController::connect_from_pairing`]. The stash is the
//! *pin's* provenance, so it is dropped the moment the form stops describing what
//! was scanned — an edit, a disconnect, or a successful pairing connect; a *failed*
//! one keeps it, so a retry stays one tap.
//!
//! **The held redirect.** A connect that raised a credential offer does not push
//! [`SESSIONS_LOCATION`] when it settles: it hands the label to the screen's
//! `offer_prompt`, and the push belongs to whichever of
//! [`ConnectController::accept_offer_then_navigate`] /
//! [`ConnectController::decline_offer_then_navigate`] answers. The accept pushes
//! from the *save's* completion, so the prompt is answered on the form that raised
//! it; the failure paths push too, since a declined credential is no reason to
//! withhold the session. [`ConnectController::resolving_offer`] spans the hold and
//! joins [`ConnectController::busy`].

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clean_signals::{ControllerCore, RunOptions};
use frust::{Get, GetUntracked, RwSignal, Set, With, spawn_local};
use zeroize::Zeroizing;

use crate::connection::{ConnectTarget, ConnectionController, CredentialSaver, TrustMode};
use crate::failure::AppFailure;
use crate::features::connect::di::{Credentials, SavedServers};
use crate::features::connect::domain::entities::{
    CredentialStatus, PairingPayload, PairingTrust, SavedServer, StoreRequest, UnlockRequest,
    UnlockedCredential, epoch_millis,
};
use crate::features::connect::domain::use_cases::parse_endpoint::{Endpoint, parse_endpoint};
use crate::features::connect::domain::use_cases::{SaveCredential, UnlockCredential};
use crate::features::settings::di::SharedAuthTokensRepository;
use crate::features::settings::domain::entities::CreateTokenRequest;
use crate::routes::Nav;
use crate::ui::toasts::app_toasts;

/// Where a successful connect lands. The Sessions route's own spelling lives in
/// `crate::routes`; this is the one place the Connect flow names it.
pub const SESSIONS_LOCATION: &str = "/sessions";

/// What a settled connect's credential offer tells the screen's dialog.
pub struct CredentialOffer {
    /// The offered server's label — the words the dialog names it by.
    pub label: String,
    /// Whether a stored credential already exists, so the dialog asks to
    /// **replace** one rather than to keep a first. Read before the dial; the login
    /// upsert in between carries `has_credential` forward rather than rewriting it,
    /// so the answer is the same either side of the connect.
    pub replacing: bool,
}

/// What [`ConnectController::connect`] calls when a settled connect raised an
/// offer instead of navigating. The screen supplies it, since raising a dialog
/// needs the overlay host this controller deliberately does not hold; an [`Rc`] for
/// the same reason [`Nav`] is one.
pub type OfferPrompt = Rc<dyn Fn(CredentialOffer)>;

/// The trust posture a fresh form starts on — the safe one.
const DEFAULT_TRUST_MODE: TrustMode = TrustMode::System;

/// Whether a fresh form offers to remember the server. `true`, matching the mock.
const DEFAULT_REMEMBER: bool = true;

/// The prefix every token this device mints for itself carries, so a server's
/// token list says which device asked for each one.
const TOKEN_NAME_PREFIX: &str = "muxr-mobile";

/// How much of a server's label rides in a minted token's name. Long enough to
/// tell two rigs apart in the settings list, short enough to stay one line.
const TOKEN_LABEL_MAX: usize = 24;

/// The Connect screen's view model. See the [module docs](self).
pub struct ConnectController {
    core: ControllerCore<AppFailure>,
    /// The saved-server store [`Self::reload_saved`] reads.
    repository: SavedServers,
    /// The credential store, for the one ungated operation (dropping a credential
    /// whose key the device invalidated); every gated one runs through
    /// `ControllerCore::run`.
    credentials: Credentials,
    /// `settings`' auth-token store — where a credential's token is minted and
    /// revoked.
    tokens: SharedAuthTokensRepository,
    /// Gate + store, behind `ControllerCore::run`.
    save_credential: SaveCredential,
    /// Gate + read, likewise.
    unlock_credential: UnlockCredential,
    /// The app-scoped connection handle a successful connect drives.
    connection: Arc<ConnectionController>,
    /// Whether a credential save is already running — the single-flight guard a
    /// double-tapped "Save" would otherwise mint a second server token through.
    saving_credential: AtomicBool,
    /// The server-URL field's text (`host:port`).
    pub url: RwSignal<String>,
    /// The login-token field's text — the form's one secret, [`Zeroizing`]-wrapped
    /// so a replace or drop scrubs the buffer. Two edges scrub it: disconnect
    /// ([`Self::clear_token`]) and connect ([`Self::connect`]'s completion); a
    /// connect that *fails* keeps it, since the user needs it to retry. The
    /// platform IME's own suggestion surface is out of reach — see
    /// `docs/ARCHITECTURE.md`'s Planned section.
    pub token: RwSignal<Zeroizing<String>>,
    /// The selected TLS posture.
    pub trust_mode: RwSignal<TrustMode>,
    /// Whether a successful connect should offer to remember this server.
    pub remember: RwSignal<bool>,
    /// The saved-server list, as last read from the repository.
    pub saved: RwSignal<Vec<SavedServer>>,
    /// Whether the last connect attempt failed validation — the screen inks the URL
    /// field's label with it. Cleared by any edit to [`Self::url`] and by a
    /// successful connect.
    pub invalid: RwSignal<bool>,
    /// The pairing code scanned in this screen's lifetime, or `None`. Holding the
    /// whole payload is what lets [`Self::connect_from_pairing`] dial the *scanned*
    /// credential rather than whatever the token field reads.
    pub pending_pairing: RwSignal<Option<PairingPayload>>,
    /// Whether a biometric unlock is in flight — the other half of [`Self::busy`].
    /// Separate from the connection's own `connecting` because no socket is open
    /// yet, and the CTA must still read as working.
    pub unlocking: RwSignal<bool>,
    /// Whether a credential offer is still being answered — the held redirect.
    pub resolving_offer: RwSignal<bool>,
}

impl ConnectController {
    /// A fresh controller with an empty form and the saved list already loaded.
    pub fn new(
        repository: SavedServers,
        credentials: Credentials,
        tokens: SharedAuthTokensRepository,
        connection: Arc<ConnectionController>,
    ) -> Self {
        let controller = Self {
            core: ControllerCore::new(),
            repository,
            save_credential: SaveCredential::new(Arc::clone(&credentials)),
            unlock_credential: UnlockCredential::new(Arc::clone(&credentials)),
            credentials,
            tokens,
            connection,
            saving_credential: AtomicBool::new(false),
            url: RwSignal::new(String::new()),
            token: RwSignal::new(Zeroizing::new(String::new())),
            trust_mode: RwSignal::new(DEFAULT_TRUST_MODE),
            remember: RwSignal::new(DEFAULT_REMEMBER),
            saved: RwSignal::new(Vec::new()),
            invalid: RwSignal::new(false),
            pending_pairing: RwSignal::new(None),
            unlocking: RwSignal::new(false),
            resolving_offer: RwSignal::new(false),
        };
        controller.reload_saved();
        controller
    }

    /// The [`CredentialSaver`] `crate::connection` calls on an accepted offer —
    /// this controller, reached **weakly**: the connection handle outlives every
    /// screen, and a strong reference would keep a disposed controller alive for the
    /// process's life. A screen that has gone away makes the accept a no-op.
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

    /// Set the server-URL field, clearing any stale validation complaint.
    ///
    /// The **manual-edit** seam, so also where a live pairing is checked: an
    /// address that no longer names the scanned host and port drops the payload (an
    /// unparseable one counts as a mismatch). That is why
    /// [`Self::apply_scanned_payload`] writes [`Self::url`] directly.
    pub fn set_url(&self, url: String) {
        self.invalidate_pairing_unless_covering(&url);
        self.url.set(url);
        if self.invalid.get_untracked() {
            self.invalid.set(false);
        }
    }

    /// Set the login-token field, in a fresh [`Zeroizing`]. A hand-typed token
    /// drops a live pairing: the pinned CTA sends the *payload's* token, so a form
    /// whose field says one thing while that CTA would send another is a form that
    /// lies about what it is about to do.
    pub fn set_token(&self, token: String) {
        self.token.set(Zeroizing::new(token));
        self.clear_pending_pairing();
    }

    /// Scrub the login-token field back to empty — the screen's disconnect-edge
    /// watch. See [`Self::token`] for the other edge.
    pub fn clear_token(&self) {
        self.token.set(Zeroizing::new(String::new()));
    }

    /// Select `mode` as the connection's TLS posture.
    pub fn select_trust_mode(&self, mode: TrustMode) {
        self.trust_mode.set(mode);
    }

    /// Flip the remember-this-server toggle.
    pub fn toggle_remember(&self) {
        self.remember.set(!self.remember.get_untracked());
    }

    /// The saved row at `index`, as the list was last read — `None` for an
    /// out-of-range index (the list changed between paint and press).
    pub fn saved_row(&self, index: usize) -> Option<SavedServer> {
        self.saved.get_untracked().get(index).cloned()
    }

    /// Refill the form from the saved row at `index`. Out-of-range indices are
    /// ignored.
    pub fn select_saved(&self, index: usize) {
        let Some(server) = self.saved_row(index) else {
            return;
        };
        self.prefill(&server);
    }

    /// Refill the form from `server`. The fallback every refused unlock ends with:
    /// whatever the gate said, the user is left one token from connecting by hand.
    pub fn prefill(&self, server: &SavedServer) {
        self.set_url(server.endpoint());
        self.trust_mode.set(server.trust_mode);
    }

    /// The saved row at `index` was tapped — reconnect through its stored credential
    /// where there is one, else refill the form. A row with no credential has
    /// nothing to unlock, so tapping it must not raise a prompt.
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

    /// The saved row at `index`, if tapping it must raise a confirmation **before**
    /// the biometric unlock: a stored credential under
    /// [`TrustMode::NoVerification`], whose unlock recovers a real secret that
    /// [`Self::recover_and_connect`] then replays over a channel authenticating
    /// nothing. `None` for any other row. A predicate only — the dialog belongs to
    /// the screen, which holds the overlay host.
    pub fn saved_row_needs_confirmation(&self, index: usize) -> Option<SavedServer> {
        let server = self.saved_row(index)?;
        (server.has_credential && server.trust_mode == TrustMode::NoVerification).then_some(server)
    }

    /// Whether a dial is in flight — the connection controller's own signal, not a
    /// second copy: two flags for one fact drift. Also the single-flight guard both
    /// CTAs inherit.
    pub fn connecting(&self) -> RwSignal<bool> {
        self.connection.connecting
    }

    /// Whether *anything* the CTAs share is in flight: a dial, an unlock, or an
    /// unanswered offer. A **tracked** read — what a piece renders busy from.
    pub fn busy(&self) -> bool {
        self.connection.connecting.get() || self.unlocking.get() || self.resolving_offer.get()
    }

    /// [`Self::busy`] without subscribing — the guard a press handler reads, where
    /// there is no owner to subscribe. `pub(crate)` for the FAB's own use.
    pub(crate) fn busy_untracked(&self) -> bool {
        self.connection.connecting.get_untracked()
            || self.unlocking.get_untracked()
            || self.resolving_offer.get_untracked()
    }

    // -----------------------------------------------------------------------
    // The pairing path (see the module docs)
    // -----------------------------------------------------------------------

    /// Prefill the form from a scanned `muxr://pair` payload and stash it; never
    /// connects. [`Self::url`]/[`Self::token`] are written **directly**, since
    /// their setters are the manual-edit seams and would clear the very payload
    /// this is stashing.
    pub fn apply_scanned_payload(&self, payload: PairingPayload) {
        self.url.set(pairing_endpoint(&payload));
        self.token.set(payload.token.clone());
        self.trust_mode.set(pairing_trust_mode(&payload.trust));
        self.invalid.set(false);
        self.pending_pairing.set(Some(payload));
    }

    /// Drop the stashed pairing and hand the posture back to the manual default: the
    /// pin came with the payload and cannot outlive it (a left-behind
    /// [`TrustMode::Pinned`] would fail closed at dial time). A no-op when empty.
    pub fn clear_pending_pairing(&self) {
        if self.pending_pairing.with(Option::is_none) {
            return;
        }
        self.pending_pairing.set(None);
        self.trust_mode.set(DEFAULT_TRUST_MODE);
    }

    /// Drop the stashed pairing unless `url` still names its host and port.
    fn invalidate_pairing_unless_covering(&self, url: &str) {
        let covered = self.pending_pairing.with(|pending| match pending {
            None => true,
            Some(payload) => {
                parse_endpoint(url).is_ok_and(|endpoint| pairing_covers(payload, &endpoint))
            }
        });
        if !covered {
            self.clear_pending_pairing();
        }
    }

    /// Connect with the stashed pairing's own credentials — the pinned CTA. A no-op
    /// with nothing stashed, and ignored while [`Self::busy`] holds.
    ///
    /// On success the token field is scrubbed, the payload consumed, and **the
    /// paired credential's one prompt** raised — the session is already live, so a
    /// denial changes nothing about it. On failure the form is left exactly as
    /// filled, payload included, so a retry is one tap.
    pub fn connect_from_pairing(self: &Arc<Self>, nav: &Nav) {
        if self.busy_untracked() {
            return;
        }
        let Some(payload) = self.pending_pairing.get_untracked() else {
            return;
        };
        self.invalid.set(false);
        let nav = nav.clone();
        // Both signals are `Copy`, and the closure outlives this borrow of `self`
        // whenever the dial is a real one (see `Self::connect`).
        let token = self.token;
        let pending = self.pending_pairing;
        let target = pairing_target(&payload);
        let upsert = self.login_upsert(&target);
        let controller = Arc::clone(self);
        // The scanned token is what gets stored — never a freshly minted one:
        // `muxrctl` already minted this credential, and the QR *is* it.
        let scanned = payload.token.clone();
        let label = target.label.clone();
        let endpoint = upsert.endpoint();
        self.connection.connect_with(target, move |outcome| {
            if outcome.is_ok() {
                upsert.run();
                token.set(Zeroizing::new(String::new()));
                pending.set(None);
                controller.save_paired_credential(endpoint, label, scanned);
                nav.push(SESSIONS_LOCATION);
            }
        });
    }

    /// The [`ConnectTarget`] this form describes, or why it doesn't — split out so
    /// validation + target mapping is unit-testable without a live navigator.
    ///
    /// `offer_credential` is set whenever the user asked to be remembered and this
    /// connect is not a pairing — **including for an endpoint already saved**,
    /// which is precisely where a stored credential needs replacing (its token can
    /// have been revoked server-side). The unlock path builds its own target with
    /// the flag off and never comes through here.
    ///
    /// A hand-picked [`TrustMode::Pinned`] carries no fingerprint and fails closed
    /// before dialing rather than dropping to a weaker posture. The one exception
    /// is a **matching stashed pairing**: its posture and fingerprint ride through
    /// here, so the ordinary CTA connects rather than failing closed on the pin its
    /// own prefill installed. Such a connect is never offerable either.
    pub fn validated_target(&self) -> Result<ConnectTarget, AppFailure> {
        let endpoint = parse_endpoint(&self.url.get_untracked())?;
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
            // A known endpoint keeps its stored label; a new one is labelled by
            // its own host, which is what the credential offer will show.
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

    /// Validate the form and, if it holds up, open the connection and — once it is
    /// actually up — push [`SESSIONS_LOCATION`] through the app's one guarded
    /// router, so it resolves exactly as a deep link would.
    ///
    /// `nav` is lent rather than held: it wraps an `Rc`-backed `Router` and so is
    /// neither `Send` nor `Sync`, while this controller must be both. The
    /// completion closure carries that `Rc` across the dial and runs on the UI
    /// thread only. `offer_prompt` is the screen's for the same reason.
    ///
    /// A rejection raises a toast and lights [`Self::invalid`]; nothing navigates.
    /// A refused dial raises its own toast, leaves the form as typed (token
    /// included, so a retry is one tap), and never navigates. **The push is
    /// conditional** — see the module docs' held redirect.
    pub fn connect(&self, nav: &Nav, offer_prompt: OfferPrompt) {
        if self.busy_untracked() {
            return;
        }
        match self.validated_target() {
            Ok(target) => {
                self.invalid.set(false);
                let nav = nav.clone();
                // Login-token hygiene: the token's job ends the instant a
                // connection opens with it. The signal is captured by value
                // (`RwSignal` is `Copy`) because the closure outlives this borrow
                // of `self` whenever the dial is a real one.
                let token = self.token;
                // Likewise `Copy`: the settled connect reads the offer back off the
                // connection's own signal (where the read-only half of the
                // eligibility decision has already been applied).
                let offer = self.connection.credential_offer;
                let resolving = self.resolving_offer;
                // Whether the offer this connect may raise is a *replacement* —
                // taken before the dial, from the same saved list
                // `validated_target` just read.
                let replacing = self.stores_credential_for(&target.host, target.port);
                let upsert = self.login_upsert(&target);
                self.connection.connect_with(target, move |outcome| {
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
                        None => nav.push(SESSIONS_LOCATION),
                    }
                });
            }
            Err(failure) => {
                self.invalid.set(true);
                app_toasts().raise(&failure);
            }
        }
    }

    /// The user accepted the offer the redirect is held on: run the save, then push
    /// [`SESSIONS_LOCATION`] once it has settled — **however** it settled.
    /// Navigating from the save's own completion is the ordering this flow exists
    /// for: the prompt is answered on Connect, not over a Sessions screen.
    pub fn accept_offer_then_navigate(&self, nav: &Nav) {
        let nav = nav.clone();
        let resolving = self.resolving_offer;
        self.connection.accept_credential_offer_with(move || {
            resolving.set(false);
            nav.push(SESSIONS_LOCATION);
        });
    }

    /// The user declined, or dismissed the dialog without answering (all the same
    /// answer): drop the offer and push [`SESSIONS_LOCATION`], which is what the
    /// connect would have done on its own.
    pub fn decline_offer_then_navigate(&self, nav: &Nav) {
        self.connection.dismiss_credential_offer();
        self.resolving_offer.set(false);
        nav.push(SESSIONS_LOCATION);
    }

    // -----------------------------------------------------------------------
    // Saved-server persistence (the module docs' flow 5)
    // -----------------------------------------------------------------------

    /// The metadata write a successful connect to `target` performs — built before
    /// the target moves into the connection controller, run from the completion.
    fn login_upsert(&self, target: &ConnectTarget) -> LoginUpsert {
        LoginUpsert {
            repository: Arc::clone(&self.repository),
            connection: Arc::clone(&self.connection),
            saved: self.saved,
            host: target.host.clone(),
            port: target.port,
            trust_mode: target.trust_mode,
            label: target.label.clone(),
            fingerprint: target.fingerprint.clone(),
        }
    }

    /// [`Self::reload_saved`] by another name, callable from the completion
    /// closures that hold no `&self`.
    fn reload_into(repository: &SavedServers, saved: RwSignal<Vec<SavedServer>>) {
        match repository.list() {
            Ok(servers) => saved.set(servers),
            Err(failure) => app_toasts().raise(&failure),
        }
    }

    /// Rewrite the saved row for `endpoint` through `edit`, returning the token name
    /// it carried **before**. A no-op for an endpoint with no row: every credential
    /// flow runs after a login that already wrote one.
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

    /// Mint a token for this device, store it behind the gate, and record it on the
    /// saved row. Returns immediately; `on_done` runs once the chain has settled —
    /// the seam the held redirect resumes on, so it fires on **every** path out,
    /// the two early refusals included (a read-only session, and a save already
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

    /// [`Self::begin_credential_save`]'s body, in order: mint, gate + store, record,
    /// rotate. Two orderings are the contract:
    ///
    /// - a **denied gate revokes the token it just minted**, because nothing holds
    ///   it any more and an unrevoked one sits on the server forever; and
    /// - the **previous** token is revoked only *after* the new credential is
    ///   persisted, so a failure between the two leaves the user with a working old
    ///   token rather than none at all.
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
                // The mint is the caller's own step rather than a use case run
                // through `core`, so its failure is emitted here explicitly — the
                // screen's toast bridge listens on the same sink either way.
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

    /// Store the scanned token for a just-paired server: one prompt, no mint, and
    /// silent whatever the gate says — the session is already live. The saved row is
    /// already written by the login upsert; this only flips its credential fields.
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
            // No token name: `muxrctl` minted this token, and this device never
            // learns what the server calls it — so there is nothing to revoke on a
            // later delete.
            controller.update_saved(&endpoint, |row| {
                row.has_credential = true;
                row.token_name = None;
            });
            // A pairing target is never offerable, so there should be no offer
            // pending — but the credential this just stored would make one moot.
            controller.connection.dismiss_credential_offer();
        });
    }

    // -----------------------------------------------------------------------
    // Flow 3 — logging in with a saved credential
    // -----------------------------------------------------------------------

    /// Unlock `server`'s stored credential and connect with it, under the row's
    /// **own** stored posture and pin and with no save offer. Every other outcome
    /// reports [`CredentialStatus::message`] and prefills the manual form, except a
    /// cancel, which is silent by contract; an invalidated key additionally forgets
    /// the credential it can no longer read. Ignored while busy.
    ///
    /// **Callers** own the no-verification confirmation: by the time this runs it
    /// was either unnecessary or already given.
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
            // A passed gate that recovered nothing is the not-found branch,
            // whatever the store reported it as.
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
        let target = ConnectTarget {
            host: server.host.clone(),
            port: server.port,
            trust_mode: server.trust_mode,
            label: server.label.clone(),
            // Never offerable: this credential is stored already.
            offer_credential: false,
            token,
            // A stored credential is by definition a remembered login.
            remember: true,
            fingerprint: server.fingerprint.clone(),
        };
        let upsert = self.login_upsert(&target);
        let nav = nav.clone();
        self.connection.connect_with(target, move |outcome| {
            if outcome.is_ok() {
                upsert.run();
                nav.push(SESSIONS_LOCATION);
            }
        });
    }

    /// Drop a credential the device can no longer read (a re-enrollment invalidated
    /// its key) and clear the metadata claiming one exists. Best-effort and
    /// ungated: a user who cannot read it must still be able to be rid of it.
    async fn forget_unreadable(&self, endpoint: &str) {
        if let Err(failure) = self.credentials.delete(endpoint.to_string()).await {
            log::warn!("credentials: dropping an invalidated entry failed: {failure:?}");
        }
        self.update_saved(endpoint, |row| {
            row.has_credential = false;
            row.token_name = None;
        });
    }

    // -----------------------------------------------------------------------
    // Flow 4 — forgetting a saved server (this device only)
    // -----------------------------------------------------------------------

    /// Drop everything this device holds for `server`: its stored credential, then
    /// its saved row.
    ///
    /// **Strictly local** — no RPC at all, so the token keeps working until the
    /// user revokes it and removing the row of the currently-connected server is
    /// harmless. Returns immediately; the form is left as the user has it, but
    /// anything derived from the row's `has_credential` is rewritten by the refresh
    /// at the end of the flow.
    pub fn forget_saved_server(self: &Arc<Self>, server: SavedServer) {
        let controller = Arc::clone(self);
        spawn_local(async move {
            controller.forget_saved(server).await;
        });
    }

    /// [`Self::forget_saved_server`]'s body.
    ///
    /// **Ordering contract: the secret first, the row second — and the row only if
    /// the secret went.** The row *is* the key its secret is filed under, so a row
    /// removed ahead of a failed credential delete would strand a secret nothing
    /// can ever name again.
    async fn forget_saved(&self, server: SavedServer) {
        let endpoint = server.endpoint();
        if let Err(failure) = self.credentials.delete(endpoint.clone()).await {
            app_toasts().raise(&failure);
            return;
        }
        if let Err(failure) = self.repository.remove(&endpoint) {
            app_toasts().raise(&failure);
            // The secret is gone either way, so a row that survived the remove must
            // at least stop claiming one — otherwise its next tap raises a gate
            // that can only answer "not found".
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

    /// Gate + store one secret through `ControllerCore::run`, which is what keeps
    /// the blocking platform call off the UI thread. A run failure cannot happen
    /// through the seam's contract; treating it as failed keeps this total.
    async fn gate_store(
        &self,
        server_key: String,
        token: Zeroizing<String>,
        reason: String,
    ) -> CredentialStatus {
        self.core
            .run(
                &self.save_credential,
                StoreRequest {
                    server_key,
                    token,
                    reason,
                },
                RunOptions::default(),
            )
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

    /// Revoke a server-side token, best effort. Every caller is past the point of no
    /// return — the credential has either just been replaced or never got stored.
    async fn revoke_quietly(&self, name: String) {
        if let Err(failure) = self.tokens.revoke_token(name).await {
            log::warn!("credentials: revoking a replaced token failed: {failure:?}");
        }
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
            // A Connect screen that has gone away saves nothing — but the caller's
            // completion still runs, since a dropped one would strand whatever was
            // waiting on it (the seam's own contract).
            None => on_done(),
        }
    }
}

/// The saved-server row a successful connect writes back — run from a completion
/// closure that holds no `&self`, hence the cloned handles.
struct LoginUpsert {
    repository: SavedServers,
    connection: Arc<ConnectionController>,
    saved: RwSignal<Vec<SavedServer>>,
    host: String,
    port: u16,
    trust_mode: TrustMode,
    label: String,
    fingerprint: Option<String>,
}

impl LoginUpsert {
    /// The `host:port` this write is for.
    fn endpoint(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Write the row, on the UI thread, from a settled connect. **The credential
    /// fields are carried forward** from whatever row already exists: rebuilding
    /// them would wipe `has_credential` on every login — including the one the
    /// recover path performs, erasing the credential it had just used.
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
            // Only the posture that pins carries a pin — a stale fingerprint on a
            // downgraded row would outlive the pairing that produced it.
            fingerprint: match self.trust_mode {
                TrustMode::Pinned => self.fingerprint.clone(),
                _ => None,
            },
        };
        if let Err(failure) = self.repository.save(server) {
            app_toasts().raise(&failure);
            return;
        }
        ConnectController::reload_into(&self.repository, self.saved);
    }
}

/// `muxr-mobile-<label>-<base36 timestamp>`. The stamp makes it unique, because
/// the daemon addresses a token *by name* and a second save for the same rig must
/// not collide with the token it is replacing.
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
        // A label with nothing name-safe in it still needs *something* before the
        // stamp, or the name reads as a stray double dash.
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

// ---------------------------------------------------------------------------
// Pairing → form / target mapping
// ---------------------------------------------------------------------------

/// The endpoint text a scanned payload prefills the field with: `host:port`, an
/// IPv6 literal **bracketed** so the address still reads as one host and one port.
/// `parse_endpoint` is mock-grade and cannot take that form apart again, which is
/// why [`pairing_covers`] strips the brackets rather than round-tripping.
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

/// Whether `endpoint` still names the host and port `payload` was scanned for. The
/// host comparison accepts the bracketed IPv6 form [`pairing_endpoint`] writes,
/// since that is the text sitting in the field after a prefill.
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
/// payload rather than the form, with the endpoint as a fallback label so a saved
/// row is never labelled empty. `remember` is always `true` and `offer_credential`
/// always `false` (flow 2 owns that save).
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
        let at = |raw: &str| {
            parse_endpoint(raw).is_ok_and(|endpoint| pairing_covers(&payload, &endpoint))
        };
        assert!(at("192.168.1.100:50051"), "the prefilled text itself");
        assert!(!at("192.168.1.100:50052"), "another port");
        assert!(!at("192.168.1.101:50051"), "another host");
    }

    #[test]
    fn a_bracketed_ipv6_prefill_still_covers_its_own_payload() {
        // The round trip `pairing_endpoint` → `parse_endpoint` cannot make on its
        // own (that parser is mock-grade and keeps the brackets), which is why
        // `pairing_covers` strips them rather than trusting the parse.
        let payload = pinned("fd00::1", 50051, "ab");
        let endpoint = parse_endpoint(&pairing_endpoint(&payload)).expect("host:port parses");
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
}
