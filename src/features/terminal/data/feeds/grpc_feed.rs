//! [`GrpcTerminalFeed`] — the live `muxrd` byte source: one `AttachTerminal`
//! bidirectional stream behind this feature's own
//! [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed) port,
//! with a bounded reconnect schedule. `LiveSession::enter` builds one for a real
//! attached session; [`DemoFeed`](super::demo_feed::DemoFeed) is the fixture
//! replayed for anything else.
//!
//! **Wire contract** (`proto/muxr.proto`): `attach: AttachReq{session, rows,
//! cols}` — plus the [`ResumeTarget`] hints, when one is set — is the
//! **mandatory first frame** of every stream, reconnects included, then
//! `input`/`resize`/`mouse` out and `render`/`control` back. Input goes **on
//! the stream**, never through `WriteToPane`, so a keystroke keeps its order
//! relative to the resizes around it; the attach frame replays the **current**
//! grid, since [`TerminalFeed::resize`] caches `cols`/`rows`. Two control kinds
//! are understood ([`CONTROL_CONNECTION_ID`], [`CONTROL_EXIT`]) and every other is
//! *ignored*, never an error, so a newer `muxrd` can add one; `exit` is a clean
//! close, with no reconnect.
//!
//! Three contracts are stated where they are enforced: the fixed, jitter-free
//! reconnect schedule ([`RECONNECT_BACKOFF`]), the inbound flow control that
//! **pauses the reader rather than dropping anything** ([`INBOUND_HIGH_WATER`],
//! [`await_capacity`]), and the threading split ([`Shared`] — the UI thread
//! drains, a background task that writes no signal reads). The loop is spelled by
//! hand because production `tokio` carries the `sync` feature only, so the backoff
//! nap goes through [`frust::spawn_blocking`] and the two-way race is [`race`];
//! those plus [`frust::spawn`] are this file's only reaches for `frust::` in an
//! otherwise framework-free data layer. **Test seam**: the schedule and the nap
//! are arguments of a `#[cfg(test)]`-only `detached` constructor, which also hands
//! the pump future back rather than spawning it.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;

use crate::features::terminal::domain::feed::{TerminalFeed, WheelKind};
use crate::net::MuxrClient;
use crate::net::control::{CONTROL_CONNECTION_ID, CONTROL_EXIT};
use crate::net::proto::{
    AttachReq, ClientFrame, MouseInput, MouseKind, Resize, ServerFrame, client_frame, server_frame,
};

/// The reconnect schedule: 1, 2, 4, 8, 16 seconds, **no jitter**, matching the
/// Flutter client this replaces. The contract; nothing overrides it.
///
/// A stream that errors or ends takes the next step; the counter resets on the
/// **first render frame after a (re)connect** — a stream that opens and delivers
/// nothing has proved nothing — and on
/// [`reset_backoff`](GrpcTerminalFeed::reset_backoff), the manual retry. Running
/// off the end parks the loop at [`FeedStatus::Disconnected`]. A drop reports
/// [`FeedStatus::Reconnecting`] for the **whole step, nap included**, since the
/// link strip renders nothing for `Connected` and would otherwise leave a frozen
/// grid with no banner for up to 16 seconds. `UNAUTHENTICATED` short-circuits all
/// of it: the loop stops at once and invokes the injected auth-expired callback.
pub const RECONNECT_BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

/// Frames the outbound channel buffers before a send is dropped. Generous on
/// purpose: outbound is keystrokes and resizes at human rate, so filling it means
/// a wedged transport, where the only alternatives are dropping a frame or
/// blocking the UI thread on a dead socket. Drops are counted.
const OUTBOUND_CAPACITY: usize = 256;

/// Queued inbound bytes at which the pump stops reading the stream. 2 MiB is
/// generous for interactive use and still bounded: the UI drains every ~33 ms, so
/// reaching it means a burst faster than ~60 MB/s or a UI that stopped draining.
/// A **pause** threshold, not an admission filter.
pub const INBOUND_HIGH_WATER: usize = 2 * 1024 * 1024;

/// Queued inbound bytes the drain side must bring the queue back under before the
/// pump resumes reading — a quarter of [`INBOUND_HIGH_WATER`], so a queue hovering
/// at the mark cannot stop/start the read on every frame.
pub const INBOUND_LOW_WATER: usize = INBOUND_HIGH_WATER / 4;

/// Queued inbound **frames** at which the pump stops reading — the frame-count
/// half of the flow control, since a flood of tiny frames stays under the byte cap
/// while each still costs a queue slot and an allocation.
pub const INBOUND_HIGH_WATER_FRAMES: usize = 4096;

/// Queued inbound frames the drain side must bring the queue back under before the
/// pump resumes — a quarter of [`INBOUND_HIGH_WATER_FRAMES`], for the reason
/// [`INBOUND_LOW_WATER`] records.
pub const INBOUND_LOW_WATER_FRAMES: usize = INBOUND_HIGH_WATER_FRAMES / 4;

/// Where a feed's session currently stands. Plain shared state, polled by the
/// screen that owns the feed — deliberately not a signal.
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
    /// No further attempt will be made on its own: the schedule ran out, or the
    /// credential expired. The screen's retry is
    /// [`GrpcTerminalFeed::reset_backoff`].
    Disconnected,
    /// The server said [`CONTROL_EXIT`]. Terminal — nothing will arrive again.
    Closed,
}

/// Where a **fresh** attach should land, as far as this client knows: the
/// space/tab/pane the user last moved to themselves.
///
/// Sent on every `AttachReq` — see [`ResumeSlot`] for why only user-driven moves
/// write it. Every field is best-effort and independent: the server resolves them
/// pane → tab → space and falls through on anything stale, so an unset field
/// (`None`, the wire's proto3 default) is simply no hint, and a hint the server
/// cannot resolve is not an error. herdr sessions only; a zellij attach is the
/// whole composited session and follows the server's own focus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumeTarget {
    /// The opaque herdr workspace id (`Space::id`).
    pub space_id: Option<String>,
    /// The tab id `GetLayout` emits (`TabEntry::tab_id`).
    pub tab_id: Option<u64>,
    /// The pane id `GetLayout` emits (`PaneEntry::id`).
    pub pane_id: Option<u32>,
}

/// The shared, updatable [`ResumeTarget`] a feed attaches with: the screen writes
/// it, the pump reads it on **every** attach — the first one and each backoff
/// reconnect alike, since both go through one code path.
///
/// Held behind an `Arc` rather than reached through the feed handle, because the
/// two ends live on different sides of the screen's `Rc`/`Arc` split: the feed is
/// owned by the live session (`Rc`, UI thread), the writer is the controller
/// (`Arc`, `Send + Sync`).
///
/// **Only user-driven moves write it** (the controller's own contract): a layout
/// poll answers with whatever focus the *server* currently reports, which after a
/// reconnect is the daemon-global one — recording that would overwrite the very
/// intent this slot exists to replay.
#[derive(Debug, Default)]
pub struct ResumeSlot {
    target: Mutex<ResumeTarget>,
}

impl ResumeSlot {
    /// The hint as it stands. Poisoning is recovered from rather than propagated:
    /// a panic elsewhere must not wedge every later attach.
    pub fn target(&self) -> ResumeTarget {
        self.target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Remember the pane a user just committed to, in the tab it lives in
    /// (`None` when the tab is no longer listed — the pane rung of the server's
    /// ladder still resolves on its own). The space is **kept**: it is the axis
    /// that survives a restart the numeric ids do not.
    pub fn remember_pane(&self, tab_id: Option<u64>, pane_id: u32) {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        target.tab_id = tab_id;
        target.pane_id = Some(pane_id);
    }

    /// Remember the space a user just switched to, clearing the tab and pane
    /// hints — those named a tab and a pane of the space being left.
    pub fn remember_space(&self, space_id: String) {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        *target = ResumeTarget {
            space_id: Some(space_id),
            tab_id: None,
            pane_id: None,
        };
    }
}

/// A live `muxrd` terminal session behind the
/// [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed) port.
/// See the [module docs](self). Dropping it stops the pump and closes the request
/// stream, so the server sees the attach end rather than waiting out a keepalive.
pub struct GrpcTerminalFeed {
    shared: Arc<Shared>,
    /// The deque [`TerminalFeed::drain_output`] swaps the shared inbound queue
    /// into, so the sink runs with no lock held and the pump never waits on an
    /// emulator parse. Always empty between drains — what makes the swap correct —
    /// and it keeps its capacity, so a wake allocates nothing.
    scratch: VecDeque<Vec<u8>>,
}

impl GrpcTerminalFeed {
    /// Attach to `session` at a `cols` x `rows` grid and start pumping.
    ///
    /// `on_auth_expired` fires at most once, on the first `UNAUTHENTICATED` this
    /// session sees; the caller wires it to whatever owns the app's credential.
    /// `resume` is the shared hint every attach this feed makes carries — see
    /// [`ResumeSlot`]; a caller with nothing to resume hands over a default one.
    /// The pump runs on the background reactive runtime, so this must be called
    /// with that runtime up — i.e. from app code, never a bare `#[tokio::test]`.
    pub fn attach(
        client: MuxrClient,
        session: impl Into<String>,
        cols: u16,
        rows: u16,
        on_auth_expired: Box<dyn Fn() + Send + Sync>,
        resume: Arc<ResumeSlot>,
    ) -> Self {
        let (feed, pump) = Self::build(Pump {
            client,
            session: session.into(),
            backoff: RECONNECT_BACKOFF.to_vec(),
            waiter: Waiter::Frust,
            on_auth_expired,
            shared: Arc::new(Shared::new(cols, rows, resume)),
        });
        frust::spawn(pump);
        feed
    }

    /// Where this session stands right now.
    pub fn status(&self) -> FeedStatus {
        self.shared.state().status
    }

    /// The server-minted connection id from this stream's
    /// [`CONTROL_CONNECTION_ID`] event, once one has arrived. The *newest* id wins:
    /// a re-attached stream mints its own, so a caller routing by it must re-read
    /// rather than cache it across a drop.
    pub fn connection_id(&self) -> Option<String> {
        self.shared.state().connection_id.clone()
    }

    /// Reset the backoff to its first step and, if the loop has parked on an
    /// exhausted schedule, re-arm it — the manual "retry" the screen offers on
    /// [`FeedStatus::Disconnected`]. A no-op once the session has ended for good.
    pub fn reset_backoff(&self) {
        self.shared.retry.store(true, Ordering::Release);
        self.shared.wake.notify_one();
    }

    /// Stop the pump and close the request stream. Idempotent; [`Drop`] calls it.
    /// Forceful: the outbound sender is dropped immediately rather than drained,
    /// since a graceful variant would have to wait on a stream that, by
    /// construction, never completes on its own.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.set_outbound(None);
        self.shared.wake.notify_one();
    }

    /// The backoff step the next drop would wait out — a diagnostic/test accessor
    /// for the schedule the [module docs](self) pin.
    pub fn backoff_step(&self) -> usize {
        self.shared.state().backoff_step
    }

    /// Outbound frames dropped because the session was down or its channel full
    /// (see [`OUTBOUND_CAPACITY`]) — a diagnostic counter.
    pub fn dropped_frames(&self) -> u64 {
        self.shared.state().dropped_frames
    }

    /// Render frames received but not yet drained — what
    /// [`INBOUND_HIGH_WATER_FRAMES`] bounds. A diagnostic/test accessor.
    pub fn pending_writes(&self) -> usize {
        self.shared.state().inbound.len()
    }

    /// Bytes those frames hold — what [`INBOUND_HIGH_WATER`] bounds. Diagnostic.
    pub fn pending_bytes(&self) -> usize {
        self.shared.state().inbound_bytes
    }

    /// Times the pump stopped reading because the queue reached a high-water mark
    /// — a diagnostic counter. A non-zero count means a producer outran this
    /// session's drain; nothing was lost, the server was simply held off.
    pub fn read_pauses(&self) -> u64 {
        self.shared.state().read_pauses
    }

    /// The hint the **next** attach this feed makes will carry — the slot's
    /// current value, read through the feed for a caller holding only the feed.
    pub fn resume_target(&self) -> ResumeTarget {
        self.shared.resume.target()
    }

    /// The feed and its pump future, unspawned. The one construction path.
    fn build(pump: Pump) -> (Self, impl Future<Output = ()> + Send + use<>) {
        let feed = Self {
            shared: Arc::clone(&pump.shared),
            scratch: VecDeque::new(),
        };
        (feed, run_pump(pump))
    }

    /// [`attach`](Self::attach) with the schedule and the nap opened up, and the
    /// pump handed back rather than spawned — the [module docs](self)' test seam.
    #[cfg(test)]
    fn detached(
        client: MuxrClient,
        session: &str,
        cols: u16,
        rows: u16,
        on_auth_expired: Box<dyn Fn() + Send + Sync>,
        backoff: &[Duration],
        resume: Arc<ResumeSlot>,
    ) -> (Self, impl Future<Output = ()> + Send + use<>) {
        Self::build(Pump {
            client,
            session: session.to_string(),
            backoff: backoff.to_vec(),
            waiter: Waiter::Tokio,
            on_auth_expired,
            shared: Arc::new(Shared::new(cols, rows, resume)),
        })
    }
}

impl TerminalFeed for GrpcTerminalFeed {
    /// Hand over every render frame received since the last call, in arrival order,
    /// exactly once. One write per received frame: this feed coalesces nothing,
    /// since the pump above it already does and splitting or merging ANSI here
    /// would hide what the server actually sent. Also the **resume** half of the
    /// inbound flow control: the swap empties the queue, waking a paused pump.
    fn drain_output(&mut self, sink: &mut dyn FnMut(&[u8])) -> usize {
        {
            let mut state = self.shared.state();
            if state.inbound.is_empty() {
                return 0;
            }
            std::mem::swap(&mut state.inbound, &mut self.scratch);
            state.inbound_bytes = 0;
        }
        // Unconditional rather than only on a high-to-low crossing: a stored permit
        // costs one extra re-check on the pump's next pause, whereas a missed wake
        // would hold the reader off until the drain after this one.
        self.shared.drained.notify_one();
        let writes = self.scratch.len();
        for chunk in &self.scratch {
            sink(chunk);
        }
        self.scratch.clear();
        writes
    }

    /// Queue `bytes` as one `input` frame. Returns immediately — the echo comes
    /// back through the render stream, not from here. Bytes typed while the session
    /// is down are dropped and counted ([`GrpcTerminalFeed::dropped_frames`]): a
    /// reconnect replays the grid, never a keystroke the server never saw.
    fn send_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.shared.send(ClientFrame {
            kind: Some(client_frame::Kind::Input(bytes.to_vec())),
        });
    }

    /// Cache the new grid and, if a stream is up, tell the server about it. The
    /// cache is the load-bearing half: it is what the *next* attach frame carries,
    /// so a resize made while disconnected still lands. An unchanged pair sends
    /// nothing — the widget calls this on every layout.
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

    /// Whether the pump can still deliver anything. `false` only once the session
    /// ended for good — a server [`CONTROL_EXIT`], an expired credential, or a
    /// [`shutdown`](GrpcTerminalFeed::shutdown) — **and** the last frames have
    /// drained. A [`FeedStatus::Disconnected`] feed is still open: its pump is
    /// parked, not gone, and `reset_backoff` re-arms the same session.
    fn is_open(&self) -> bool {
        let state = self.shared.state();
        !state.finished || !state.inbound.is_empty()
    }

    /// Queue `kind` at `(col, row)` as one `mouse` frame — the same
    /// try-send-or-drop path [`send_input`](Self::send_input) takes.
    fn send_mouse(&self, kind: WheelKind, col: u32, row: u32) {
        self.shared.send(ClientFrame {
            kind: Some(client_frame::Kind::Mouse(MouseInput {
                kind: wire_wheel_kind(kind) as i32,
                col,
                row,
            })),
        });
    }
}

/// [`WheelKind`] onto the wire's `MouseKind`.
fn wire_wheel_kind(kind: WheelKind) -> MouseKind {
    match kind {
        WheelKind::Up => MouseKind::WheelUp,
        WheelKind::Down => MouseKind::WheelDown,
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

/// The state the UI thread and the pump task share. Everything mutable is behind
/// the one `Mutex`; the two flags beside it are what a `notify_one` wake means.
struct Shared {
    state: Mutex<State>,
    /// The pump's one wake source: a backoff nap, a parked-on-exhaustion wait and a
    /// live stream read all race against it. `notify_one` stores a permit when the
    /// pump is between waits, so a retry or shutdown raised then is never lost.
    wake: Notify,
    /// The drain side's own wake, raised by
    /// [`drain_output`](TerminalFeed::drain_output) and awaited only by a pump
    /// paused on [`INBOUND_HIGH_WATER`]. Separate from [`Shared::wake`] because a
    /// `wake` can consume a retry, while a drain is pure capacity news.
    drained: Notify,
    shutdown: AtomicBool,
    retry: AtomicBool,
    /// Where the next attach should land — its own lock, not a [`State`] field,
    /// since the screen writes it from the UI thread while the pump holds `state`
    /// across a whole attempt's bookkeeping.
    resume: Arc<ResumeSlot>,
}

struct State {
    /// Render payloads awaiting the next drain, in arrival order.
    inbound: VecDeque<Vec<u8>>,
    /// Bytes held across every entry of [`State::inbound`] — the quantity the flow
    /// control bounds, kept alongside rather than recomputed, since the pump
    /// consults it before every read.
    inbound_bytes: usize,
    status: FeedStatus,
    connection_id: Option<String>,
    cols: u16,
    rows: u16,
    /// The current stream's request half, replaced on every attach.
    outbound: Option<mpsc::Sender<ClientFrame>>,
    /// Set when the pump returns for good — the input to [`TerminalFeed::is_open`].
    finished: bool,
    backoff_step: usize,
    dropped_frames: u64,
    read_pauses: u64,
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

/// How the pump waits out a backoff step. Two arms rather than an injected
/// closure: production must not carry a boxed-future indirection for a test seam.
enum Waiter {
    /// The app: a nap on the blocking pool, the same shape `presentation::term`'s
    /// own pump takes, since production `tokio` has no `time` feature.
    Frust,
    /// A `#[tokio::test]`, whose runtime has a timer and whose process has no
    /// `ReactiveRuntime` for `frust::spawn_blocking` to find.
    #[cfg(test)]
    Tokio,
}

impl Waiter {
    async fn sleep(&self, delay: Duration) {
        match self {
            // A raced-away nap leaves its pool thread sleeping out the remainder;
            // harmless, and slicing it into polls would be a 16-second poll loop.
            Waiter::Frust => {
                let _ = frust::spawn_blocking(move || std::thread::sleep(delay)).await;
            }
            #[cfg(test)]
            Waiter::Tokio => tokio::time::sleep(delay).await,
        }
    }
}

impl Shared {
    fn new(cols: u16, rows: u16, resume: Arc<ResumeSlot>) -> Self {
        Self {
            state: Mutex::new(State {
                inbound: VecDeque::new(),
                inbound_bytes: 0,
                status: FeedStatus::Idle,
                connection_id: None,
                cols,
                rows,
                outbound: None,
                finished: false,
                backoff_step: 0,
                dropped_frames: 0,
                read_pauses: 0,
            }),
            wake: Notify::new(),
            drained: Notify::new(),
            shutdown: AtomicBool::new(false),
            retry: AtomicBool::new(false),
            resume,
        }
    }

    /// The shared state. Poisoning is recovered from rather than propagated — a
    /// panic elsewhere must not make a live terminal permanently unreadable.
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

    /// Queue one render payload for the next drain, accounting its bytes. Always
    /// accepts: a frame the pump has already read has nowhere else to go, and the
    /// bound is enforced by *not reading the next one*.
    fn push_render(&self, bytes: Vec<u8>) {
        let mut state = self.state();
        state.inbound_bytes += bytes.len();
        state.inbound.push_back(bytes);
    }

    /// Whether the queue has reached [`INBOUND_HIGH_WATER`] or
    /// [`INBOUND_HIGH_WATER_FRAMES`] — the pump asks before every read. Either cap
    /// alone is enough to pause.
    fn inbound_saturated(&self) -> bool {
        let state = self.state();
        state.inbound_bytes >= INBOUND_HIGH_WATER
            || state.inbound.len() >= INBOUND_HIGH_WATER_FRAMES
    }

    /// Whether a drain has brought the queue back under **both**
    /// [`INBOUND_LOW_WATER`] and [`INBOUND_LOW_WATER_FRAMES`], which is what lets a
    /// paused pump start reading again.
    fn inbound_has_room(&self) -> bool {
        let state = self.state();
        state.inbound_bytes <= INBOUND_LOW_WATER && state.inbound.len() <= INBOUND_LOW_WATER_FRAMES
    }

    fn count_read_pause(&self) {
        self.state().read_pauses += 1;
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

    /// The pump's last act: release the request stream and mark the session over,
    /// so [`TerminalFeed::is_open`] stops the UI pump too. A torn-down feed stops
    /// claiming to be connected — but a `Closed` one keeps its own, more specific
    /// answer, since the server said why.
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
            // Back to `Reconnecting` before the nap below, not after it: a stream
            // that dropped once `attach` had published `Connected` would otherwise
            // keep claiming to be connected for the whole backoff step, and the link
            // strip renders nothing at all for a connected feed.
            Attempt::Dropped => shared.set_status(FeedStatus::Reconnecting),
            Attempt::Closed | Attempt::Shutdown => break,
            Attempt::AuthExpired => {
                // One credential the server refuses will be refused four more times.
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
                // Out of schedule: stop attempting, and wait for the screen's retry
                // rather than hammering a server that is plainly down.
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

/// One attach attempt: open the stream with its mandatory first frame, then pump
/// it until it ends.
///
/// The **one** place an `AttachReq` is built, which is what makes the resume hint
/// ride the backoff reconnect as well as the first attach: the loop above calls
/// this for both. The hint is read here rather than captured with the pump, so a
/// move the user made while the link was down is the one that lands.
async fn attach(client: &MuxrClient, session: &str, shared: &Shared) -> Attempt {
    let (outbound, requests) = mpsc::channel(OUTBOUND_CAPACITY);
    let (cols, rows) = shared.dims();
    let resume = shared.resume.target();
    if outbound
        .try_send(ClientFrame {
            kind: Some(client_frame::Kind::Attach(AttachReq {
                session: session.to_string(),
                rows: u32::from(rows),
                cols: u32::from(cols),
                // Unset spells itself as the wire's proto3 default, which is the
                // hint-less shape the server reads as "today's behavior".
                resume_space_id: resume.space_id.unwrap_or_default(),
                resume_tab_id: resume.tab_id.unwrap_or_default(),
                resume_pane_id: resume.pane_id.unwrap_or_default(),
            })),
        })
        .is_err()
    {
        // A channel this code just built with room for hundreds; unreachable in
        // practice, and a dropped attach frame is a stream muxrd would reject
        // anyway, so treat it as a failed attempt rather than panic.
        return Attempt::Dropped;
    }
    // Published only after the attach frame is queued, so a keystroke racing the
    // handshake can never overtake it into the stream.
    shared.set_outbound(Some(outbound));

    // A fresh stub per attempt: the client's channel may have been rebuilt since
    // the last one, and a stub holds whichever channel it was minted from.
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

/// Read server frames until the stream ends, the server says [`CONTROL_EXIT`], or
/// a shutdown races in. Reads only while the drain side keeps up: see
/// [`await_capacity`], the pause that turns a UI that has stopped draining into
/// HTTP/2 backpressure on the server rather than unbounded growth here.
async fn pump_stream(mut stream: tonic::Streaming<ServerFrame>, shared: &Shared) -> Attempt {
    let mut saw_render = false;
    loop {
        if let Some(attempt) = await_capacity(shared).await {
            return attempt;
        }
        let message = match race(shared.wake.notified(), stream.message()).await {
            Winner::First(()) => match shared.take_wake() {
                Wake::Shutdown => return Attempt::Shutdown,
                // A retry raised on a live stream is just a step reset.
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
                    // This stream has delivered, so the next drop starts over.
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

/// Hold the reader off the stream while the queue sits at [`INBOUND_HIGH_WATER`]
/// or [`INBOUND_HIGH_WATER_FRAMES`], returning once a drain has brought it back
/// under both low-water marks — the pause half of the inbound flow control.
///
/// `Some` when the wait ended the attempt instead of resuming it: a shutdown is
/// the one thing besides a drain that reaches a paused reader, which keeps
/// teardown from waiting on a drain that will never come. A no-op — one lock, no
/// await — on every frame below the mark, which is every frame of a real session.
async fn await_capacity(shared: &Shared) -> Option<Attempt> {
    if !shared.inbound_saturated() {
        return None;
    }
    shared.count_read_pause();
    loop {
        // The drain's permit is stored rather than lost if it lands between the
        // check above and this wait, so the pause cannot miss its own wake.
        if let Winner::First(()) = race(shared.wake.notified(), shared.drained.notified()).await
            && shared.take_wake() == Wake::Shutdown
        {
            return Some(Attempt::Shutdown);
        }
        // Re-read the level rather than trust the wake: a retry, a stale permit or
        // a drain that left the queue still full all land here.
        if shared.inbound_has_room() {
            return None;
        }
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

/// Poll `first`, then `second`, on every wake, and finish with whichever completes
/// first — the two-future `tokio::select!` this crate cannot spell, since
/// `select!` lives behind a feature production does not carry. `first` is polled
/// first, so a pending shutdown always wins a tie.
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

    /// The injected schedule every reconnect test runs on: two steps, both short
    /// enough that a whole exhaust-and-re-arm cycle costs milliseconds.
    const TEST_BACKOFF: [Duration; 2] = [Duration::from_millis(5), Duration::from_millis(5)];

    /// One step long enough that "still waiting out the nap" is observable rather
    /// than a millisecond to win a race against. Never waited out: the feed is
    /// dropped mid-nap, which wakes it.
    const LONG_BACKOFF: [Duration; 1] = [Duration::from_secs(2)];

    /// The session name every test attaches to.
    const SESSION: &str = "alpha";

    /// One flooded render frame. Large enough that a handful crosses
    /// [`INBOUND_HIGH_WATER`], small enough to stay under tonic's 4 MiB ceiling.
    const FLOOD_FRAME: usize = 256 * 1024;

    /// Frames a flood pushes: twice what the high-water mark holds, so the pump
    /// parks with roughly half the flood still behind it on the wire.
    const FLOOD_FRAMES: usize = 2 * INBOUND_HIGH_WATER / FLOOD_FRAME;

    /// Bytes one flood pushes in total.
    const FLOOD_TOTAL: usize = FLOOD_FRAME * FLOOD_FRAMES;

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
            self.feed_resuming(cols, rows, backoff, Arc::new(ResumeSlot::default()))
        }

        /// [`feed_with`](Self::feed_with) over a caller-held [`ResumeSlot`] — the
        /// screen's own handle onto the hint, which a test writes *between* two
        /// attaches the way a user's move does.
        fn feed_resuming(
            &self,
            cols: u16,
            rows: u16,
            backoff: &[Duration],
            resume: Arc<ResumeSlot>,
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
                resume,
            );
            tokio::spawn(pump);
            (feed, expiries)
        }
    }

    /// Drain into a `Vec` of owned writes, checking the reported count against what
    /// the sink saw.
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

    /// Bytes one drain handed over.
    fn drained_bytes(feed: &mut GrpcTerminalFeed) -> usize {
        drain(feed).iter().map(Vec::len).sum()
    }

    /// Push [`FLOOD_TOTAL`] at a feed nothing is draining, and wait for its pump
    /// to park on the high-water mark.
    async fn flood_until_paused(script: &TerminalScript, feed: &GrpcTerminalFeed) {
        for _ in 0..FLOOD_FRAMES {
            script.push_render(vec![b'x'; FLOOD_FRAME]);
        }
        wait_until("the pump parks on the high-water mark", || {
            feed.read_pauses() >= 1
        })
        .await;
    }

    /// Push empty render frames at a feed nothing is draining, until its pump parks
    /// on [`INBOUND_HIGH_WATER_FRAMES`] — the frame-count counterpart of
    /// [`flood_until_paused`]. Crossing 4096 frames takes far more pushes than the
    /// fixture's own 64-deep local command channel can hold at once, so this pushes
    /// a small batch from inside [`wait_until`]'s own probe, repeating on every
    /// failed check; each batch drains through the real stream during the probe's
    /// poll delay before the next is pushed.
    async fn flood_tiny_frames_until_paused(script: &TerminalScript, feed: &GrpcTerminalFeed) {
        const BATCH: usize = 16;
        wait_until("the pump parks on the frame-count high-water mark", || {
            for _ in 0..BATCH {
                script.push_render(Vec::new());
            }
            feed.read_pauses() >= 1
        })
        .await;
    }

    /// The mandatory first frame carries the session and the grid the feed was
    /// built at — and, with nothing to resume, nothing else.
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
        assert_eq!(
            (
                attach.resume_space_id.as_str(),
                attach.resume_tab_id,
                attach.resume_pane_id
            ),
            ("", 0, 0),
            "a session with no history behind it attaches exactly as it always \
             did: the resume fields sit at their proto3 defaults"
        );

        wait_until("the feed reports itself connected", || {
            feed.status() == FeedStatus::Connected
        })
        .await;
        assert!(feed.is_open());
    }

    /// The hint the screen recorded rides the **reconnect's** own attach frame —
    /// the whole point of the slot: the first stream carried nothing, the move
    /// happened while it was up, and the fresh attach asks for it back.
    #[tokio::test]
    async fn a_reconnect_attaches_with_the_resume_target_the_screen_recorded() {
        let rig = Rig::start().await;
        let script = rig.script();
        let resume = Arc::new(ResumeSlot::default());
        let (feed, _) = rig.feed_resuming(80, 24, &TEST_BACKOFF, Arc::clone(&resume));
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;
        assert_eq!(
            script.attaches().remove(0).resume_pane_id,
            0,
            "the first attach had nothing to resume"
        );

        // What the sheet's two user-driven moves write, in the order a user makes
        // them: a space switch, then a pane commit inside it.
        resume.remember_space("ws-7".to_string());
        resume.remember_pane(Some(42), 7);
        script.kill();

        wait_until("the feed re-attaches", || script.attach_count() == 2).await;
        let reattach = script.attaches().remove(1);
        assert_eq!(
            (
                reattach.resume_space_id.as_str(),
                reattach.resume_tab_id,
                reattach.resume_pane_id
            ),
            ("ws-7", 42, 7),
            "the reconnect names the space, tab and pane the user last chose \
             instead of taking the daemon's global focus"
        );
        assert_eq!(
            reattach.session, SESSION,
            "…and everything else about the frame is unchanged"
        );
        assert_eq!(
            feed.resume_target(),
            ResumeTarget {
                space_id: Some("ws-7".to_string()),
                tab_id: Some(42),
                pane_id: Some(7),
            },
            "the feed reads the screen's live slot, not a copy taken at attach"
        );
    }

    /// The slot's own rules, the ones the controller's two user-driven paths lean
    /// on: a pane move keeps the space (the rung that survives a muxrd restart),
    /// a space move drops the tab/pane that belonged to the space being left, and
    /// a pane move with no known tab clears the stale tab rather than keeping it.
    #[test]
    fn a_pane_move_keeps_the_space_and_a_space_move_clears_the_tab_and_pane() {
        let slot = ResumeSlot::default();
        assert_eq!(slot.target(), ResumeTarget::default(), "no hint to start");

        slot.remember_space("ws-1".to_string());
        assert_eq!(
            slot.target(),
            ResumeTarget {
                space_id: Some("ws-1".to_string()),
                tab_id: None,
                pane_id: None,
            }
        );

        slot.remember_pane(Some(3), 31);
        assert_eq!(
            slot.target(),
            ResumeTarget {
                space_id: Some("ws-1".to_string()),
                tab_id: Some(3),
                pane_id: Some(31),
            },
            "a pane move refines the hint rather than replacing it"
        );

        slot.remember_space("ws-2".to_string());
        assert_eq!(
            slot.target(),
            ResumeTarget {
                space_id: Some("ws-2".to_string()),
                tab_id: None,
                pane_id: None,
            },
            "a tab and a pane of the space just left would resume the wrong view"
        );

        slot.remember_pane(None, 12);
        assert_eq!(
            slot.target(),
            ResumeTarget {
                space_id: Some("ws-2".to_string()),
                tab_id: None,
                pane_id: Some(12),
            },
            "an unlisted tab leaves the tab rung unset — never a stale id the \
             server would resolve to some other tab"
        );
    }

    /// Keystrokes reach the server as `input` frames, and what the server sends back
    /// comes out of `drain_output` once, in order — but never inside the same
    /// dispatch, which is the whole difference from `DemoFeed`.
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

    /// A wheel-scroll event reaches the server as one `mouse` frame, carrying
    /// its kind and zero-based cell coordinates verbatim.
    #[tokio::test]
    async fn mouse_events_reach_the_server_as_mouse_frames() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;

        feed.send_mouse(WheelKind::Up, 3, 7);
        feed.send_mouse(WheelKind::Down, 79, 23);
        wait_until("the server observes both mouse frames", || {
            script.mice().len() == 2
        })
        .await;
        assert_eq!(
            script.mice(),
            vec![
                testing::MouseObservation {
                    kind: crate::net::proto::MouseKind::WheelUp,
                    col: 3,
                    row: 7,
                },
                testing::MouseObservation {
                    kind: crate::net::proto::MouseKind::WheelDown,
                    col: 79,
                    row: 23,
                },
            ]
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

    /// A drop reports `Reconnecting` for the *whole* step — the backoff nap
    /// included, not just the attach attempts at either end of it. The status is
    /// what the link strip renders, and it renders nothing for a connected feed.
    #[tokio::test]
    async fn a_drop_reports_reconnecting_while_the_backoff_nap_runs() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed_with(80, 24, &LONG_BACKOFF);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;

        script.kill();
        wait_until("the drop lands", || feed.status() != FeedStatus::Connected).await;
        assert_eq!(
            feed.status(),
            FeedStatus::Reconnecting,
            "a feed waiting out a backoff step is reconnecting, not connected"
        );
        assert_eq!(
            script.attach_count(),
            1,
            "the nap is still running: nothing has re-attached to make that true trivially"
        );
    }

    /// A producer outrunning the drain parks the pump on the high-water mark
    /// rather than growing the queue, and a drain starts it reading again.
    #[tokio::test]
    async fn a_flood_pauses_the_reader_until_a_drain_makes_room() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;

        flood_until_paused(&script, &feed).await;
        assert!(
            feed.pending_bytes() < INBOUND_HIGH_WATER + FLOOD_FRAME,
            "the queue is bounded by the mark plus the one frame that crossed it, not by what the server sent: {} bytes",
            feed.pending_bytes()
        );
        assert!(
            feed.pending_bytes() < FLOOD_TOTAL,
            "the rest of the flood is still on the server, held off by a reader that stopped reading"
        );

        // Draining empties the queue and wakes the pump, and the whole flood
        // arrives — nothing was dropped to make room.
        let mut received = drained_bytes(&mut feed);
        wait_until("the rest of the flood arrives once reading resumes", || {
            received += drained_bytes(&mut feed);
            received == FLOOD_TOTAL
        })
        .await;
        assert_eq!(
            feed.status(),
            FeedStatus::Connected,
            "the session is intact"
        );
    }

    /// A flood of many *tiny* (here, empty) render frames stays nowhere near
    /// [`INBOUND_HIGH_WATER`]'s byte accounting, yet each still costs a `VecDeque`
    /// slot and a `Vec` allocation — [`INBOUND_HIGH_WATER_FRAMES`] catches that
    /// shape and pauses the reader on frame count alone.
    #[tokio::test]
    async fn a_flood_of_tiny_frames_pauses_the_reader_via_the_frame_count_cap() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;

        flood_tiny_frames_until_paused(&script, &feed).await;
        assert!(
            feed.pending_bytes() < INBOUND_HIGH_WATER,
            "a flood of empty frames must never cross the byte cap on its own: {} bytes",
            feed.pending_bytes()
        );
        assert!(
            (INBOUND_HIGH_WATER_FRAMES..INBOUND_HIGH_WATER_FRAMES * 2)
                .contains(&feed.pending_writes()),
            "the queue is bounded by the frame-count mark, not left to grow \
             unboundedly while bytes stay low: {} frames",
            feed.pending_writes()
        );

        // Draining resumes the reader, and the session keeps delivering rather than
        // staying parked forever — nothing was dropped to make room.
        drain(&mut feed);
        script.push_render(b"after\r\n".to_vec());
        wait_until("the session keeps delivering once reading resumes", || {
            feed.pending_writes() >= 1
        })
        .await;
        assert_eq!(
            feed.status(),
            FeedStatus::Connected,
            "the session is intact"
        );
    }

    /// The mark is a pause threshold, not an admission filter: a single frame
    /// bigger than the whole cap is queued whole, and the session carries on.
    #[tokio::test]
    async fn a_frame_larger_than_the_cap_is_accepted_whole() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;

        let oversized = INBOUND_HIGH_WATER + FLOOD_FRAME;
        script.push_render(vec![b'z'; oversized]);
        wait_until("the oversized frame lands", || {
            feed.pending_bytes() >= oversized
        })
        .await;
        let writes = drain(&mut feed);
        assert_eq!(
            writes.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![oversized],
            "an over-cap frame is handed over whole, never split and never refused"
        );

        script.push_render(b"after\r\n".to_vec());
        wait_until("the session keeps delivering", || {
            feed.pending_writes() == 1
        })
        .await;
        assert_eq!(drain(&mut feed), vec![b"after\r\n".to_vec()]);
    }

    /// A shutdown reaches a paused reader — `Drop` takes this same path, so
    /// teardown never waits on a drain that will never come.
    #[tokio::test]
    async fn a_shutdown_wakes_a_reader_paused_on_the_high_water_mark() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;
        flood_until_paused(&script, &feed).await;

        feed.shutdown();
        wait_until("the pump returns without a drain", || {
            feed.status() == FeedStatus::Disconnected
        })
        .await;
        assert_eq!(script.attach_count(), 1, "a shutdown never reconnects");
    }

    /// A `ServerFrame` stream is one sequence, so a pause holds back a server
    /// `exit` too — and the first drain both resumes the reader and delivers
    /// the close.
    #[tokio::test]
    async fn an_exit_behind_a_flood_closes_once_the_drain_resumes_the_reader() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == FeedStatus::Connected
        })
        .await;
        flood_until_paused(&script, &feed).await;

        script.send_exit();
        assert_eq!(
            feed.status(),
            FeedStatus::Connected,
            "a paused reader reads nothing at all, control frames included"
        );

        let mut received = 0;
        wait_until("the close lands once draining resumes the reader", || {
            received += drained_bytes(&mut feed);
            feed.status() == FeedStatus::Closed
        })
        .await;
        received += drained_bytes(&mut feed);
        assert_eq!(
            received, FLOOD_TOTAL,
            "the whole flood arrived ahead of the close: a pause defers bytes, never drops them"
        );
        wait_until("the pump returns", || !feed.is_open()).await;
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

    /// The shipped inbound bound is the documented one: 2 MiB of queued render bytes
    /// pauses the reader, and a quarter of that resumes it — a low-water mark at the
    /// high-water one would resume the instant it paused.
    #[test]
    fn the_shipped_inbound_bound_is_two_mib_resuming_at_a_quarter_of_it() {
        assert_eq!(INBOUND_HIGH_WATER, 2 * 1024 * 1024);
        assert_eq!(INBOUND_LOW_WATER, 512 * 1024);
    }

    /// The shipped frame-count bound is the documented one: 4096 queued render
    /// frames pauses the reader, and a quarter of that resumes it — the same shape
    /// as the byte bound, alongside rather than instead of it.
    #[test]
    fn the_shipped_inbound_frame_bound_is_4096_resuming_at_a_quarter_of_it() {
        assert_eq!(INBOUND_HIGH_WATER_FRAMES, 4096);
        assert_eq!(INBOUND_LOW_WATER_FRAMES, 1024);
    }
}
