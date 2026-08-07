//! [`MuxrClient`] — one connected `muxrd` handle: the channel, the session
//! token, and the two stubs everything above this layer calls through.
//!
//! **Teardown is a drop, always.** There is no `shutdown()` and there will not
//! be one: an `AttachTerminal` stream never completes on its own, so a graceful
//! shutdown that waits for in-flight RPCs hangs forever. Dropping the last
//! clone closes the transport, which is the whole contract.
//!
//! **Rebuild is single-flight.** A dropped connection typically fails several
//! in-flight calls at once, so every one of their error handlers reaches for a
//! reconnect at the same moment. [`MuxrClient::reconnect`] takes a lock and
//! re-checks a generation counter behind it: the first caller dials, the rest
//! observe the newer generation and return without opening a second connection.
//! Stubs are minted per call from the current channel, so a caller that already
//! holds one keeps using the old connection until it next asks for a stub —
//! which is what a retry does.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use tokio::sync::Mutex;
use tonic::transport::Channel;

use crate::failure::AppFailure;
use crate::net::auth::{AuthedStub, SessionToken, UnauthStub, authed_stub, unauth_stub};
use crate::net::channel;
use crate::net::tls::TrustConfig;

/// A connected `muxrd` client. Cheap to clone — every clone shares one channel,
/// one session token and one rebuild guard.
#[derive(Clone, Debug)]
pub struct MuxrClient {
    inner: Arc<Inner>,
}

/// The shared state behind every [`MuxrClient`] clone.
#[derive(Debug)]
struct Inner {
    host: String,
    port: u16,
    trust: TrustConfig,
    token: Arc<SessionToken>,
    channel: ArcSwap<Channel>,
    /// Bumped on every successful rebuild; the value a [`MuxrClient::reconnect`]
    /// caller read before queueing for the lock tells it whether someone else
    /// already did the work.
    generation: AtomicU64,
    rebuild: Mutex<()>,
}

impl MuxrClient {
    /// Dial a target and hold the connection.
    ///
    /// The returned client carries no session token: `GetVersion` and `Login`
    /// are reachable through [`Self::unauth`] immediately, everything else once
    /// [`Self::set_session_token`] has run.
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

    /// The stub for the two unauthenticated paths, `GetVersion` and `Login`.
    pub fn unauth(&self) -> UnauthStub {
        unauth_stub(self.channel())
    }

    /// The stub for every authenticated RPC — each call carries the current
    /// session token.
    pub fn authed(&self) -> AuthedStub {
        authed_stub(self.channel(), Arc::clone(&self.inner.token))
    }

    /// Install the session token a `Login` response minted, scrubbing any
    /// previous one.
    pub fn set_session_token(&self, token: &str) {
        self.inner.token.set(token);
    }

    /// Drop the session token, scrubbing it. Authenticated calls fail locally
    /// until a new one is installed.
    pub fn clear_session_token(&self) {
        self.inner.token.clear();
    }

    /// Whether a session token is currently held.
    pub fn has_session_token(&self) -> bool {
        self.inner.token.is_set()
    }

    /// The host this client dialled.
    pub fn host(&self) -> &str {
        &self.inner.host
    }

    /// The port this client dialled.
    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// Rebuild the channel to the same target, at most once per concurrent
    /// burst (see the module docs). The session token is untouched — a dropped
    /// connection does not invalidate a credential.
    pub async fn reconnect(&self) -> Result<(), AppFailure> {
        let seen = self.inner.generation.load(Ordering::Acquire);
        let _guard = self.inner.rebuild.lock().await;
        if self.inner.generation.load(Ordering::Acquire) != seen {
            // Someone else rebuilt while this caller waited for the lock; their
            // channel is the current one, and dialing again would open a second.
            return Ok(());
        }

        let channel =
            channel::connect(&self.inner.host, self.inner.port, &self.inner.trust).await?;
        self.inner.channel.store(Arc::new(channel));
        self.inner.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// The current channel, cloned — `Channel` is itself a cheap handle.
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

    /// A pinned dial against the fixture's own certificate completes the
    /// handshake and answers an unauthenticated RPC.
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

    /// A pin for a *different* certificate must fail the handshake. Asserting
    /// "not success" rather than a message keeps the test honest about which
    /// rustls alert surfaces; the accept-any dial to the same fixture is what
    /// proves the refusal came from the pin and not from a dead server.
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

    /// The dev trust mode connects to a certificate it has never seen.
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

    /// The whole credential round trip: log in over the unauthenticated stub,
    /// install the minted token, and reach an authenticated RPC with it.
    #[tokio::test]
    async fn login_then_authed_rpc_round_trips() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;

        // Before a login, the authenticated stub refuses locally.
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

    /// A token the server does not recognise comes back as `UNAUTHENTICATED`
    /// and maps to a non-retryable auth failure — the "please reconnect" path.
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

    /// Clearing the token takes the credential away from stubs minted later.
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

    /// Concurrent reconnects collapse into one dial: the generation counter
    /// advances once, not once per caller.
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
        // The rebuilt channel is live.
        assert!(client.unauth().get_version(Empty {}).await.is_ok());
    }

    /// Target metadata survives on the handle, and a client is cheap to clone.
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
}
