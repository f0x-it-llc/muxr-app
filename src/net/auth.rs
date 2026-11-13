//! The bearer credential and the interceptor that stamps it. Only `GetVersion`/`Login` are
//! public, hence two stubs. A rotation scrubs the old token once its last in-flight reader
//! drops it; tonic's own `MetadataValue` copy of the header is not scrubbed.

use std::sync::Arc;

use arc_swap::ArcSwapOption;
use tonic::metadata::MetadataValue;
use tonic::service::Interceptor;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::Channel;
use tonic::{Request, Status};
use zeroize::Zeroizing;

pub const AUTHORIZATION_KEY: &str = "authorization";

const BEARER_PREFIX: &str = "Bearer ";

pub type UnauthStub = crate::net::proto::muxr_client::MuxrClient<Channel>;

pub type AuthedStub =
    crate::net::proto::muxr_client::MuxrClient<InterceptedService<Channel, BearerInterceptor>>;

/// Stores the whole `Bearer <token>` header so the interceptor never formats an unscrubbed `String`.
#[derive(Default)]
pub struct SessionToken {
    header: ArcSwapOption<Zeroizing<String>>,
}

/// A derived `Debug` would leak the token into logs.
impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionToken")
            .field("is_set", &self.is_set())
            .finish()
    }
}

impl SessionToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, token: &str) {
        let next = Zeroizing::new(format!("{BEARER_PREFIX}{token}"));
        drop(self.header.swap(Some(Arc::new(next))));
    }

    pub fn clear(&self) {
        drop(self.header.swap(None));
    }

    pub fn is_set(&self) -> bool {
        self.header.load().is_some()
    }

    fn header_value(&self) -> Option<Arc<Zeroizing<String>>> {
        self.header.load_full()
    }
}

/// Installed only on [`AuthedStub`], so it never reaches `GetVersion`/`Login`.
#[derive(Clone, Debug)]
pub struct BearerInterceptor {
    token: Arc<SessionToken>,
}

impl BearerInterceptor {
    pub fn new(token: Arc<SessionToken>) -> Self {
        Self { token }
    }
}

impl Interceptor for BearerInterceptor {
    fn call(&mut self, mut request: Request<()>) -> Result<Request<()>, Status> {
        // Fail locally rather than send an anonymous request.
        let Some(header) = self.token.header_value() else {
            return Err(Status::unauthenticated("no session token"));
        };
        let value = MetadataValue::try_from(header.as_str())
            .map_err(|_| Status::unauthenticated("session token is not valid header ASCII"))?;
        request.metadata_mut().insert(AUTHORIZATION_KEY, value);
        Ok(request)
    }
}

pub fn unauth_stub(channel: Channel) -> UnauthStub {
    UnauthStub::new(channel)
}

pub fn authed_stub(channel: Channel, token: Arc<SessionToken>) -> AuthedStub {
    crate::net::proto::muxr_client::MuxrClient::with_interceptor(
        channel,
        BearerInterceptor::new(token),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn interceptor_refuses_without_a_token() {
        let token = Arc::new(SessionToken::new());
        let mut interceptor = BearerInterceptor::new(token);

        let status = interceptor
            .call(Request::new(()))
            .expect_err("no token, no request");
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
    }

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
