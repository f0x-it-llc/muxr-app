//! The bearer credential: a swappable session token and the interceptor that
//! stamps it onto every authenticated call.
//!
//! **The wire contract, from `muxrd`.** Authenticated RPCs carry
//! `authorization: Bearer <session_token>`. Exactly two paths are public —
//! `/muxr.v1.Muxr/GetVersion` and `/muxr.v1.Muxr/Login` — and an absent,
//! malformed or expired token on any other path is answered
//! `Status::unauthenticated`. That split is the reason for two stubs rather than
//! one interceptor that skips a couple of methods: [`unauth_stub`] is what dials
//! version + login, [`authed_stub`] is everything after.
//!
//! **The token is mutable process state.** A re-login swaps it while calls are
//! in flight, so it lives behind an `ArcSwapOption`: the interceptor's read is
//! lock-free, and a rotation never rebuilds the channel or stalls a stream pump.
//! The stored value is a `Zeroizing<String>`, so the previous token's buffer is
//! scrubbed when its last reader drops it — deferred, not immediate, if a call
//! is mid-flight holding a clone, which is the correct order (scrubbing a buffer
//! another thread is still reading would be the bug).
//!
//! The one residue this cannot cover is tonic's own copy: a `MetadataValue` is a
//! `Bytes` the transport owns, so the header bytes for an in-flight request live
//! outside this module's control until that request completes.

use std::sync::Arc;

use arc_swap::ArcSwapOption;
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use tonic::{Request, Status};
use zeroize::Zeroizing;

/// The metadata key `muxrd` reads the bearer credential from.
pub const AUTHORIZATION_KEY: &str = "authorization";

/// The credential scheme prefix, including its separating space.
const BEARER_PREFIX: &str = "Bearer ";

/// A stub for the two unauthenticated paths (`GetVersion`, `Login`).
pub type UnauthStub = crate::net::proto::muxr_client::MuxrClient<Channel>;

/// A stub whose every call carries the current session token.
pub type AuthedStub =
    crate::net::proto::muxr_client::MuxrClient<InterceptedService<Channel, BearerInterceptor>>;

/// The process's current session token, swappable while calls are in flight.
///
/// Stores the whole header *value* (`Bearer <token>`) rather than the bare
/// token: the interceptor then never formats a fresh, unscrubbed `String`
/// holding the secret on the hot path.
#[derive(Default)]
pub struct SessionToken {
    header: ArcSwapOption<Zeroizing<String>>,
}

/// Reports only whether a credential is held — a derived `Debug` would print the
/// session token into any log line that formats a client.
impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionToken")
            .field("is_set", &self.is_set())
            .finish()
    }
}

impl SessionToken {
    /// A token store holding no credential — every authenticated call fails
    /// until [`Self::set`] runs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a session token, scrubbing the previous one.
    pub fn set(&self, token: &str) {
        let next = Zeroizing::new(format!("{BEARER_PREFIX}{token}"));
        // Dropping the swapped-out value is what scrubs it: `Zeroizing` zeroes
        // on drop, and the drop happens once the last in-flight reader releases
        // its `Arc`.
        drop(self.header.swap(Some(Arc::new(next))));
    }

    /// Drop the session token, scrubbing it.
    pub fn clear(&self) {
        drop(self.header.swap(None));
    }

    /// Whether a token is currently installed.
    pub fn is_set(&self) -> bool {
        self.header.load().is_some()
    }

    /// The current `authorization` header value, if any.
    fn header_value(&self) -> Option<Arc<Zeroizing<String>>> {
        self.header.load_full()
    }
}

/// Stamps `authorization: Bearer <session token>` onto every request it sees.
///
/// Installed only on [`AuthedStub`], so it never reaches `GetVersion`/`Login`.
#[derive(Clone, Debug)]
pub struct BearerInterceptor {
    token: Arc<SessionToken>,
}

impl BearerInterceptor {
    /// Bind an interceptor to a token store; later swaps of that store are seen
    /// by every stub already built from it.
    pub fn new(token: Arc<SessionToken>) -> Self {
        Self { token }
    }
}

impl Interceptor for BearerInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        // Failing here rather than sending an anonymous request is deliberate:
        // the server's answer would be `unauthenticated` anyway, and failing
        // locally keeps a token-less call off the wire entirely.
        let Some(header) = self.token.header_value() else {
            return Err(Status::unauthenticated("no session token"));
        };
        let value = MetadataValue::try_from(header.as_str())
            .map_err(|_| Status::unauthenticated("session token is not valid header ASCII"))?;
        request.metadata_mut().insert(AUTHORIZATION_KEY, value);
        Ok(request)
    }
}

/// The stub for `GetVersion`/`Login` — no credential attached, by design.
pub fn unauth_stub(channel: Channel) -> UnauthStub {
    UnauthStub::new(channel)
}

/// The stub for every authenticated RPC.
pub fn authed_stub(channel: Channel, token: Arc<SessionToken>) -> AuthedStub {
    crate::net::proto::muxr_client::MuxrClient::with_interceptor(
        channel,
        BearerInterceptor::new(token),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stored value is the whole header, so a stamped request reads exactly
    /// what `muxrd` parses.
    #[test]
    fn interceptor_stamps_the_bearer_header() {
        let token = Arc::new(SessionToken::new());
        token.set("session-abc");
        let mut interceptor = BearerInterceptor::new(Arc::clone(&token));

        let request = interceptor
            .call(Request::new(()))
            .expect("a set token stamps");
        let header = request
            .metadata()
            .get(AUTHORIZATION_KEY)
            .expect("authorization is present");
        assert_eq!(header.to_str().expect("ascii"), "Bearer session-abc");
    }

    /// A token-less authenticated call never reaches the wire.
    #[test]
    fn interceptor_refuses_without_a_token() {
        let token = Arc::new(SessionToken::new());
        let mut interceptor = BearerInterceptor::new(token);

        let status = interceptor
            .call(Request::new(()))
            .expect_err("no token, no request");
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
    }

    /// A rotation is visible to an interceptor built before it, and clearing
    /// takes the credential away again.
    #[test]
    fn token_rotation_and_clear_are_seen_by_existing_interceptors() {
        let token = Arc::new(SessionToken::new());
        let mut interceptor = BearerInterceptor::new(Arc::clone(&token));

        token.set("first");
        assert!(token.is_set());
        token.set("second");
        let request = interceptor.call(Request::new(())).expect("rotated token");
        assert_eq!(
            request
                .metadata()
                .get(AUTHORIZATION_KEY)
                .expect("authorization is present")
                .to_str()
                .expect("ascii"),
            "Bearer second"
        );

        token.clear();
        assert!(!token.is_set());
        assert!(interceptor.call(Request::new(())).is_err());
    }

    /// A token with a byte no HTTP header may carry is rejected locally rather
    /// than panicking inside the transport.
    #[test]
    fn non_ascii_token_is_a_local_failure() {
        let token = Arc::new(SessionToken::new());
        token.set("bad\ntoken");
        let mut interceptor = BearerInterceptor::new(token);

        assert_eq!(
            interceptor
                .call(Request::new(()))
                .expect_err("a newline cannot go in a header")
                .code(),
            tonic::Code::Unauthenticated
        );
    }
}
