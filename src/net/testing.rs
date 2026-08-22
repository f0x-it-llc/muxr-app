//! An in-process `muxrd` stand-in: a real TLS gRPC server on a loopback port,
//! serving the real generated stubs, so the client stack is exercised
//! without the docker rig.
//!
//! `#[cfg(test)]`, not a feature flag: needs `rcgen` + tonic's
//! `server`/`router`/`transport`, all `[dev-dependencies]`-only, so it's
//! reachable from the crate's unit tests but not a `tests/*.rs` integration
//! suite (linked against the dev-dependency-free library build).
//!
//! Auth mirrors `muxrd`, checked per method via [`require_bearer`] (a tonic
//! interceptor can't see the URI to build an allowlist); unanswered RPCs
//! return `UNIMPLEMENTED`, never a plausible canned value.
//!
//! `AttachTerminal` is scripted via [`TerminalScript`] (render frame, clean
//! [`exit`](crate::net::control::CONTROL_EXIT), or mid-flight drop); the
//! one-shot workspace-verb RPCs are just recorded via [`ActionLog`].

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

// Control-event kinds come from `control`, never re-spelled as literals —
// see that module's docs on the drift two independent constants would hide.
use crate::net::client::MuxrClient;
use crate::net::control::{CONTROL_CONNECTION_ID, CONTROL_EXIT};
use crate::net::proto::muxr_server::{Muxr, MuxrServer};
use crate::net::proto::{
    ActionAck, AttachReq, ClientFrame, CloseSpaceReq, ControlEvent, CreateSessionReq,
    CreateSpaceReq, CreateTokenReq, Empty, Layout, ListPushTargetsReq, ListPushTargetsResp,
    LoginRequest, LoginResponse, MouseInput, MouseKind, NewPaneReq, NewTabReq, PaneTarget,
    RegisterPushTargetReq, RemovePushTargetReq, RenamePaneReq, RenameSessionReq, RenameSpaceReq,
    RenameTabReq, ResizePaneReq, RevokeTokenReq, ScrollDirection, ScrollReq, ServerFrame,
    SessionInfo, SessionList, SessionRef, SpaceList, SwitchSpaceReq, TabTarget,
    ToggleFullscreenReq, TokenInfo, TokenList, VersionInfo, WriteToPaneReq, client_frame,
    server_frame,
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

/// How long [`wait_until`] gives a condition before failing — long enough
/// not to flake on a loaded machine, short enough a stuck pump fails fast.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How often [`wait_until`] re-checks — a poll, since the awaited condition
/// can live on either side of the wire.
const WAIT_POLL: Duration = Duration::from_millis(1);

/// The id [`Muxr::attach_terminal`] mints for the `nth` stream it serves
/// (1-based, in attach order) and announces as its
/// [`CONTROL_CONNECTION_ID`] event.
pub fn connection_id(nth: usize) -> String {
    format!("fixture-conn-{nth}")
}

/// Poll `probe` until it holds, failing the test after [`WAIT_TIMEOUT`] with
/// `label` in the message — a fixed sleep would be either a flake or a slow
/// suite, since the two stream sides share no clock to await on.
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
    actions: Arc<ActionLog>,
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
        let actions = Arc::new(ActionLog::default());
        let router = Server::builder()
            .tls_config(ServerTlsConfig::new().identity(identity))
            .expect("the fixture identity is a valid server TLS config")
            .add_service(MuxrServer::new(FixtureService {
                terminal: Arc::clone(&terminal),
                actions: Arc::clone(&actions),
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
            actions,
            server,
        }
    }

    /// The server side of this fixture's `AttachTerminal` streams: what the
    /// client sent, and the levers a test pulls to answer it.
    pub fn terminal(&self) -> Arc<TerminalScript> {
        Arc::clone(&self.terminal)
    }

    /// The server side of this fixture's one-shot action RPCs — see
    /// [`ActionLog`].
    pub fn actions(&self) -> Arc<ActionLog> {
        Arc::clone(&self.actions)
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

/// The `ActionAck` every recording RPC below answers with — record then
/// succeed. A test scripting a refusal drives
/// [`crate::net::error::ack_result`] against a hand-built `ActionAck`
/// directly instead.
fn ack_ok() -> ActionAck {
    ActionAck {
        ok: true,
        error: String::new(),
        info: String::new(),
    }
}

/// A typed record of every one-shot workspace-verb RPC seen
/// (`CreateSpace`/`RenameSpace`/`NewTab`/…/`ScrollPane`) — unlike
/// [`TerminalScript`] there's nothing to drive, each RPC just answers once.
/// Shared with a test through [`MuxrdFixture::actions`].
#[derive(Default)]
pub struct ActionLog {
    state: Mutex<ActionState>,
}

#[derive(Default)]
struct ActionState {
    create_space: Vec<CreateSpaceReq>,
    rename_space: Vec<RenameSpaceReq>,
    new_tab: Vec<NewTabReq>,
    rename_tab: Vec<RenameTabReq>,
    new_pane: Vec<NewPaneReq>,
    rename_pane: Vec<RenamePaneReq>,
    focus_pane: Vec<PaneTarget>,
    scroll_pane: Vec<ScrollReq>,
}

impl ActionLog {
    /// Poisoning is recovered from rather than propagated — the same recovery
    /// every other fixture lock in this module takes.
    fn state(&self) -> MutexGuard<'_, ActionState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every `CreateSpace` request seen, oldest first.
    pub fn create_space_calls(&self) -> Vec<CreateSpaceReq> {
        self.state().create_space.clone()
    }

    /// Every `RenameSpace` request seen, oldest first.
    pub fn rename_space_calls(&self) -> Vec<RenameSpaceReq> {
        self.state().rename_space.clone()
    }

    /// Every `NewTab` request seen, oldest first.
    pub fn new_tab_calls(&self) -> Vec<NewTabReq> {
        self.state().new_tab.clone()
    }

    /// Every `RenameTab` request seen, oldest first.
    pub fn rename_tab_calls(&self) -> Vec<RenameTabReq> {
        self.state().rename_tab.clone()
    }

    /// Every `NewPane` request seen, oldest first.
    pub fn new_pane_calls(&self) -> Vec<NewPaneReq> {
        self.state().new_pane.clone()
    }

    /// Every `RenamePane` request seen, oldest first.
    pub fn rename_pane_calls(&self) -> Vec<RenamePaneReq> {
        self.state().rename_pane.clone()
    }

    /// Every `FocusPane` request seen, oldest first.
    pub fn focus_pane_calls(&self) -> Vec<PaneTarget> {
        self.state().focus_pane.clone()
    }

    /// Every `ScrollPane` request seen, oldest first.
    pub fn scroll_pane_calls(&self) -> Vec<ScrollReq> {
        self.state().scroll_pane.clone()
    }

    fn record_create_space(&self, request: CreateSpaceReq) {
        self.state().create_space.push(request);
    }

    fn record_rename_space(&self, request: RenameSpaceReq) {
        self.state().rename_space.push(request);
    }

    fn record_new_tab(&self, request: NewTabReq) {
        self.state().new_tab.push(request);
    }

    fn record_rename_tab(&self, request: RenameTabReq) {
        self.state().rename_tab.push(request);
    }

    fn record_new_pane(&self, request: NewPaneReq) {
        self.state().new_pane.push(request);
    }

    fn record_rename_pane(&self, request: RenamePaneReq) {
        self.state().rename_pane.push(request);
    }

    fn record_focus_pane(&self, request: PaneTarget) {
        self.state().focus_pane.push(request);
    }

    fn record_scroll_pane(&self, request: ScrollReq) {
        self.state().scroll_pane.push(request);
    }
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
    /// The resume hints the frame carried, **verbatim** — proto3 defaults
    /// (`""`/`0`) are what "no hint" looks like on the wire, so a test asserting
    /// a hint-less attach asserts exactly these.
    pub resume_space_id: String,
    pub resume_tab_id: u64,
    pub resume_pane_id: u32,
}

/// One observed `mouse` frame on an `AttachTerminal` stream.
///
/// Wire types (`MouseKind`), never a feature's own — see [`net`](self)'s
/// module docs on why nothing under `src/net` names a feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseObservation {
    pub kind: MouseKind,
    pub col: u32,
    pub row: u32,
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
    mice: Vec<MouseObservation>,
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

    /// Every `mouse` frame, in arrival order.
    pub fn mice(&self) -> Vec<MouseObservation> {
        self.state().mice.clone()
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

    /// Record one `mouse` frame; an unrecognised `kind` falls back to
    /// [`MouseKind::WheelUp`] (variant `0`), the same never-fail rule every
    /// wire `i32`-to-enum read in this app's `data` layer takes.
    fn record_mouse(&self, mouse: MouseInput) {
        self.state().mice.push(MouseObservation {
            kind: MouseKind::try_from(mouse.kind).unwrap_or(MouseKind::WheelUp),
            col: mouse.col,
            row: mouse.row,
        });
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
                Ok(Some(ClientFrame { kind: Some(client_frame::Kind::Mouse(mouse)) })) => {
                    script.record_mouse(mouse);
                }
                // A second attach frame, or an empty oneof: nothing this
                // fixture models, and never a reason to fail.
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
    actions: Arc<ActionLog>,
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
                resume_space_id: attach.resume_space_id,
                resume_tab_id: attach.resume_tab_id,
                resume_pane_id: attach.resume_pane_id,
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
        request: Request<PaneTarget>,
    ) -> Result<Response<ActionAck>, Status> {
        require_bearer(&request)?;
        self.actions.record_focus_pane(request.into_inner());
        Ok(Response::new(ack_ok()))
    }

    async fn close_pane(
        &self,
        _request: Request<PaneTarget>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("ClosePane"))
    }

    async fn new_pane(&self, request: Request<NewPaneReq>) -> Result<Response<ActionAck>, Status> {
        require_bearer(&request)?;
        self.actions.record_new_pane(request.into_inner());
        Ok(Response::new(ack_ok()))
    }

    async fn rename_pane(
        &self,
        request: Request<RenamePaneReq>,
    ) -> Result<Response<ActionAck>, Status> {
        require_bearer(&request)?;
        self.actions.record_rename_pane(request.into_inner());
        Ok(Response::new(ack_ok()))
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

    async fn new_tab(&self, request: Request<NewTabReq>) -> Result<Response<ActionAck>, Status> {
        require_bearer(&request)?;
        self.actions.record_new_tab(request.into_inner());
        Ok(Response::new(ack_ok()))
    }

    async fn close_tab(&self, _request: Request<TabTarget>) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("CloseTab"))
    }

    async fn go_to_tab(&self, _request: Request<TabTarget>) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("GoToTab"))
    }

    async fn rename_tab(
        &self,
        request: Request<RenameTabReq>,
    ) -> Result<Response<ActionAck>, Status> {
        require_bearer(&request)?;
        self.actions.record_rename_tab(request.into_inner());
        Ok(Response::new(ack_ok()))
    }

    async fn scroll_pane(
        &self,
        request: Request<ScrollReq>,
    ) -> Result<Response<ActionAck>, Status> {
        require_bearer(&request)?;
        self.actions.record_scroll_pane(request.into_inner());
        Ok(Response::new(ack_ok()))
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
        request: Request<CreateSpaceReq>,
    ) -> Result<Response<ActionAck>, Status> {
        require_bearer(&request)?;
        self.actions.record_create_space(request.into_inner());
        Ok(Response::new(ack_ok()))
    }

    async fn rename_space(
        &self,
        request: Request<RenameSpaceReq>,
    ) -> Result<Response<ActionAck>, Status> {
        require_bearer(&request)?;
        self.actions.record_rename_space(request.into_inner());
        Ok(Response::new(ack_ok()))
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

    async fn register_push_target(
        &self,
        _request: Request<RegisterPushTargetReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("RegisterPushTarget"))
    }

    async fn list_push_targets(
        &self,
        _request: Request<ListPushTargetsReq>,
    ) -> Result<Response<ListPushTargetsResp>, Status> {
        Err(unsupported("ListPushTargets"))
    }

    async fn remove_push_target(
        &self,
        _request: Request<RemovePushTargetReq>,
    ) -> Result<Response<ActionAck>, Status> {
        Err(unsupported("RemovePushTarget"))
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

    /// A one-shot workspace-verb RPC requires the same bearer credential as
    /// every other authenticated RPC, records its request, and acks.
    #[tokio::test]
    async fn a_workspace_verb_rpc_requires_auth_records_and_acks() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;

        let request = CreateSpaceReq {
            session: "zellij:dev".to_string(),
            label: "new space".to_string(),
            connection_id: "conn-1".to_string(),
        };

        let refused = client
            .authed()
            .create_space(request.clone())
            .await
            .expect_err("no token, no call");
        assert_eq!(refused.code(), tonic::Code::Unauthenticated);
        assert!(fixture.actions().create_space_calls().is_empty());

        client.set_session_token(SESSION_TOKEN);
        let ack = client
            .authed()
            .create_space(request.clone())
            .await
            .expect("an authenticated call succeeds")
            .into_inner();
        assert!(ack.ok);
        assert_eq!(fixture.actions().create_space_calls(), vec![request]);
    }

    /// The remaining seven workspace-verb RPCs each record their exact request
    /// and ack — one call per RPC, the same "record then succeed" contract
    /// [`ActionLog`] documents.
    #[tokio::test]
    async fn every_remaining_workspace_verb_rpc_records_and_acks() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;
        client.set_session_token(SESSION_TOKEN);
        let actions = fixture.actions();

        let rename_space = RenameSpaceReq {
            session: "zellij:dev".to_string(),
            space_id: "ws-1".to_string(),
            label: "renamed".to_string(),
            connection_id: "conn-1".to_string(),
        };
        assert!(
            client
                .authed()
                .rename_space(rename_space.clone())
                .await
                .expect("acked")
                .into_inner()
                .ok
        );
        assert_eq!(actions.rename_space_calls(), vec![rename_space]);

        let new_tab = NewTabReq {
            session: "zellij:dev".to_string(),
            tab_name: "build".to_string(),
        };
        assert!(
            client
                .authed()
                .new_tab(new_tab.clone())
                .await
                .expect("acked")
                .into_inner()
                .ok
        );
        assert_eq!(actions.new_tab_calls(), vec![new_tab]);

        let rename_tab = RenameTabReq {
            session: "zellij:dev".to_string(),
            tab_id: 7,
            name: "renamed".to_string(),
        };
        assert!(
            client
                .authed()
                .rename_tab(rename_tab.clone())
                .await
                .expect("acked")
                .into_inner()
                .ok
        );
        assert_eq!(actions.rename_tab_calls(), vec![rename_tab]);

        let new_pane = NewPaneReq {
            session: "zellij:dev".to_string(),
            floating: true,
            pane_name: String::new(),
        };
        assert!(
            client
                .authed()
                .new_pane(new_pane.clone())
                .await
                .expect("acked")
                .into_inner()
                .ok
        );
        assert_eq!(actions.new_pane_calls(), vec![new_pane]);

        let target = PaneTarget {
            session: "zellij:dev".to_string(),
            pane_id: 3,
            is_plugin: false,
            connection_id: "conn-1".to_string(),
        };
        let rename_pane = RenamePaneReq {
            target: Some(target.clone()),
            name: "renamed".to_string(),
        };
        assert!(
            client
                .authed()
                .rename_pane(rename_pane.clone())
                .await
                .expect("acked")
                .into_inner()
                .ok
        );
        assert_eq!(actions.rename_pane_calls(), vec![rename_pane]);

        assert!(
            client
                .authed()
                .focus_pane(target.clone())
                .await
                .expect("acked")
                .into_inner()
                .ok
        );
        assert_eq!(actions.focus_pane_calls(), vec![target.clone()]);

        let scroll = ScrollReq {
            target: Some(target),
            direction: ScrollDirection::Down as i32,
        };
        assert!(
            client
                .authed()
                .scroll_pane(scroll.clone())
                .await
                .expect("acked")
                .into_inner()
                .ok
        );
        assert_eq!(actions.scroll_pane_calls(), vec![scroll]);
    }

    /// A `mouse` frame on an `AttachTerminal` stream is recorded verbatim,
    /// alongside `input`/`resize` on the same stream.
    #[tokio::test]
    async fn attach_terminal_records_mouse_frames() {
        let fixture = MuxrdFixture::start().await;
        let client = fixture.connect_pinned().await;
        client.set_session_token(SESSION_TOKEN);
        let script = fixture.terminal();

        let (outbound, requests) = mpsc::channel(4);
        outbound
            .send(ClientFrame {
                kind: Some(client_frame::Kind::Attach(AttachReq {
                    session: "alpha".to_string(),
                    rows: 24,
                    cols: 80,
                    ..Default::default()
                })),
            })
            .await
            .expect("the channel has room");
        let mut stream = client
            .authed()
            .attach_terminal(ReceiverStream::new(requests))
            .await
            .expect("the fixture accepts the attach")
            .into_inner();
        // Drain the connection-id announcement so the stream is fully live
        // before sending a mouse frame.
        stream.message().await.expect("a control frame arrives");

        outbound
            .send(ClientFrame {
                kind: Some(client_frame::Kind::Mouse(MouseInput {
                    kind: MouseKind::WheelDown as i32,
                    col: 12,
                    row: 5,
                })),
            })
            .await
            .expect("the channel has room");

        wait_until("the mouse frame is recorded", || script.mice().len() == 1).await;
        assert_eq!(
            script.mice(),
            vec![MouseObservation {
                kind: MouseKind::WheelDown,
                col: 12,
                row: 5,
            }]
        );
    }
}
