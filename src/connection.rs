//! [`ConnectionController`] — muxr's one app-scoped connection handle,
//! mock-only (no `muxrd` gRPC round trip yet — see `docs/ARCHITECTURE.md`'s
//! Planned section).
//!
//! Lives beside [`crate::failure`] rather than inside any one feature's
//! `{domain, data, presentation}` slice: whether the app is connected gates
//! *routing* itself (`crate::routes`'s session guard) and is read by at least
//! two features (Sessions renders [`ConnectionController::server_info`]'s
//! latency, Settings later renders the rest), so it is composition-root state,
//! not `connect`'s alone.
//!
//! Constructed exactly once, in `MuxrApp::init`, via
//! [`provide_connection_controller`] — published under the root owner
//! (`clean_signals_frust::provide_controller`) so any screen recovers the
//! *same* handle via [`expect_connection_controller`] (itself a thin
//! `use_context` wrapper), and so does `crate::routes`'s top-level session
//! guard, which has no `&mut AppState` to reach a controller through any
//! other way.
//!
//! # Failure seam
//!
//! [`ConnectionController`] embeds a [`ControllerCore`] by composition (the
//! `examples/huddle` controller shape — see e.g. its `ChannelsController`) and
//! exposes it via `AsRef<ControllerCore<AppFailure>>`, so a future toast
//! bridge can subscribe to `controller.as_ref().failures()` the same way it
//! would for any other controller in this app. A mock [`ConnectionController::connect`]
//! never actually fails, but the surface exists now so that bridge has one
//! idiom to consume across every controller, mock or real.
//!
//! # Credential offer placement
//!
//! [`ConnectionController::credential_offer`] is hosted here (not on `connect`)
//! because the Flutter app it mirrors found the Connect screen unmounts on a
//! successful redirect before its own offer UI could show — Sessions, which
//! is where the redirect lands, is what actually renders the offer.

use std::sync::Arc;

use clean_signals::ControllerCore;
use clean_signals_frust::{expect_controller, provide_controller};
use frust::{RwSignal, Set};

use crate::failure::AppFailure;

/// Mock latency [`ConnectionController::connect`] fills [`ServerInfo`] with —
/// Sessions renders it; there is no real round trip yet to time.
const MOCK_LATENCY_MS: u32 = 42;
/// Mock server version string [`ConnectionController::connect`] fills
/// [`ServerInfo`] with — Settings later renders it.
const MOCK_SERVER_VERSION: &str = "muxrd/mock";

/// A mock server's connection facts — Sessions renders [`Self::latency_ms`]
/// today; Settings renders the rest once it has a real screen.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerInfo {
    /// `host:port`, as dialed.
    pub endpoint: String,
    /// A human label for [`ConnectTarget::trust_mode`] (e.g. "System trust").
    pub tls_label: String,
    /// The mock `muxrd` version string.
    pub version: String,
    /// Mock round-trip latency, milliseconds.
    pub latency_ms: u32,
}

/// A mock connection's TLS verification posture — carried on [`ConnectTarget`]
/// and rendered back as [`ServerInfo::tls_label`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustMode {
    /// The platform's system trust store validates the certificate normally.
    System,
    /// Trust-on-first-use: pin whatever certificate the first connect sees.
    TrustOnFirstUse,
    /// A certificate pinned ahead of time (the future QR-pairing flow).
    Pinned,
}

impl TrustMode {
    /// The label [`ServerInfo::tls_label`] renders for this mode.
    fn label(self) -> &'static str {
        match self {
            TrustMode::System => "System trust",
            TrustMode::TrustOnFirstUse => "Trust on first use",
            TrustMode::Pinned => "Pinned certificate",
        }
    }
}

/// A connect request's target + options — plain data (see the module docs'
/// mock-only note). Wave 2's Connect screen builds one from its form; today
/// only this module's own tests and `tests/routes.rs`'s guard tests construct
/// one directly.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectTarget {
    /// The host to dial (mock — never actually resolved/connected to).
    pub host: String,
    /// The port to dial.
    pub port: u16,
    /// This target's TLS verification posture.
    pub trust_mode: TrustMode,
    /// A user-facing label for this server (e.g. "Home rig") — carried
    /// through to [`ConnectionController::credential_offer`] when
    /// [`Self::offer_credential`] is set.
    pub label: String,
    /// Whether a successful connect should offer to save this target's
    /// credential (see [`ConnectionController::credential_offer`]'s doc for
    /// why Sessions, not Connect, hosts the offer UI).
    pub offer_credential: bool,
}

/// Muxr's one app-scoped connection handle. See the [module docs](self).
pub struct ConnectionController {
    core: ControllerCore<AppFailure>,
    /// Whether the app currently holds a (mock) live connection. Read by
    /// `crate::routes`'s session guard and by any screen.
    pub connected: RwSignal<bool>,
    /// The connected server's mock facts, or `None` while disconnected.
    pub server_info: RwSignal<Option<ServerInfo>>,
    /// The server label whose credential Sessions should offer to save, or
    /// `None` when there is nothing to offer.
    pub credential_offer: RwSignal<Option<String>>,
}

impl ConnectionController {
    fn new() -> Self {
        Self {
            core: ControllerCore::new(),
            connected: RwSignal::new(false),
            server_info: RwSignal::new(None),
            credential_offer: RwSignal::new(None),
        }
    }

    /// Mock-connect to `target`. Never fails (see the module docs' Failure
    /// seam note): fills [`Self::server_info`] with mock data derived from
    /// `target`, flips [`Self::connected`], and — only when
    /// `target.offer_credential` is set — records `target.label` on
    /// [`Self::credential_offer`] for Sessions to offer saving.
    pub fn connect(&self, target: ConnectTarget) {
        let tls_label = target.trust_mode.label().to_string();
        let endpoint = format!("{}:{}", target.host, target.port);
        self.server_info.set(Some(ServerInfo {
            endpoint,
            tls_label,
            version: MOCK_SERVER_VERSION.to_string(),
            latency_ms: MOCK_LATENCY_MS,
        }));
        self.connected.set(true);
        if target.offer_credential {
            self.credential_offer.set(Some(target.label));
        }
    }

    /// Clears every connection-scoped signal. Callers navigate away
    /// themselves — this never touches the router.
    pub fn disconnect(&self) {
        self.connected.set(false);
        self.server_info.set(None);
        self.credential_offer.set(None);
    }

    /// Accepts the pending credential offer. A mock no-op beyond clearing it
    /// — no secure-storage plugin is wired to actually save anything yet.
    pub fn accept_credential_offer(&self) {
        self.credential_offer.set(None);
    }

    /// Dismisses the pending credential offer without saving it.
    pub fn dismiss_credential_offer(&self) {
        self.credential_offer.set(None);
    }
}

impl AsRef<ControllerCore<AppFailure>> for ConnectionController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

/// Construct the app's one [`ConnectionController`] and publish it under the
/// root owner. Called exactly once, from `MuxrApp::init` — see the
/// [module docs](self).
pub fn provide_connection_controller() {
    provide_controller(Arc::new(ConnectionController::new()));
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
    use frust::GetUntracked;

    use super::*;

    fn target(offer_credential: bool) -> ConnectTarget {
        ConnectTarget {
            host: "mock.example".to_string(),
            port: 7777,
            trust_mode: TrustMode::TrustOnFirstUse,
            label: "Mock rig".to_string(),
            offer_credential,
        }
    }

    #[test]
    fn connect_fills_server_info_and_flips_connected() {
        let controller = ConnectionController::new();
        controller.connect(target(false));

        assert!(controller.connected.get_untracked());
        let info = controller
            .server_info
            .get_untracked()
            .expect("connect fills server_info");
        assert_eq!(info.endpoint, "mock.example:7777");
        assert_eq!(info.tls_label, "Trust on first use");
        assert_eq!(info.latency_ms, MOCK_LATENCY_MS);
    }

    #[test]
    fn connect_without_offer_credential_leaves_the_offer_none() {
        let controller = ConnectionController::new();
        controller.connect(target(false));

        assert_eq!(controller.credential_offer.get_untracked(), None);
    }

    #[test]
    fn connect_with_offer_credential_records_the_targets_label() {
        let controller = ConnectionController::new();
        controller.connect(target(true));

        assert_eq!(
            controller.credential_offer.get_untracked(),
            Some("Mock rig".to_string())
        );
    }

    #[test]
    fn disconnect_clears_every_connection_scoped_signal() {
        let controller = ConnectionController::new();
        controller.connect(target(true));

        controller.disconnect();

        assert!(!controller.connected.get_untracked());
        assert_eq!(controller.server_info.get_untracked(), None);
        assert_eq!(controller.credential_offer.get_untracked(), None);
    }

    #[test]
    fn accept_and_dismiss_credential_offer_both_clear_it() {
        let accepted = ConnectionController::new();
        accepted.connect(target(true));
        accepted.accept_credential_offer();
        assert_eq!(accepted.credential_offer.get_untracked(), None);

        let dismissed = ConnectionController::new();
        dismissed.connect(target(true));
        dismissed.dismiss_credential_offer();
        assert_eq!(dismissed.credential_offer.get_untracked(), None);
    }
}
