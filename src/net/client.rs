//! [`MuxrClient`]: one connected `muxrd` handle. Teardown is always a drop: an `AttachTerminal`
//! stream never completes, so a graceful `shutdown()` would hang. Stubs are minted per call, so
//! a holder keeps the old connection until its next stub.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use tokio::sync::Mutex;
use tonic::transport::Channel;

use crate::failure::AppFailure;
use crate::net::auth::{AuthedStub, SessionToken, UnauthStub, authed_stub, unauth_stub};
use crate::net::channel;
use crate::net::tls::TrustConfig;

/// Every clone shares one channel, one session token and one rebuild guard.
#[derive(Clone, Debug)]
pub struct MuxrClient {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    host: String,
    port: u16,
    trust: TrustConfig,
    token: Arc<SessionToken>,
    channel: ArcSwap<Channel>,
    /// Bumped on every rebuild so a queued `reconnect` caller sees the work is done.
    generation: AtomicU64,
    rebuild: Mutex<()>,
}

impl MuxrClient {
    /// Carries no session token: only [`Self::unauth`] works until [`Self::set_session_token`].
    pub async fn connect(host: &str, port: u16, trust: TrustConfig) -> Result<Self, AppFailure> {
        let channel = channel::connect(host, port, &trust).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                host: host.to_string(),
                port,
                trust,
                token: Arc::new(SessionToken::new()),
                channel: ArcSwap::from_pointee(channel),
                generation: AtomicU64::new(0),
                rebuild: Mutex::new(()),
            }),
        })
    }

    pub fn unauth(&self) -> UnauthStub {
        unauth_stub(self.channel())
    }

    pub fn authed(&self) -> AuthedStub {
        authed_stub(self.channel(), Arc::clone(&self.inner.token))
    }

    pub fn set_session_token(&self, token: &str) {
        self.inner.token.set(token);
    }

    pub fn clear_session_token(&self) {
        self.inner.token.clear();
    }

    pub fn has_session_token(&self) -> bool {
        self.inner.token.is_set()
    }

    pub fn host(&self) -> &str {
        &self.inner.host
    }

    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// A clone rather than a separate dial; tells a live client's failure from a superseded one's.
    pub fn is_same(&self, other: &MuxrClient) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Single-flight: concurrent callers collapse into one dial. The session token is untouched.
    pub async fn reconnect(&self) -> Result<(), AppFailure> {
        let seen = self.inner.generation.load(Ordering::Acquire);
        let _guard = self.inner.rebuild.lock().await;
        if self.inner.generation.load(Ordering::Acquire) != seen {
            // Rebuilt while this caller waited; dialing again would open a second connection.
            return Ok(());
        }

        let channel =
            channel::connect(&self.inner.host, self.inner.port, &self.inner.trust).await?;
        self.inner.channel.store(Arc::new(channel));
        self.inner.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    fn channel(&self) -> Channel {
        Channel::clone(&self.inner.channel.load())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::proto::{Empty, LoginRequest};
    use crate::net::testing::{self, MuxrdFixture};
    use crate::net::{error, tls};

    #[tokio::test]
    async fn pinned_trust_connects_to_the_matching_certificate() {
        let fixture = MuxrdFixture::start().await;
        let trust = tls::TrustConfig::pinned_from_hex(fixture.fingerprint_hex())
            .expect("the fixture prints a well-formed pin");

        let client = MuxrClient::connect("127.0.0.1", fixture.port(), trust)
            .await
            .expect("pinned handshake succeeds");
        let version = client
            .unauth()
            .get_version(Empty {})
            .await
            .expect("GetVersion needs no credential")
            .into_inner();

        assert_eq!(version.server_version, testing::SERVER_VERSION);
    }

    #[tokio::test]
    async fn wrong_pin_fails_the_handshake() {
        let fixture = MuxrdFixture::start().await;
        let wrong = tls::TrustConfig::PinnedSha256([0xAB; tls::PIN_LEN]);

        let failure = MuxrClient::connect("127.0.0.1", fixture.port(), wrong)
            .await
            .expect_err("a pin that matches nothing must not connect");
        assert!(matches!(failure, AppFailure::Network(_)));

        assert!(
            MuxrClient::connect(
                "127.0.0.1",
                fixture.port(),
                tls::TrustConfig::InsecureAcceptAny
            )
            .await
            .is_ok(),
            "the same fixture accepts a connection, so the pin is what refused"
        );
    }

    #[tokio::test]
    async fn insecure_accept_any_connects() {
        let fixture = MuxrdFixture::start().await;

        let client = MuxrClient::connect(
            "127.0.0.1",
            fixture.port(),
            tls::TrustConfig::InsecureAcceptAny,
        )
        .await
        .expect("accept-any handshake succeeds");
        assert!(client.unauth().get_version(Empty {}).await.is_ok());
    }

    #[tokio::test]
    async fn login_then_authed_rpc_round_trips() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;

        let refused = client
            .authed()
            .list_sessions(Empty {})
            .await
            .expect_err("no token, no call");
        assert!(matches!(error::from_status(&refused), AppFailure::Auth(_)));

        let login = client
            .unauth()
            .login(LoginRequest {
                auth_token: testing::AUTH_TOKEN.to_string(),
                remember_me: false,
            })
            .await
            .expect("Login needs no credential")
            .into_inner();
        assert_eq!(login.session_token, testing::SESSION_TOKEN);

        client.set_session_token(&login.session_token);
        assert!(client.has_session_token());

        let sessions = client
            .authed()
            .list_sessions(Empty {})
            .await
            .expect("a valid bearer token reaches an authenticated RPC")
            .into_inner();
        let names: Vec<&str> = sessions
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect();
        assert_eq!(names, testing::SESSION_NAMES);
    }

    #[tokio::test]
    async fn bad_token_maps_to_an_auth_failure() {
        use clean_signals::Failure;

        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;
        client.set_session_token("not-the-fixture-token");

        let status = client
            .authed()
            .list_sessions(Empty {})
            .await
            .expect_err("an unknown token is rejected");
        assert_eq!(status.code(), tonic::Code::Unauthenticated);

        let failure = error::from_status(&status);
        assert!(matches!(failure, AppFailure::Auth(_)));
        assert!(!failure.is_retryable());
    }

    #[tokio::test]
    async fn clearing_the_token_closes_the_authenticated_surface() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;
        client.set_session_token(testing::SESSION_TOKEN);
        assert!(client.authed().list_sessions(Empty {}).await.is_ok());

        client.clear_session_token();
        assert!(!client.has_session_token());
        assert!(client.authed().list_sessions(Empty {}).await.is_err());
    }

    #[tokio::test]
    async fn concurrent_reconnects_are_single_flight() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;

        let (first, second, third) =
            tokio::join!(client.reconnect(), client.reconnect(), client.reconnect());
        first.expect("rebuild succeeds");
        second.expect("rebuild succeeds");
        third.expect("rebuild succeeds");

        assert_eq!(client.inner.generation.load(Ordering::Acquire), 1);
        assert!(client.unauth().get_version(Empty {}).await.is_ok());
    }

    #[tokio::test]
    async fn client_reports_its_target_and_shares_state_across_clones() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;
        let clone = client.clone();

        assert_eq!(client.host(), "127.0.0.1");
        assert_eq!(client.port(), fixture.port());

        clone.set_session_token(testing::SESSION_TOKEN);
        assert!(client.has_session_token());
    }

    #[tokio::test]
    async fn a_clone_is_the_same_client_and_a_second_dial_is_not() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;
        let other = fixture.connect_pinned().await;

        assert!(client.is_same(&client.clone()));
        assert!(client.clone().is_same(&client));
        assert!(!client.is_same(&other));
        assert!(!other.is_same(&client));
    }
}
