//! One `AttachTerminal` stream behind the `TerminalFeed` port. `AttachReq` is the mandatory first
//! frame of every stream, reconnects included; input rides the stream, never `WriteToPane`, so a
//! keystroke keeps its order against resizes. Unknown control kinds are ignored; `exit` is final.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::time::{Duration, Instant};

use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::sync::{Notify, broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::Code;

use crate::features::terminal::domain::feed::{LinkStatus, TerminalFeed, WheelKind};
use crate::net::MuxrClient;
use crate::net::control::{CONTROL_CONNECTION_ID, CONTROL_EXIT};
use crate::net::proto::{
    AttachReq, ClientFrame, MouseInput, MouseKind, Resize, ServerFrame, client_frame, server_frame,
};
use crate::net::wake::{self, WakeReason};

/// No jitter. Resets on the first render frame after a (re)connect and on
/// [`reset_backoff`](GrpcTerminalFeed::reset_backoff); running off the end parks at
/// [`LinkStatus::Disconnected`]. `UNAUTHENTICATED` stops the loop and fires the auth callback.
pub const RECONNECT_BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

/// The first wake of a burst acts at once; a later one inside the window acts once when it
/// closes. A wake drops the attempt in flight, rebuilds the channel, resets the schedule and
/// re-attaches, skipping the reset when the last wake-driven attempt failed within two windows.
pub const WAKE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Filling it means a wedged transport: a send past it is dropped and counted, never blocking.
const OUTBOUND_CAPACITY: usize = 256;

/// The pump stops reading here, a pause threshold rather than an admission filter: the server is
/// held off by HTTP/2 backpressure and nothing is dropped.
pub const INBOUND_HIGH_WATER: usize = 2 * 1024 * 1024;

/// A quarter of the high-water mark, so a queue hovering at it cannot stop/start every frame.
pub const INBOUND_LOW_WATER: usize = INBOUND_HIGH_WATER / 4;

/// Bounds a flood of tiny frames that stays under the byte cap.
pub const INBOUND_HIGH_WATER_FRAMES: usize = 4096;

pub const INBOUND_LOW_WATER_FRAMES: usize = INBOUND_HIGH_WATER_FRAMES / 4;

/// Best-effort hints the server resolves pane → tab → space, falling through on anything stale;
/// `None` is no hint. herdr only: a zellij attach follows the server's own focus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumeTarget {
    pub space_id: Option<String>,
    pub tab_id: Option<u64>,
    pub pane_id: Option<u32>,
}

/// Read on every attach, reconnects included. Only user-driven moves write it: after a reconnect
/// a layout poll reports the daemon-global focus, which would overwrite the intent.
#[derive(Debug, Default)]
pub struct ResumeSlot {
    target: Mutex<ResumeTarget>,
}

impl ResumeSlot {
    pub fn target(&self) -> ResumeTarget {
        self.target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Keeps the space: it survives a restart the numeric ids do not.
    pub fn remember_pane(&self, tab_id: Option<u64>, pane_id: u32) {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        target.tab_id = tab_id;
        target.pane_id = Some(pane_id);
    }

    pub fn remember_space(&self, space_id: String) {
        let mut target = self.target.lock().unwrap_or_else(PoisonError::into_inner);
        *target = ResumeTarget {
            space_id: Some(space_id),
            tab_id: None,
            pane_id: None,
        };
    }
}

/// Dropping it closes the request stream, so the server sees the attach end at once.
pub struct GrpcTerminalFeed {
    shared: Arc<Shared>,
    /// Swapped with the shared queue so the sink runs with no lock held; always empty between
    /// drains.
    scratch: VecDeque<Vec<u8>>,
}

impl GrpcTerminalFeed {
    /// `on_auth_expired` fires at most once. Needs the background reactive runtime up, so never
    /// call it from a bare `#[tokio::test]`.
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
            wakes: Wakes::new(wake::subscribe(), WAKE_DEBOUNCE, Waiter::Frust),
            on_auth_expired,
            shared: Arc::new(Shared::new(cols, rows, resume)),
        });
        frust::spawn(pump);
        feed
    }

    pub fn status(&self) -> LinkStatus {
        self.shared.state().status
    }

    /// The newest id wins: a re-attach mints its own, so re-read rather than cache it.
    pub fn connection_id(&self) -> Option<String> {
        self.shared.state().connection_id.clone()
    }

    /// Re-arms a loop parked on an exhausted schedule; a no-op once the session has ended.
    pub fn reset_backoff(&self) {
        self.shared.retry.store(true, Ordering::Release);
        self.shared.wake.notify_one();
    }

    /// Idempotent and forceful: the outbound sender drops at once rather than draining.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Release);
        self.shared.set_outbound(None);
        self.shared.wake.notify_one();
    }

    pub fn backoff_step(&self) -> usize {
        self.shared.state().backoff_step
    }

    pub fn dropped_frames(&self) -> u64 {
        self.shared.state().dropped_frames
    }

    pub fn pending_writes(&self) -> usize {
        self.shared.state().inbound.len()
    }

    pub fn pending_bytes(&self) -> usize {
        self.shared.state().inbound_bytes
    }

    pub fn read_pauses(&self) -> u64 {
        self.shared.state().read_pauses
    }

    /// Read through the feed, which pins that the pump holds the live slot, not a copy.
    #[cfg(test)]
    fn resume_target(&self) -> ResumeTarget {
        self.shared.resume.target()
    }

    #[cfg(test)]
    fn client_instance_id(&self) -> String {
        self.shared.client_instance_id.clone()
    }

    fn build(pump: Pump) -> (Self, impl Future<Output = ()> + Send + use<>) {
        let feed = Self {
            shared: Arc::clone(&pump.shared),
            scratch: VecDeque::new(),
        };
        (feed, run_pump(pump))
    }

    #[cfg(test)]
    fn detached(
        client: MuxrClient,
        session: &str,
        (cols, rows): (u16, u16),
        on_auth_expired: Box<dyn Fn() + Send + Sync>,
        backoff: &[Duration],
        resume: Arc<ResumeSlot>,
        wakes: Wakes,
    ) -> (Self, impl Future<Output = ()> + Send + use<>) {
        Self::build(Pump {
            client,
            session: session.to_string(),
            backoff: backoff.to_vec(),
            waiter: Waiter::Tokio,
            wakes,
            on_auth_expired,
            shared: Arc::new(Shared::new(cols, rows, resume)),
        })
    }
}

impl TerminalFeed for GrpcTerminalFeed {
    /// Exactly once, in arrival order, one write per frame: coalescing here would hide what the
    /// server sent. The swap is also the resume half of the inbound flow control.
    fn drain_output(&mut self, sink: &mut dyn FnMut(&[u8])) -> usize {
        {
            let mut state = self.shared.state();
            if state.inbound.is_empty() {
                return 0;
            }
            std::mem::swap(&mut state.inbound, &mut self.scratch);
            state.inbound_bytes = 0;
        }
        // Unconditional: a stored permit costs one re-check, a missed wake stalls the reader.
        self.shared.drained.notify_one();
        let writes = self.scratch.len();
        for chunk in &self.scratch {
            sink(chunk);
        }
        self.scratch.clear();
        writes
    }

    /// Bytes typed while down are dropped and counted: a reconnect replays the grid, never input.
    fn send_input(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.shared.send(ClientFrame {
            kind: Some(client_frame::Kind::Input(bytes.to_vec())),
        });
    }

    /// The cache is what the next attach frame carries, so a resize while disconnected lands.
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

    /// `false` only once the session ended for good and the last frames drained; a
    /// [`LinkStatus::Disconnected`] feed is parked, not gone.
    fn is_open(&self) -> bool {
        let state = self.shared.state();
        !state.finished || !state.inbound.is_empty()
    }

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

struct Pump {
    client: MuxrClient,
    session: String,
    backoff: Vec<Duration>,
    waiter: Waiter,
    wakes: Wakes,
    on_auth_expired: Box<dyn Fn() + Send + Sync>,
    shared: Arc<Shared>,
}

/// Injected so a test drives a bus of its own, not the process-global one.
struct Wakes {
    rx: broadcast::Receiver<WakeReason>,
    debounce: Duration,
    /// For a trailing act, the close of the window rather than the wake's arrival.
    last: Option<Instant>,
    /// Struct state, so a wait dropped by the race it sits in loses nothing.
    pending: bool,
    waiter: Waiter,
    /// A fresh `Notify` per armed timer, so a timer firing late cannot leave a stale permit that
    /// cuts the next owed wake's wait short.
    closed: Arc<Notify>,
    armed: bool,
    #[cfg(test)]
    timers: usize,
}

impl Wakes {
    fn new(rx: broadcast::Receiver<WakeReason>, debounce: Duration, waiter: Waiter) -> Self {
        Self {
            rx,
            debounce,
            last: None,
            pending: false,
            waiter,
            closed: Arc::new(Notify::new()),
            armed: false,
            #[cfg(test)]
            timers: 0,
        }
    }

    fn remaining(&self) -> Duration {
        self.last.map_or(Duration::ZERO, |at| {
            self.debounce.saturating_sub(at.elapsed())
        })
    }

    /// A lagged receiver counts as one wake and queued wakes fold into it; a wake inside the
    /// window resolves once when it closes. Cancel-safe: the owed wake is struct state.
    async fn next(&mut self) {
        loop {
            if self.pending {
                let remaining = self.remaining();
                if remaining.is_zero() {
                    self.pending = false;
                    self.armed = false;
                    self.last = Some(Instant::now());
                    return;
                }
                // One timer per owed wake, however often this wait is dropped and rebuilt.
                if !self.armed {
                    self.armed = true;
                    self.closed = Arc::new(Notify::new());
                    #[cfg(test)]
                    {
                        self.timers += 1;
                    }
                    self.waiter.start_timer(remaining, Arc::clone(&self.closed));
                }
                match race(self.rx.recv(), self.closed.notified()).await {
                    Winner::First(Err(RecvError::Closed)) => self.closed.notified().await,
                    Winner::First(_) => {
                        while matches!(self.rx.try_recv(), Ok(_) | Err(TryRecvError::Lagged(_))) {}
                    }
                    // The timer fired; if the clock says a sliver is left, it is re-armed.
                    Winner::Second(()) => self.armed = false,
                }
                continue;
            }
            match self.rx.recv().await {
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => std::future::pending::<()>().await,
            }
            while matches!(self.rx.try_recv(), Ok(_) | Err(TryRecvError::Lagged(_))) {}
            if self.remaining().is_zero() {
                self.last = Some(Instant::now());
                return;
            }
            self.pending = true;
        }
    }
}

/// The UI thread drains; a background task that writes no signal reads.
struct Shared {
    state: Mutex<State>,
    /// `notify_one` stores a permit between waits, so a retry or shutdown raised then is kept.
    wake: Notify,
    /// Separate from `wake`: a wake can consume a retry, while a drain is pure capacity news.
    drained: Notify,
    shutdown: AtomicBool,
    retry: AtomicBool,
    /// What tells a failed wake-driven dial from a stream that was up and later dropped.
    opened: AtomicBool,
    /// Its own lock: the screen writes it while the pump holds `state` across an attempt.
    resume: Arc<ResumeSlot>,
    /// Minted once per feed and reused by every re-attach; never logged in full.
    client_instance_id: String,
}

struct State {
    inbound: VecDeque<Vec<u8>>,
    inbound_bytes: usize,
    status: LinkStatus,
    connection_id: Option<String>,
    cols: u16,
    rows: u16,
    outbound: Option<mpsc::Sender<ClientFrame>>,
    finished: bool,
    backoff_step: usize,
    dropped_frames: u64,
    read_pauses: u64,
}

/// What a `wake` notification turned out to mean.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wake {
    /// A stale permit, or the nap ran out.
    Elapsed,
    Retry,
    Network,
    Shutdown,
}

/// How one attach attempt ended.
enum Attempt {
    Dropped,
    Closed,
    AuthExpired,
    Shutdown,
    Rewake,
}

/// Two arms, not a closure: production carries no boxed-future indirection for a test seam.
enum Waiter {
    /// Production `tokio` has no `time` feature, so the app naps on the blocking pool.
    Frust,
    /// A test process has no `ReactiveRuntime` for `frust::spawn_blocking` to find.
    #[cfg(test)]
    Tokio,
}

impl Waiter {
    fn start_timer(&self, delay: Duration, closed: Arc<Notify>) {
        match self {
            // Detached on purpose: a dropped join handle leaves the nap running.
            Waiter::Frust => {
                drop(frust::spawn_blocking(move || {
                    std::thread::sleep(delay);
                    closed.notify_one();
                }));
            }
            #[cfg(test)]
            Waiter::Tokio => {
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    closed.notify_one();
                });
            }
        }
    }

    async fn sleep(&self, delay: Duration) {
        match self {
            // A raced-away nap leaves its pool thread sleeping out the remainder; harmless.
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
                status: LinkStatus::Idle,
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
            opened: AtomicBool::new(false),
            resume,
            client_instance_id: mint_client_instance_id(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_status(&self, status: LinkStatus) {
        self.state().status = status;
    }

    fn dims(&self) -> (u16, u16) {
        let state = self.state();
        (state.cols, state.rows)
    }

    fn set_outbound(&self, outbound: Option<mpsc::Sender<ClientFrame>>) {
        self.state().outbound = outbound;
    }

    fn send(&self, frame: ClientFrame) {
        let mut state = self.state();
        let queued = match state.outbound.as_ref() {
            // `try_send`: this runs on the UI thread, where awaiting a wedged socket would freeze.
            Some(outbound) => outbound.try_send(frame).is_ok(),
            None => false,
        };
        if !queued {
            state.dropped_frames += 1;
        }
    }

    /// Always accepts; the bound is enforced by not reading the next frame.
    fn push_render(&self, bytes: Vec<u8>) {
        let mut state = self.state();
        state.inbound_bytes += bytes.len();
        state.inbound.push_back(bytes);
    }

    /// Either cap alone pauses.
    fn inbound_saturated(&self) -> bool {
        let state = self.state();
        state.inbound_bytes >= INBOUND_HIGH_WATER
            || state.inbound.len() >= INBOUND_HIGH_WATER_FRAMES
    }

    /// Resuming needs both low-water marks.
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

    /// A `Closed` feed keeps its status, since the server said why.
    fn retire(&self) {
        let mut state = self.state();
        if self.shutdown.load(Ordering::Acquire) && state.status != LinkStatus::Closed {
            state.status = LinkStatus::Disconnected;
        }
        state.outbound = None;
        state.finished = true;
    }
}

async fn run_pump(pump: Pump) {
    let Pump {
        client,
        session,
        backoff,
        waiter,
        mut wakes,
        on_auth_expired,
        shared,
    } = pump;
    let mut redial = false;
    // A wake right behind a failed wake-driven dial must not reset the schedule again.
    let mut wake_driven = false;
    let mut wake_failed_at: Option<Instant> = None;

    while !shared.shutdown.load(Ordering::Acquire) {
        shared.set_status(LinkStatus::Reconnecting);
        shared.opened.store(false, Ordering::Release);
        let attempt = if redial {
            match unless_signalled(&shared, &mut wakes, client.reconnect()).await {
                Ok(Ok(())) => {
                    redial = false;
                    attach(&client, &session, &shared, &mut wakes).await
                }
                Ok(Err(failure)) => {
                    log::debug!("terminal feed: channel rebuild failed: {failure}");
                    Attempt::Dropped
                }
                Err(attempt) => attempt,
            }
        } else {
            attach(&client, &session, &shared, &mut wakes).await
        };
        if shared.opened.load(Ordering::Acquire) {
            wake_driven = false;
            wake_failed_at = None;
        } else if wake_driven && matches!(attempt, Attempt::Dropped) {
            wake_failed_at = Some(Instant::now());
        }
        match attempt {
            Attempt::Rewake => {
                wake_reset(&shared, wake_failed_at, wakes.debounce * 2);
                wake_driven = true;
                redial = true;
                continue;
            }
            // Before the nap: the link strip renders nothing for a connected feed.
            Attempt::Dropped => shared.set_status(LinkStatus::Reconnecting),
            Attempt::Closed | Attempt::Shutdown => break,
            Attempt::AuthExpired => {
                // One credential the server refuses will be refused four more times.
                shared.set_status(LinkStatus::Disconnected);
                on_auth_expired();
                break;
            }
        }

        let step = shared.state().backoff_step;
        let wake = match backoff.get(step) {
            Some(delay) => {
                shared.state().backoff_step = step + 1;
                wait_backoff(&shared, &mut wakes, &waiter, *delay).await
            }
            None => {
                // Out of schedule: wait for the screen's retry or a wake.
                shared.set_status(LinkStatus::Disconnected);
                park(&shared, &mut wakes).await
            }
        };
        match wake {
            Wake::Shutdown => break,
            Wake::Network => {
                wake_reset(&shared, wake_failed_at, wakes.debounce * 2);
                wake_driven = true;
                redial = true;
            }
            Wake::Retry | Wake::Elapsed => {}
        }
    }

    shared.retire();
}

/// Callers pass two debounce windows: the debounce already spaces acted-on wakes a full window
/// apart.
fn wake_reset(shared: &Shared, failed_at: Option<Instant>, window: Duration) {
    if !failed_at.is_some_and(|at| at.elapsed() < window) {
        shared.reset_step();
    }
}

fn attach_request(
    session: &str,
    cols: u16,
    rows: u16,
    resume: ResumeTarget,
    client_instance_id: &str,
) -> AttachReq {
    AttachReq {
        session: session.to_string(),
        rows: u32::from(rows),
        cols: u32::from(cols),
        // Unset is the proto3 default, the hint-less shape.
        resume_space_id: resume.space_id.unwrap_or_default(),
        resume_tab_id: resume.tab_id.unwrap_or_default(),
        resume_pane_id: resume.pane_id.unwrap_or_default(),
        client_instance_id: client_instance_id.to_string(),
    }
}

/// A UUID v4, simple form. The server only compares it for equality, so it need not be secret.
fn mint_client_instance_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Reads the resume hint per attempt, so a reconnect carries the last move recorded before the
/// drop.
async fn attach(client: &MuxrClient, session: &str, shared: &Shared, wakes: &mut Wakes) -> Attempt {
    let (outbound, requests) = mpsc::channel(OUTBOUND_CAPACITY);
    let (cols, rows) = shared.dims();
    let request = attach_request(
        session,
        cols,
        rows,
        shared.resume.target(),
        &shared.client_instance_id,
    );
    if outbound
        .try_send(ClientFrame {
            kind: Some(client_frame::Kind::Attach(request)),
        })
        .is_err()
    {
        // Unreachable in practice; a failed attempt rather than a panic.
        return Attempt::Dropped;
    }
    // Published only after the attach frame is queued, so a keystroke cannot overtake it.
    shared.set_outbound(Some(outbound));

    // A fresh stub per attempt: the client's channel may have been rebuilt.
    let mut stub = client.authed();
    let opened = unless_signalled(
        shared,
        wakes,
        stub.attach_terminal(ReceiverStream::new(requests)),
    )
    .await;
    let stream = match opened {
        Ok(Ok(response)) => response.into_inner(),
        Ok(Err(status)) => {
            shared.set_outbound(None);
            return classify(&status);
        }
        Err(attempt) => {
            shared.set_outbound(None);
            return attempt;
        }
    };

    shared.opened.store(true, Ordering::Release);
    shared.set_status(LinkStatus::Connected);
    let outcome = pump_stream(stream, shared, wakes).await;
    shared.set_outbound(None);
    outcome
}

/// Reads only while the drain keeps up ([`await_capacity`]), so a UI that stops draining becomes
/// HTTP/2 backpressure on the server rather than unbounded growth here.
async fn pump_stream(
    mut stream: tonic::Streaming<ServerFrame>,
    shared: &Shared,
    wakes: &mut Wakes,
) -> Attempt {
    let mut saw_render = false;
    loop {
        if let Some(attempt) = await_capacity(shared, wakes).await {
            return attempt;
        }
        let message = match read_or_signal(shared, wakes, stream.message()).await {
            Event::Shutdown => return Attempt::Shutdown,
            // Returning drops the stream, which is what cancels the call.
            Event::Rewake => return Attempt::Rewake,
            // A retry raised on a live stream is just a step reset.
            Event::Skip => continue,
            Event::Message(message) => message,
        };

        let frame = match message {
            Ok(Some(frame)) => frame,
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
                    shared.set_status(LinkStatus::Closed);
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

enum Event<T> {
    Shutdown,
    Rewake,
    Skip,
    Message(T),
}

/// Priority: [`Shared::wake`], then the read, then the app's wake, so an `exit` the server
/// delivered stays terminal instead of becoming a re-attach to an ended session.
async fn read_or_signal<T>(
    shared: &Shared,
    wakes: &mut Wakes,
    read: impl Future<Output = T>,
) -> Event<T> {
    match race(shared.wake.notified(), race(read, wakes.next())).await {
        Winner::First(()) => match shared.take_wake() {
            Wake::Shutdown => Event::Shutdown,
            Wake::Retry | Wake::Elapsed | Wake::Network => Event::Skip,
        },
        Winner::Second(Winner::First(message)) => Event::Message(message),
        Winner::Second(Winner::Second(())) => Event::Rewake,
    }
}

fn classify(status: &tonic::Status) -> Attempt {
    if status.code() == Code::Unauthenticated {
        Attempt::AuthExpired
    } else {
        Attempt::Dropped
    }
}

/// `Some` when a shutdown or a wake ended the attempt instead, so teardown never waits on a drain.
/// Below the mark this is one lock and no await.
async fn await_capacity(shared: &Shared, wakes: &mut Wakes) -> Option<Attempt> {
    if !shared.inbound_saturated() {
        return None;
    }
    shared.count_read_pause();
    loop {
        // The drain's permit is stored, so the pause cannot miss its own wake.
        match race(signal(shared, wakes), shared.drained.notified()).await {
            Winner::First(Wake::Shutdown) => return Some(Attempt::Shutdown),
            Winner::First(Wake::Network) => return Some(Attempt::Rewake),
            Winner::First(Wake::Retry | Wake::Elapsed) | Winner::Second(()) => {}
        }
        // Re-read the level: a retry, a stale permit or a still-full queue all land here.
        if shared.inbound_has_room() {
            return None;
        }
    }
}

async fn wait_backoff(
    shared: &Shared,
    wakes: &mut Wakes,
    waiter: &Waiter,
    delay: Duration,
) -> Wake {
    match race(signal(shared, wakes), waiter.sleep(delay)).await {
        Winner::First(wake) => wake,
        Winner::Second(()) => Wake::Elapsed,
    }
}

async fn park(shared: &Shared, wakes: &mut Wakes) -> Wake {
    loop {
        match signal(shared, wakes).await {
            // A stale permit: still parked.
            Wake::Elapsed => continue,
            wake => return wake,
        }
    }
}

/// [`Shared::wake`] is polled first, so a pending shutdown wins a tie.
async fn signal(shared: &Shared, wakes: &mut Wakes) -> Wake {
    match race(shared.wake.notified(), wakes.next()).await {
        Winner::First(()) => shared.take_wake(),
        Winner::Second(()) => Wake::Network,
    }
}

/// A retry or a stale permit leaves `work` running.
async fn unless_signalled<F: Future>(
    shared: &Shared,
    wakes: &mut Wakes,
    work: F,
) -> Result<F::Output, Attempt> {
    let mut work = std::pin::pin!(work);
    loop {
        match race(signal(shared, wakes), work.as_mut()).await {
            Winner::First(Wake::Shutdown) => return Err(Attempt::Shutdown),
            Winner::First(Wake::Network) => return Err(Attempt::Rewake),
            Winner::First(Wake::Retry | Wake::Elapsed) => {}
            Winner::Second(output) => return Ok(output),
        }
    }
}

enum Winner<A, B> {
    First(A),
    Second(B),
}

/// Production `tokio` carries no `select!`; `first` wins a tie.
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
    use crate::net::wake::WakeBus;

    const TEST_BACKOFF: [Duration; 2] = [Duration::from_millis(5), Duration::from_millis(5)];

    /// Never waited out: the feed is dropped mid-nap.
    const LONG_BACKOFF: [Duration; 1] = [Duration::from_secs(2)];

    /// Still far longer than a burst published in one go.
    const TEST_DEBOUNCE: Duration = Duration::from_millis(150);

    /// Overflows the bus's per-subscriber buffer.
    const WAKE_FLOOD: usize = 64;

    const SESSION: &str = "alpha";

    /// A handful crosses [`INBOUND_HIGH_WATER`]; one stays under tonic's 4 MiB ceiling.
    const FLOOD_FRAME: usize = 256 * 1024;

    /// Twice the high-water mark, so the pump parks with half the flood still on the wire.
    const FLOOD_FRAMES: usize = 2 * INBOUND_HIGH_WATER / FLOOD_FRAME;

    const FLOOD_TOTAL: usize = FLOOD_FRAME * FLOOD_FRAMES;

    struct Rig {
        fixture: MuxrdFixture,
        client: MuxrClient,
        bus: WakeBus,
    }

    impl Rig {
        async fn start() -> Self {
            let fixture = MuxrdFixture::start().await;
            let client = fixture.connect_pinned().await;
            client.set_session_token(testing::SESSION_TOKEN);
            Self {
                fixture,
                client,
                bus: WakeBus::new(),
            }
        }

        fn script(&self) -> Arc<TerminalScript> {
            self.fixture.terminal()
        }

        fn wake(&self, reason: WakeReason) {
            self.bus.publish(reason);
        }

        fn connections(&self) -> usize {
            self.fixture.connections()
        }

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
                (cols, rows),
                Box::new(move || {
                    bump.fetch_add(1, Ordering::Relaxed);
                }),
                backoff,
                resume,
                Wakes::new(self.bus.subscribe(), TEST_DEBOUNCE, Waiter::Tokio),
            );
            tokio::spawn(pump);
            (feed, expiries)
        }
    }

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

    fn drained_bytes(feed: &mut GrpcTerminalFeed) -> usize {
        drain(feed).iter().map(Vec::len).sum()
    }

    async fn flood_until_paused(script: &TerminalScript, feed: &GrpcTerminalFeed) {
        for _ in 0..FLOOD_FRAMES {
            script.push_render(vec![b'x'; FLOOD_FRAME]);
        }
        wait_until("the pump parks on the high-water mark", || {
            feed.read_pauses() >= 1
        })
        .await;
    }

    /// Pushes small batches from inside the probe: 4096 frames overflow the fixture's 64-deep
    /// command channel.
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
            feed.status() == LinkStatus::Connected
        })
        .await;
        assert!(feed.is_open());
    }

    #[tokio::test]
    async fn a_reconnect_attaches_with_the_resume_target_the_screen_recorded() {
        let rig = Rig::start().await;
        let script = rig.script();
        let resume = Arc::new(ResumeSlot::default());
        let (feed, _) = rig.feed_resuming(80, 24, &TEST_BACKOFF, Arc::clone(&resume));
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
        assert_eq!(
            script.attaches().remove(0).resume_pane_id,
            0,
            "the first attach had nothing to resume"
        );

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

    #[tokio::test]
    async fn a_feed_reuses_its_client_instance_id_across_a_reattach() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        let id = feed.client_instance_id();
        assert_eq!(id.len(), 32, "a 128-bit id, as 32 hex characters");
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "the id is lowercase hex"
        );

        script.kill();
        wait_until("the feed re-attaches", || script.attach_count() == 2).await;
        wait_until("the re-attached stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
        assert_eq!(
            feed.client_instance_id(),
            id,
            "a re-attach reuses the id the mount minted"
        );

        let (other, _) = rig.feed(80, 24);
        assert_ne!(
            other.client_instance_id(),
            id,
            "a new mount (a new feed) mints a new id"
        );
    }

    #[test]
    fn attach_request_carries_the_instance_id_beside_the_resume_hints() {
        let resume = ResumeTarget {
            space_id: Some("ws-1".to_string()),
            tab_id: Some(3),
            pane_id: Some(31),
        };
        let req = attach_request(SESSION, 80, 24, resume, "0123456789abcdef0123456789abcdef");
        assert_eq!(req.client_instance_id, "0123456789abcdef0123456789abcdef");
        assert_eq!(
            (req.session.as_str(), req.cols, req.rows),
            (SESSION, 80, 24)
        );
        assert_eq!(
            (
                req.resume_space_id.as_str(),
                req.resume_tab_id,
                req.resume_pane_id
            ),
            ("ws-1", 3, 31)
        );
    }

    #[test]
    fn minted_instance_ids_are_32_lowercase_hex_and_distinct() {
        let (a, b) = (mint_client_instance_id(), mint_client_instance_id());
        for id in [&a, &b] {
            assert_eq!(id.len(), 32);
            assert!(
                id.bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "the id is lowercase hex"
            );
        }
        assert_ne!(a, b);
    }

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

    #[tokio::test]
    async fn input_reaches_the_server_and_render_bytes_drain_once_in_order() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
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

    #[tokio::test]
    async fn resize_reaches_the_server_and_is_replayed_on_reconnect() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
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

    #[tokio::test]
    async fn mouse_events_reach_the_server_as_mouse_frames() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
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

    #[tokio::test]
    async fn an_exit_control_closes_the_feed_without_reconnecting() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        script.send_exit();
        wait_until("the feed observes the close", || {
            feed.status() == LinkStatus::Closed
        })
        .await;
        wait_until("the pump returns", || !feed.is_open()).await;
        assert_eq!(
            script.attach_count(),
            1,
            "a clean close is not a drop: nothing re-attaches"
        );
    }

    #[tokio::test]
    async fn a_dropped_stream_advances_the_schedule_and_a_render_resets_it() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        // A stream that delivered nothing proves nothing, so each drop takes the next step.
        script.kill();
        wait_until("the first reconnect lands", || script.attach_count() == 2).await;
        assert_eq!(feed.backoff_step(), 1);

        script.kill();
        wait_until("the second reconnect lands", || script.attach_count() == 3).await;
        assert_eq!(feed.backoff_step(), 2);

        wait_until("the third stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
        script.push_render(b"live\r\n".to_vec());
        wait_until("the render frame lands", || feed.pending_writes() == 1).await;
        assert_eq!(feed.backoff_step(), 0);
    }

    #[tokio::test]
    async fn a_drop_reports_reconnecting_while_the_backoff_nap_runs() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed_with(80, 24, &LONG_BACKOFF);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        script.kill();
        wait_until("the drop lands", || feed.status() != LinkStatus::Connected).await;
        assert_eq!(
            feed.status(),
            LinkStatus::Reconnecting,
            "a feed waiting out a backoff step is reconnecting, not connected"
        );
        assert_eq!(
            script.attach_count(),
            1,
            "the nap is still running: nothing has re-attached to make that true trivially"
        );
    }

    #[tokio::test]
    async fn a_flood_pauses_the_reader_until_a_drain_makes_room() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
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

        let mut received = drained_bytes(&mut feed);
        wait_until("the rest of the flood arrives once reading resumes", || {
            received += drained_bytes(&mut feed);
            received == FLOOD_TOTAL
        })
        .await;
        assert_eq!(
            feed.status(),
            LinkStatus::Connected,
            "the session is intact"
        );
    }

    #[tokio::test]
    async fn a_flood_of_tiny_frames_pauses_the_reader_via_the_frame_count_cap() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
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

        drain(&mut feed);
        script.push_render(b"after\r\n".to_vec());
        wait_until("the session keeps delivering once reading resumes", || {
            feed.pending_writes() >= 1
        })
        .await;
        assert_eq!(
            feed.status(),
            LinkStatus::Connected,
            "the session is intact"
        );
    }

    #[tokio::test]
    async fn a_frame_larger_than_the_cap_is_accepted_whole() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
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

    #[tokio::test]
    async fn a_shutdown_wakes_a_reader_paused_on_the_high_water_mark() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
        flood_until_paused(&script, &feed).await;

        feed.shutdown();
        wait_until("the pump returns without a drain", || {
            feed.status() == LinkStatus::Disconnected
        })
        .await;
        assert_eq!(script.attach_count(), 1, "a shutdown never reconnects");
    }

    /// A `ServerFrame` stream is one sequence, so a pause holds back an `exit` too.
    #[tokio::test]
    async fn an_exit_behind_a_flood_closes_once_the_drain_resumes_the_reader() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
        flood_until_paused(&script, &feed).await;

        script.send_exit();
        assert_eq!(
            feed.status(),
            LinkStatus::Connected,
            "a paused reader reads nothing at all, control frames included"
        );

        let mut received = 0;
        wait_until("the close lands once draining resumes the reader", || {
            received += drained_bytes(&mut feed);
            feed.status() == LinkStatus::Closed
        })
        .await;
        received += drained_bytes(&mut feed);
        assert_eq!(
            received, FLOOD_TOTAL,
            "the whole flood arrived ahead of the close: a pause defers bytes, never drops them"
        );
        wait_until("the pump returns", || !feed.is_open()).await;
    }

    #[tokio::test]
    async fn an_exhausted_schedule_disconnects_until_reset_backoff_re_arms_it() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);

        // Two steps, so the third drop runs off the end.
        for attach in 1..=3 {
            wait_until("the stream is up", || {
                script.attach_count() == attach && feed.status() == LinkStatus::Connected
            })
            .await;
            script.kill();
        }
        wait_until("the schedule runs out", || {
            feed.status() == LinkStatus::Disconnected
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

    #[tokio::test]
    async fn unauthenticated_fires_the_callback_once_and_never_retries() {
        let rig = Rig::start().await;
        let script = rig.script();
        rig.client.set_session_token("not-the-fixtures-token");
        let (feed, expiries) = rig.feed(80, 24);

        wait_until("the feed gives up", || {
            feed.status() == LinkStatus::Disconnected
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

        feed.reset_backoff();
        assert_eq!(expiries.load(Ordering::Relaxed), 1);
        assert_eq!(script.attach_count(), 0);
    }

    #[tokio::test]
    async fn shutdown_ends_the_stream_and_finishes_the_pump() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        feed.shutdown();
        wait_until("the pump returns", || !feed.is_open()).await;
        assert_eq!(script.attach_count(), 1, "a shutdown never reconnects");
        assert_eq!(
            feed.status(),
            LinkStatus::Disconnected,
            "a torn-down feed stops claiming to be connected"
        );

        let mut feed = feed;
        feed.send_input(b"x");
        assert_eq!(feed.dropped_frames(), 1);
    }

    #[tokio::test]
    async fn the_fixture_sees_one_instance_id_across_a_feeds_reattach() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (_feed, _) = rig.feed(80, 24);
        wait_until("the first attach lands", || script.attach_count() == 1).await;

        script.kill();
        wait_until("the feed re-attaches", || script.attach_count() == 2).await;
        let attaches = script.attaches();
        let id = &attaches[0].client_instance_id;
        assert_eq!(id.len(), 32, "a non-empty 128-bit id, as 32 hex characters");
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(
            &attaches[1].client_instance_id, id,
            "the re-attach names the same client view"
        );
    }

    #[tokio::test]
    async fn the_fixture_sees_different_instance_ids_from_two_feeds() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (_first, _) = rig.feed(80, 24);
        let (_second, _) = rig.feed(80, 24);
        wait_until("both feeds attach", || script.attach_count() == 2).await;

        let attaches = script.attaches();
        assert!(!attaches[0].client_instance_id.is_empty());
        assert!(!attaches[1].client_instance_id.is_empty());
        assert_ne!(
            attaches[0].client_instance_id, attaches[1].client_instance_id,
            "each mount mints its own id"
        );
    }

    #[tokio::test]
    async fn a_wake_on_a_stalled_stream_re_attaches_at_once_on_a_fresh_channel() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
        script.stall();
        wait_until("the server goes silent", || script.stalls() == 1).await;
        assert_eq!(
            feed.status(),
            LinkStatus::Connected,
            "a stall looks healthy"
        );

        let woke = Instant::now();
        rig.wake(WakeReason::NetworkChanged);
        wait_until("the wake re-attaches", || script.attach_count() == 2).await;
        assert!(
            woke.elapsed() < WAKE_DEBOUNCE,
            "re-attached {:?} after the wake: a keepalive is {:?} away",
            woke.elapsed(),
            crate::net::channel::KEEP_ALIVE_INTERVAL
        );
        assert_eq!(rig.connections(), 2, "the re-attach dialled a new channel");
        let attaches = script.attaches();
        assert_eq!(
            attaches[1].client_instance_id, attaches[0].client_instance_id,
            "the same client view re-attaches"
        );
        wait_until("the new stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
    }

    /// The pump polls its wakes ahead of every read, so once this returns any earlier wake was
    /// acted on or debounced.
    async fn round_trip(script: &TerminalScript, feed: &mut GrpcTerminalFeed) {
        let seen = script.inputs().len();
        feed.send_input(b"!");
        wait_until("the echo comes back", || feed.pending_writes() >= 1).await;
        assert_eq!(script.inputs().len(), seen + 1);
        drain(feed);
    }

    #[tokio::test]
    async fn a_burst_of_wakes_inside_the_debounce_rebuilds_twice_at_most() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        let burst = Instant::now();
        for reason in [WakeReason::NetworkChanged, WakeReason::Foregrounded] {
            rig.wake(reason);
            rig.wake(reason);
            tokio::task::yield_now().await;
        }
        rig.wake(WakeReason::Foregrounded);
        assert!(burst.elapsed() < TEST_DEBOUNCE, "the burst fits the window");

        wait_until("the leading and the trailing wake re-attach", || {
            script.attach_count() == 3 && feed.status() == LinkStatus::Connected
        })
        .await;
        round_trip(&script, &mut feed).await;
        tokio::time::sleep(2 * TEST_DEBOUNCE).await;
        assert_eq!(script.attach_count(), 3, "the leading and one trailing");
        assert_eq!(rig.connections(), 3, "two rebuilds for the burst, not five");
    }

    #[tokio::test]
    async fn a_wake_inside_the_window_acts_once_more_when_it_closes() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        let first = Instant::now();
        rig.wake(WakeReason::NetworkChanged);
        wait_until("the first wake re-attaches", || {
            script.attach_count() == 2 && feed.status() == LinkStatus::Connected
        })
        .await;
        assert_eq!(rig.connections(), 2);

        tokio::time::sleep(TEST_DEBOUNCE / 3).await;
        rig.wake(WakeReason::Foregrounded);
        tokio::time::sleep(TEST_DEBOUNCE / 3).await;
        assert!(first.elapsed() < TEST_DEBOUNCE, "still inside the window");
        assert_eq!(script.attach_count(), 2, "nothing before the window ends");
        assert_eq!(rig.connections(), 2);

        wait_until("the trailing wake re-attaches", || {
            script.attach_count() == 3 && feed.status() == LinkStatus::Connected
        })
        .await;
        assert!(
            first.elapsed() >= TEST_DEBOUNCE,
            "acted {:?} after the first wake, inside its window",
            first.elapsed()
        );
        assert_eq!(rig.connections(), 3);
        tokio::time::sleep(2 * TEST_DEBOUNCE).await;
        assert_eq!(script.attach_count(), 3, "once, not repeatedly");
        assert_eq!(rig.connections(), 3);
    }

    #[tokio::test]
    async fn a_wake_after_the_debounce_window_acts_again() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        rig.wake(WakeReason::Foregrounded);
        wait_until("the first wake re-attaches", || {
            script.attach_count() == 2 && feed.status() == LinkStatus::Connected
        })
        .await;
        tokio::time::sleep(TEST_DEBOUNCE).await;
        rig.wake(WakeReason::NetworkChanged);
        wait_until("the second wake re-attaches", || script.attach_count() == 3).await;
        assert_eq!(rig.connections(), 3);
    }

    #[tokio::test]
    async fn a_wake_during_a_backoff_nap_cuts_the_nap_short() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed_with(80, 24, &LONG_BACKOFF);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
        script.kill();
        wait_until("the drop lands", || feed.status() != LinkStatus::Connected).await;
        assert_eq!(feed.backoff_step(), 1, "the nap is under way");

        let woke = Instant::now();
        rig.wake(WakeReason::NetworkChanged);
        wait_until("the wake re-attaches", || script.attach_count() == 2).await;
        assert!(
            woke.elapsed() < LONG_BACKOFF[0],
            "the nap was cut short, not waited out"
        );
        assert_eq!(feed.backoff_step(), 0, "a wake starts the schedule over");
        assert_eq!(rig.connections(), 2, "on a freshly dialled channel");
    }

    #[tokio::test]
    async fn a_wake_re_attaches_a_disconnected_feed_without_a_retry() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        for attach in 1..=3 {
            wait_until("the stream is up", || {
                script.attach_count() == attach && feed.status() == LinkStatus::Connected
            })
            .await;
            script.kill();
        }
        wait_until("the schedule runs out", || {
            feed.status() == LinkStatus::Disconnected
        })
        .await;

        rig.wake(WakeReason::Foregrounded);
        wait_until("the wake re-attaches", || {
            script.attach_count() == 4 && feed.status() == LinkStatus::Connected
        })
        .await;
        assert_eq!(feed.backoff_step(), 0);
        assert_eq!(rig.connections(), 2);
    }

    #[tokio::test]
    async fn wakes_after_an_exit_do_nothing() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
        script.send_exit();
        wait_until("the pump returns", || !feed.is_open()).await;

        rig.wake(WakeReason::NetworkChanged);
        rig.wake(WakeReason::Foregrounded);
        tokio::task::yield_now().await;
        assert_eq!(feed.status(), LinkStatus::Closed);
        assert_eq!(script.attach_count(), 1);
        assert_eq!(rig.connections(), 1);
    }

    #[tokio::test]
    async fn an_exit_published_with_a_suppressed_wake_leaves_the_feed_closed() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        rig.wake(WakeReason::NetworkChanged);
        wait_until("the first wake re-attaches", || {
            script.attach_count() == 2 && feed.status() == LinkStatus::Connected
        })
        .await;
        rig.wake(WakeReason::Foregrounded);
        script.send_exit();
        wait_until("the pump returns", || !feed.is_open()).await;

        tokio::time::sleep(2 * TEST_DEBOUNCE).await;
        assert_eq!(feed.status(), LinkStatus::Closed);
        assert_eq!(script.attach_count(), 2);
        assert_eq!(rig.connections(), 2);
    }

    #[tokio::test]
    async fn a_buffered_frame_wins_over_a_ready_wake_and_a_shutdown_over_both() {
        let bus = WakeBus::new();
        let shared = Shared::new(80, 24, Arc::new(ResumeSlot::default()));
        let mut wakes = Wakes::new(bus.subscribe(), TEST_DEBOUNCE, Waiter::Tokio);

        bus.publish(WakeReason::NetworkChanged);
        let event = read_or_signal(&shared, &mut wakes, std::future::ready(7)).await;
        assert!(
            matches!(event, Event::Message(7)),
            "the buffered frame wins"
        );

        let event = read_or_signal(&shared, &mut wakes, std::future::pending::<u8>()).await;
        assert!(
            matches!(event, Event::Rewake),
            "…and the wake is still owed once nothing is buffered"
        );

        bus.publish(WakeReason::NetworkChanged);
        shared.shutdown.store(true, Ordering::Release);
        shared.wake.notify_one();
        let event = read_or_signal(&shared, &mut wakes, std::future::ready(7)).await;
        assert!(matches!(event, Event::Shutdown), "shutdown beats both");
    }

    #[tokio::test]
    async fn wakes_after_an_auth_expiry_do_nothing() {
        let rig = Rig::start().await;
        rig.client.set_session_token("not-the-fixtures-token");
        let (feed, expiries) = rig.feed(80, 24);
        wait_until("the pump returns", || !feed.is_open()).await;

        rig.wake(WakeReason::NetworkChanged);
        tokio::task::yield_now().await;
        assert_eq!(feed.status(), LinkStatus::Disconnected);
        assert_eq!(expiries.load(Ordering::Relaxed), 1);
        assert_eq!(rig.connections(), 1);
    }

    #[tokio::test]
    async fn a_lagged_wake_receiver_re_attaches_exactly_once() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (mut feed, _) = rig.feed(80, 24);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        for _ in 0..WAKE_FLOOD {
            rig.wake(WakeReason::NetworkChanged);
        }
        wait_until("the wake re-attaches", || {
            script.attach_count() == 2 && feed.status() == LinkStatus::Connected
        })
        .await;
        round_trip(&script, &mut feed).await;
        assert_eq!(script.attach_count(), 2);
        assert_eq!(rig.connections(), 2);
    }

    #[tokio::test]
    async fn wakes_fold_a_lagged_burst_and_hold_one_inside_the_window_until_it_closes() {
        let bus = WakeBus::new();
        let mut wakes = Wakes::new(bus.subscribe(), TEST_DEBOUNCE, Waiter::Tokio);
        for _ in 0..WAKE_FLOOD {
            bus.publish(WakeReason::Foregrounded);
        }
        wakes.next().await;
        let acted = Instant::now();
        assert!(wakes.rx.is_empty(), "the whole lagged burst is one wake");

        bus.publish(WakeReason::NetworkChanged);
        bus.publish(WakeReason::NetworkChanged);
        let inside = race(wakes.next(), tokio::task::yield_now()).await;
        assert!(matches!(inside, Winner::Second(())), "inside the window");
        assert!(wakes.rx.is_empty(), "…consumed, not left queued");
        assert!(wakes.pending, "…and remembered on the struct");

        // The wait above was dropped mid-way; a later one still resolves, once the window ends.
        wakes.next().await;
        assert!(
            acted.elapsed() >= TEST_DEBOUNCE,
            "not before the window closed"
        );
        assert!(!wakes.pending);
        let again = race(wakes.next(), tokio::time::sleep(2 * TEST_DEBOUNCE)).await;
        assert!(matches!(again, Winner::Second(())), "owed exactly once");

        tokio::time::sleep(TEST_DEBOUNCE).await;
        bus.publish(WakeReason::NetworkChanged);
        let after = race(wakes.next(), tokio::task::yield_now()).await;
        assert!(
            matches!(after, Winner::First(())),
            "after the window: at once"
        );
    }

    #[tokio::test]
    async fn wakes_behind_a_failed_wake_driven_dial_do_not_restart_the_schedule() {
        const NAP: [Duration; 2] = [Duration::from_millis(40), Duration::from_millis(40)];
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed_with(80, 24, &NAP);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
        script.refuse_attaches(true);
        script.kill();
        wait_until("the schedule runs out", || {
            feed.status() == LinkStatus::Disconnected && script.refused() == 2
        })
        .await;
        assert_eq!(feed.backoff_step(), 2);
        assert_eq!(rig.connections(), 1);

        rig.wake(WakeReason::NetworkChanged);
        wait_until("the first wake climbs the schedule again", || {
            feed.status() == LinkStatus::Disconnected && script.refused() == 5
        })
        .await;
        assert_eq!(feed.backoff_step(), 2);
        assert_eq!(rig.connections(), 2);

        for wake in 1..=3 {
            tokio::time::sleep(TEST_DEBOUNCE + Duration::from_millis(10)).await;
            rig.wake(WakeReason::Foregrounded);
            wait_until("the wake costs one attempt", || {
                script.refused() == 5 + wake
                    && rig.connections() == 2 + wake
                    && feed.status() == LinkStatus::Disconnected
            })
            .await;
            assert_eq!(feed.backoff_step(), 2, "the schedule was not restarted");
            assert_eq!(feed.status(), LinkStatus::Disconnected);
        }
        assert_eq!(
            rig.connections(),
            5,
            "one dial per wake, never a fresh climb"
        );
    }

    #[tokio::test]
    async fn a_success_clears_a_failed_wake_driven_dial_before_the_next_wake() {
        const NAP: [Duration; 2] = [Duration::from_millis(40), Duration::from_millis(40)];
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed_with(80, 24, &NAP);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;
        script.refuse_attaches(true);
        script.kill();
        wait_until("the schedule runs out", || {
            feed.status() == LinkStatus::Disconnected && script.refused() == 2
        })
        .await;

        rig.wake(WakeReason::NetworkChanged);
        wait_until("the wake-driven attempts fail", || {
            feed.status() == LinkStatus::Disconnected && script.refused() == 5
        })
        .await;
        script.refuse_attaches(false);

        rig.wake(WakeReason::Foregrounded);
        wait_until("the second wake attaches", || {
            script.attach_count() == 2 && feed.status() == LinkStatus::Connected
        })
        .await;
        script.kill();
        wait_until("the schedule is spent again", || {
            feed.status() == LinkStatus::Disconnected
        })
        .await;
        assert_eq!(feed.backoff_step(), 2);

        rig.wake(WakeReason::NetworkChanged);
        wait_until("the third wake attaches", || {
            script.attach_count() == 3 && feed.status() == LinkStatus::Connected
        })
        .await;
        assert_eq!(feed.backoff_step(), 0, "the success cleared the failure");
    }

    #[tokio::test]
    async fn an_owed_wake_costs_one_timer_however_often_its_wait_is_rebuilt() {
        let bus = WakeBus::new();
        let mut wakes = Wakes::new(bus.subscribe(), TEST_DEBOUNCE, Waiter::Tokio);
        bus.publish(WakeReason::Foregrounded);
        wakes.next().await;
        bus.publish(WakeReason::NetworkChanged);

        for _ in 0..50 {
            let inside = race(wakes.next(), tokio::task::yield_now()).await;
            assert!(matches!(inside, Winner::Second(())));
        }
        assert!(wakes.pending);
        assert_eq!(wakes.timers, 1, "one timer for the owed wake");

        wakes.next().await;
        assert_eq!(wakes.timers, 1, "the close needed no second timer");
        let again = race(wakes.next(), tokio::time::sleep(2 * TEST_DEBOUNCE)).await;
        assert!(matches!(again, Winner::Second(())), "acted exactly once");
        assert_eq!(wakes.timers, 1);
    }

    #[tokio::test]
    async fn a_late_timer_signal_does_not_cost_the_next_owed_wake_a_second_timer() {
        let bus = WakeBus::new();
        let mut wakes = Wakes::new(bus.subscribe(), TEST_DEBOUNCE, Waiter::Tokio);
        bus.publish(WakeReason::Foregrounded);
        wakes.next().await;

        // Act through the clock before ever awaiting the signal.
        bus.publish(WakeReason::NetworkChanged);
        let inside = race(wakes.next(), tokio::task::yield_now()).await;
        assert!(matches!(inside, Winner::Second(())));
        assert_eq!(wakes.timers, 1);
        tokio::time::sleep(TEST_DEBOUNCE + Duration::from_millis(30)).await;
        wakes.next().await;
        assert!(!wakes.pending);
        // The timer fired meanwhile; its signal must not outlive the wake it served.
        tokio::task::yield_now().await;

        bus.publish(WakeReason::NetworkChanged);
        let inside = race(wakes.next(), tokio::task::yield_now()).await;
        assert!(matches!(inside, Winner::Second(())));
        let owed = Instant::now();
        wakes.next().await;
        assert!(
            owed.elapsed() > Duration::from_millis(20),
            "not on a stale permit"
        );
        assert_eq!(wakes.timers, 2, "one timer for each owed wake");
        let again = race(wakes.next(), tokio::time::sleep(2 * TEST_DEBOUNCE)).await;
        assert!(matches!(again, Winner::Second(())), "acted exactly once");
        assert_eq!(wakes.timers, 2);
    }

    #[tokio::test]
    async fn a_wake_after_a_successful_attach_still_restarts_the_schedule() {
        let rig = Rig::start().await;
        let script = rig.script();
        let (feed, _) = rig.feed_with(80, 24, &LONG_BACKOFF);
        wait_until("the stream is up", || {
            feed.status() == LinkStatus::Connected
        })
        .await;

        for attach in 2..=3 {
            script.kill();
            wait_until("the nap is under way", || feed.backoff_step() == 1).await;
            tokio::time::sleep(TEST_DEBOUNCE).await;
            rig.wake(WakeReason::NetworkChanged);
            wait_until("the wake re-attaches", || {
                script.attach_count() == attach && feed.status() == LinkStatus::Connected
            })
            .await;
            assert_eq!(feed.backoff_step(), 0, "a wake starts the schedule over");
        }
        assert_eq!(rig.connections(), 3);
    }

    #[test]
    fn the_shipped_schedule_is_one_two_four_eight_sixteen_seconds_with_no_jitter() {
        assert_eq!(
            RECONNECT_BACKOFF.map(|step| step.as_secs()),
            [1, 2, 4, 8, 16]
        );
    }

    #[test]
    fn the_shipped_inbound_bound_is_two_mib_resuming_at_a_quarter_of_it() {
        assert_eq!(INBOUND_HIGH_WATER, 2 * 1024 * 1024);
        assert_eq!(INBOUND_LOW_WATER, 512 * 1024);
    }

    #[test]
    fn the_shipped_inbound_frame_bound_is_4096_resuming_at_a_quarter_of_it() {
        assert_eq!(INBOUND_HIGH_WATER_FRAMES, 4096);
        assert_eq!(INBOUND_LOW_WATER_FRAMES, 1024);
    }
}
