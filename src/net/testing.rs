//! An in-process `muxrd` stand-in: a real TLS gRPC server on a loopback port,
//! serving the real generated stubs, so the client stack can be exercised
//! without the docker rig.
//!
//! **`#[cfg(test)]`, not a feature flag.** The fixture needs `rcgen` and
//! tonic's `server`/`router`/`transport` features, all of which are
//! `[dev-dependencies]` — reachable from the crate's own unit tests and from
//! nothing else. A `tests/*.rs` integration suite links the *library* build,
//! which Cargo compiles without dev-dependencies, so a fixture-driven test
//! belongs in a `#[cfg(test)] mod tests` inside `src/net`, and that is where
//! the client's handshake/credential coverage lives. Promoting this to a
//! `tests/support/` module is the move if an integration suite ever needs it;
//! `Cargo.toml` is single-owner and gains no feature for it.
//!
//! **Auth mirrors `muxrd`, checked per method rather than in a layer.** Only
//! `GetVersion` and `Login` are public; every other RPC calls
//! [`require_bearer`] first and answers `Status::unauthenticated` on a missing,
//! malformed or unknown credential. A tonic interceptor cannot express that
//! allowlist — it is handed a `Request<()>` with the metadata and extensions
//! only, never the URI, so it cannot tell `Login` from `ListSessions`.
//!
//! Only the RPCs a test needs are answered; every other method returns
//! `UNIMPLEMENTED` rather than a plausible-looking canned value, so a future
//! test that reaches one fails loudly instead of asserting against fiction.

use std::net::SocketAddr;

use rcgen::{CertifiedKey, generate_simple_self_signed};
use tokio::task::JoinHandle;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status};

use crate::net::client::MuxrClient;
use crate::net::proto::muxr_server::{Muxr, MuxrServer};
use crate::net::proto::{
    ActionAck, CloseSpaceReq, CreateSessionReq, CreateSpaceReq, CreateTokenReq, Empty, Layout,
    LoginRequest, LoginResponse, NewPaneReq, NewTabReq, PaneTarget, RenamePaneReq,
    RenameSessionReq, RenameSpaceReq, RenameTabReq, ResizePaneReq, RevokeTokenReq, ScrollReq,
    ServerFrame, SessionInfo, SessionList, SessionRef, SpaceList, SwitchSpaceReq, TabTarget,
    ToggleFullscreenReq, TokenInfo, TokenList, VersionInfo, WriteToPaneReq,
};
use crate::net::tls::{TrustConfig, leaf_fingerprint_hex};

/// The pairing token [`Muxr::login`] accepts.
pub const AUTH_TOKEN: &str = "fixture-auth-token";

/// The session token [`Muxr::login`] mints and every authenticated RPC demands.
pub const SESSION_TOKEN: &str = "fixture-session-token";

/// The canned session names [`Muxr::list_sessions`] returns.
pub const SESSION_NAMES: [&str; 2] = ["alpha", "beta"];

/// The version string [`Muxr::get_version`] reports.
pub const SERVER_VERSION: &str = "0.0.0-fixture";

/// A running fixture server plus the certificate material a client needs to
/// pin it. Dropping the fixture stops the server.
pub struct MuxrdFixture {
    addr: SocketAddr,
    cert_der: Vec<u8>,
    fingerprint_hex: String,
    server: JoinHandle<()>,
}

impl MuxrdFixture {
    /// Generate a self-signed certificate with an `127.0.0.1` IP SAN — the SAN
    /// shape `muxrd` itself serves — bind an ephemeral loopback port and start
    /// serving on the caller's runtime.
    pub async fn start() -> Self {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["127.0.0.1".to_string()])
                .expect("rcgen generates a self-signed certificate");
        let cert_der = cert.der().to_vec();
        let fingerprint_hex = leaf_fingerprint_hex(&cert_der);
        let identity = Identity::from_pem(cert.pem(), signing_key.serialize_pem());

        let incoming = TcpIncoming::bind(
            "127.0.0.1:0"
                .parse::<SocketAddr>()
                .expect("a loopback address literal"),
        )
        .expect("binds an ephemeral loopback port");
        let addr = incoming
            .local_addr()
            .expect("a bound listener has an address");

        let router = Server::builder()
            .tls_config(ServerTlsConfig::new().identity(identity))
            .expect("the fixture identity is a valid server TLS config")
            .add_service(MuxrServer::new(FixtureService));
        let server = tokio::spawn(async move {
            // The fixture is torn down by aborting this task, so a serve error
            // at shutdown is expected and carries nothing a test can act on.
            let _ = router.serve_with_incoming(incoming).await;
        });

        Self {
            addr,
            cert_der,
            fingerprint_hex,
            server,
        }
    }

    /// The loopback address the fixture is serving on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The ephemeral port the fixture is serving on.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The DER bytes of the certificate the fixture presents.
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// SHA-256 over [`Self::cert_der`], lowercase hex — the pin a client needs.
    pub fn fingerprint_hex(&self) -> &str {
        &self.fingerprint_hex
    }

    /// A client connected to this fixture under its own pin.
    pub async fn connect_pinned(&self) -> MuxrClient {
        let trust = TrustConfig::pinned_from_hex(&self.fingerprint_hex)
            .expect("the fixture prints a well-formed pin");
        MuxrClient::connect("127.0.0.1", self.port(), trust)
            .await
            .expect("the fixture accepts its own pin")
    }
}

impl Drop for MuxrdFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// `muxrd`'s bearer contract: `authorization: Bearer <session_token>`, rejected
/// as `UNAUTHENTICATED` whenever it is absent, malformed or unknown.
fn require_bearer<T>(request: &Request<T>) -> Result<(), Status> {
    let raw = request
        .metadata()
        .get("authorization")
        .ok_or_else(|| Status::unauthenticated("missing authorization metadata"))?;
    let value = raw
        .to_str()
        .map_err(|_| Status::unauthenticated("authorization is not ASCII"))?;
    let token = value
        .strip_prefix("Bearer ")
        .ok_or_else(|| Status::unauthenticated("authorization is not a bearer credential"))?;
    if token == SESSION_TOKEN {
        Ok(())
    } else {
        Err(Status::unauthenticated("unknown session token"))
    }
}

/// The status every RPC outside the fixture's answered subset returns.
fn unsupported(rpc: &str) -> Status {
    Status::unimplemented(format!("{rpc} is outside the fixture's answered subset"))
}

/// The fixture's `muxr.v1.Muxr` implementation.
struct FixtureService;

#[tonic::async_trait]
impl Muxr for FixtureService {
    async fn get_version(&self, _request: Request<Empty>) -> Result<Response<VersionInfo>, Status> {
        Ok(Response::new(VersionInfo {
            server_version: SERVER_VERSION.to_string(),
            ..Default::default()
        }))
    }

    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        if request.get_ref().auth_token != AUTH_TOKEN {
            return Err(Status::unauthenticated("unknown auth token"));
        }
        Ok(Response::new(LoginResponse {
            session_token: SESSION_TOKEN.to_string(),
            is_read_only: false,
        }))
    }

    /// Named so the trait's associated stream type resolves; the fixture serves
    /// no terminal stream.
    type AttachTerminalStream = tokio_stream::Iter<std::vec::IntoIter<Result<ServerFrame, Status>>>;

    async fn attach_terminal(
        &self,
        _request: Request<tonic::Streaming<crate::net::proto::ClientFrame>>,
    ) -> Result<Response<Self::AttachTerminalStream>, Status> {
        Err(unsupported("AttachTerminal"))
    }

    async fn list_sessions(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<SessionList>, Status> {
        require_bearer(&request)?;
        Ok(Response::new(SessionList {
            sessions: SESSION_NAMES
                .iter()
                .map(|name| SessionInfo {
                    name: (*name).to_string(),
                    ..Default::default()
                })
                .collect(),
        }))
    }

    async fn get_layout(&self, _request: Request<SessionRef>) -> Result<Response<Layout>, Status> {
        Err(unsupported("GetLayout"))
    }

    async fn write_to_pane(
        &self,
        _request: Request<WriteToPaneReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("WriteToPane"))
    }

    async fn focus_pane(
        &self,
        _request: Request<PaneTarget>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("FocusPane"))
    }

    async fn close_pane(
        &self,
        _request: Request<PaneTarget>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("ClosePane"))
    }

    async fn new_pane(&self, _request: Request<NewPaneReq>) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("NewPane"))
    }

    async fn rename_pane(
        &self,
        _request: Request<RenamePaneReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("RenamePane"))
    }

    async fn resize_pane(
        &self,
        _request: Request<ResizePaneReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("ResizePane"))
    }

    async fn toggle_pane_floating(
        &self,
        _request: Request<PaneTarget>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("TogglePaneFloating"))
    }

    async fn toggle_pane_fullscreen(
        &self,
        _request: Request<ToggleFullscreenReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("TogglePaneFullscreen"))
    }

    async fn new_tab(&self, _request: Request<NewTabReq>) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("NewTab"))
    }

    async fn close_tab(&self, _request: Request<TabTarget>) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("CloseTab"))
    }

    async fn go_to_tab(&self, _request: Request<TabTarget>) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("GoToTab"))
    }

    async fn rename_tab(
        &self,
        _request: Request<RenameTabReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("RenameTab"))
    }

    async fn scroll_pane(
        &self,
        _request: Request<ScrollReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("ScrollPane"))
    }

    async fn rename_session(
        &self,
        _request: Request<RenameSessionReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("RenameSession"))
    }

    async fn kill_session(
        &self,
        _request: Request<SessionRef>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("KillSession"))
    }

    async fn create_session(
        &self,
        _request: Request<CreateSessionReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("CreateSession"))
    }

    async fn get_spaces(
        &self,
        _request: Request<SessionRef>,
    ) -> Result<Response<SpaceList>, Status> {
        Err(unsupported("GetSpaces"))
    }

    async fn switch_space(
        &self,
        _request: Request<SwitchSpaceReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("SwitchSpace"))
    }

    async fn create_space(
        &self,
        _request: Request<CreateSpaceReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("CreateSpace"))
    }

    async fn rename_space(
        &self,
        _request: Request<RenameSpaceReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("RenameSpace"))
    }

    async fn close_space(
        &self,
        _request: Request<CloseSpaceReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("CloseSpace"))
    }

    async fn create_token(
        &self,
        _request: Request<CreateTokenReq>,
    ) -> Result<Response<TokenInfo>, Status> {
        Err(unsupported("CreateToken"))
    }

    async fn list_tokens(&self, _request: Request<Empty>) -> Result<Response<TokenList>, Status> {
        Err(unsupported("ListTokens"))
    }

    async fn revoke_token(
        &self,
        _request: Request<RevokeTokenReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("RevokeToken"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture's own credential check: the exact header shape passes, and
    /// each way of getting it wrong is `UNAUTHENTICATED`.
    #[test]
    fn require_bearer_accepts_only_the_minted_token() {
        let bearer = |value: &str| {
            let mut request = Request::new(());
            request.metadata_mut().insert(
                "authorization",
                value.parse().expect("an ASCII metadata value"),
            );
            request
        };

        assert!(require_bearer(&bearer(&format!("Bearer {SESSION_TOKEN}"))).is_ok());

        for wrong in [
            "Bearer nope".to_string(),
            format!("Basic {SESSION_TOKEN}"),
            SESSION_TOKEN.to_string(),
        ] {
            assert_eq!(
                require_bearer(&bearer(&wrong))
                    .expect_err("a malformed credential is rejected")
                    .code(),
                tonic::Code::Unauthenticated,
                "{wrong}"
            );
        }

        assert_eq!(
            require_bearer(&Request::new(()))
                .expect_err("no metadata at all is rejected")
                .code(),
            tonic::Code::Unauthenticated
        );
    }

    /// The fixture's certificate is pinnable: its printed fingerprint parses
    /// back into a pin, which is what every pinned test depends on.
    #[tokio::test]
    async fn fixture_publishes_a_pinnable_fingerprint() {
        let fixture = MuxrdFixture::start().await;
        assert!(!fixture.cert_der().is_empty());
        assert_eq!(
            TrustConfig::pinned_from_hex(fixture.fingerprint_hex()),
            Ok(
                TrustConfig::pinned_from_hex(&leaf_fingerprint_hex(fixture.cert_der()))
                    .expect("its own fingerprint")
            )
        );
        assert_ne!(fixture.port(), 0);
    }
}
