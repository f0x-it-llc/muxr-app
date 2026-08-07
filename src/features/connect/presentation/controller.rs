//! [`ConnectController`] — the Connect screen's view model.
//!
//! Hosted by `super::pages`'s `ConnectScreen` through
//! [`clean_signals_frust::use_controller`], so it is built once when the `/`
//! route mounts and disposed on unmount (`docs/ARCHITECTURE.md`'s "Building a
//! Screen" charter). It owns every **observed** piece of the screen: the form's
//! four fields, the saved-server list, and the one validation flag — all
//! `RwSignal`s, per the charter's state-split rule (non-reactive UI state lives
//! in the component's own `State` instead).
//!
//! # Shape
//!
//! Embeds a [`ControllerCore`] by composition and exposes it via
//! `AsRef<ControllerCore<AppFailure>>` — the `examples/huddle` controller shape,
//! matched here for the same reason `crate::connection::ConnectionController`
//! matches it: one idiom for every controller in this app. This core carries no
//! in-flight work of its own: the saved-server repository is synchronous (see
//! `domain::repositories`' note), and the one asynchronous thing the screen
//! starts — the dial — runs under the *connection* controller's core, which is
//! where its failures surface.
//!
//! # Injected, never resolved here
//!
//! Every dependency arrives through the constructor: this feature's two stores
//! (the saved-server repository and the credential service, both published by
//! `super::super::di::provide`), the app-scoped [`ConnectionController`], and —
//! the one cross-feature reach — `settings`' own auth-token repository. The
//! screen resolves them all with `use_context` inside its `use_controller`
//! factory, where a reactive owner is live — **not** from inside a press
//! handler, which runs outside the shell's `ReactiveRuntime::with_owner` scope
//! and so sees no ambient owner at all (`frust-shell-desktop`'s `dispatch` is
//! outside it; `use_context` would silently return `None`). Holding the `Arc`s
//! is what makes the press handlers safe.
//!
//! **Why this controller mints server tokens.** A saved credential *is* a
//! server-side token minted for this device, and minting one is
//! `AuthTokensRepository::create_token` — `settings`' contract, resolved through
//! `settings::di`'s own `expect_` function by the screen. That is the
//! repository-widening the `terminal` feature already established
//! (`docs/ARCHITECTURE.md`'s cross-feature note: a page resolves a sibling's
//! `di::expect_*` directly rather than re-routing a copy through its own), used
//! here for the same reason: one owner for a fact, and the token list has one
//! already.
//!
//! # The credential flows
//!
//! Five, and each one is a named method here (the Flutter client's
//! `connect_controller.dart`/`connection_controller.dart` split, collapsed onto
//! this one controller because muxr's connection handle deliberately owns no
//! feature repository):
//!
//! 1. **Manual save** — [`ConnectController::begin_credential_save`], reached
//!    through [`crate::connection::CredentialSaver`] when the user accepts the
//!    offer Sessions raises. Mints a fresh read-write token, gates and stores
//!    *that* secret, then rotates: the previous token is revoked only once the
//!    new credential is safely persisted, and a gate denial revokes the token it
//!    just minted rather than orphaning it on the server.
//! 2. **QR save** — [`ConnectController::connect_from_pairing`]'s success
//!    branch. Connect first, then **one** prompt to store the scanned token
//!    (nothing is minted — the QR's token *is* the credential). A denial leaves
//!    the session live and silently unsaved.
//! 3. **Login with a saved credential** —
//!    [`ConnectController::recover_and_connect`]: unlock, then connect with the
//!    recovered token under the row's own stored posture. Every other outcome
//!    reports [`CredentialStatus::message`] and prefills the manual form.
//! 4. **Delete** — ungated, and owned by the Settings screen (this feature's
//!    only delete is `forget_unreadable`'s cleanup after an invalidated key).
//! 5. **Upsert on login** — every successful connect through this controller
//!    writes the server's metadata row back (`LoginUpsert`), carrying the
//!    credential fields forward: rebuilding the row from scratch on each login
//!    would erase the very `has_credential` the recover path had just used.
//!
//! **No gated call happens on the UI thread.** Both gate operations go through
//! `ControllerCore::run` on the async use cases, whose production
//! implementation puts the blocking platform call on `frust::spawn_blocking`
//! (`data::repositories::secure_credential_service`'s thread rule) — this
//! controller only ever awaits.
//!
//! # Failure vocabulary
//!
//! Every failure this controller raises is an [`AppFailure`] surfaced through
//! [`crate::ui::toasts::app_toasts`] — never a string built here. Validation maps
//! through `domain::use_cases::parse_endpoint`, whose own docs record the one
//! vocabulary gap (`AppFailure` has no `Validation` arm).
//!
//! A *refused dial* is the exception, and deliberately so: it is raised on
//! [`ConnectionController`]'s own failure sink, which the composition root
//! already bridges to the same toasts. Re-raising it here would show the user
//! two toasts for one failure.
//!
//! # The pairing path
//!
//! A scanned `muxr://pair` code ([`super::pages`]'s QR sheet) never connects by
//! itself: [`ConnectController::apply_scanned_payload`] **prefills** the form
//! and stashes the payload in [`ConnectController::pending_pairing`], and the
//! explicit pinned CTA ([`super::pieces::connect_button`]'s extra action) is
//! what calls [`ConnectController::connect_from_pairing`]. That scan-prefills /
//! action-connects split is the Flutter client's own shape
//! (`connect_controller.dart`'s `applyScannedPayload` + `connectFromPairing`),
//! and it is the reason nothing here dials off a camera frame.
//!
//! A stashed payload is the *pin's* provenance, so it is dropped the moment the
//! form stops describing the thing that was scanned: an edit to either the
//! endpoint or the login token clears it (see [`ConnectController::set_url`] /
//! [`ConnectController::set_token`]), as does a disconnect (the screen's own
//! edge calls [`ConnectController::clear_pending_pairing`]) and a successful
//! pairing connect. A *failed* one keeps it, so a retry stays one tap — the
//! same keep-the-form-filled contract a refused manual dial already has.
//!
//! **Where the QR credential save attaches.** A pairing connect deliberately
//! sets [`ConnectTarget::offer_credential`] to `false`: the manual
//! "save this server?" offer must not fire for a scanned credential, because
//! the secure-storage write for a paired token is its own one-prompt flow —
//! [`ConnectController::connect_from_pairing`]'s success branch, beside the
//! token scrub.

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

/// The trust posture a fresh form starts on — the safe one, and the mock's own
/// pre-selected row (`docs/redesign.html`'s `.radio-dot.sel` sits on "Public
/// domain (CA)").
const DEFAULT_TRUST_MODE: TrustMode = TrustMode::System;

/// Whether a fresh form offers to remember the server. `true`, matching the
/// mock, which renders `.toggle.on`.
const DEFAULT_REMEMBER: bool = true;

/// The prefix every token this device mints for itself carries — the Flutter
/// client's `zelli-mobile-…` shape, under this app's own name, so a server's
/// token list says which device asked for each one.
const TOKEN_NAME_PREFIX: &str = "muxr-mobile";

/// How much of a server's label rides in a minted token's name. Long enough to
/// tell two rigs apart in the settings list, short enough that the name stays
/// one line.
const TOKEN_LABEL_MAX: usize = 24;

/// The Connect screen's view model. See the [module docs](self).
pub struct ConnectController {
    core: ControllerCore<AppFailure>,
    /// The saved-server store [`Self::reload_saved`] reads.
    repository: SavedServers,
    /// The credential store, for the one operation that runs no gate (dropping
    /// a credential whose key the device invalidated). Every *gated* operation
    /// goes through [`Self::save_credential`]/[`Self::unlock_credential`]
    /// instead, so it is driven by `ControllerCore::run` like every other
    /// awaited call here.
    credentials: Credentials,
    /// `settings`' auth-token store — where a saved credential's token is
    /// minted and revoked (see the module docs' cross-feature note).
    tokens: SharedAuthTokensRepository,
    /// Gate + store, behind `ControllerCore::run`.
    save_credential: SaveCredential,
    /// Gate + read, likewise.
    unlock_credential: UnlockCredential,
    /// The app-scoped connection handle a successful connect drives (see the
    /// module docs' injection note for why it is held rather than resolved).
    connection: Arc<ConnectionController>,
    /// Whether a credential save is already running — the single-flight guard a
    /// double-tapped "Save" would otherwise mint a second server token through
    /// (`connection_controller.dart`'s own `_savingCredential`).
    saving_credential: AtomicBool,
    /// The server-URL field's text (`host:port`).
    pub url: RwSignal<String>,
    /// The login-token field's text — the form's one secret, so it is a
    /// [`Zeroizing`]-wrapped `String` rather than a plain one: replacing or
    /// dropping the wrapper scrubs the buffer instead of leaving stale
    /// plaintext parked in a freed allocation. That covers the *at-rest*
    /// half; two edges cover the *when* — the disconnect edge, through
    /// [`Self::clear_token`] (`super::pages::ConnectScreen::build`'s
    /// `connection.connected` true→false watch), and the connect edge, where
    /// [`Self::connect`]'s completion closure scrubs this same signal the same
    /// way. So a mounted-but-covered Connect screen (Sessions stacks over it
    /// rather than replacing it) never keeps a stale token alive across either
    /// transition. A connect that *fails* deliberately keeps it: the user needs
    /// it to retry. What this does **not** cover — the platform IME's own
    /// suggestion/recents surface — is recorded in `docs/ARCHITECTURE.md`'s
    /// Planned section.
    pub token: RwSignal<Zeroizing<String>>,
    /// The selected TLS posture.
    pub trust_mode: RwSignal<TrustMode>,
    /// Whether a successful connect should offer to remember this server.
    pub remember: RwSignal<bool>,
    /// The saved-server list, as last read from the repository.
    pub saved: RwSignal<Vec<SavedServer>>,
    /// Whether the last connect attempt failed validation — the screen inks the
    /// URL field's label with it. Cleared by any edit to [`Self::url`] (see
    /// [`Self::set_url`]) and by a successful connect.
    pub invalid: RwSignal<bool>,
    /// The pairing code scanned in this screen's lifetime, or `None` when
    /// none is live — the module docs' pairing path. Holding the whole
    /// [`PairingPayload`] (rather than only its fingerprint) is what lets
    /// [`Self::connect_from_pairing`] dial the *scanned* credential rather
    /// than whatever the token field currently reads; the payload's own
    /// [`Zeroizing`] token and redacting [`std::fmt::Debug`] carry the same
    /// hygiene [`Self::token`] does.
    pub pending_pairing: RwSignal<Option<PairingPayload>>,
    /// Whether a biometric unlock is in flight — the other half of
    /// [`Self::busy`]. Separate from the connection's own `connecting` because
    /// no socket is open yet: the user is answering a system prompt, and the
    /// CTA must still read as working.
    pub unlocking: RwSignal<bool>,
}

impl ConnectController {
    /// A fresh controller over this feature's two stores, `settings`' token
    /// store and the app's connection handle, with an empty form and the saved
    /// list already loaded (see [`Self::reload_saved`]).
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
        };
        controller.reload_saved();
        controller
    }

    /// The [`CredentialSaver`] `crate::connection` calls when the user accepts
    /// the save offer — this controller, reached weakly.
    ///
    /// Weakly on purpose: the connection handle outlives every screen, and a
    /// strong reference here would keep a disposed controller (and the whole
    /// screen's state behind it) alive for the process's life. A Connect screen
    /// that has genuinely gone away makes an accepted offer a no-op instead,
    /// which is the honest outcome — the flow needs the repositories that
    /// screen resolved.
    pub fn credential_saver(self: &Arc<Self>) -> Arc<dyn CredentialSaver> {
        Arc::new(ControllerCredentialSaver {
            controller: Arc::downgrade(self),
        })
    }

    /// Re-read the saved-server list into [`Self::saved`]. A repository failure
    /// raises a toast and leaves the previous list in place — a screen that
    /// can't list its servers is still a screen the user can type into.
    pub fn reload_saved(&self) {
        match self.repository.list() {
            Ok(servers) => self.saved.set(servers),
            Err(failure) => app_toasts().raise(&failure),
        }
    }

    /// Set the server-URL field, clearing any stale validation complaint — the
    /// user is answering it.
    ///
    /// This is the **manual-edit** seam (the field's own `on_change`), so it is
    /// also where a live pairing is checked: if the newly typed address no
    /// longer names the scanned host and port, the payload is dropped. That is
    /// `connect_controller.dart`'s `onAddressChanged` rule verbatim — re-parse,
    /// compare host and port, clear on a mismatch (an unparseable address
    /// counts as one) — and it is why
    /// [`Self::apply_scanned_payload`] writes [`Self::url`] directly instead of
    /// coming through here: a prefill is not an edit.
    pub fn set_url(&self, url: String) {
        self.invalidate_pairing_unless_covering(&url);
        self.url.set(url);
        if self.invalid.get_untracked() {
            self.invalid.set(false);
        }
    }

    /// Set the login-token field. Wraps `token` in a fresh [`Zeroizing`] —
    /// the signal's previous wrapper is dropped (and so scrubbed) by the
    /// replace.
    ///
    /// A hand-typed token drops a live pairing too. That half has **no**
    /// counterpart in `connect_controller.dart` (whose only clearing trigger is
    /// the address), and it is deliberate: [`Self::connect_from_pairing`] sends
    /// the *payload's* token, so a form whose token field says one thing while
    /// the pinned CTA would send another is a form that lies about what it is
    /// about to do.
    pub fn set_token(&self, token: String) {
        self.token.set(Zeroizing::new(token));
        self.clear_pending_pairing();
    }

    /// Scrub the login-token field back to empty — the screen's own
    /// disconnect-edge watch. See [`Self::token`]'s doc for the other edge and
    /// for what scrubbing does and does not cover. A fresh
    /// `Zeroizing::new(String::new())` rather than mutating the held buffer in
    /// place: the signal's replace still drops (and so zeroizes) the stale one.
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

    /// Refill the form from the saved row at `index`. Out-of-range indices are
    /// ignored (the list can only have shrunk between the frame that painted
    /// the row and the press that hit it).
    pub fn select_saved(&self, index: usize) {
        let Some(server) = self.saved.get_untracked().get(index).cloned() else {
            return;
        };
        self.prefill(&server);
    }

    /// Refill the form from `server` — its endpoint and the posture it was last
    /// trusted under.
    ///
    /// The fallback every refused unlock ends with (`connect_controller.dart`'s
    /// `selectSaved`): whatever the gate said, the user is left one token away
    /// from connecting by hand.
    pub fn prefill(&self, server: &SavedServer) {
        self.set_url(server.endpoint());
        self.trust_mode.set(server.trust_mode);
    }

    /// The saved row at `index` was tapped — reconnect through its stored
    /// credential where there is one, and otherwise just refill the form.
    ///
    /// The Flutter client's own split (`connect_page.dart`: `hasCredential ?
    /// recoverAndConnect : selectSaved`). A row with no credential has nothing
    /// to unlock, so tapping it must not raise a prompt.
    pub fn saved_row_tapped(self: &Arc<Self>, index: usize, nav: &Nav) {
        let Some(server) = self.saved.get_untracked().get(index).cloned() else {
            return;
        };
        if server.has_credential {
            self.recover_and_connect(server, nav);
        } else {
            self.prefill(&server);
        }
    }

    /// Whether a dial is in flight — the connect button's busy state, shared
    /// by **both** CTAs (the manual one and the pinned one).
    ///
    /// The connection controller's own signal rather than a second copy here:
    /// the dial is *its* work, and two flags for one fact drift. It is also the
    /// single-flight guard both paths inherit — `connect_with` ignores a second
    /// connect while one is in flight, so the two buttons cannot open two
    /// sockets between them.
    pub fn connecting(&self) -> RwSignal<bool> {
        self.connection.connecting
    }

    /// Whether *anything* the CTAs share is in flight: a dial, or the biometric
    /// unlock that precedes one ([`Self::unlocking`]). A **tracked** read — this
    /// is what a piece renders its busy state from.
    pub fn busy(&self) -> bool {
        self.connection.connecting.get() || self.unlocking.get()
    }

    /// [`Self::busy`] without subscribing — the guard a press handler reads,
    /// where there is no owner to subscribe.
    fn busy_untracked(&self) -> bool {
        self.connection.connecting.get_untracked() || self.unlocking.get_untracked()
    }

    // -----------------------------------------------------------------------
    // The pairing path (see the module docs)
    // -----------------------------------------------------------------------

    /// Prefill the form from a scanned `muxr://pair` payload and stash it.
    ///
    /// Never connects — that is the pinned CTA's job
    /// ([`Self::connect_from_pairing`]). What lands: the endpoint
    /// ([`pairing_endpoint`]), the payload's own token, the trust posture the
    /// code carries ([`pairing_trust_mode`] — `Pinned` for a pin,
    /// [`TrustMode::System`] for a CA pairing, which is what makes the trust
    /// selector render honestly), and any stale validation complaint cleared,
    /// since the user did not type this.
    ///
    /// [`Self::url`]/[`Self::token`] are written **directly** rather than
    /// through their setters: those two are the manual-edit seams, and going
    /// through them here would clear the very payload this is stashing.
    pub fn apply_scanned_payload(&self, payload: PairingPayload) {
        self.url.set(pairing_endpoint(&payload));
        self.token.set(payload.token.clone());
        self.trust_mode.set(pairing_trust_mode(&payload.trust));
        self.invalid.set(false);
        self.pending_pairing.set(Some(payload));
    }

    /// Drop the stashed pairing, if any, and hand the trust posture back to
    /// the manual selector's default — the pin came with the payload, so it
    /// cannot outlive it (a left-behind [`TrustMode::Pinned`] would fail closed
    /// at dial time with no fingerprint to dial under).
    ///
    /// A no-op with nothing stashed, so callers need no guard of their own.
    /// The screen's disconnect edge calls this beside [`Self::clear_token`]
    /// (`connect_controller.dart` clears its pending trust on the same edge).
    pub fn clear_pending_pairing(&self) {
        if self.pending_pairing.with(Option::is_none) {
            return;
        }
        self.pending_pairing.set(None);
        self.trust_mode.set(DEFAULT_TRUST_MODE);
    }

    /// Drop the stashed pairing unless `url` still names its host and port —
    /// [`Self::set_url`]'s half of the invalidation rule.
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

    /// Connect with the stashed pairing's own credentials — the pinned CTA.
    ///
    /// A no-op with nothing stashed (the CTA only renders while there is), and
    /// ignored outright while a dial is already in flight (the connection
    /// controller's own single-flight guard — see [`Self::connecting`]).
    ///
    /// On success: the token field is scrubbed and the payload consumed (the
    /// session is live; nothing is left holding the QR's secret), then the same
    /// guarded push to [`SESSIONS_LOCATION`] the manual path makes. On failure
    /// the form is left exactly as filled — payload included — so the CTA
    /// stays and a retry is one tap; the failure itself surfaces through the
    /// connection controller's own toast sink, like every other refused dial.
    /// (`connect_controller.dart` instead clears its pending pairing on
    /// failure and degrades to the manual form; muxr keeps the one-tap retry,
    /// which its keep-the-token-on-failure contract already implies.)
    ///
    /// **The paired credential's one prompt** rides on the success branch: the
    /// session is already live, so the scanned token is offered to the gate
    /// exactly once ([`Self::save_paired_credential`]) and a denial changes
    /// nothing about the session. Connect first, store second, one prompt per
    /// successful pairing tap, zero on a failed one.
    ///
    /// `nav` is lent rather than held, for the reason [`Self::connect`]'s doc
    /// spells out.
    pub fn connect_from_pairing(self: &Arc<Self>, nav: &Nav) {
        let Some(payload) = self.pending_pairing.get_untracked() else {
            return;
        };
        self.invalid.set(false);
        let nav = nav.clone();
        // Both signals are `Copy`, and the closure outlives this borrow of
        // `self` whenever the dial is a real one (see `Self::connect`).
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

    /// The [`ConnectTarget`] this form describes, or why it doesn't.
    ///
    /// Split out from [`Self::connect`] so the validation + target mapping is
    /// unit-testable without a live navigator. `offer_credential` is set only
    /// when the user asked to be remembered **and** this endpoint isn't already
    /// saved — re-offering a server the user already stored is noise. That is
    /// the caller's half of the credential-offer decision `crate::connection`
    /// documents; the read-only half is the connection controller's.
    ///
    /// A hand-picked [`TrustMode::Pinned`] carries no `fingerprint` and so
    /// fails closed before dialing rather than silently dropping to a weaker
    /// posture — the manual form collects no pin. The one exception is a
    /// **matching stashed pairing**: when [`Self::pending_pairing`] names the
    /// very endpoint that was typed, its trust posture and fingerprint ride
    /// through here too, so the ordinary CTA connects rather than fails closed
    /// on the pin its own prefill installed (`connect_controller.dart`'s
    /// `submit` gives a matching pairing the same priority). Such a connect is
    /// never *offerable* either: a scanned credential's save is the pairing
    /// flow's own one-prompt step, never the manual offer (see the module
    /// docs).
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
            offer_credential: !paired && remember && known.is_none(),
            host: endpoint.host,
            port: endpoint.port,
            trust_mode,
            token: self.token.get_untracked(),
            remember,
            fingerprint,
        })
    }

    /// Validate the form and, if it holds up, open the connection and — once it
    /// is actually up — navigate to [`SESSIONS_LOCATION`] through the app's one
    /// guarded router (see `crate::routes`), so this push resolves exactly as a
    /// deep link to the same path would.
    ///
    /// `nav` is passed in rather than held: [`Nav`] wraps an `Rc`-backed
    /// `Router` and so is neither `Send` nor `Sync`, while this controller must
    /// be both (`use_controller`'s bound). The screen resolves the handle in
    /// its own `Component::init`, keeps it in its `Component::State`, and lends
    /// it here. The completion closure below is what carries that `Rc` across
    /// the dial — it runs on the UI thread, and only there.
    ///
    /// A rejection raises the failure as a toast and lights [`Self::invalid`];
    /// nothing navigates and no connection opens. A *refused dial* raises its
    /// own toast through the connection controller's sink (see the module
    /// docs), leaves the form as the user typed it — token included, so a retry
    /// is one tap — and never navigates.
    pub fn connect(&self, nav: &Nav) {
        match self.validated_target() {
            Ok(target) => {
                self.invalid.set(false);
                let nav = nav.clone();
                // Login-token hygiene: the token's job ends the instant a
                // connection opens with it — see `Self::token`'s doc for the
                // other edge this pairs with. The signal is captured by value
                // (`RwSignal` is `Copy`) because the closure outlives this
                // borrow of `self` whenever the dial is a real one.
                let token = self.token;
                let upsert = self.login_upsert(&target);
                self.connection.connect_with(target, move |outcome| {
                    if outcome.is_ok() {
                        upsert.run();
                        token.set(Zeroizing::new(String::new()));
                        nav.push(SESSIONS_LOCATION);
                    }
                });
            }
            Err(failure) => {
                self.invalid.set(true);
                app_toasts().raise(&failure);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Saved-server persistence (see the module docs' flow 5)
    // -----------------------------------------------------------------------

    /// The metadata write a successful connect to `target` performs.
    ///
    /// Built **before** the target moves into the connection controller, and run
    /// from the completion closure — the caller-side placement
    /// `connection_controller.dart` does inline inside its own `login()`. It
    /// stays feature-side here because `crate::connection` deliberately owns no
    /// repository (its module docs' credential-offer split).
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

    /// Re-read the saved list into [`Self::saved`], raising a repository
    /// failure as a toast — [`Self::reload_saved`] by another name, callable
    /// from the completion closures that hold no `&self`.
    fn reload_into(repository: &SavedServers, saved: RwSignal<Vec<SavedServer>>) {
        match repository.list() {
            Ok(servers) => saved.set(servers),
            Err(failure) => app_toasts().raise(&failure),
        }
    }

    /// Rewrite the saved row for `endpoint` through `edit`, returning the token
    /// name it carried **before** the edit.
    ///
    /// A no-op (and `None`) for an endpoint with no row: every credential flow
    /// runs after a successful login, which has already written one, so a
    /// missing row means the list was cleared underneath the flow.
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

    /// Mint a token for this device, store it behind the gate, and record it on
    /// the saved row — [`crate::connection::CredentialSaver`]'s work, reached
    /// when the user accepts the offer Sessions raises.
    ///
    /// Returns immediately: the mint, the prompt and the two stores all run on
    /// the UI thread's task queue behind this call. Ignored outright while a
    /// save is already running (a double-tapped "Save" must not mint two
    /// server-side tokens) and for a read-only session, which cannot mint at
    /// all.
    pub fn begin_credential_save(self: &Arc<Self>, endpoint: String, label: String) {
        if self.connection.is_read_only.get_untracked() {
            return;
        }
        if self.saving_credential.swap(true, Ordering::SeqCst) {
            return;
        }
        let controller = Arc::clone(self);
        spawn_local(async move {
            controller.save_session_credential(endpoint, label).await;
            controller.saving_credential.store(false, Ordering::SeqCst);
        });
    }

    /// [`Self::begin_credential_save`]'s body, in order: mint, gate + store,
    /// record, rotate.
    ///
    /// Two orderings are the contract, both from
    /// `connection_controller.dart`'s own `saveCredential`:
    ///
    /// - a **denied gate revokes the token it just minted**, because nothing
    ///   holds it any more and an unrevoked one sits on the server forever; and
    /// - the **previous** token is revoked only *after* the new credential is
    ///   persisted, so a failure between the two leaves the user with a working
    ///   old token rather than none at all.
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
                // through `core`, so its failure is emitted here explicitly —
                // the screen's toast bridge listens on the same sink either way.
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

    /// Store the scanned token for a just-paired server: one prompt, no mint.
    ///
    /// Silent whatever the gate says — the session is already live, and a
    /// pairing the user completed is not a moment to interrupt with a
    /// biometric complaint (`connect_controller.dart`'s own note: a denied
    /// store leaves `hasCredential:false` and no error). The saved row is
    /// already written by the login upsert; this only flips its credential
    /// fields.
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
            // learns what the server calls it — so there is nothing to revoke
            // on a later delete.
            controller.update_saved(&endpoint, |row| {
                row.has_credential = true;
                row.token_name = None;
            });
            // Belt and braces: a pairing target is never offerable
            // (`pairing_target`), so there should be no offer pending — but the
            // credential this just stored would make one moot anyway.
            controller.connection.dismiss_credential_offer();
        });
    }

    // -----------------------------------------------------------------------
    // Flow 3 — logging in with a saved credential
    // -----------------------------------------------------------------------

    /// Unlock `server`'s stored credential and connect with it.
    ///
    /// On success the recovered token dials under the row's **own** stored
    /// posture and pin (a QR-paired rig reconnects pinned, exactly as it was
    /// paired), with no save offer — a credential this connect just read is one
    /// the user obviously already has.
    ///
    /// Every other outcome reports [`CredentialStatus::message`] and prefills
    /// the manual form, except a cancel, which is silent by contract. An
    /// invalidated key additionally forgets the credential it can no longer
    /// read ([`Self::forget_unreadable`]).
    ///
    /// Ignored while a dial or another unlock is in flight — the same
    /// single-flight rule the CTAs share.
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

    /// Drop a credential the device can no longer read (its key was
    /// invalidated by a biometric re-enrollment) and clear the metadata that
    /// claims one exists.
    ///
    /// The delete is best-effort and ungated — the plugin's own documented
    /// recovery for an invalidated key is to remove and re-set the entry, and a
    /// user who cannot read it must still be able to be rid of it.
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
    // Shared credential plumbing
    // -----------------------------------------------------------------------

    /// Gate + store one secret, through `ControllerCore::run` (which is what
    /// keeps the blocking platform call on the blocking pool — see the module
    /// docs' threading note). A run failure cannot happen through the seam's own
    /// contract; treating it as [`CredentialStatus::Failed`] keeps this total.
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

    /// Say what `status` means, if it means anything to say (a success and a
    /// cancel do not — see [`CredentialStatus::message`]).
    fn report(&self, status: CredentialStatus) {
        if let Some(message) = status.message() {
            app_toasts().show(message);
        }
    }

    /// Revoke a server-side token, best effort: a failure is logged and
    /// otherwise ignored.
    ///
    /// Every caller is already past the point of no return — the credential it
    /// belongs to has either just been replaced or never got stored — so a
    /// revoke that cannot reach the server must not become the user's problem.
    async fn revoke_quietly(&self, name: String) {
        if let Err(failure) = self.tokens.revoke_token(name).await {
            log::warn!("credentials: revoking a replaced token failed: {failure:?}");
        }
    }
}

/// The [`CredentialSaver`] a [`ConnectController`] hands to
/// `crate::connection` — see [`ConnectController::credential_saver`] for why it
/// holds the controller weakly.
struct ControllerCredentialSaver {
    controller: std::sync::Weak<ConnectController>,
}

impl CredentialSaver for ControllerCredentialSaver {
    fn save_credential(&self, endpoint: String, label: String) {
        if let Some(controller) = self.controller.upgrade() {
            controller.begin_credential_save(endpoint, label);
        }
    }
}

/// The saved-server row a successful connect writes back.
///
/// Built from the target *before* it moves into the connection controller, and
/// run from the completion closure — which holds no `&self`, hence the cloned
/// handles rather than a borrow. See
/// [`ConnectController::login_upsert`].
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

    /// Write the row. Runs on the UI thread, from a settled connect.
    ///
    /// **The credential fields are carried forward** from whatever row already
    /// exists: rebuilding them from scratch here would wipe `has_credential` on
    /// every login — including the login the recover path itself performs,
    /// which would erase the credential it had just used
    /// (`connection_controller.dart` records the same bug).
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
            // Only the posture that pins carries a pin — a stale fingerprint on
            // a downgraded row would outlive the pairing that produced it.
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

/// The name a token minted for this device carries:
/// `muxr-mobile-<label>-<base36 timestamp>`.
///
/// The Flutter client's shape (`zelli-mobile-<label>-<ts36>`) under this app's
/// own name. The label is slugged and the stamp makes it unique, because the
/// daemon addresses a token *by name* and a second save for the same rig must
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
        // A label with nothing name-safe in it still needs *something* before
        // the stamp, or the name reads as a stray double dash.
        return "device".to_string();
    }
    trimmed.to_string()
}

/// `value` in base 36 — the compact stamp a minted token's name ends with
/// (Dart's `toRadixString(36)`). A non-positive clock reads as `0`.
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

/// The endpoint text a scanned payload prefills the server-URL field with.
///
/// `host:port`, with an IPv6 literal **bracketed** so the reconstructed
/// address still reads as one host and one port — the Flutter client's own
/// fix, kept here because the same string is what the user then edits. It is
/// deliberately not something `parse_endpoint` can take apart again (that
/// parser is documented mock-grade: no bracket syntax), which is exactly why
/// [`pairing_covers`] compares hosts with the brackets stripped rather than
/// leaning on a round trip.
fn pairing_endpoint(payload: &PairingPayload) -> String {
    if payload.host.contains(':') {
        return format!("[{}]:{}", payload.host, payload.port);
    }
    format!("{}:{}", payload.host, payload.port)
}

/// The connect-form posture a pairing's own trust hint maps onto.
///
/// The two enums are deliberately separate types (see
/// `domain::entities::PairingTrust`), and this is the one mapping between
/// them: a pin becomes [`TrustMode::Pinned`] with its fingerprint threaded
/// separately ([`pinned_fingerprint`]), a CA pairing becomes the ordinary
/// system-trust posture, which is what the platform store already validates.
fn pairing_trust_mode(trust: &PairingTrust) -> TrustMode {
    match trust {
        PairingTrust::Pin { .. } => TrustMode::Pinned,
        PairingTrust::Ca => TrustMode::System,
    }
}

/// The pinned certificate's fingerprint, or `None` for a CA pairing — the
/// data half of [`pairing_trust_mode`]'s posture.
fn pinned_fingerprint(payload: &PairingPayload) -> Option<String> {
    match &payload.trust {
        PairingTrust::Pin { fp_hex } => Some(fp_hex.clone()),
        PairingTrust::Ca => None,
    }
}

/// Whether `endpoint` — a parsed server-URL field — still names the host and
/// port `payload` was scanned for.
///
/// The host comparison accepts the bracketed IPv6 form [`pairing_endpoint`]
/// writes, since that is the very text sitting in the field after a prefill.
fn pairing_covers(payload: &PairingPayload, endpoint: &Endpoint) -> bool {
    endpoint.port == payload.port && unbracketed(&endpoint.host) == payload.host
}

/// `host` with one layer of IPv6 brackets removed, if it has them.
fn unbracketed(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

/// The [`ConnectTarget`] a stashed pairing dials under.
///
/// Every field comes from the payload rather than the form: the token the code
/// carried (not whatever the field reads), the posture and fingerprint it
/// pinned, and its own label — falling back to the endpoint when the QR named
/// none, so a saved row is never labelled with an empty string. `remember` is
/// always `true` (a paired login is a keeper — the 28-day session
/// `ConnectTarget::remember` documents), and `offer_credential` always `false`
/// (see the module docs' attach-point note).
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
        // The round trip `pairing_endpoint` → `parse_endpoint` cannot make on
        // its own (that parser is mock-grade and keeps the brackets), which is
        // why `pairing_covers` strips them rather than trusting the parse.
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
