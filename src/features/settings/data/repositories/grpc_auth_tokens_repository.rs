//! The real [`AuthTokensRepository`] over the live `muxrd` client.

use std::sync::Arc;

use zeroize::Zeroizing;

use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AuthToken, CreateTokenRequest, MintedToken, TokenScope,
};
use crate::features::settings::domain::repositories::AuthTokensRepository;
use crate::net::proto::{CreateTokenReq, Empty, RevokeTokenReq, TokenInfo};
use crate::net::{MuxrClient, error};

const NOT_CONNECTED: &str = "no connection is open";

/// Holds the controller `Arc`: a per-call `use_context` would panic inside an owner-less `spawn_local` future.
pub struct GrpcAuthTokensRepository {
    connection: Arc<ConnectionController>,
}

impl GrpcAuthTokensRepository {
    pub fn new(connection: Arc<ConnectionController>) -> Self {
        Self { connection }
    }

    /// Re-resolved per call since the connection can drop; disconnected is a retryable failure because `/settings` is not route-guarded.
    fn client(&self) -> Result<MuxrClient, AppFailure> {
        self.connection
            .client()
            .ok_or_else(|| AppFailure::Network(NOT_CONNECTED.to_string()))
    }
}

fn map_status(
    controller: &ConnectionController,
    client: &MuxrClient,
    status: &tonic::Status,
) -> AppFailure {
    // UNAUTHENTICATED fires the session-expired hook for the client the call used.
    let failure = error::from_status(status);
    if matches!(failure, AppFailure::Auth(_)) {
        controller.on_session_expired_for(client);
    }
    failure
}

fn scope_from_wire(read_only: bool) -> TokenScope {
    if read_only {
        TokenScope::ReadOnly
    } else {
        TokenScope::ReadWrite
    }
}

fn auth_token_from_wire(info: &TokenInfo) -> AuthToken {
    AuthToken {
        name: info.name.clone(),
        scope: scope_from_wire(info.read_only),
        created: info.created_at.clone(),
    }
}

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
            .map_err(|status| map_status(&self.connection, &client, &status))?
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
            .map_err(|status| map_status(&self.connection, &client, &status))?
            .into_inner();
        // Lift the secret out so the wire copy is scrubbed.
        let secret = Zeroizing::new(std::mem::take(&mut info.token));
        Ok(minted_token_from_wire(&info, secret))
    }

    async fn revoke_token(&self, name: String) -> Result<(), AppFailure> {
        let client = self.client()?;
        let ack = client
            .authed()
            .revoke_token(RevokeTokenReq { name })
            .await
            .map_err(|status| map_status(&self.connection, &client, &status))?
            .into_inner();
        // A refused revoke is a successful RPC carrying a refusal.
        error::ack_result(&ack)
    }
}

#[cfg(test)]
mod tests {
    use frust::GetUntracked;

    use super::*;
    use crate::net::testing::MuxrdFixture;

    async fn live_connected(fixture: &MuxrdFixture) -> (Arc<ConnectionController>, MuxrClient) {
        let client = fixture.connect_pinned().await;
        let controller = ConnectionController::with_live_client_for_tests(client.clone());
        (controller, client)
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
        // Empty is the proto's "unknown"; it is never substituted.
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

    #[tokio::test]
    async fn an_unauthenticated_status_maps_to_auth_and_fires_the_session_expired_hook() {
        let fixture = MuxrdFixture::start().await;
        let (controller, client) = live_connected(&fixture).await;
        assert!(controller.connected.get_untracked());

        let failure = map_status(
            &controller,
            &client,
            &tonic::Status::unauthenticated("expired"),
        );

        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(
            !controller.connected.get_untracked(),
            "the session-expired hook tears the connection down"
        );
    }

    #[tokio::test]
    async fn a_retryable_status_maps_to_network_and_leaves_the_connection_alone() {
        let fixture = MuxrdFixture::start().await;
        let (controller, client) = live_connected(&fixture).await;

        let failure = map_status(&controller, &client, &tonic::Status::unavailable("down"));

        assert!(matches!(failure, AppFailure::Network(_)));
        assert!(
            controller.connected.get_untracked(),
            "a retryable failure never tears the connection down"
        );
    }

    #[tokio::test]
    async fn a_repeated_unauthenticated_status_tears_the_connection_down_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let fixture = MuxrdFixture::start().await;
        let (controller, client) = live_connected(&fixture).await;
        let raised = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&raised);
        let _subscription = controller
            .core()
            .failures()
            .subscribe(move |_failure: &AppFailure| {
                counter.fetch_add(1, Ordering::SeqCst);
            });

        for _ in 0..3 {
            map_status(
                &controller,
                &client,
                &tonic::Status::unauthenticated("expired"),
            );
        }

        assert_eq!(raised.load(Ordering::SeqCst), 1);
    }
}
