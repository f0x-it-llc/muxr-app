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
//! matches it: one idiom for every controller in this app, mock or real. The
//! core carries no in-flight work yet (nothing this controller does is async —
//! see `domain::repositories`' note on the synchronous repository), but it is
//! what `use_controller` disposes and what a later real dial will run through.
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

use std::sync::Arc;

use clean_signals::ControllerCore;
use frust::{GetUntracked, Router, RwSignal, Set};

use crate::AppState;
use crate::connection::{ConnectTarget, ConnectionController, TrustMode};
use crate::failure::AppFailure;
use crate::features::connect::di::SavedServers;
use crate::features::connect::domain::entities::SavedServer;
use crate::features::connect::domain::use_cases::parse_endpoint::parse_endpoint;
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
    /// The login-token field's text.
    pub token: RwSignal<String>,
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
            token: RwSignal::new(String::new()),
            trust_mode: RwSignal::new(DEFAULT_TRUST_MODE),
            remember: RwSignal::new(DEFAULT_REMEMBER),
            saved: RwSignal::new(Vec::new()),
            invalid: RwSignal::new(false),
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
    pub fn set_url(&self, url: String) {
        self.url.set(url);
        if self.invalid.get_untracked() {
            self.invalid.set(false);
        }
    }

    /// Set the login-token field.
    pub fn set_token(&self, token: String) {
        self.token.set(token);
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

    /// The [`ConnectTarget`] this form describes, or why it doesn't.
    ///
    /// Split out from [`Self::connect`] so the validation + target mapping is
    /// unit-testable without a live navigator. `offer_credential` is set only
    /// when the user asked to be remembered **and** this endpoint isn't already
    /// saved — re-offering a server the user already stored is noise.
    pub fn validated_target(&self) -> Result<ConnectTarget, AppFailure> {
        let endpoint = parse_endpoint(&self.url.get_untracked())?;
        let saved = self.saved.get_untracked();
        let known = saved
            .iter()
            .find(|server| server.host == endpoint.host && server.port == endpoint.port);
        Ok(ConnectTarget {
            // A known endpoint keeps its stored label; a new one is labelled by
            // its own host, which is what the credential offer will show.
            label: known.map_or_else(|| endpoint.host.clone(), |server| server.label.clone()),
            offer_credential: self.remember.get_untracked() && known.is_none(),
            host: endpoint.host,
            port: endpoint.port,
            trust_mode: self.trust_mode.get_untracked(),
        })
    }

    /// Validate the form and, if it holds up, open the (mock) connection and
    /// navigate to [`SESSIONS_LOCATION`].
    ///
    /// `router` is passed in rather than held: [`Router`] is `Rc`-backed and so
    /// neither `Send` nor `Sync`, while this controller must be both
    /// (`use_controller`'s bound). The screen owns the handle in its own
    /// `Component::State` and lends it here.
    ///
    /// A rejection raises the failure as a toast and lights [`Self::invalid`];
    /// nothing navigates and no connection opens.
    pub fn connect(&self, router: &Router<AppState>) {
        match self.validated_target() {
            Ok(target) => {
                self.invalid.set(false);
                self.connection.connect(target);
                router.push(SESSIONS_LOCATION);
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
