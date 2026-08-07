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
//! Both dependencies arrive through the constructor: the saved-server repository
//! (published by `super::super::di::provide`) and the app-scoped
//! [`ConnectionController`]. The screen resolves them with `use_context` inside
//! its `use_controller` factory, where a reactive owner is live — **not** from
//! inside a press handler, which runs outside the shell's
//! `ReactiveRuntime::with_owner` scope and so sees no ambient owner at all
//! (`frust-shell-desktop`'s `dispatch` is outside it; `use_context` would
//! silently return `None`). Holding the `Arc`s is what makes the press handlers
//! safe.
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
//! the secure-storage write for a paired token is its own one-prompt flow. That
//! flow extends [`ConnectController::connect_from_pairing`]'s success branch
//! (beside the token scrub), and stores through the same seam
//! `crate::connection::accept_credential_offer` documents — the `connect`
//! feature's credential-persistence work, not this task's.

use std::sync::Arc;

use clean_signals::ControllerCore;
use frust::{GetUntracked, RwSignal, Set, With};
use zeroize::Zeroizing;

use crate::connection::{ConnectTarget, ConnectionController, TrustMode};
use crate::failure::AppFailure;
use crate::features::connect::di::SavedServers;
use crate::features::connect::domain::entities::{PairingPayload, PairingTrust, SavedServer};
use crate::features::connect::domain::use_cases::parse_endpoint::{Endpoint, parse_endpoint};
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

/// The Connect screen's view model. See the [module docs](self).
pub struct ConnectController {
    core: ControllerCore<AppFailure>,
    /// The saved-server store [`Self::reload_saved`] reads.
    repository: SavedServers,
    /// The app-scoped connection handle a successful connect drives (see the
    /// module docs' injection note for why it is held rather than resolved).
    connection: Arc<ConnectionController>,
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
}

impl ConnectController {
    /// A fresh controller over `repository` and `connection`, with an empty form
    /// and the saved list already loaded (see [`Self::reload_saved`]).
    pub fn new(repository: SavedServers, connection: Arc<ConnectionController>) -> Self {
        let controller = Self {
            core: ControllerCore::new(),
            repository,
            connection,
            url: RwSignal::new(String::new()),
            token: RwSignal::new(Zeroizing::new(String::new())),
            trust_mode: RwSignal::new(DEFAULT_TRUST_MODE),
            remember: RwSignal::new(DEFAULT_REMEMBER),
            saved: RwSignal::new(Vec::new()),
            invalid: RwSignal::new(false),
            pending_pairing: RwSignal::new(None),
        };
        controller.reload_saved();
        controller
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

    /// Refill the form from the saved row at `index` — the saved-row tap.
    /// Out-of-range indices are ignored (the list can only have shrunk between
    /// the frame that painted the row and the press that hit it).
    pub fn select_saved(&self, index: usize) {
        let Some(server) = self.saved.get_untracked().get(index).cloned() else {
            return;
        };
        self.set_url(server.endpoint());
        self.trust_mode.set(server.trust_mode);
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
    /// `nav` is lent rather than held, for the reason [`Self::connect`]'s doc
    /// spells out.
    pub fn connect_from_pairing(&self, nav: &Nav) {
        let Some(payload) = self.pending_pairing.get_untracked() else {
            return;
        };
        self.invalid.set(false);
        let nav = nav.clone();
        // Both signals are `Copy`, and the closure outlives this borrow of
        // `self` whenever the dial is a real one (see `Self::connect`).
        let token = self.token;
        let pending = self.pending_pairing;
        self.connection
            .connect_with(pairing_target(&payload), move |outcome| {
                if outcome.is_ok() {
                    token.set(Zeroizing::new(String::new()));
                    pending.set(None);
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
                self.connection.connect_with(target, move |outcome| {
                    if outcome.is_ok() {
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
