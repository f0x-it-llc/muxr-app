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
//!
//! **`AttachTerminal` is scripted, not canned.** The one streaming RPC needs a
//! server side a test can *drive* — [`TerminalScript`] records what the client
//! sent (attach handshakes, input, resizes) and pushes what the server should
//! say back (a render frame, a clean [`exit`](CONTROL_EXIT), or a mid-flight
//! drop for a reconnect test). Every other behavior is fixed: a stream must
//! open with an `attach` frame, it answers with a [`connection_id`](CONTROL_CONNECTION_ID)
//! control event, and it echoes received input back as render frames, which is
//! the smallest stand-in for a PTY that makes "what I typed came back, in
//! order" assertable.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rcgen::{CertifiedKey, generate_simple_self_signed};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Identity, Server, ServerTlsConfig};
use tonic::{Request, Response, Status};

// The two control-event kinds the client parses. Imported rather than
// re-spelled as literals here: the fixture has to say exactly what the feed
// listens for, and two independent string constants is precisely the drift a
// green test would then hide.
use crate::features::terminal::data::feeds::grpc_feed::{CONTROL_CONNECTION_ID, CONTROL_EXIT};
use crate::net::client::MuxrClient;
use crate::net::proto::muxr_server::{Muxr, MuxrServer};
use crate::net::proto::{
    ActionAck, ClientFrame, CloseSpaceReq, ControlEvent, CreateSessionReq, CreateSpaceReq,
    CreateTokenReq, Empty, Layout, LoginRequest, LoginResponse, NewPaneReq, NewTabReq, PaneTarget,
    RenamePaneReq, RenameSessionReq, RenameSpaceReq, RenameTabReq, ResizePaneReq, RevokeTokenReq,
    ScrollReq, ServerFrame, SessionInfo, SessionList, SessionRef, SpaceList, SwitchSpaceReq,
    TabTarget, ToggleFullscreenReq, TokenInfo, TokenList, VersionInfo, WriteToPaneReq,
    client_frame, server_frame,
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

/// Frames one scripted `AttachTerminal` stream buffers in each direction.
const ATTACH_CAPACITY: usize = 64;

/// How long [`wait_until`] gives a condition before failing the test. Long
/// enough that a loaded machine never flakes, short enough that a genuinely
/// stuck pump fails rather than hangs the suite.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How often [`wait_until`] re-checks. A poll rather than a notification
/// because the conditions worth waiting on live on both sides of the wire.
const WAIT_POLL: Duration = Duration::from_millis(1);

/// The id [`Muxr::attach_terminal`] mints for the `nth` stream it serves
/// (1-based, in attach order) and announces as its
/// [`CONTROL_CONNECTION_ID`] event.
pub fn connection_id(nth: usize) -> String {
    format!("fixture-conn-{nth}")
}

/// Poll `probe` until it holds, failing the test after [`WAIT_TIMEOUT`] with
/// `label` in the message.
///
/// The fixture's answer to "wait for the other side to catch up": a fixed
/// sleep would be either a flake or a slow suite, and the two sides of a
/// stream have no shared clock to await on.
pub async fn wait_until(label: &str, mut probe: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    while !probe() {
        assert!(
            Instant::now() < deadline,
            "timed out after {WAIT_TIMEOUT:?} waiting until {label}"
        );
        tokio::time::sleep(WAIT_POLL).await;
    }
}

/// A running fixture server plus the certificate material a client needs to
/// pin it. Dropping the fixture stops the server.
pub struct MuxrdFixture {
    addr: SocketAddr,
    cert_der: Vec<u8>,
    fingerprint_hex: String,
    terminal: Arc<TerminalScript>,
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

        let terminal = Arc::new(TerminalScript::default());
        let router = Server::builder()
            .tls_config(ServerTlsConfig::new().identity(identity))
            .expect("the fixture identity is a valid server TLS config")
            .add_service(MuxrServer::new(FixtureService {
                terminal: Arc::clone(&terminal),
            }));
        let server = tokio::spawn(async move {
            // The fixture is torn down by aborting this task, so a serve error
            // at shutdown is expected and carries nothing a test can act on.
            let _ = router.serve_with_incoming(incoming).await;
        });

        Self {
            addr,
            cert_der,
            fingerprint_hex,
            terminal,
            server,
        }
    }

    /// The server side of this fixture's `AttachTerminal` streams: what the
    /// client sent, and the levers a test pulls to answer it.
    pub fn terminal(&self) -> Arc<TerminalScript> {
        Arc::clone(&self.terminal)
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

/// One observed `attach` handshake — the mandatory first frame of every
/// `AttachTerminal` stream, including every reconnect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachObservation {
    /// The session name the client asked for.
    pub session: String,
    /// The grid it attached at, column-major (the wire message is
    /// `rows`-first; the app counts `cols` x `rows`).
    pub cols: u16,
    pub rows: u16,
}

/// What a test tells the currently-attached stream to do.
enum ServerCommand {
    /// Send these bytes as a `render` frame.
    Render(Vec<u8>),
    /// Send a [`CONTROL_EXIT`] control event and end cleanly.
    Exit,
    /// End the stream where it stands, saying nothing.
    Kill,
}

/// The record-and-drive half of the fixture's `AttachTerminal` support (see
/// the [module docs](self)).
///
/// Shared with the test through [`MuxrdFixture::terminal`]; every accessor
/// hands back a snapshot rather than a borrow, so a test never holds the
/// fixture's lock across an `await`.
#[derive(Default)]
pub struct TerminalScript {
    state: Mutex<ScriptState>,
}

#[derive(Default)]
struct ScriptState {
    attaches: Vec<AttachObservation>,
    inputs: Vec<Vec<u8>>,
    resizes: Vec<(u16, u16)>,
    /// The current stream's command channel, replaced on every attach. `None`
    /// before the first one.
    commands: Option<mpsc::Sender<ServerCommand>>,
}

impl TerminalScript {
    /// Poisoning is recovered from rather than propagated: a panic in one test
    /// assertion must not turn every later one into a lock error that hides it.
    fn state(&self) -> MutexGuard<'_, ScriptState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every `attach` handshake seen so far, oldest first.
    pub fn attaches(&self) -> Vec<AttachObservation> {
        self.state().attaches.clone()
    }

    /// How many streams have handshaked. The reconnect tests' clock.
    pub fn attach_count(&self) -> usize {
        self.state().attaches.len()
    }

    /// Every `input` frame's bytes, in arrival order.
    pub fn inputs(&self) -> Vec<Vec<u8>> {
        self.state().inputs.clone()
    }

    /// Every `resize` frame's `(cols, rows)` — column-major, matching the app
    /// (the wire message is `rows`-first).
    pub fn resizes(&self) -> Vec<(u16, u16)> {
        self.state().resizes.clone()
    }

    /// Send `bytes` to the client as a `render` frame.
    pub fn push_render(&self, bytes: Vec<u8>) {
        self.command(ServerCommand::Render(bytes));
    }

    /// Close the current stream the clean way: a [`CONTROL_EXIT`] event, then
    /// the end of the stream.
    pub fn send_exit(&self) {
        self.command(ServerCommand::Exit);
    }

    /// Drop the current stream mid-flight, saying nothing — the client sees
    /// its `AttachTerminal` end and takes its backoff path.
    pub fn kill(&self) {
        self.command(ServerCommand::Kill);
    }

    /// Record a handshake and take over the command channel, returning this
    /// stream's 1-based index. One locked step, so an
    /// [`attach_count`](Self::attach_count) a test just observed always names
    /// the stream a following [`kill`](Self::kill) reaches.
    fn arm(&self, attach: AttachObservation, commands: mpsc::Sender<ServerCommand>) -> usize {
        let mut state = self.state();
        state.attaches.push(attach);
        state.commands = Some(commands);
        state.attaches.len()
    }

    /// Queue a command for the current stream. A command with no stream (or
    /// one whose task has already returned) is dropped: a test that means to
    /// drive a stream waits for its attach first.
    fn command(&self, command: ServerCommand) {
        if let Some(commands) = self.state().commands.as_ref() {
            let _ = commands.try_send(command);
        }
    }

    fn record_input(&self, bytes: Vec<u8>) {
        self.state().inputs.push(bytes);
    }

    fn record_resize(&self, cols: u16, rows: u16) {
        self.state().resizes.push((cols, rows));
    }
}

/// Build a `render` frame.
fn render_frame(bytes: Vec<u8>) -> ServerFrame {
    ServerFrame {
        kind: Some(server_frame::Kind::Render(bytes)),
    }
}

/// Build a `control` frame.
fn control_frame(kind: &str, payload: String) -> ServerFrame {
    ServerFrame {
        kind: Some(server_frame::Kind::Control(ControlEvent {
            kind: kind.to_string(),
            payload,
        })),
    }
}

/// A `u32` cell count off the wire, clamped into the `u16` the app counts
/// cells in. A grid wider than 65535 columns is not a case worth modelling.
fn cells(value: u32) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// One attached stream, for its whole life: announce the connection id, then
/// serve whatever the test scripts and whatever the client sends, until either
/// side is done.
///
/// Sole owner of the response channel's sender, which is what makes "drop the
/// stream" expressible at all — returning from here ends the client's stream.
async fn serve_attach(
    index: usize,
    mut inbound: tonic::Streaming<ClientFrame>,
    frames: mpsc::Sender<Result<ServerFrame, Status>>,
    mut commands: mpsc::Receiver<ServerCommand>,
    script: Arc<TerminalScript>,
) {
    let announce = control_frame(CONTROL_CONNECTION_ID, connection_id(index));
    if frames.send(Ok(announce)).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(ServerCommand::Render(bytes)) => {
                    if frames.send(Ok(render_frame(bytes))).await.is_err() {
                        return;
                    }
                }
                Some(ServerCommand::Exit) => {
                    let _ = frames
                        .send(Ok(control_frame(CONTROL_EXIT, String::new())))
                        .await;
                    return;
                }
                // A dropped command channel means a newer stream took over;
                // this one is as dead as a killed one.
                Some(ServerCommand::Kill) | None => return,
            },
            frame = inbound.message() => match frame {
                Ok(Some(ClientFrame { kind: Some(client_frame::Kind::Input(bytes)) })) => {
                    script.record_input(bytes.clone());
                    // A PTY echoes what you type; so does this.
                    if frames.send(Ok(render_frame(bytes))).await.is_err() {
                        return;
                    }
                }
                Ok(Some(ClientFrame { kind: Some(client_frame::Kind::Resize(resize)) })) => {
                    script.record_resize(cells(resize.cols), cells(resize.rows));
                }
                // A second attach frame, a mouse event, or an empty oneof:
                // nothing this fixture models, and never a reason to fail.
                Ok(Some(_)) => {}
                // The client hung up, or the transport did.
                Ok(None) | Err(_) => return,
            },
        }
    }
}

/// The fixture's `muxr.v1.Muxr` implementation.
struct FixtureService {
    terminal: Arc<TerminalScript>,
}

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

    /// The scripted stream [`serve_attach`] writes into — concrete rather than
    /// boxed, since one channel is the whole response side.
    type AttachTerminalStream = ReceiverStream<Result<ServerFrame, Status>>;

    /// The one streaming RPC. Refuses anything but an `attach` first frame —
    /// `muxrd`'s own contract, and the fixture must not let a client that
    /// forgot it pass.
    async fn attach_terminal(
        &self,
        request: Request<tonic::Streaming<ClientFrame>>,
    ) -> Result<Response<Self::AttachTerminalStream>, Status> {
        require_bearer(&request)?;
        let mut inbound = request.into_inner();
        let first = inbound
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("an AttachTerminal stream sent no frames"))?;
        let Some(client_frame::Kind::Attach(attach)) = first.kind else {
            return Err(Status::invalid_argument(
                "the first frame on an AttachTerminal stream must be attach",
            ));
        };

        let (frames_tx, frames_rx) = mpsc::channel(ATTACH_CAPACITY);
        let (commands_tx, commands_rx) = mpsc::channel(ATTACH_CAPACITY);
        let index = self.terminal.arm(
            AttachObservation {
                session: attach.session,
                cols: cells(attach.cols),
                rows: cells(attach.rows),
            },
            commands_tx,
        );
        tokio::spawn(serve_attach(
            index,
            inbound,
            frames_tx,
            commands_rx,
            Arc::clone(&self.terminal),
        ));

        Ok(Response::new(ReceiverStream::new(frames_rx)))
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
