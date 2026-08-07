//! [`GrpcTerminalFeed`] — the live `muxrd` byte source: one `AttachTerminal`
//! bidirectional stream behind this feature's own
//! [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed)
//! port, with a bounded reconnect schedule.
//!
//! Constructed by `presentation::controller`'s `LiveSession::enter` for a
//! real attached session — the route carries `?session=<id>` and the app
//! holds a live client; [`DemoFeed`](super::demo_feed::DemoFeed) is what the
//! overflow menu's "Demo session" row (and any session `LiveSession` can't
//! attach to) replays instead. This module is the transport and its
//! fixture-backed tests.
//!
//! # The wire contract
//!
//! `AttachTerminal(stream ClientFrame) -> stream ServerFrame`, from
//! `proto/muxr.proto`:
//!
//! | Direction | Frame | When |
//! |---|---|---|
//! | client → server | `attach: AttachReq{session, rows, cols}` | **mandatory first frame** of every stream, including every reconnect |
//! | client → server | `input: bytes` | a keystroke, raw |
//! | client → server | `resize: Resize{rows, cols}` | the grid changed shape |
//! | server → client | `render: bytes` | raw ANSI for the emulator |
//! | server → client | `control: ControlEvent{kind, payload}` | out of band |
//!
//! **Input goes on the stream, never through `WriteToPane`.** That RPC is the
//! one-shot pane-targeted write other features use; a keystroke belongs to the
//! attached stream, where it keeps its order relative to the resizes around it.
//!
//! **The attach frame replays the *current* grid, not the one the session
//! started on.** [`TerminalFeed::resize`] caches `cols`/`rows` and — when a
//! stream is up — sends a `resize` frame; the cached pair is what the next
//! attach carries, so a reconnect after a rotation attaches at the shape the
//! user is actually looking at.
//!
//! **Two control kinds are understood** ([`CONTROL_CONNECTION_ID`],
//! [`CONTROL_EXIT`]); every other kind is *ignored*, never an error, so a
//! newer `muxrd` can add one without breaking this client. The connection id
//! is captured for the caller ([`GrpcTerminalFeed::connection_id`]) — the
//! per-connection routing token `PaneTarget`-style requests echo back — and is
//! never surfaced as terminal output. `exit` is a clean server-side close:
//! status [`FeedStatus::Closed`], and **no** reconnect.
//!
//! # Reconnect: a fixed, jitter-free schedule
//!
//! [`RECONNECT_BACKOFF`] — 1, 2, 4, 8, 16 seconds, no jitter — matching the
//! Flutter client this app replaces. A stream that errors (any non-auth
//! cause) or ends takes the next step in that schedule; the step counter
//! resets on the **first render frame after a (re)connect** (a stream that
//! opens and delivers nothing has not proved anything) and on an explicit
//! [`reset_backoff`](GrpcTerminalFeed::reset_backoff), which is the manual
//! "retry" affordance. Running off the end of the schedule is
//! [`FeedStatus::Disconnected`]: the loop stops attempting and parks until
//! `reset_backoff` re-arms it.
//!
//! `UNAUTHENTICATED` short-circuits all of that. A credential that the server
//! refuses will refuse the next four attempts too, so the loop stops at once,
//! invokes the constructor-injected auth-expired callback (the app wires it to
//! the connection controller's session-expired hook) and leaves the feed
//! [`FeedStatus::Disconnected`] and closed.
//!
//! # Threading: a shared byte buffer, and not one signal
//!
//! The 30 Hz UI pump (`presentation::term`'s `run_feed`) calls
//! [`drain_output`](TerminalFeed::drain_output) on the **UI thread**, so this
//! feed's inbound side is a plain shared buffer that a background pump task
//! fills. That task writes no signal and touches no reactive state at all;
//! coalescing repaints stays the UI pump's job, exactly as
//! `presentation::term`'s module docs require. Status is shared the same way,
//! behind the same lock, and read by polling
//! [`status`](GrpcTerminalFeed::status) — no signal in this layer either.
//!
//! **No synchronous echo.** [`DemoFeed`](super::demo_feed::DemoFeed) echoes
//! typed bytes back inside the same dispatch, and `InputSeam::dispatch` drains
//! that echo synchronously. A real server echoes through the render stream
//! instead, one network round trip later: [`send_input`](TerminalFeed::send_input)
//! queues a frame and returns, and an immediate `drain_output` legitimately
//! reports `0`. That synchronous drain is an *optimization* for a feed that
//! can answer instantly, never a contract this port carries — see
//! `presentation::term`'s "The one exception: a synchronous echo drain".
//!
//! # Why the loop is spelled by hand
//!
//! Production `tokio` here carries the `sync` feature only — `Cargo.toml`'s
//! own comment makes that omission a contract, because the process runtime is
//! frust's. So there is no `tokio::time` to sleep on and no `tokio::select!`
//! to race with: the backoff nap goes through [`frust::spawn_blocking`] (the
//! same off-UI-thread nap `presentation::term`'s pump takes) and the two-way
//! race is [`race`], a dozen lines of [`std::future::poll_fn`]. Together with
//! [`frust::spawn`] for the pump itself, those are this file's only reaches
//! for `frust::` in an otherwise framework-free data layer — the same
//! sanctioned, documented exception `settings`' `PrefsRepository` takes for
//! its own off-thread write.
//!
//! # Test seam
//!
//! [`RECONNECT_BACKOFF`] **is** the contract, so it is a constant rather than
//! a knob: no caller may shorten what a user experiences. The schedule and the
//! nap are nonetheless constructor arguments of a `#[cfg(test)]`-only
//! `detached` constructor, which also hands the pump future back instead of
//! spawning it. Both halves are forced: a `#[tokio::test]` cannot wait out 31
//! real seconds, and without the shell's reactive runtime `frust::spawn` drops
//! the pump on the floor while `frust::spawn_blocking` panics outright. Same
//! shape, and same reason, as `DemoFeed::manual`'s injected clock.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;

use crate::features::terminal::domain::feed::TerminalFeed;
use crate::net::MuxrClient;
use crate::net::proto::{AttachReq, ClientFrame, Resize, ServerFrame, client_frame, server_frame};

/// The control event carrying this stream's server-minted connection id — the
/// per-connection routing token `muxrd` expects echoed back on a later
/// pane/tab/space request (`proto/muxr.proto`'s own "echoed from
/// AttachTerminal connection_id" fields).
pub const CONTROL_CONNECTION_ID: &str = "connection_id";

/// The control event announcing a clean server-side close. Terminal: the feed
/// goes [`FeedStatus::Closed`] and never reconnects.
pub const CONTROL_EXIT: &str = "exit";

/// The reconnect schedule: 1, 2, 4, 8, 16 seconds, **no jitter**, matching the
/// Flutter client's own `terminal_session` timings. See the [module
/// docs](self) — this constant is the contract; nothing in the app overrides
/// it.
pub const RECONNECT_BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

/// Frames the outbound channel buffers before a send is dropped.
///
/// Generous on purpose: the outbound direction is keystrokes and resizes at
/// human rate, so filling this means the transport is wedged, and the only
/// alternatives at that point are dropping a frame or blocking the UI thread
/// on a dead socket. Dropping is counted
/// ([`GrpcTerminalFeed::dropped_frames`]), never silent.
const OUTBOUND_CAPACITY: usize = 256;

/// Where a feed's session currently stands. Plain shared state, polled by the
/// screen that owns the feed — deliberately not a signal (see the [module
/// docs](self)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeedStatus {
    /// Constructed; the pump has not opened its first stream yet.
    #[default]
    Idle,
    /// An attach attempt is in flight, or the loop is waiting out a backoff
    /// step.
    Reconnecting,
    /// The stream is open and delivering.
    Connected,
    /// No further attempt will be made on its own: the schedule ran out, or
    /// the credential expired. The screen offers a retry, which is
    /// [`GrpcTerminalFeed::reset_backoff`].
    Disconnected,
    /// The server said [`CONTROL_EXIT`]. Terminal — nothing will arrive again.
    Closed,
}

/// A live `muxrd` terminal session behind the
/// [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed)
/// port. See the [module docs](self).
///
/// Dropping it stops the pump and closes the request stream, so the server
/// sees the attach end rather than waiting out a keepalive.
pub struct GrpcTerminalFeed {
    shared: Arc<Shared>,
    /// The deque [`TerminalFeed::drain_output`] swaps the shared inbound queue
    /// out into, so the sink runs with no lock held and the pump never waits
    /// on an emulator parse. Always empty between drains — that is what makes
    /// the swap (rather than a copy) correct, and it keeps its capacity, so a
    /// wake allocates nothing but the byte buffers the wire handed over.
    scratch: VecDeque<Vec<u8>>,
}

impl GrpcTerminalFeed {
    /// Attach to `session` at a `cols` x `rows` grid and start pumping.
    ///
    /// `on_auth_expired` fires at most once, on the first `UNAUTHENTICATED`
    /// this session sees (see the [module docs](self)); the caller wires it to
    /// whatever owns the app's credential.
    ///
    /// The pump runs on the background reactive runtime, so this must be
    /// called with that runtime up — i.e. from app code, never from a bare
    /// `#[tokio::test]` (which is what `detached` is for).
    pub fn attach(
        client: MuxrClient,
        session: impl Into<String>,
        cols: u16,
        rows: u16,
        on_auth_expired: Box<dyn Fn() + Send + Sync>,
    ) -> Self {
        let (feed, pump) = Self::build(Pump {
            client,
            session: session.into(),
            backoff: RECONNECT_BACKOFF.to_vec(),
            waiter: Waiter::Frust,
            on_auth_expired,
            shared: Arc::new(Shared::new(cols, rows)),
        });
        frust::spawn(pump);
        feed
    }

    /// Where this session stands right now.
    pub fn status(&self) -> FeedStatus {
        self.shared.state().status
    }

    /// The server-minted connection id from this stream's
    /// [`CONTROL_CONNECTION_ID`] event, once one has arrived.
    ///
    /// Survives a reconnect only in the sense that the *newest* id wins: a
    /// re-attached stream mints its own, and a caller routing by it must read
    /// it again rather than cache it across a drop.
    pub fn connection_id(&self) -> Option<String> {
        self.shared.state().connection_id.clone()
    }

    /// Reset the backoff to its first step and, if the loop has parked on an
    /// exhausted schedule, re-arm it — the manual "retry" the screen offers on
    /// [`FeedStatus::Disconnected`].
    ///
    /// A no-op once the session has ended for good (a server `exit`, or an
    /// expired credential): there is nothing left to retry, and the pump is
    /// gone.
    pub fn reset_backoff(&self) {
        self.shared.retry.store(true, Ordering::Release);
        self.shared.wake.notify_one();
    }

    /// Stop the pump and close the request stream. Idempotent; [`Drop`] calls
    /// it.
    ///
    /// Forceful: the outbound sender is dropped immediately rather than
    /// drained, so anything queued behind a wedged transport goes with it. A
    /// graceful variant would have to wait on a stream that, by construction,
    /// never completes on its own.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.set_outbound(None);
        self.shared.wake.notify_one();
    }

    /// The backoff step the next drop would wait out — a diagnostic/test
    /// accessor for the schedule the [module docs](self) pin. Never read by
    /// production code.
    pub fn backoff_step(&self) -> usize {
        self.shared.state().backoff_step
    }

    /// Outbound frames dropped because the session was down or its channel
    /// full (see [`OUTBOUND_CAPACITY`]) — a diagnostic counter, never read by
    /// production code.
    pub fn dropped_frames(&self) -> u64 {
        self.shared.state().dropped_frames
    }

    /// Render frames received but not yet drained — a diagnostic/test
    /// accessor. Never read by production code, which drains instead of
    /// asking.
    pub fn pending_writes(&self) -> usize {
        self.shared.state().inbound.len()
    }

    /// The feed and its pump future, unspawned. The one construction path;
    /// [`attach`](Self::attach) is this plus [`frust::spawn`].
    fn build(pump: Pump) -> (Self, impl Future<Output = ()> + Send + use<>) {
        let feed = Self {
            shared: Arc::clone(&pump.shared),
            scratch: VecDeque::new(),
        };
        (feed, run_pump(pump))
    }

    /// [`attach`](Self::attach) with the schedule and the nap opened up, and
    /// the pump handed back rather than spawned — see the [module docs](self)'
    /// test seam.
    #[cfg(test)]
    fn detached(
        client: MuxrClient,
        session: &str,
        cols: u16,
        rows: u16,
        on_auth_expired: Box<dyn Fn() + Send + Sync>,
        backoff: &[Duration],
    ) -> (Self, impl Future<Output = ()> + Send + use<>) {
        Self::build(Pump {
            client,
            session: session.to_string(),
            backoff: backoff.to_vec(),
            waiter: Waiter::Tokio,
            on_auth_expired,
            shared: Arc::new(Shared::new(cols, rows)),
        })
    }
}

impl TerminalFeed for GrpcTerminalFeed {
    /// Hand over every render frame received since the last call, in arrival
    /// order, exactly once.
    ///
    /// One write per received frame: this feed coalesces nothing, because the
    /// pump above it already does (`presentation::term`'s one-generation-per-wake
    /// rule), and splitting or merging ANSI here would only hide what the
    /// server actually sent.
    fn drain_output(&mut self, sink: &mut dyn FnMut(&[u8])) -> usize {
        {
            let mut state = self.shared.state();
            if state.inbound.is_empty() {
                return 0;
            }
            std::mem::swap(&mut state.inbound, &mut self.scratch);
        }
        let writes = self.scratch.len();
        for chunk in &self.scratch {
            sink(chunk);
        }
        self.scratch.clear();
        writes
    }

    /// Queue `bytes` as one `input` frame. Returns immediately — the echo
    /// comes back through the render stream, not from here (see the [module
    /// docs](self)).
    ///
    /// Bytes typed while the session is down are dropped and counted
    /// ([`GrpcTerminalFeed::dropped_frames`]): a reconnect replays the grid,
    /// never a keystroke the server never saw.
    fn send_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.shared.send(ClientFrame {
            kind: Some(client_frame::Kind::Input(bytes.to_vec())),
        });
    }

    /// Cache the new grid and, if a stream is up, tell the server about it.
    ///
    /// The cache is the load-bearing half: it is what the *next* attach frame
    /// carries, so a resize that happened while disconnected still lands. An
    /// unchanged pair sends nothing — the widget calls this on every layout,
    /// and most layouts are the same shape as the last.
    fn resize(&mut self, cols: u16, rows: u16) {
        {
            let mut state = self.shared.state();
            if (state.cols, state.rows) == (cols, rows) {
                return;
            }
            state.cols = cols;
            state.rows = rows;
        }
        self.shared.send(ClientFrame {
            kind: Some(client_frame::Kind::Resize(Resize {
                rows: u32::from(rows),
                cols: u32::from(cols),
            })),
        });
    }

    /// Whether the pump can still deliver anything.
    ///
    /// `false` only once the session has ended for good — a server
    /// [`CONTROL_EXIT`], an expired credential, or a
    /// [`shutdown`](GrpcTerminalFeed::shutdown) — **and** the last frames have
    /// been drained. A [`FeedStatus::Disconnected`] feed is still open: its
    /// pump is parked, not gone, and
    /// [`reset_backoff`](GrpcTerminalFeed::reset_backoff) re-arms the very
    /// same session. Anything else keeps the trait's own default answer, for
    /// the reason `DemoFeed::is_open` records: a live session can speak at any
    /// time, with nothing observable queued in advance.
    fn is_open(&self) -> bool {
        let state = self.shared.state();
        !state.finished || !state.inbound.is_empty()
    }
}

impl Drop for GrpcTerminalFeed {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Everything the pump owns for the life of a session.
struct Pump {
    client: MuxrClient,
    session: String,
    backoff: Vec<Duration>,
    waiter: Waiter,
    on_auth_expired: Box<dyn Fn() + Send + Sync>,
    shared: Arc<Shared>,
}

/// The state the UI thread and the pump task share. Everything mutable is
/// behind the one `Mutex`; the two flags beside it are what a `notify_one`
/// wake means.
struct Shared {
    state: Mutex<State>,
    /// The pump's one wake source: a backoff nap, a parked-on-exhaustion wait
    /// and a live stream read all race against it. `notify_one` (rather than
    /// `notify_waiters`) stores a permit when the pump is between waits, so a
    /// retry or a shutdown raised at the wrong instant is never lost.
    wake: Notify,
    shutdown: AtomicBool,
    retry: AtomicBool,
}

struct State {
    /// Render payloads awaiting the next drain, in arrival order.
    inbound: VecDeque<Vec<u8>>,
    status: FeedStatus,
    connection_id: Option<String>,
    cols: u16,
    rows: u16,
    /// The current stream's request half, replaced on every attach and cleared
    /// whenever no stream is up.
    outbound: Option<mpsc::Sender<ClientFrame>>,
    /// Set when the pump returns for good — the input to
    /// [`TerminalFeed::is_open`].
    finished: bool,
    backoff_step: usize,
    dropped_frames: u64,
}

/// What a `wake` notification turned out to mean.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wake {
    /// Nothing to act on — a stale permit, or the nap simply ran out.
    Elapsed,
    /// A manual retry: attempt now, from the first backoff step.
    Retry,
    /// Tear down.
    Shutdown,
}

/// How one attach attempt ended.
enum Attempt {
    /// The stream errored or ended: take the next backoff step.
    Dropped,
    /// The server sent [`CONTROL_EXIT`].
    Closed,
    /// `UNAUTHENTICATED`, on open or mid-stream.
    AuthExpired,
    /// [`GrpcTerminalFeed::shutdown`] fired.
    Shutdown,
}

/// How the pump waits out a backoff step.
///
/// Two arms rather than an injected closure: production must not carry a
/// boxed-future indirection for a seam only a test uses. See the [module
/// docs](self)' test seam for why one arm cannot serve both.
enum Waiter {
    /// The app: a nap on the blocking pool, the same shape
    /// `presentation::term`'s own pump takes, since production `tokio` has no
    /// `time` feature.
    Frust,
    /// A `#[tokio::test]`, whose runtime has a timer and whose process has no
    /// `ReactiveRuntime` for `frust::spawn_blocking` to find.
    #[cfg(test)]
    Tokio,
}

impl Waiter {
    async fn sleep(&self, delay: Duration) {
        match self {
            // A raced-away nap leaves its pool thread sleeping out the
            // remainder; harmless, and the alternative (slicing the nap into
            // polls) is a 16-second-long poll loop for one cancellation.
            Waiter::Frust => {
                let _ = frust::spawn_blocking(move || std::thread::sleep(delay)).await;
            }
            #[cfg(test)]
            Waiter::Tokio => tokio::time::sleep(delay).await,
        }
    }
}

impl Shared {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            state: Mutex::new(State {
                inbound: VecDeque::new(),
                status: FeedStatus::Idle,
                connection_id: None,
                cols,
                rows,
                outbound: None,
                finished: false,
                backoff_step: 0,
                dropped_frames: 0,
            }),
            wake: Notify::new(),
            shutdown: AtomicBool::new(false),
            retry: AtomicBool::new(false),
        }
    }

    /// The shared state. Poisoning is recovered from rather than propagated —
    /// a panic elsewhere must not make a live terminal permanently unreadable.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_status(&self, status: FeedStatus) {
        self.state().status = status;
    }

    fn dims(&self) -> (u16, u16) {
        let state = self.state();
        (state.cols, state.rows)
    }

    fn set_outbound(&self, outbound: Option<mpsc::Sender<ClientFrame>>) {
        self.state().outbound = outbound;
    }

    /// Queue one frame for the current stream, dropping (and counting) it if
    /// there is no stream or its channel is full.
    fn send(&self, frame: ClientFrame) {
        let mut state = self.state();
        let queued = match state.outbound.as_ref() {
            // `try_send`, never `send`: this runs on the UI thread, where an
            // await on a wedged socket would freeze the app.
            Some(outbound) => outbound.try_send(frame).is_ok(),
            None => false,
        };
        if !queued {
            state.dropped_frames += 1;
        }
    }

    fn push_render(&self, bytes: Vec<u8>) {
        self.state().inbound.push_back(bytes);
    }

    fn set_connection_id(&self, id: String) {
        self.state().connection_id = Some(id);
    }

    fn reset_step(&self) {
        self.state().backoff_step = 0;
    }

    /// Read what a wake meant, clearing the retry flag it consumed.
    fn take_wake(&self) -> Wake {
        if self.shutdown.load(Ordering::Acquire) {
            Wake::Shutdown
        } else if self.retry.swap(false, Ordering::AcqRel) {
            self.reset_step();
            Wake::Retry
        } else {
            Wake::Elapsed
        }
    }

    /// The pump's last act: release the request stream and mark the session
    /// over, so [`TerminalFeed::is_open`] stops the UI pump too.
    ///
    /// A torn-down feed also stops claiming to be connected — but a `Closed`
    /// one keeps its own, more specific answer, since the server said why.
    fn retire(&self) {
        let mut state = self.state();
        if self.shutdown.load(Ordering::Acquire) && state.status != FeedStatus::Closed {
            state.status = FeedStatus::Disconnected;
        }
        state.outbound = None;
        state.finished = true;
    }
}

/// The session loop: attach, pump the stream, and — unless the session ended
/// for good — wait out a backoff step and attach again.
async fn run_pump(pump: Pump) {
    let Pump {
        client,
        session,
        backoff,
        waiter,
        on_auth_expired,
        shared,
    } = pump;

    while !shared.shutdown.load(Ordering::Acquire) {
        shared.set_status(FeedStatus::Reconnecting);
        match attach(&client, &session, &shared).await {
            Attempt::Dropped => {}
            Attempt::Closed | Attempt::Shutdown => break,
            Attempt::AuthExpired => {
                // One credential the server refuses will be refused four more
                // times; the session is over until something re-authenticates.
                shared.set_status(FeedStatus::Disconnected);
                on_auth_expired();
                break;
            }
        }

        let step = shared.state().backoff_step;
        let wake = match backoff.get(step) {
            Some(delay) => {
                shared.state().backoff_step = step + 1;
                wait_backoff(&shared, &waiter, *delay).await
            }
            None => {
                // Out of schedule: stop attempting, and wait for the screen's
                // retry rather than hammering a server that is plainly down.
                shared.set_status(FeedStatus::Disconnected);
                park(&shared).await
            }
        };
        if wake == Wake::Shutdown {
            break;
        }
    }

    shared.retire();
}

/// One attach attempt: open the stream with its mandatory first frame, then
/// pump it until it ends.
async fn attach(client: &MuxrClient, session: &str, shared: &Shared) -> Attempt {
    let (outbound, requests) = mpsc::channel(OUTBOUND_CAPACITY);
    let (cols, rows) = shared.dims();
    if outbound
        .try_send(ClientFrame {
            kind: Some(client_frame::Kind::Attach(AttachReq {
                session: session.to_string(),
                rows: u32::from(rows),
                cols: u32::from(cols),
            })),
        })
        .is_err()
    {
        // A channel this code just built with room for hundreds; unreachable
        // in practice, and a dropped attach frame is a stream muxrd would
        // reject anyway, so treat it as a failed attempt rather than panic.
        return Attempt::Dropped;
    }
    // Published only after the attach frame is queued, so a keystroke racing
    // the handshake can never overtake it into the stream.
    shared.set_outbound(Some(outbound));

    // A fresh stub per attempt: the client's channel may have been rebuilt
    // since the last one, and a stub holds whichever channel it was minted
    // from.
    let mut stub = client.authed();
    let stream = match stub.attach_terminal(ReceiverStream::new(requests)).await {
        Ok(response) => response.into_inner(),
        Err(status) => {
            shared.set_outbound(None);
            return classify(&status);
        }
    };

    shared.set_status(FeedStatus::Connected);
    let outcome = pump_stream(stream, shared).await;
    shared.set_outbound(None);
    outcome
}

/// Read server frames until the stream ends, the server says
/// [`CONTROL_EXIT`], or a shutdown races in.
async fn pump_stream(mut stream: tonic::Streaming<ServerFrame>, shared: &Shared) -> Attempt {
    let mut saw_render = false;
    loop {
        let message = match race(shared.wake.notified(), stream.message()).await {
            Winner::First(()) => match shared.take_wake() {
                Wake::Shutdown => return Attempt::Shutdown,
                // A retry raised on a live stream is just a step reset, which
                // `take_wake` already did.
                Wake::Retry | Wake::Elapsed => continue,
            },
            Winner::Second(message) => message,
        };

        let frame = match message {
            Ok(Some(frame)) => frame,
            // A server that ended the stream is a drop like any other.
            Ok(None) => return Attempt::Dropped,
            Err(status) => return classify(&status),
        };

        match frame.kind {
            Some(server_frame::Kind::Render(bytes)) => {
                if !saw_render {
                    // This stream has actually delivered, so the next drop
                    // starts the schedule over.
                    saw_render = true;
                    shared.reset_step();
                }
                shared.push_render(bytes);
            }
            Some(server_frame::Kind::Control(event)) => match event.kind.as_str() {
                CONTROL_CONNECTION_ID => shared.set_connection_id(event.payload),
                CONTROL_EXIT => {
                    shared.set_status(FeedStatus::Closed);
                    return Attempt::Closed;
                }
                // An unknown control kind is a newer server, not an error.
                _ => {}
            },
            // A frame whose oneof this build does not know: same rule.
            None => {}
        }
    }
}

/// Which end an error `Status` is: an expired credential, or one more drop.
fn classify(status: &tonic::Status) -> Attempt {
    if status.code() == Code::Unauthenticated {
        Attempt::AuthExpired
    } else {
        Attempt::Dropped
    }
}

/// Wait out one backoff step, cut short by a manual retry or a shutdown.
async fn wait_backoff(shared: &Shared, waiter: &Waiter, delay: Duration) -> Wake {
    match race(shared.wake.notified(), waiter.sleep(delay)).await {
        Winner::First(()) => shared.take_wake(),
        Winner::Second(()) => Wake::Elapsed,
    }
}

/// Park an exhausted schedule until a retry re-arms it or a shutdown ends it.
async fn park(shared: &Shared) -> Wake {
    loop {
        shared.wake.notified().await;
        match shared.take_wake() {
            // A stale permit: still parked.
            Wake::Elapsed => continue,
            wake => return wake,
        }
    }
}

/// Which of [`race`]'s two futures finished first.
enum Winner<A, B> {
    First(A),
    Second(B),
}

/// Poll `first`, then `second`, on every wake, and finish with whichever
/// completes first — dropping the loser.
///
/// The two-future `tokio::select!` this crate cannot spell: `select!` lives
/// behind tokio's `macros` feature, and production carries `sync` only (see
/// the [module docs](self)). `first` is polled first, so a pending shutdown
/// always wins a tie against the work it is cancelling.
async fn race<A: Future, B: Future>(first: A, second: B) -> Winner<A::Output, B::Output> {
    let mut first = std::pin::pin!(first);
    let mut second = std::pin::pin!(second);
    std::future::poll_fn(move |cx| {
        if let Poll::Ready(value) = first.as_mut().poll(cx) {
            return Poll::Ready(Winner::First(value));
        }
        if let Poll::Ready(value) = second.as_mut().poll(cx) {
            return Poll::Ready(Winner::Second(value));
        }
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU64;

    use super::*;
    use crate::net::testing::{self, MuxrdFixture, TerminalScript, wait_until};

    /// The injected schedule every reconnect test runs on: two steps, both
    /// short enough that a whole exhaust-and-re-arm cycle costs milliseconds
    /// (the shipped [`RECONNECT_BACKOFF`] would cost 31 seconds).
    const TEST_BACKOFF: [Duration; 2] = [Duration::from_millis(5), Duration::from_millis(5)];

    /// The session name every test attaches to.
    const SESSION: &str = "alpha";

    /// A fixture, a token-bearing client, and the script that drives the
    /// server side of the attach stream.
    struct Rig {
        fixture: MuxrdFixture,
        client: MuxrClient,
    }

    impl Rig {
        async fn start() -> Self {
            let fixture = MuxrdFixture::start().await;
            let client = fixture.connect_pinned().await;
            client.set_session_token(testing::SESSION_TOKEN);
            Self { fixture, client }
        }

        fn script(&self) -> Arc<TerminalScript> {
            self.fixture.terminal()
        }

        /// A feed on [`TEST_BACKOFF`], its pump spawned on the test runtime,
        /// plus the counter its auth-expired callback bumps.
        fn feed(&self, cols: u16, rows: u16) -> (GrpcTerminalFeed, Arc<AtomicU64>) {
            self.feed_with(cols, rows, &TEST_BACKOFF)
        }

        fn feed_with(
            &self,
            cols: u16,
            rows: u16,
            backoff: &[Duration],
        ) -> (GrpcTerminalFeed, Arc<AtomicU64>) {
            let expiries = Arc::new(AtomicU64::new(0));
            let bump = Arc::clone(&expiries);
            let (feed, pump) = GrpcTerminalFeed::detached(
                self.client.clone(),
                SESSION,
                cols,
                rows,
                Box::new(move || {
                    bump.fetch_add(1, Ordering::Relaxed);
                }),
                backoff,
            );
            tokio::spawn(pump);
            (feed, expiries)
        }
    }

    /// Drain into a `Vec` of owned writes, checking the reported count against
    /// what the sink saw — the same shape `DemoFeed`'s own tests assert on.
    fn drain(feed: &mut GrpcTerminalFeed) -> Vec<Vec<u8>> {
        let mut writes = Vec::new();
        let reported = feed.drain_output(&mut |bytes| writes.push(bytes.to_vec()));
        assert_eq!(
            reported,
            writes.len(),
            "the reported write count must match what the sink saw"
        );
        writes
    }

    /// The mandatory first frame carries the session and the grid the feed was
    /// built at.
    #[tokio::test]
    async fn attach_carries_the_session_and_the_grid() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);

        wait_until("the server observes one attach", || {
            script.attach_count() == 1
        })
        .await;
        let attach = script.attaches().remove(0);
        assert_eq!(attach.session, SESSION);
        assert_eq!((attach.cols, attach.rows), (80, 24));

        wait_until("the feed reports itself connected", || {
            feed.status() == FeedStatus::Connected
        })
        .await;
        assert!(feed.is_open());
    }

    /// Keystrokes reach the server as `input` frames, and what the server
    /// sends back comes out of `drain_output` once, in order — but never
    /// inside the same dispatch, which is the whole difference from `DemoFeed`.
    #[tokio::test]
    async fn input_reaches_the_server_and_render_bytes_drain_once_in_order() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;

        feed.send_input(b"ls");
        feed.send_input(b" -la\r");
        assert!(
            drain(&mut feed).is_empty(),
            "a real server echoes over the wire: nothing is available in the same dispatch"
        );

        wait_until("the server observes both input frames", || {
            script.inputs().len() == 2
        })
        .await;
        assert_eq!(
            script.inputs(),
            vec![b"ls".to_vec(), b" -la\r".to_vec()],
            "input goes on the stream, whole and in order"
        );

        wait_until("both echoes arrive back", || feed.pending_writes() == 2).await;
        assert_eq!(drain(&mut feed), vec![b"ls".to_vec(), b" -la\r".to_vec()]);
        assert!(
            drain(&mut feed).is_empty(),
            "a drained frame is handed over exactly once"
        );
    }

    /// A resize reaches the server, and the cached pair is what the next
    /// attach replays.
    #[tokio::test]
    async fn resize_reaches_the_server_and_is_replayed_on_reconnect() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;

        feed.resize(80, 24);
        feed.resize(100, 40);
        wait_until("the server observes the resize", || {
            !script.resizes().is_empty()
        })
        .await;
        assert_eq!(
            script.resizes(),
            vec![(100, 40)],
            "an unchanged grid sends nothing; a changed one sends exactly once"
        );

        script.kill();
        wait_until("the feed re-attaches", || script.attach_count() == 2).await;
        let reattach = script.attaches().remove(1);
        assert_eq!(
            (reattach.cols, reattach.rows),
            (100, 40),
            "the reconnect attaches at the grid the user is looking at"
        );
    }

    /// The connection id is captured off the control stream and exposed, and
    /// never reaches the emulator as output.
    #[tokio::test]
    async fn connection_id_is_captured_and_never_drained_as_output() {
        let rig = Rig::start().await;
        let (mut feed, _) = rig.feed(80, 24);

        wait_until("the connection id lands", || feed.connection_id().is_some()).await;
        assert_eq!(
            feed.connection_id(),
            Some(testing::connection_id(1)),
            "the id is the server's, verbatim"
        );
        assert!(
            drain(&mut feed).is_empty(),
            "a control event is not terminal output"
        );
    }

    /// A server `exit` closes the feed for good: no reconnect, and the pump is
    /// gone.
    #[tokio::test]
    async fn an_exit_control_closes_the_feed_without_reconnecting() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;

        script.send_exit();
        wait_until("the feed observes the close", || {
            feed.status() == FeedStatus::Closed
        })
        .await;
        wait_until("the pump returns", || !feed.is_open()).await;
        assert_eq!(
            script.attach_count(),
            1,
            "a clean close is not a drop: nothing re-attaches"
        );
    }

    /// A dropped stream walks the schedule, and the first render frame after a
    /// reconnect puts the step back to zero.
    #[tokio::test]
    async fn a_dropped_stream_advances_the_schedule_and_a_render_resets_it() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;

        // A stream that delivered nothing before dropping proves nothing, so
        // each drop takes the next step.
        script.kill();
        wait_until("the first reconnect lands", || script.attach_count() == 2).await;
        assert_eq!(feed.backoff_step(), 1);

        script.kill();
        wait_until("the second reconnect lands", || script.attach_count() == 3).await;
        assert_eq!(feed.backoff_step(), 2);

        // …and one render frame is all it takes to earn a fresh schedule.
        wait_until("the third stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;
        script.push_render(b"live\r\n".to_vec());
        wait_until("the render frame lands", || feed.pending_writes() == 1).await;
        assert_eq!(feed.backoff_step(), 0);
    }

    /// Running off the end of the schedule stops the loop and offers the
    /// screen a retry; `reset_backoff` takes it.
    #[tokio::test]
    async fn an_exhausted_schedule_disconnects_until_reset_backoff_re_arms_it() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);

        // Two steps, so the third drop runs off the end.
        for attach in 1..=3 {
            wait_until("the stream is up", || {
                script.attach_count() == attach && feed.status() == FeedStatus::Connected
            })
            .await;
            script.kill();
        }
        wait_until("the schedule runs out", || {
            feed.status() == FeedStatus::Disconnected
        })
        .await;
        assert_eq!(script.attach_count(), 3, "the loop stopped attempting");
        assert!(
            feed.is_open(),
            "a parked pump is not a finished one — the retry re-arms this same feed"
        );

        feed.reset_backoff();
        wait_until("the manual retry re-attaches", || {
            script.attach_count() == 4
        })
        .await;
        assert_eq!(feed.backoff_step(), 0, "a manual retry starts over");
    }

    /// An `UNAUTHENTICATED` open short-circuits the whole backoff loop: one
    /// callback, no retry, and a feed that is done.
    #[tokio::test]
    async fn unauthenticated_fires_the_callback_once_and_never_retries() {
        let rig = Rig::start().await;
        let script = rig.script();
        rig.client.set_session_token("not-the-fixtures-token");
        let (feed, expiries) = rig.feed(80, 24);

        wait_until("the feed gives up", || {
            feed.status() == FeedStatus::Disconnected
        })
        .await;
        wait_until("the pump returns", || !feed.is_open()).await;
        assert_eq!(
            expiries.load(Ordering::Relaxed),
            1,
            "the auth-expired hook fires exactly once"
        );
        assert_eq!(
            script.attach_count(),
            0,
            "the credential never got past the bearer check"
        );

        // Nothing re-arms an expired credential: the retry affordance is a
        // no-op once the pump has returned.
        feed.reset_backoff();
        assert_eq!(expiries.load(Ordering::Relaxed), 1);
        assert_eq!(script.attach_count(), 0);
    }

    /// A shutdown ends the request stream, which is how the server learns the
    /// client is gone.
    #[tokio::test]
    async fn shutdown_ends_the_stream_and_finishes_the_pump() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;

        feed.shutdown();
        wait_until("the pump returns", || !feed.is_open()).await;
        assert_eq!(script.attach_count(), 1, "a shutdown never reconnects");
        assert_eq!(
            feed.status(),
            FeedStatus::Disconnected,
            "a torn-down feed stops claiming to be connected"
        );

        // The outbound half is gone with it, so a late keystroke is dropped
        // rather than queued against a dead stream.
        let mut feed = feed;
        feed.send_input(b"x");
        assert_eq!(feed.dropped_frames(), 1);
    }

    /// The shipped schedule is the documented one — the constant callers
    /// depend on, not a test-injected stand-in.
    #[test]
    fn the_shipped_schedule_is_one_two_four_eight_sixteen_seconds_with_no_jitter() {
        assert_eq!(
            RECONNECT_BACKOFF.map(|step| step.as_secs()),
            [1, 2, 4, 8, 16]
        );
    }
}
