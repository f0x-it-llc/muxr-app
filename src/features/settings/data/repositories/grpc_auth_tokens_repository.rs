//! [`GrpcAuthTokensRepository`] — the real [`AuthTokensRepository`] over the
//! connected `muxrd` client.
//!
//! **Held once, not per-call**: the `ConnectionController` `Arc` is set at DI
//! time under the ambient owner. Every real call here runs inside a raw
//! `frust::spawn_local` future with no ambient `Owner`, so a per-call
//! `use_context` lookup there would panic on the first RPC (masked in tests,
//! since `tests/support::setup` installs a permanent owner). [`client`](Self::client)
//! still re-resolves the live `MuxrClient` on every call — the connection can
//! drop/reconnect between calls on one repository instance; `None` maps to a
//! retryable [`AppFailure::Network`] rather than panicking, since `/settings`
//! is never route-guarded.
//!
//! This is `data`'s one reach past its own feature into `crate::connection`
//! (composition-root state, not a sibling feature's layer) — the same
//! widening `sessions`/`terminal`'s real repositories document.
//!
//! **Failure mapping**: every `tonic::Status` crosses
//! [`crate::net::error::from_status`]; `UNAUTHENTICATED` also fires
//! `ConnectionController::on_session_expired` (idempotent). `RevokeToken`'s
//! `ActionAck` maps through [`crate::net::error::ack_result`] instead — a
//! refused revoke is a successful RPC carrying a refusal, not a transport
//! error.
//!
//! **The one-time secret**: `ListTokens` never carries one (`TokenInfo.token`
//! is empty on every list entry); only `CreateToken`'s response does, lifted
//! out with the same `mem::take` hygiene `crate::connection`'s `Login` flow
//! uses, so the scrubbed wire copy never outlives the call.
//!
//! Tests here are mapping-only, over directly constructed proto messages —
//! `src/net/testing.rs`'s fixture answers these three RPCs with
//! `UNIMPLEMENTED` today, so a fixture round trip is future work.

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

/// [`GrpcAuthTokensRepository::client`]'s failure detail when disconnected —
/// retryable, since `/settings` is reachable while disconnected.
const NOT_CONNECTED: &str = "no connection is open";

/// The real [`AuthTokensRepository`], over the app's live `muxrd` connection.
pub struct GrpcAuthTokensRepository {
    connection: Arc<ConnectionController>,
}

impl GrpcAuthTokensRepository {
    /// Resolved once by `di::provide` and held from then on — see the
    /// [module docs](self).
    pub fn new(connection: Arc<ConnectionController>) -> Self {
        Self { connection }
    }

    /// The live client, or [`NOT_CONNECTED`] when nothing is connected.
    fn client(&self) -> Result<crate::net::MuxrClient, AppFailure> {
        self.connection
            .client()
            .ok_or_else(|| AppFailure::Network(NOT_CONNECTED.to_string()))
    }
}

/// Maps a gRPC status via [`error::from_status`], firing the session-expired
/// hook on `UNAUTHENTICATED`.
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
        // Lift the secret into `Zeroizing`, leaving the wire response
        // emptied — the same `mem::take` hygiene `connection.rs`'s `Login`
        // flow uses.
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
    /// is `#[cfg(test)]`-only, reachable here since `cargo test` compiles this
    /// whole crate under `--cfg test`.
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
        // Empty is the proto's own "unknown" — never substituted here.
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
