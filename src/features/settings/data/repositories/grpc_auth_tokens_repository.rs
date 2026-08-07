//! [`GrpcAuthTokensRepository`] — the real [`AuthTokensRepository`] over the
//! connected `muxrd` client.
//!
//! # Where the client comes from — resolved once, at DI time
//!
//! Unlike a per-call lookup, this repository holds the app's one
//! [`ConnectionController`](crate::connection::ConnectionController) as a
//! plain `Arc` field, resolved **exactly once**, in `settings::di::provide`
//! (composition-root wiring, run from `MuxrApp::init` under its ambient
//! reactive owner) — the same shape
//! [`GrpcSessionsRepository`](crate::features::sessions::data::repositories::GrpcSessionsRepository)
//! and
//! [`GrpcTerminalRepository`](crate::features::terminal::data::repositories::GrpcTerminalRepository)
//! already use.
//!
//! That is a deliberate correction, not a style choice: every real call site
//! on this repository runs inside a raw `frust::spawn_local` future body (a
//! settings-page load/create/revoke handler), and the only thing polling
//! that future — `ReactiveRuntime::pump_local` — installs **no** ambient
//! reactive `Owner`. A per-call
//! [`expect_connection_controller`](crate::connection::expect_connection_controller)/
//! `use_context` lookup from inside that poller sees `Owner::current() ==
//! None` and `.expect()`-panics
//! (`clean_signals_frust::expect_controller`'s own contract) on the very
//! first RPC this repository makes against a real connection — masked in
//! every test because `tests/support::setup` installs a permanent ambient
//! `Owner` no real app process ever has. Resolving the controller once, at
//! DI time, under the one context that *is* guaranteed a live owner, closes
//! that gap for good.
//!
//! [`client`](GrpcAuthTokensRepository::client) still asks the held
//! controller for the live [`MuxrClient`](crate::net::MuxrClient) fresh on
//! every call — that part alone stays per-call, and correctly so: the
//! connection can drop and reconnect between two calls on the same
//! repository instance. `None` (nothing connected, or a teardown raced this
//! call) maps to a retryable [`AppFailure::Network`] rather than panicking:
//! `/settings` is one of the two routes this app never guards, so every
//! token-repository call site can run while disconnected.
//!
//! This is the one place in `data` that reaches past its own feature into
//! `crate::connection` — a deliberate, narrow widening of the layer table's
//! `data` row (own domain + external I/O crates), the identical widening
//! `sessions`/`terminal`'s own real repositories document: `crate::connection`
//! is composition-root state beside `crate::failure`, not a sibling feature's
//! layer, and every call here stays at its plain, non-reactive surface
//! (`client()`, `on_session_expired()`) — nothing here reads or writes an
//! `RwSignal`, so this stays as framework-free in spirit as the rest of
//! `data`.
//!
//! # Failure mapping
//!
//! Every `tonic::Status` this module sees crosses through
//! [`crate::net::error::from_status`], the app's one gRPC→[`AppFailure`]
//! mapping site — with one addition local to this repository: an
//! `UNAUTHENTICATED` status also fires
//! [`ConnectionController::on_session_expired`](crate::connection::ConnectionController::on_session_expired),
//! the one hook every rejected-session repository call funnels into. That
//! hook is itself idempotent (several calls failing at once still tear the
//! connection down exactly once), so this module adds no debouncing of its
//! own. `RevokeToken`'s `ActionAck` is mapped through
//! [`crate::net::error::ack_result`] instead — a refused revoke is a
//! successful RPC carrying a refusal, not a transport error.
//!
//! # The one-time secret
//!
//! `ListTokens` never carries a token's secret (`TokenInfo.token` is empty on
//! every list entry — the proto's own doc comment); only a `CreateToken`
//! response does, exactly once. [`MintedToken::token`] is where that secret
//! lives on this side, lifted out of the wire response with the same
//! `mem::take` hygiene `crate::connection`'s `Login` flow uses for its own
//! session token, so the scrubbed wire copy never lingers in the decoded
//! response struct a moment longer than it has to.
//!
//! # Tests
//!
//! Mapping-only, over directly constructed proto messages — no fixture round
//! trip. `src/net/testing.rs`'s in-process fixture answers `ListTokens`/
//! `CreateToken`/`RevokeToken` with `UNIMPLEMENTED` today (that file's own
//! module docs), so an end-to-end test of the three RPCs themselves is a
//! later task's addition, not this one's.

use std::sync::Arc;

use zeroize::Zeroizing;

use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AuthToken, CreateTokenRequest, MintedToken, TokenScope,
};
use crate::features::settings::domain::repositories::AuthTokensRepository;
use crate::net::error;
use crate::net::proto::{CreateTokenReq, Empty, RevokeTokenReq, TokenInfo};

/// [`GrpcAuthTokensRepository::client`]'s failure detail when nothing is
/// connected — the retryable half of the contract
/// `ConnectionController::client` documents: `/settings` is never
/// route-guarded, so this is reachable, not a bug.
const NOT_CONNECTED: &str = "no connection is open";

/// The real [`AuthTokensRepository`], over the app's live `muxrd` connection.
/// See the [module docs](self).
pub struct GrpcAuthTokensRepository {
    connection: Arc<ConnectionController>,
}

impl GrpcAuthTokensRepository {
    /// A repository over the app's one [`ConnectionController`] — resolved
    /// once by the caller (`di::provide`) and held from then on. See the
    /// [module docs](self).
    pub fn new(connection: Arc<ConnectionController>) -> Self {
        Self { connection }
    }

    /// The live client, or [`NOT_CONNECTED`] when nothing is connected. See
    /// the [module docs](self)' "Where the client comes from" section.
    fn client(&self) -> Result<crate::net::MuxrClient, AppFailure> {
        self.connection
            .client()
            .ok_or_else(|| AppFailure::Network(NOT_CONNECTED.to_string()))
    }
}

/// Map a gRPC status through [`error::from_status`], firing
/// [`ConnectionController::on_session_expired`] on the one code that means
/// the session itself is gone — see the [module docs](self).
fn map_status(controller: &ConnectionController, status: &tonic::Status) -> AppFailure {
    let failure = error::from_status(status);
    if matches!(failure, AppFailure::Auth(_)) {
        controller.on_session_expired();
    }
    failure
}

/// A token's scope from the wire's one `read_only` bool.
fn scope_from_wire(read_only: bool) -> TokenScope {
    if read_only {
        TokenScope::ReadOnly
    } else {
        TokenScope::ReadWrite
    }
}

/// A `ListTokens`/`CreateToken` entry's metadata half — never its secret (see
/// the [module docs](self)).
fn auth_token_from_wire(info: &TokenInfo) -> AuthToken {
    AuthToken {
        name: info.name.clone(),
        scope: scope_from_wire(info.read_only),
        created: info.created_at.clone(),
    }
}

/// A `CreateToken` response's full shape, `secret` already lifted out of the
/// wire struct by the caller (see the trait impl below).
fn minted_token_from_wire(info: &TokenInfo, secret: Zeroizing<String>) -> MintedToken {
    MintedToken {
        name: info.name.clone(),
        scope: scope_from_wire(info.read_only),
        created: info.created_at.clone(),
        token: secret,
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl AuthTokensRepository for GrpcAuthTokensRepository {
    async fn list_tokens(&self) -> Result<Vec<AuthToken>, AppFailure> {
        let client = self.client()?;
        let list = client
            .authed()
            .list_tokens(Empty {})
            .await
            .map_err(|status| map_status(&self.connection, &status))?
            .into_inner();
        Ok(list.tokens.iter().map(auth_token_from_wire).collect())
    }

    async fn create_token(&self, request: CreateTokenRequest) -> Result<MintedToken, AppFailure> {
        let client = self.client()?;
        let mut info = client
            .authed()
            .create_token(CreateTokenReq {
                name: request.name,
                read_only: request.read_only,
            })
            .await
            .map_err(|status| map_status(&self.connection, &status))?
            .into_inner();
        // The secret crosses exactly once: lift it into the caller's
        // `Zeroizing` wrapper and leave the decoded response holding an empty
        // string — the same `mem::take` hygiene `connection.rs`'s `Login`
        // flow uses for its own session token.
        let secret = Zeroizing::new(std::mem::take(&mut info.token));
        Ok(minted_token_from_wire(&info, secret))
    }

    async fn revoke_token(&self, name: String) -> Result<(), AppFailure> {
        let client = self.client()?;
        let ack = client
            .authed()
            .revoke_token(RevokeTokenReq { name })
            .await
            .map_err(|status| map_status(&self.connection, &status))?
            .into_inner();
        error::ack_result(&ack)
    }
}

#[cfg(test)]
mod tests {
    use frust::GetUntracked;

    use super::*;
    use crate::connection::{ConnectTarget, TrustMode};

    /// A mock-backed, unpublished controller — `ConnectionController::detached`
    /// is `#[cfg(test)]`-only, reachable here because a `cargo test` run
    /// compiles this whole crate (this file included) under `--cfg test`.
    fn detached_connected() -> Arc<ConnectionController> {
        let controller = Arc::new(ConnectionController::detached());
        controller.connect(ConnectTarget {
            host: "mock.example".to_string(),
            port: 7777,
            trust_mode: TrustMode::NoVerification,
            label: "mock".to_string(),
            offer_credential: false,
            token: Zeroizing::new(String::new()),
            remember: false,
            fingerprint: None,
        });
        controller
    }

    #[test]
    fn a_token_infos_metadata_maps_verbatim_and_never_carries_a_secret() {
        let info = TokenInfo {
            name: "laptop".to_string(),
            token: String::new(),
            read_only: false,
            created_at: "2026-08-05 12:00:00".to_string(),
        };
        let token = auth_token_from_wire(&info);
        assert_eq!(token.name, "laptop");
        assert_eq!(token.scope, TokenScope::ReadWrite);
        assert_eq!(token.created, "2026-08-05 12:00:00");
    }

    #[test]
    fn the_wires_read_only_flag_maps_to_the_read_only_scope() {
        let info = TokenInfo {
            name: "phone".to_string(),
            token: String::new(),
            read_only: true,
            created_at: String::new(),
        };
        assert_eq!(auth_token_from_wire(&info).scope, TokenScope::ReadOnly);
    }

    #[test]
    fn an_empty_created_at_is_carried_through_rather_than_invented() {
        // "" if unknown" is the proto's own contract; this layer never
        // substitutes a placeholder date.
        let info = TokenInfo {
            name: "old".to_string(),
            token: String::new(),
            read_only: false,
            created_at: String::new(),
        };
        assert_eq!(auth_token_from_wire(&info).created, "");
    }

    #[test]
    fn a_create_response_lifts_the_secret_out_and_scrubs_the_wire_copy() {
        let mut response = TokenInfo {
            name: "laptop".to_string(),
            token: "s3cr3t-once".to_string(),
            read_only: false,
            created_at: "2026-08-05".to_string(),
        };
        let secret = Zeroizing::new(std::mem::take(&mut response.token));
        let minted = minted_token_from_wire(&response, secret);

        assert_eq!(minted.token.as_str(), "s3cr3t-once");
        assert_eq!(minted.name, "laptop");
        assert!(
            response.token.is_empty(),
            "the wire copy is scrubbed once the secret is lifted out"
        );
    }

    #[test]
    fn an_unauthenticated_status_maps_to_auth_and_fires_the_session_expired_hook() {
        let controller = detached_connected();
        assert!(controller.connected.get_untracked());

        let failure = map_status(&controller, &tonic::Status::unauthenticated("expired"));

        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(
            !controller.connected.get_untracked(),
            "the session-expired hook tears the connection down"
        );
    }

    #[test]
    fn a_retryable_status_maps_to_network_and_leaves_the_connection_alone() {
        let controller = detached_connected();

        let failure = map_status(&controller, &tonic::Status::unavailable("down"));

        assert!(matches!(failure, AppFailure::Network(_)));
        assert!(
            controller.connected.get_untracked(),
            "a retryable failure never tears the connection down"
        );
    }

    #[test]
    fn a_repeated_unauthenticated_status_tears_the_connection_down_exactly_once() {
        // `on_session_expired`'s own idempotence, exercised through this
        // module's one caller of it.
        use std::sync::atomic::{AtomicUsize, Ordering};

        let controller = detached_connected();
        let raised = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&raised);
        let _subscription = controller
            .core()
            .failures()
            .subscribe(move |_failure: &AppFailure| {
                counter.fetch_add(1, Ordering::SeqCst);
            });

        for _ in 0..3 {
            map_status(&controller, &tonic::Status::unauthenticated("expired"));
        }

        assert_eq!(raised.load(Ordering::SeqCst), 1);
    }
}
