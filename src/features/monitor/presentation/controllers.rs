//! Monitor view models: the app-scoped vitals summary and the per-mount vitals and process views.
//! Every stream is fenced by a generation; a screen calls `observe` from `build`, so a connection
//! epoch change ends the old server's stream and state before anything reopens.

use std::collections::VecDeque;
use std::future::{Future, poll_fn};
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clean_signals::ControllerCore;
use frust::{Get, GetUntracked, RwSignal, Set, Update, WithUntracked};
use tokio::sync::{Notify, broadcast};

use crate::connection::{CAP_HOST_VITALS, ConnectionController};
use crate::failure::AppFailure;
use crate::features::monitor::di::SharedMonitorRepository;
use crate::features::monitor::domain::entities::{
    HistoryRange, HostInfo, ProcessItem, ProcessQuery, ProcessSignal, ProcessSnapshot, ProcessSort,
    SeriesKind, VitalsDetail, VitalsHistory, VitalsItem, VitalsSample,
};
use crate::features::monitor::domain::repositories::MonitorWatch;
use crate::net::wake::WakeReason;

use super::series::SeriesBuffer;

pub const INTERVALS_MS: [u32; 3] = [2_000, 3_000, 5_000];
pub const DEFAULT_INTERVAL_MS: u32 = 3_000;
pub const SUMMARY_INTERVAL_MS: u32 = 5_000;
pub const LIVE_WINDOW_MS: u32 = 60_000;
pub const FILTER_DEBOUNCE: Duration = Duration::from_millis(300);
pub const PROCESS_LIMIT: u32 = 50;
pub const PROCESS_INTERVAL_MS: u32 = 3_000;
pub const SIGNAL_IN_FLIGHT: &str = "a signal is already on its way";
pub const MONITOR_UNAVAILABLE: &str = "host monitoring is not available on this connection";

const INBOX_CAPACITY: usize = 64;

const LIVE_KINDS: [SeriesKind; 4] = [
    SeriesKind::CpuTotal,
    SeriesKind::CpuSystem,
    SeriesKind::NetRxBps,
    SeriesKind::NetTxBps,
];

/// The server aligns each stream to its own sampling ticks, so spacing can exceed the interval by a tick.
pub fn stale_after(interval_ms: u32) -> Duration {
    Duration::from_millis(2 * u64::from(interval_ms) + 2_000)
}

pub fn live_capacity(interval_ms: u32) -> usize {
    (LIVE_WINDOW_MS / interval_ms.max(1)).max(1) as usize
}

pub type Task = Pin<Box<dyn Future<Output = ()>>>;
type SleepFn = Arc<dyn Fn(Duration) -> Task + Send + Sync>;
type WakesFn = Arc<dyn Fn() -> broadcast::Receiver<WakeReason> + Send + Sync>;
type Permit = Arc<dyn Fn() -> bool + Send + Sync>;
type Sink<I> = Box<dyn Fn(I) + Send>;

/// Where the controllers' delayed and background work runs; a suite swaps in a manual clock.
#[derive(Clone)]
pub struct Scheduler {
    spawn: fn(Task),
    sleep: SleepFn,
    wakes: WakesFn,
}

impl Scheduler {
    pub fn ui() -> Self {
        Self::new(
            frust::spawn_local,
            |delay| Box::pin(clean_signals::time::sleep(delay)),
            crate::net::wake::subscribe,
        )
    }

    pub fn new(
        spawn: fn(Task),
        sleep: impl Fn(Duration) -> Task + Send + Sync + 'static,
        wakes: impl Fn() -> broadcast::Receiver<WakeReason> + Send + Sync + 'static,
    ) -> Self {
        Self {
            spawn,
            sleep: Arc::new(sleep),
            wakes: Arc::new(wakes),
        }
    }

    fn run(&self, task: impl Future<Output = ()> + 'static) {
        (self.spawn)(Box::pin(task));
    }
}

#[derive(Clone)]
pub struct MonitorEnv {
    repository: SharedMonitorRepository,
    connection: Arc<ConnectionController>,
    permitted: Permit,
    scheduler: Scheduler,
}

impl MonitorEnv {
    pub fn new(
        repository: SharedMonitorRepository,
        connection: Arc<ConnectionController>,
        permitted: impl Fn() -> bool + Send + Sync + 'static,
        scheduler: Scheduler,
    ) -> Self {
        Self {
            repository,
            connection,
            permitted: Arc::new(permitted),
            scheduler,
        }
    }

    fn permitted(&self) -> bool {
        (self.permitted)()
    }

    fn gate(&self) -> Gate {
        self.connection
            .server_info
            .try_with_untracked(|info| match info {
                None => Gate::Disconnected,
                Some(info) if info.supports(CAP_HOST_VITALS) => Gate::Open,
                Some(_) => Gate::Unsupported,
            })
            .unwrap_or(Gate::Disconnected)
    }

    fn epoch(&self) -> Option<u64> {
        self.connection.connection_epoch.try_get_untracked()
    }

    fn track_epoch(&self) {
        let _ = self.connection.connection_epoch.try_get();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Live,
    Stale { since_unix_ms: i64 },
    Disabled,
    Unsupported,
    Disconnected,
}

impl Link {
    pub fn is_stale(self) -> bool {
        matches!(self, Link::Stale { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    Disconnected,
    Unsupported,
    Open,
}

impl Gate {
    fn link(self) -> Link {
        match self {
            Gate::Disconnected => Link::Disconnected,
            Gate::Unsupported => Link::Unsupported,
            Gate::Open => Link::Live,
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Read-compare-then-write, so an unchanged value never wakes a rebuild.
fn put<T: PartialEq + Send + Sync + 'static>(signal: RwSignal<T>, value: T) {
    if signal.try_with_untracked(|current| *current != value) == Some(true) {
        let _ = signal.try_set(value);
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

struct Inbox<I> {
    queue: Mutex<VecDeque<I>>,
    ready: Notify,
    closed: AtomicBool,
}

impl<I> Inbox<I> {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            ready: Notify::new(),
            closed: AtomicBool::new(false),
        })
    }

    fn push(&self, item: I) {
        {
            let mut queue = lock(&self.queue);
            if queue.len() >= INBOX_CAPACITY {
                queue.pop_front();
            }
            queue.push_back(item);
        }
        self.ready.notify_one();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.ready.notify_one();
    }

    async fn next_batch(&self) -> Option<Vec<I>> {
        loop {
            let closed = self.closed.load(Ordering::SeqCst);
            let batch: Vec<I> = lock(&self.queue).drain(..).collect();
            if !batch.is_empty() {
                return Some(batch);
            }
            if closed {
                return None;
            }
            self.ready.notified().await;
        }
    }
}

struct InboxSender<I>(Arc<Inbox<I>>);

impl<I> Drop for InboxSender<I> {
    fn drop(&mut self) {
        self.0.close();
    }
}

fn sink_into<I: Send + 'static>(inbox: &Arc<Inbox<I>>) -> Sink<I> {
    let sender = InboxSender(Arc::clone(inbox));
    Box::new(move |item| sender.0.push(item))
}

type Opened<I> = Option<(MonitorWatch, Arc<Inbox<I>>)>;

struct Stream<I> {
    generation: AtomicU64,
    seq: AtomicU64,
    open: Mutex<Opened<I>>,
}

impl<I> Stream<I> {
    fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            seq: AtomicU64::new(0),
            open: Mutex::new(None),
        }
    }

    fn current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    fn is_open(&self) -> bool {
        lock(&self.open).is_some()
    }

    fn close(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let ended = lock(&self.open).take();
        if let Some((watch, inbox)) = ended {
            drop(watch);
            inbox.close();
        }
    }
}

#[derive(Default)]
struct EpochMark(Mutex<Option<u64>>);

impl EpochMark {
    /// The first look only records the epoch.
    fn moved(&self, env: &MonitorEnv) -> bool {
        let Some(epoch) = env.epoch() else {
            return false;
        };
        let mut seen = lock(&self.0);
        let moved = seen.is_some_and(|seen| seen != epoch);
        *seen = Some(epoch);
        moved
    }

    fn is_current(&self, env: &MonitorEnv) -> bool {
        *lock(&self.0) == env.epoch()
    }
}

trait Streamed: Send + Sync + Sized + 'static {
    type Item: Send + 'static;

    fn stream(&self) -> &Stream<Self::Item>;
    fn env(&self) -> &MonitorEnv;
    fn controller_core(&self) -> &ControllerCore<AppFailure>;
    fn quiet_limit(&self) -> Duration;
    fn absorb(self: &Arc<Self>, generation: u64, items: Vec<Self::Item>);
    fn went_quiet(self: &Arc<Self>);
    fn woken(self: &Arc<Self>);
}

fn launch<C: Streamed>(
    controller: &Arc<C>,
    watch: impl FnOnce(Sink<C::Item>) -> MonitorWatch,
) -> u64 {
    let stream = controller.stream();
    stream.close();
    let generation = stream.generation.load(Ordering::SeqCst);
    let inbox = Inbox::new();
    let handle = watch(sink_into(&inbox));
    *lock(&stream.open) = Some((handle, Arc::clone(&inbox)));
    arm_watchdog(controller, generation);
    let weak = Arc::downgrade(controller);
    controller.env().scheduler.run(async move {
        while let Some(batch) = inbox.next_batch().await {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            if controller.controller_core().is_disposed()
                || !controller.stream().current(generation)
            {
                return;
            }
            controller.absorb(generation, batch);
            if controller.stream().current(generation) {
                arm_watchdog(&controller, generation);
            }
        }
    });
    generation
}

fn arm_watchdog<C: Streamed>(controller: &Arc<C>, generation: u64) {
    let seq = controller.stream().seq.fetch_add(1, Ordering::SeqCst) + 1;
    let quiet = (controller.env().scheduler.sleep)(controller.quiet_limit());
    let weak = Arc::downgrade(controller);
    controller.env().scheduler.run(async move {
        quiet.await;
        let Some(controller) = weak.upgrade() else {
            return;
        };
        let stream = controller.stream();
        if !controller.controller_core().is_disposed()
            && stream.current(generation)
            && stream.seq.load(Ordering::SeqCst) == seq
        {
            controller.went_quiet();
        }
    });
}

fn listen_for_wakes<C: Streamed>(controller: &Arc<C>, cancel: Arc<Notify>) {
    let mut wakes = (controller.env().scheduler.wakes)();
    let weak: Weak<C> = Arc::downgrade(controller);
    controller.env().scheduler.run(async move {
        while next_wake(&mut wakes, &cancel).await {
            let Some(controller) = weak.upgrade() else {
                return;
            };
            if controller.controller_core().is_disposed() {
                return;
            }
            controller.woken();
        }
    });
}

/// Hooks dispose and the wake loop the first time a controller starts.
fn hook_once<C: Streamed>(controller: &Arc<C>, hooked: &AtomicBool, cancel: &Arc<Notify>) {
    if hooked.swap(true, Ordering::SeqCst) {
        return;
    }
    let owner = Arc::downgrade(controller);
    let stop = Arc::clone(cancel);
    controller.controller_core().on_dispose(move || {
        stop.notify_one();
        if let Some(controller) = owner.upgrade() {
            controller.stream().close();
        }
    });
    listen_for_wakes(controller, Arc::clone(cancel));
}

async fn next_wake(wakes: &mut broadcast::Receiver<WakeReason>, cancel: &Notify) -> bool {
    let mut cancelled = pin!(cancel.notified());
    let mut woken = pin!(wakes.recv());
    poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(false);
        }
        woken
            .as_mut()
            .poll(cx)
            .map(|outcome| !matches!(outcome, Err(broadcast::error::RecvError::Closed)))
    })
    .await
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiskFill {
    pub mount: String,
    pub used_fraction: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VitalsSummary {
    pub cpu_percent: f32,
    pub memory_used_fraction: f32,
    pub fullest_disk: Option<DiskFill>,
}

fn fraction(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64) as f32
    }
}

impl VitalsSummary {
    pub fn of(sample: &VitalsSample) -> Self {
        let fullest_disk = sample
            .disks
            .iter()
            .filter(|disk| disk.total_bytes > 0)
            .map(|disk| DiskFill {
                mount: disk.mount.clone(),
                used_fraction: fraction(disk.used_bytes, disk.total_bytes),
            })
            .max_by(|a, b| a.used_fraction.total_cmp(&b.used_fraction));
        Self {
            cpu_percent: sample.cpu_percent,
            memory_used_fraction: fraction(sample.memory.used, sample.memory.total),
            fullest_disk,
        }
    }
}

/// App-scoped: `start`/`stop` are counted, so two mounts share one stream.
pub struct VitalsSummaryController {
    core: ControllerCore<AppFailure>,
    env: MonitorEnv,
    stream: Stream<VitalsItem>,
    epoch: EpochMark,
    starts: Mutex<usize>,
    disabled: AtomicBool,
    hooked: AtomicBool,
    cancel: Arc<Notify>,
    pub available: RwSignal<bool>,
    pub summary: RwSignal<Option<VitalsSummary>>,
    pub stale: RwSignal<bool>,
}

impl VitalsSummaryController {
    pub fn new(env: MonitorEnv) -> Self {
        Self {
            core: ControllerCore::new(),
            env,
            stream: Stream::new(),
            epoch: EpochMark::default(),
            starts: Mutex::new(0),
            disabled: AtomicBool::new(false),
            hooked: AtomicBool::new(false),
            cancel: Arc::new(Notify::new()),
            available: RwSignal::new(false),
            summary: RwSignal::new(None),
            stale: RwSignal::new(false),
        }
    }

    pub fn start(self: &Arc<Self>) {
        if self.core.is_disposed() {
            return;
        }
        hook_once(self, &self.hooked, &self.cancel);
        *lock(&self.starts) += 1;
        self.reconcile();
    }

    pub fn stop(&self) {
        let idle = {
            let mut starts = lock(&self.starts);
            *starts = starts.saturating_sub(1);
            *starts == 0
        };
        if idle {
            self.stream.close();
        }
    }

    pub fn observe(self: &Arc<Self>) {
        self.env.track_epoch();
        self.reconcile();
    }

    fn reconcile(self: &Arc<Self>) {
        if self.core.is_disposed() {
            return;
        }
        if self.epoch.moved(&self.env) {
            self.forget_server();
        }
        let available = self.env.gate() == Gate::Open && !self.disabled.load(Ordering::SeqCst);
        put(self.available, available);
        let wanted = available && *lock(&self.starts) > 0 && self.env.permitted();
        if !wanted {
            self.stream.close();
        } else if !self.stream.is_open() {
            let repository = Arc::clone(&self.env.repository);
            launch(self, |sink| {
                repository.watch_vitals(VitalsDetail::Summary, sink)
            });
        }
    }

    fn forget_server(&self) {
        self.stream.close();
        self.disabled.store(false, Ordering::SeqCst);
        put(self.summary, None);
        put(self.stale, false);
    }
}

impl Streamed for VitalsSummaryController {
    type Item = VitalsItem;

    fn stream(&self) -> &Stream<VitalsItem> {
        &self.stream
    }

    fn env(&self) -> &MonitorEnv {
        &self.env
    }

    fn controller_core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    fn quiet_limit(&self) -> Duration {
        stale_after(SUMMARY_INTERVAL_MS)
    }

    fn absorb(self: &Arc<Self>, _generation: u64, items: Vec<VitalsItem>) {
        if !self.epoch.is_current(&self.env) {
            self.reconcile();
            return;
        }
        let mut summary = None;
        let mut stale = None;
        for item in items {
            match item {
                VitalsItem::Host(_) => {}
                VitalsItem::Sample(sample) => {
                    summary = Some(VitalsSummary::of(&sample));
                    stale = Some(false);
                }
                VitalsItem::Live => stale = Some(false),
                VitalsItem::Dropped => stale = Some(true),
                VitalsItem::Disabled => {
                    self.disabled.store(true, Ordering::SeqCst);
                    self.stream.close();
                    put(self.available, false);
                    put(self.summary, None);
                    put(self.stale, false);
                    return;
                }
            }
        }
        if let Some(summary) = summary {
            put(self.summary, Some(summary));
        }
        if let Some(stale) = stale {
            put(self.stale, stale);
        }
    }

    fn went_quiet(self: &Arc<Self>) {
        put(self.stale, true);
    }

    fn woken(self: &Arc<Self>) {
        if self.stale.try_get_untracked() == Some(true) && self.stream.is_open() {
            self.stream.close();
            self.reconcile();
        }
    }
}

impl AsRef<ControllerCore<AppFailure>> for VitalsSummaryController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LiveSeries {
    pub cpu_total: SeriesBuffer,
    pub cpu_system: SeriesBuffer,
    pub net_rx: SeriesBuffer,
    pub net_tx: SeriesBuffer,
}

impl LiveSeries {
    pub fn new(capacity: usize) -> Self {
        Self {
            cpu_total: SeriesBuffer::new(capacity),
            cpu_system: SeriesBuffer::new(capacity),
            net_rx: SeriesBuffer::new(capacity),
            net_tx: SeriesBuffer::new(capacity),
        }
    }

    fn buffers_mut(&mut self) -> [&mut SeriesBuffer; 4] {
        [
            &mut self.cpu_total,
            &mut self.cpu_system,
            &mut self.net_rx,
            &mut self.net_tx,
        ]
    }

    fn push(&mut self, point: [Option<f32>; 4]) {
        for (buffer, slot) in self.buffers_mut().into_iter().zip(point) {
            buffer.push_slot(slot);
        }
    }

    fn push_gap(&mut self) {
        self.push([None; 4]);
    }

    fn reset(&mut self, capacity: usize) {
        for buffer in self.buffers_mut() {
            buffer.reset(capacity);
        }
    }

    /// History resampled to the live spacing, then the `kept` newest live slots after it.
    fn seed(&mut self, history: &VitalsHistory, interval_ms: u32, kept: usize) {
        for (buffer, kind) in self.buffers_mut().into_iter().zip(LIVE_KINDS) {
            let slots = buffer.capacity();
            let mut values = history.series(kind).map_or_else(
                || vec![None; slots],
                |series| resample(series, history.step_ms, interval_ms, slots),
            );
            let tail = buffer.len().saturating_sub(kept.min(slots));
            values.extend(buffer.values().skip(tail));
            buffer.replace_all(values);
        }
    }
}

fn live_point(sample: &VitalsSample) -> [Option<f32>; 4] {
    let net = sample.net.as_ref();
    [
        Some(sample.cpu_percent),
        sample.cpu_system_percent,
        net.and_then(|net| net.rx_bps).map(|bps| bps as f32),
        net.and_then(|net| net.tx_bps).map(|bps| bps as f32),
    ]
}

/// `slots` values `interval_ms` apart ending where `series` ends, each its nearest history slot.
pub fn resample(
    series: &[Option<f32>],
    step_ms: u32,
    interval_ms: u32,
    slots: usize,
) -> Vec<Option<f32>> {
    (0..slots)
        .map(|index| {
            if step_ms == 0 {
                return None;
            }
            let back_ms = (slots - 1 - index) as f64 * f64::from(interval_ms);
            let back = (back_ms / f64::from(step_ms)).round() as usize;
            series
                .len()
                .checked_sub(back + 1)
                .and_then(|slot| series[slot])
        })
        .collect()
}

fn missing_slots(last_unix_ms: i64, unix_ms: i64, interval_ms: u32) -> usize {
    let spans = (unix_ms - last_unix_ms) as f64 / f64::from(interval_ms.max(1));
    (spans.round() as i64 - 1).max(0) as usize
}

#[derive(Clone, Debug, PartialEq)]
pub struct RangeHistory {
    pub range: HistoryRange,
    pub history: VitalsHistory,
    pub revision: u64,
}

#[derive(Default)]
struct VitalsState {
    disabled: bool,
    last_sample_ms: Option<i64>,
    gap_pending: bool,
    pushed_since_open: usize,
    wanted_range: Option<HistoryRange>,
}

/// Per `/monitor` mount, hosted through `use_controller`.
pub struct MonitorController {
    core: ControllerCore<AppFailure>,
    env: MonitorEnv,
    stream: Stream<VitalsItem>,
    epoch: EpochMark,
    state: Mutex<VitalsState>,
    started: AtomicBool,
    hooked: AtomicBool,
    cancel: Arc<Notify>,
    history_in_flight: AtomicBool,
    history_generation: AtomicU64,
    history_revision: AtomicU64,
    pub host: RwSignal<Option<HostInfo>>,
    pub latest: RwSignal<Option<VitalsSample>>,
    pub link: RwSignal<Link>,
    pub interval_ms: RwSignal<u32>,
    pub paused: RwSignal<bool>,
    pub live: RwSignal<LiveSeries>,
    pub range: RwSignal<Option<HistoryRange>>,
    pub history: RwSignal<Option<RangeHistory>>,
}

impl MonitorController {
    pub fn new(env: MonitorEnv) -> Self {
        let link = env.gate().link();
        Self {
            core: ControllerCore::new(),
            env,
            stream: Stream::new(),
            epoch: EpochMark::default(),
            state: Mutex::new(VitalsState::default()),
            started: AtomicBool::new(false),
            hooked: AtomicBool::new(false),
            cancel: Arc::new(Notify::new()),
            history_in_flight: AtomicBool::new(false),
            history_generation: AtomicU64::new(0),
            history_revision: AtomicU64::new(0),
            host: RwSignal::new(None),
            latest: RwSignal::new(None),
            link: RwSignal::new(link),
            interval_ms: RwSignal::new(DEFAULT_INTERVAL_MS),
            paused: RwSignal::new(false),
            live: RwSignal::new(LiveSeries::new(live_capacity(DEFAULT_INTERVAL_MS))),
            range: RwSignal::new(None),
            history: RwSignal::new(None),
        }
    }

    pub fn start(self: &Arc<Self>) {
        if self.core.is_disposed() {
            return;
        }
        hook_once(self, &self.hooked, &self.cancel);
        self.started.store(true, Ordering::SeqCst);
        self.reconcile();
    }

    pub fn observe(self: &Arc<Self>) {
        self.env.track_epoch();
        self.reconcile();
    }

    /// Ignores a value outside [`INTERVALS_MS`].
    pub fn set_interval(self: &Arc<Self>, interval_ms: u32) {
        if self.core.is_disposed()
            || !INTERVALS_MS.contains(&interval_ms)
            || self.interval() == interval_ms
        {
            return;
        }
        put(self.interval_ms, interval_ms);
        lock(&self.state).gap_pending = false;
        let _ = self
            .live
            .try_update(|live| live.reset(live_capacity(interval_ms)));
        self.stream.close();
        self.reconcile();
    }

    pub fn set_paused(self: &Arc<Self>, paused: bool) {
        if self.core.is_disposed() || self.paused.try_get_untracked() == Some(paused) {
            return;
        }
        put(self.paused, paused);
        if paused {
            self.stream.close();
        } else {
            self.reconcile();
        }
    }

    /// Single-flight: a selection made while a fetch is out supersedes it once it lands.
    pub fn select_range(self: &Arc<Self>, range: HistoryRange) {
        if self.core.is_disposed() {
            return;
        }
        put(self.range, Some(range));
        if !self.qualifies() {
            return;
        }
        lock(&self.state).wanted_range = Some(range);
        if self.history_in_flight.swap(true, Ordering::SeqCst) {
            return;
        }
        let controller = Arc::clone(self);
        self.env.scheduler.run(async move {
            controller.fetch_wanted_ranges().await;
        });
    }

    async fn fetch_wanted_ranges(&self) {
        loop {
            let wanted = lock(&self.state).wanted_range.take();
            let Some(range) = wanted else {
                self.history_in_flight.store(false, Ordering::SeqCst);
                let late = lock(&self.state).wanted_range.is_some();
                if !late || self.history_in_flight.swap(true, Ordering::SeqCst) {
                    return;
                }
                continue;
            };
            let generation = self.history_generation.load(Ordering::SeqCst);
            let answer = self.env.repository.vitals_history(range).await;
            if self.core.is_disposed() {
                self.history_in_flight.store(false, Ordering::SeqCst);
                return;
            }
            let superseded = lock(&self.state).wanted_range.is_some();
            if superseded || self.history_generation.load(Ordering::SeqCst) != generation {
                continue;
            }
            match answer {
                Ok(history) => {
                    let revision = self.history_revision.fetch_add(1, Ordering::SeqCst) + 1;
                    let _ = self.history.try_set(Some(RangeHistory {
                        range,
                        history,
                        revision,
                    }));
                }
                Err(failure) => log::debug!("monitor: vitals_history failed: {failure}"),
            }
        }
    }

    fn interval(&self) -> u32 {
        self.interval_ms
            .try_get_untracked()
            .unwrap_or(DEFAULT_INTERVAL_MS)
    }

    fn qualifies(&self) -> bool {
        self.env.gate() == Gate::Open && !lock(&self.state).disabled && self.env.permitted()
    }

    fn reconcile(self: &Arc<Self>) {
        if self.core.is_disposed() {
            return;
        }
        if self.epoch.moved(&self.env) {
            self.forget_server();
        }
        let gate = self.env.gate();
        if gate != Gate::Open {
            self.stream.close();
            put(self.link, gate.link());
            return;
        }
        if lock(&self.state).disabled {
            self.stream.close();
            put(self.link, Link::Disabled);
            return;
        }
        if matches!(
            self.link.try_get_untracked(),
            Some(Link::Disconnected | Link::Unsupported | Link::Disabled)
        ) {
            put(self.link, Link::Live);
        }
        let paused = self.paused.try_get_untracked().unwrap_or(true);
        if !self.started.load(Ordering::SeqCst) || paused || !self.env.permitted() {
            self.stream.close();
        } else if !self.stream.is_open() {
            self.open();
        }
    }

    fn open(self: &Arc<Self>) {
        let interval_ms = self.interval();
        lock(&self.state).pushed_since_open = 0;
        let repository = Arc::clone(&self.env.repository);
        let generation = launch(self, |sink| {
            repository.watch_vitals(VitalsDetail::Full { interval_ms }, sink)
        });
        let repository = Arc::clone(&self.env.repository);
        let weak = Arc::downgrade(self);
        self.env.scheduler.run(async move {
            let answer = repository.vitals_history(HistoryRange::OneMinute).await;
            let Some(controller) = weak.upgrade() else {
                return;
            };
            if controller.core.is_disposed() || !controller.stream.current(generation) {
                return;
            }
            match answer {
                Ok(history) => controller.seed(&history, interval_ms),
                Err(failure) => log::debug!("monitor: seeding the live view failed: {failure}"),
            }
        });
    }

    fn seed(&self, history: &VitalsHistory, interval_ms: u32) {
        let kept = lock(&self.state).pushed_since_open;
        let _ = self
            .live
            .try_update(|live| live.seed(history, interval_ms, kept));
    }

    fn forget_server(&self) {
        self.stream.close();
        self.history_generation.fetch_add(1, Ordering::SeqCst);
        *lock(&self.state) = VitalsState::default();
        put(self.host, None);
        put(self.latest, None);
        put(self.range, None);
        put(self.history, None);
        let _ = self.live.try_update(|live| {
            for buffer in live.buffers_mut() {
                buffer.clear();
            }
        });
        put(self.link, self.env.gate().link());
    }

    fn record(&self, sample: &VitalsSample) {
        let interval_ms = self.interval();
        let gaps = {
            let mut state = lock(&self.state);
            let gaps = match (state.gap_pending, state.last_sample_ms) {
                (true, Some(last)) => missing_slots(last, sample.unix_ms, interval_ms)
                    .saturating_sub(1)
                    .min(live_capacity(interval_ms)),
                _ => 0,
            };
            state.gap_pending = false;
            state.last_sample_ms = Some(sample.unix_ms);
            state.pushed_since_open += gaps + 1;
            gaps
        };
        let point = live_point(sample);
        let _ = self.live.try_update(|live| {
            for _ in 0..gaps {
                live.push_gap();
            }
            live.push(point);
        });
    }

    fn go_stale(&self) -> Link {
        let (since_unix_ms, first) = {
            let mut state = lock(&self.state);
            let first = !state.gap_pending;
            if first {
                state.gap_pending = true;
                state.pushed_since_open += 1;
            }
            (state.last_sample_ms.unwrap_or_else(now_unix_ms), first)
        };
        if first {
            let _ = self.live.try_update(LiveSeries::push_gap);
        }
        Link::Stale { since_unix_ms }
    }
}

impl Streamed for MonitorController {
    type Item = VitalsItem;

    fn stream(&self) -> &Stream<VitalsItem> {
        &self.stream
    }

    fn env(&self) -> &MonitorEnv {
        &self.env
    }

    fn controller_core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    fn quiet_limit(&self) -> Duration {
        stale_after(self.interval())
    }

    fn absorb(self: &Arc<Self>, generation: u64, items: Vec<VitalsItem>) {
        if !self.epoch.is_current(&self.env) {
            self.reconcile();
            return;
        }
        let mut host = None;
        let mut latest = None;
        let mut link = None;
        for item in items {
            if !self.stream.current(generation) {
                break;
            }
            match item {
                VitalsItem::Host(info) => host = Some(info),
                VitalsItem::Sample(sample) => {
                    self.record(&sample);
                    latest = Some(sample);
                    link = Some(Link::Live);
                }
                VitalsItem::Live => link = Some(Link::Live),
                VitalsItem::Dropped => link = Some(self.go_stale()),
                VitalsItem::Disabled => {
                    lock(&self.state).disabled = true;
                    self.stream.close();
                    link = Some(Link::Disabled);
                }
            }
        }
        if let Some(host) = host {
            put(self.host, Some(host));
        }
        if let Some(latest) = latest {
            let _ = self.latest.try_set(Some(latest));
        }
        if let Some(link) = link {
            put(self.link, link);
        }
    }

    fn went_quiet(self: &Arc<Self>) {
        if self.link.try_get_untracked() == Some(Link::Live) {
            let stale = self.go_stale();
            put(self.link, stale);
        }
    }

    fn woken(self: &Arc<Self>) {
        let stale = self.link.try_get_untracked().is_some_and(Link::is_stale);
        if stale && self.stream.is_open() {
            self.stream.close();
            self.reconcile();
        }
    }
}

impl AsRef<ControllerCore<AppFailure>> for MonitorController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

pub fn default_process_query() -> ProcessQuery {
    ProcessQuery {
        limit: PROCESS_LIMIT,
        interval_ms: PROCESS_INTERVAL_MS,
        ..ProcessQuery::default()
    }
}

/// Per processes view; `query` is what the stream runs, `filter` the text as typed.
pub struct ProcessesController {
    core: ControllerCore<AppFailure>,
    env: MonitorEnv,
    stream: Stream<ProcessItem>,
    epoch: EpochMark,
    disabled: AtomicBool,
    last_snapshot_ms: Mutex<Option<i64>>,
    filter_seq: AtomicU64,
    signal_claim: AtomicBool,
    started: AtomicBool,
    hooked: AtomicBool,
    cancel: Arc<Notify>,
    pub query: RwSignal<ProcessQuery>,
    pub filter: RwSignal<String>,
    pub snapshot: RwSignal<Option<ProcessSnapshot>>,
    pub link: RwSignal<Link>,
    pub selected: RwSignal<Option<(u32, i64)>>,
    pub selected_cpu: RwSignal<SeriesBuffer>,
    pub signalling: RwSignal<bool>,
}

impl ProcessesController {
    pub fn new(env: MonitorEnv) -> Self {
        let link = env.gate().link();
        Self {
            core: ControllerCore::new(),
            env,
            stream: Stream::new(),
            epoch: EpochMark::default(),
            disabled: AtomicBool::new(false),
            last_snapshot_ms: Mutex::new(None),
            filter_seq: AtomicU64::new(0),
            signal_claim: AtomicBool::new(false),
            started: AtomicBool::new(false),
            hooked: AtomicBool::new(false),
            cancel: Arc::new(Notify::new()),
            query: RwSignal::new(default_process_query()),
            filter: RwSignal::new(String::new()),
            snapshot: RwSignal::new(None),
            link: RwSignal::new(link),
            selected: RwSignal::new(None),
            selected_cpu: RwSignal::new(SeriesBuffer::new(live_capacity(PROCESS_INTERVAL_MS))),
            signalling: RwSignal::new(false),
        }
    }

    pub fn start(self: &Arc<Self>) {
        if self.core.is_disposed() {
            return;
        }
        hook_once(self, &self.hooked, &self.cancel);
        self.started.store(true, Ordering::SeqCst);
        self.reconcile();
    }

    pub fn observe(self: &Arc<Self>) {
        self.env.track_epoch();
        self.reconcile();
    }

    pub fn set_sort(self: &Arc<Self>, sort: ProcessSort) {
        self.requery(|query| query.sort = sort);
    }

    pub fn set_reverse(self: &Arc<Self>, reverse: bool) {
        self.requery(|query| query.reverse = reverse);
    }

    /// Applied [`FILTER_DEBOUNCE`] after the last keystroke.
    pub fn set_filter(self: &Arc<Self>, text: String) {
        if self.core.is_disposed() {
            return;
        }
        put(self.filter, text.clone());
        let seq = self.filter_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let settle = (self.env.scheduler.sleep)(FILTER_DEBOUNCE);
        let weak = Arc::downgrade(self);
        self.env.scheduler.run(async move {
            settle.await;
            let Some(controller) = weak.upgrade() else {
                return;
            };
            if controller.filter_seq.load(Ordering::SeqCst) == seq {
                controller.requery(|query| query.filter = text);
            }
        });
    }

    /// Reopens only when the pin changes; a new pick starts its cpu run over.
    pub fn select(self: &Arc<Self>, pick: Option<(u32, i64)>) {
        if self.core.is_disposed() || self.selected.try_get_untracked() == Some(pick) {
            return;
        }
        put(self.selected, pick);
        let _ = self.selected_cpu.try_update(SeriesBuffer::clear);
        self.requery(|query| query.pinned = pick);
    }

    /// Single-flight through [`signalling`](Self::signalling); nothing is sent while monitoring is unavailable.
    pub async fn signal(
        &self,
        pid: u32,
        start_unix_s: i64,
        signal: ProcessSignal,
    ) -> Result<(), AppFailure> {
        let usable = self.env.gate() == Gate::Open
            && !self.disabled.load(Ordering::SeqCst)
            && self.env.permitted();
        if self.core.is_disposed() || !usable {
            return Err(AppFailure::Internal(MONITOR_UNAVAILABLE.to_string()));
        }
        if self.signal_claim.swap(true, Ordering::SeqCst) {
            return Err(AppFailure::Internal(SIGNAL_IN_FLIGHT.to_string()));
        }
        put(self.signalling, true);
        let outcome = self
            .env
            .repository
            .signal_process(pid, start_unix_s, signal)
            .await;
        self.signal_claim.store(false, Ordering::SeqCst);
        if !self.core.is_disposed() {
            put(self.signalling, false);
        }
        outcome
    }

    fn requery(self: &Arc<Self>, change: impl FnOnce(&mut ProcessQuery)) {
        if self.core.is_disposed() {
            return;
        }
        let Some(mut query) = self.query.try_get_untracked() else {
            return;
        };
        let before = query.clone();
        change(&mut query);
        if query == before {
            return;
        }
        put(self.query, query);
        self.stream.close();
        self.reconcile();
    }

    fn reconcile(self: &Arc<Self>) {
        if self.core.is_disposed() {
            return;
        }
        if self.epoch.moved(&self.env) {
            self.forget_server();
        }
        let gate = self.env.gate();
        if gate != Gate::Open {
            self.stream.close();
            put(self.link, gate.link());
            return;
        }
        if self.disabled.load(Ordering::SeqCst) {
            self.stream.close();
            put(self.link, Link::Disabled);
            return;
        }
        if matches!(
            self.link.try_get_untracked(),
            Some(Link::Disconnected | Link::Unsupported | Link::Disabled)
        ) {
            put(self.link, Link::Live);
        }
        if !self.started.load(Ordering::SeqCst) || !self.env.permitted() {
            self.stream.close();
        } else if !self.stream.is_open()
            && let Some(query) = self.query.try_get_untracked()
        {
            let repository = Arc::clone(&self.env.repository);
            launch(self, |sink| repository.watch_processes(query, sink));
        }
    }

    fn forget_server(&self) {
        self.stream.close();
        self.disabled.store(false, Ordering::SeqCst);
        *lock(&self.last_snapshot_ms) = None;
        put(self.snapshot, None);
        put(self.selected, None);
        let _ = self.selected_cpu.try_update(SeriesBuffer::clear);
        let _ = self.query.try_update(|query| query.pinned = None);
        put(self.link, self.env.gate().link());
    }

    fn record(&self, snapshot: &ProcessSnapshot) {
        *lock(&self.last_snapshot_ms) = Some(snapshot.unix_ms);
        let Some(Some((pid, start_unix_s))) = self.selected.try_get_untracked() else {
            return;
        };
        let cpu = snapshot
            .rows
            .iter()
            .find(|row| row.pid == pid && row.start_unix_s == start_unix_s)
            .map(|row| row.cpu_percent);
        let _ = self.selected_cpu.try_update(|buffer| buffer.push_slot(cpu));
    }

    fn go_stale(&self) -> Link {
        if self.selected.try_get_untracked().flatten().is_some() {
            let _ = self.selected_cpu.try_update(SeriesBuffer::push_gap);
        }
        let since_unix_ms = lock(&self.last_snapshot_ms).unwrap_or_else(now_unix_ms);
        Link::Stale { since_unix_ms }
    }
}

impl Streamed for ProcessesController {
    type Item = ProcessItem;

    fn stream(&self) -> &Stream<ProcessItem> {
        &self.stream
    }

    fn env(&self) -> &MonitorEnv {
        &self.env
    }

    fn controller_core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    fn quiet_limit(&self) -> Duration {
        let interval_ms = self
            .query
            .try_with_untracked(|query| query.interval_ms)
            .filter(|interval| *interval > 0)
            .unwrap_or(PROCESS_INTERVAL_MS);
        stale_after(interval_ms)
    }

    fn absorb(self: &Arc<Self>, generation: u64, items: Vec<ProcessItem>) {
        if !self.epoch.is_current(&self.env) {
            self.reconcile();
            return;
        }
        let mut snapshot = None;
        let mut link = None;
        for item in items {
            if !self.stream.current(generation) {
                break;
            }
            match item {
                ProcessItem::Snapshot(next) => {
                    self.record(&next);
                    snapshot = Some(next);
                    link = Some(Link::Live);
                }
                ProcessItem::Live => link = Some(Link::Live),
                ProcessItem::Dropped => link = Some(self.go_stale()),
                ProcessItem::Disabled => {
                    self.disabled.store(true, Ordering::SeqCst);
                    self.stream.close();
                    link = Some(Link::Disabled);
                }
            }
        }
        if let Some(snapshot) = snapshot {
            let _ = self.snapshot.try_set(Some(snapshot));
        }
        if let Some(link) = link {
            put(self.link, link);
        }
    }

    fn went_quiet(self: &Arc<Self>) {
        if self.link.try_get_untracked() == Some(Link::Live) {
            let stale = self.go_stale();
            put(self.link, stale);
        }
    }

    fn woken(self: &Arc<Self>) {
        let stale = self.link.try_get_untracked().is_some_and(Link::is_stale);
        if stale && self.stream.is_open() {
            self.stream.close();
            self.reconcile();
        }
    }
}

impl AsRef<ControllerCore<AppFailure>> for ProcessesController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::AtomicUsize;

    use tokio::sync::Semaphore;

    use super::*;
    use crate::connection::{ServerInfo, TrustMode};
    use crate::features::monitor::data::repositories::MockMonitorRepository;
    use crate::features::monitor::domain::entities::{DiskVolume, Memory, ProcessRow};
    use crate::features::monitor::domain::repositories::{
        MonitorRepository, ProcessSink, VitalsSink,
    };
    use crate::net::wake::WakeBus;

    const SEED_MS: i64 = 1_790_086_400_000;

    #[derive(Default)]
    struct ClockState {
        now: Duration,
        timers: Vec<(Duration, Arc<Notify>)>,
    }

    #[derive(Clone, Default)]
    struct Clock(Arc<Mutex<ClockState>>);

    impl Clock {
        fn sleep(&self, delay: Duration) -> Task {
            let ready = Arc::new(Notify::new());
            let mut state = lock(&self.0);
            let at = state.now + delay;
            state.timers.push((at, Arc::clone(&ready)));
            Box::pin(async move { ready.notified().await })
        }

        fn advance(&self, by: Duration) {
            let mut state = lock(&self.0);
            state.now += by;
            let now = state.now;
            state.timers.retain(|(at, ready)| {
                let due = *at <= now;
                if due {
                    ready.notify_one();
                }
                !due
            });
        }
    }

    fn run(test: impl Future<Output = ()>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime");
        tokio::task::LocalSet::new().block_on(&runtime, test);
    }

    async fn settle() {
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
    }

    fn server(endpoint: &str, capable: bool) -> ServerInfo {
        let capabilities = if capable {
            BTreeSet::from([CAP_HOST_VITALS.to_string()])
        } else {
            BTreeSet::new()
        };
        ServerInfo {
            endpoint: endpoint.to_string(),
            trust_mode: TrustMode::System,
            tls_label: String::new(),
            version: String::new(),
            latency_ms: 0,
            backend_versions: Vec::new(),
            available_backends: Vec::new(),
            capabilities,
            is_read_only: false,
            server_version: String::new(),
            latest_version: None,
        }
    }

    struct Rig {
        mock: Arc<MockMonitorRepository>,
        connection: Arc<ConnectionController>,
        allowed: Arc<AtomicBool>,
        clock: Clock,
        bus: WakeBus,
    }

    impl Rig {
        fn new(info: Option<ServerInfo>) -> Self {
            let rig = Self {
                mock: Arc::new(MockMonitorRepository::seeded()),
                connection: Arc::new(ConnectionController::detached()),
                allowed: Arc::new(AtomicBool::new(true)),
                clock: Clock::default(),
                bus: WakeBus::new(),
            };
            rig.land(info);
            rig
        }

        fn capable() -> Self {
            Self::new(Some(server("rig:7777", true)))
        }

        fn land(&self, info: Option<ServerInfo>) {
            self.connection.server_info.set(info);
            self.connection.connection_epoch.update(|epoch| *epoch += 1);
        }

        fn env_over(&self, repository: SharedMonitorRepository) -> MonitorEnv {
            let allowed = Arc::clone(&self.allowed);
            let clock = self.clock.clone();
            let bus = self.bus.clone();
            MonitorEnv::new(
                repository,
                Arc::clone(&self.connection),
                move || allowed.load(Ordering::SeqCst),
                Scheduler::new(
                    |task| {
                        tokio::task::spawn_local(task);
                    },
                    move |delay| clock.sleep(delay),
                    move || bus.subscribe(),
                ),
            )
        }

        fn env(&self) -> MonitorEnv {
            self.env_over(Arc::clone(&self.mock) as SharedMonitorRepository)
        }

        fn summary(&self) -> Arc<VitalsSummaryController> {
            Arc::new(VitalsSummaryController::new(self.env()))
        }

        fn monitor(&self) -> Arc<MonitorController> {
            Arc::new(MonitorController::new(self.env()))
        }

        fn processes(&self) -> Arc<ProcessesController> {
            Arc::new(ProcessesController::new(self.env()))
        }

        fn assert_untouched(&self) {
            assert!(self.mock.vitals_opens().is_empty());
            assert!(self.mock.process_opens().is_empty());
            assert!(self.mock.history_requests().is_empty());
            assert!(self.mock.signals().is_empty());
        }
    }

    struct Gated {
        inner: Arc<MockMonitorRepository>,
        gate: Semaphore,
        running: AtomicUsize,
        most: AtomicUsize,
    }

    impl Gated {
        fn over(inner: &Arc<MockMonitorRepository>) -> Arc<Self> {
            Arc::new(Self {
                inner: Arc::clone(inner),
                gate: Semaphore::new(0),
                running: AtomicUsize::new(0),
                most: AtomicUsize::new(0),
            })
        }

        async fn pass(&self) {
            let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(running, Ordering::SeqCst);
            self.gate.acquire().await.expect("an open gate").forget();
            self.running.fetch_sub(1, Ordering::SeqCst);
        }

        fn release(&self, calls: usize) {
            self.gate.add_permits(calls);
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
    #[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
    impl MonitorRepository for Gated {
        fn watch_vitals(&self, detail: VitalsDetail, sink: VitalsSink) -> MonitorWatch {
            self.inner.watch_vitals(detail, sink)
        }

        async fn vitals_history(&self, range: HistoryRange) -> Result<VitalsHistory, AppFailure> {
            self.pass().await;
            self.inner.vitals_history(range).await
        }

        fn watch_processes(&self, query: ProcessQuery, sink: ProcessSink) -> MonitorWatch {
            self.inner.watch_processes(query, sink)
        }

        async fn signal_process(
            &self,
            pid: u32,
            start_unix_s: i64,
            signal: ProcessSignal,
        ) -> Result<(), AppFailure> {
            self.pass().await;
            self.inner.signal_process(pid, start_unix_s, signal).await
        }
    }

    fn sample(unix_ms: i64, cpu: f32) -> VitalsSample {
        VitalsSample {
            unix_ms,
            cpu_percent: cpu,
            cpu_system_percent: Some(cpu / 4.0),
            ..VitalsSample::default()
        }
    }

    fn snapshot(pid: u32, start_unix_s: i64, cpu: f32) -> ProcessSnapshot {
        ProcessSnapshot {
            unix_ms: SEED_MS,
            rows: vec![ProcessRow {
                pid,
                start_unix_s,
                cpu_percent: cpu,
                ..ProcessRow::default()
            }],
            ..ProcessSnapshot::default()
        }
    }

    fn cpu_tail(monitor: &MonitorController, count: usize) -> Vec<Option<f32>> {
        let values: Vec<Option<f32>> = monitor.live.get_untracked().cpu_total.values().collect();
        values[values.len().saturating_sub(count)..].to_vec()
    }

    fn dispose<C: AsRef<ControllerCore<AppFailure>>>(controller: &C) {
        controller.as_ref().dispose();
    }

    #[test]
    fn without_the_capability_nothing_reaches_the_repository_and_the_link_reads_unsupported() {
        run(async {
            let rig = Rig::new(Some(server("old:7777", false)));
            let summary = rig.summary();
            let monitor = rig.monitor();
            let processes = rig.processes();
            summary.start();
            monitor.start();
            monitor.select_range(HistoryRange::OneHour);
            processes.start();
            let refused = processes.signal(1, 0, ProcessSignal::Term).await;
            settle().await;
            assert_eq!(monitor.link.get_untracked(), Link::Unsupported);
            assert_eq!(processes.link.get_untracked(), Link::Unsupported);
            assert!(!summary.available.get_untracked());
            assert!(refused.is_err());
            rig.assert_untouched();
        });
    }

    #[test]
    fn while_disconnected_nothing_reaches_the_repository_and_the_link_reads_disconnected() {
        run(async {
            let rig = Rig::new(None);
            let summary = rig.summary();
            let monitor = rig.monitor();
            let processes = rig.processes();
            summary.start();
            monitor.start();
            monitor.select_range(HistoryRange::OneHour);
            processes.start();
            settle().await;
            assert_eq!(monitor.link.get_untracked(), Link::Disconnected);
            assert_eq!(processes.link.get_untracked(), Link::Disconnected);
            assert!(!summary.available.get_untracked());
            rig.assert_untouched();
        });
    }

    #[test]
    fn without_permission_no_stream_opens_and_no_history_is_fetched() {
        run(async {
            let rig = Rig::capable();
            rig.allowed.store(false, Ordering::SeqCst);
            let summary = rig.summary();
            let monitor = rig.monitor();
            let processes = rig.processes();
            summary.start();
            monitor.start();
            monitor.select_range(HistoryRange::FiveMinutes);
            processes.start();
            let refused = processes.signal(1, 0, ProcessSignal::Term).await;
            settle().await;
            assert!(refused.is_err());
            assert!(summary.available.get_untracked());
            rig.assert_untouched();
        });
    }

    #[test]
    fn the_summary_counts_its_starts_so_two_mounts_share_one_stream() {
        run(async {
            let rig = Rig::capable();
            let summary = rig.summary();
            summary.start();
            summary.start();
            settle().await;
            assert_eq!(rig.mock.vitals_opens(), [VitalsDetail::Summary]);
            summary.stop();
            assert_eq!(rig.mock.open_watches(), 1);
            summary.stop();
            assert_eq!(rig.mock.open_watches(), 0);
            assert_eq!(rig.mock.watch_drops(), 1);
        });
    }

    #[test]
    fn the_summary_reads_cpu_memory_and_the_fullest_disk() {
        run(async {
            let rig = Rig::capable();
            let summary = rig.summary();
            summary.start();
            settle().await;
            let reading = summary.summary.get_untracked().expect("a reading");
            assert_eq!(reading.cpu_percent, 23.5);
            assert!((reading.memory_used_fraction - 0.375).abs() < 1e-6);
            assert_eq!(
                reading.fullest_disk.map(|disk| disk.mount),
                Some("/".into())
            );
            assert!(summary.available.get_untracked());
            assert!(!summary.stale.get_untracked());
        });
    }

    #[test]
    fn an_epoch_change_reopens_the_summary_once_and_clears_the_old_reading() {
        run(async {
            let rig = Rig::capable();
            let summary = rig.summary();
            summary.start();
            settle().await;
            assert!(summary.summary.get_untracked().is_some());
            rig.land(Some(server("other:7777", true)));
            summary.observe();
            assert_eq!(rig.mock.vitals_opens().len(), 2);
            assert_eq!(summary.summary.get_untracked(), None);
            settle().await;
            summary.observe();
            assert_eq!(rig.mock.vitals_opens().len(), 2);
            assert_eq!(rig.mock.watch_drops(), 1);
        });
    }

    #[test]
    fn a_disabled_summary_stays_unavailable_until_the_next_epoch() {
        run(async {
            let rig = Rig::capable();
            let summary = rig.summary();
            summary.start();
            settle().await;
            rig.mock.set_disabled(true);
            settle().await;
            assert!(!summary.available.get_untracked());
            summary.observe();
            rig.bus.publish(WakeReason::Foregrounded);
            settle().await;
            assert_eq!(rig.mock.vitals_opens().len(), 1);
            rig.mock.set_disabled(false);
            rig.land(Some(server("other:7777", true)));
            summary.observe();
            assert!(summary.available.get_untracked());
            assert_eq!(rig.mock.vitals_opens().len(), 2);
        });
    }

    #[test]
    fn the_first_sample_lands_with_the_host_in_one_drain_and_seeds_the_live_minute() {
        run(async {
            let rig = Rig::capable();
            let monitor = rig.monitor();
            monitor.start();
            assert_eq!(
                rig.mock.vitals_opens(),
                [VitalsDetail::Full {
                    interval_ms: DEFAULT_INTERVAL_MS
                }]
            );
            tokio::task::yield_now().await;
            let host = monitor.host.get_untracked().expect("the host");
            let latest = monitor.latest.get_untracked().expect("the sample");
            assert_eq!(host.hostname, "workbench");
            assert_eq!(latest.cpu_percent, 23.5);
            settle().await;
            let live = monitor.live.get_untracked();
            assert_eq!(live.cpu_total.len(), live_capacity(DEFAULT_INTERVAL_MS));
            assert_eq!(cpu_tail(&monitor, 1), [Some(23.5)]);
            let seeded = live.cpu_total.values().filter(Option::is_some).count();
            assert!(seeded > 10, "{seeded} seeded slots");
            assert_eq!(monitor.link.get_untracked(), Link::Live);
            assert_eq!(rig.mock.history_requests(), [HistoryRange::OneMinute]);
        });
    }

    #[test]
    fn a_dropped_stream_reads_stale_keeps_the_sample_gaps_the_buffers_and_resumes() {
        run(async {
            let rig = Rig::capable();
            let monitor = rig.monitor();
            monitor.start();
            settle().await;
            rig.mock.set_dropped(true);
            settle().await;
            assert_eq!(
                monitor.link.get_untracked(),
                Link::Stale {
                    since_unix_ms: SEED_MS
                }
            );
            assert!(monitor.latest.get_untracked().is_some());
            assert_eq!(cpu_tail(&monitor, 1), [None]);
            rig.mock.push_sample(sample(SEED_MS + 9_000, 40.0));
            rig.mock.set_dropped(false);
            settle().await;
            assert_eq!(monitor.link.get_untracked(), Link::Live);
            assert_eq!(
                monitor.latest.get_untracked().map(|s| s.cpu_percent),
                Some(40.0)
            );
            assert_eq!(cpu_tail(&monitor, 4), [Some(23.5), None, None, Some(40.0)]);
        });
    }

    #[test]
    fn no_sample_for_twice_the_interval_plus_two_seconds_reads_stale() {
        run(async {
            let rig = Rig::capable();
            let monitor = rig.monitor();
            monitor.start();
            settle().await;
            let limit = stale_after(DEFAULT_INTERVAL_MS);
            assert_eq!(limit, Duration::from_secs(8));
            rig.clock.advance(limit - Duration::from_millis(1));
            settle().await;
            assert_eq!(monitor.link.get_untracked(), Link::Live);
            rig.clock.advance(Duration::from_millis(1));
            settle().await;
            assert_eq!(
                monitor.link.get_untracked(),
                Link::Stale {
                    since_unix_ms: SEED_MS
                }
            );
            rig.mock.push_sample(sample(SEED_MS + 9_000, 30.0));
            settle().await;
            assert_eq!(monitor.link.get_untracked(), Link::Live);
        });
    }

    #[test]
    fn a_disabled_monitor_reads_disabled_and_never_retries() {
        run(async {
            let rig = Rig::capable();
            let monitor = rig.monitor();
            monitor.start();
            settle().await;
            rig.mock.set_disabled(true);
            settle().await;
            assert_eq!(monitor.link.get_untracked(), Link::Disabled);
            rig.bus.publish(WakeReason::NetworkChanged);
            monitor.observe();
            rig.clock.advance(Duration::from_secs(60));
            settle().await;
            assert_eq!(monitor.link.get_untracked(), Link::Disabled);
            assert_eq!(rig.mock.vitals_opens().len(), 1);
        });
    }

    #[test]
    fn an_interval_change_reopens_with_the_new_interval() {
        run(async {
            let rig = Rig::capable();
            let monitor = rig.monitor();
            monitor.start();
            settle().await;
            monitor.set_interval(5_000);
            monitor.set_interval(4_000);
            settle().await;
            assert_eq!(
                rig.mock.vitals_opens(),
                [
                    VitalsDetail::Full { interval_ms: 3_000 },
                    VitalsDetail::Full { interval_ms: 5_000 }
                ]
            );
            assert_eq!(monitor.interval_ms.get_untracked(), 5_000);
            assert_eq!(monitor.live.get_untracked().cpu_total.capacity(), 12);
            assert_eq!(rig.mock.open_watches(), 1);
        });
    }

    #[test]
    fn pausing_stops_the_stream_and_resuming_reopens_it() {
        run(async {
            let rig = Rig::capable();
            let monitor = rig.monitor();
            monitor.start();
            settle().await;
            monitor.set_paused(true);
            assert_eq!(rig.mock.open_watches(), 0);
            assert!(monitor.paused.get_untracked());
            monitor.set_paused(false);
            assert_eq!(rig.mock.vitals_opens().len(), 2);
            assert_eq!(rig.mock.open_watches(), 1);
        });
    }

    #[test]
    fn an_epoch_change_while_mounted_drops_the_old_server_and_reopens_once() {
        run(async {
            let rig = Rig::capable();
            let monitor = rig.monitor();
            monitor.start();
            settle().await;
            assert!(monitor.host.get_untracked().is_some());
            rig.land(Some(server("other:7777", true)));
            monitor.observe();
            assert_eq!(monitor.host.get_untracked(), None);
            assert_eq!(monitor.latest.get_untracked(), None);
            assert!(monitor.live.get_untracked().cpu_total.is_empty());
            assert_eq!(rig.mock.vitals_opens().len(), 2);
            assert_eq!(rig.mock.watch_drops(), 1);
            settle().await;
            monitor.observe();
            assert_eq!(rig.mock.vitals_opens().len(), 2);
            assert!(monitor.host.get_untracked().is_some());
        });
    }

    #[test]
    fn a_switch_to_a_server_without_the_capability_ends_the_stream() {
        run(async {
            let rig = Rig::capable();
            let monitor = rig.monitor();
            monitor.start();
            settle().await;
            rig.land(Some(server("old:7777", false)));
            monitor.observe();
            assert_eq!(monitor.link.get_untracked(), Link::Unsupported);
            assert_eq!(rig.mock.open_watches(), 0);
            assert_eq!(rig.mock.vitals_opens().len(), 1);
        });
    }

    #[test]
    fn a_wake_reopens_a_stale_stream_but_not_a_live_one() {
        run(async {
            let rig = Rig::capable();
            let monitor = rig.monitor();
            monitor.start();
            settle().await;
            rig.bus.publish(WakeReason::Foregrounded);
            settle().await;
            assert_eq!(rig.mock.vitals_opens().len(), 1);
            rig.mock.set_dropped(true);
            settle().await;
            rig.bus.publish(WakeReason::NetworkChanged);
            settle().await;
            assert_eq!(rig.mock.vitals_opens().len(), 2);
            assert_eq!(rig.mock.open_watches(), 1);
        });
    }

    #[test]
    fn range_selection_is_single_flight_and_the_last_selection_wins() {
        run(async {
            let rig = Rig::capable();
            let gated = Gated::over(&rig.mock);
            let monitor = Arc::new(MonitorController::new(
                rig.env_over(Arc::clone(&gated) as SharedMonitorRepository),
            ));
            monitor.select_range(HistoryRange::FiveMinutes);
            settle().await;
            monitor.select_range(HistoryRange::OneHour);
            settle().await;
            assert_eq!(monitor.history.get_untracked(), None);
            assert_eq!(monitor.range.get_untracked(), Some(HistoryRange::OneHour));
            gated.release(2);
            settle().await;
            assert_eq!(
                rig.mock.history_requests(),
                [HistoryRange::FiveMinutes, HistoryRange::OneHour]
            );
            assert_eq!(gated.most.load(Ordering::SeqCst), 1);
            let shown = monitor.history.get_untracked().expect("a fetched range");
            assert_eq!(shown.range, HistoryRange::OneHour);
            let cpu = shown.history.series(SeriesKind::CpuTotal).expect("cpu");
            assert_eq!(cpu.len(), 60);
            assert_eq!(cpu[..3], [None, None, None]);
            assert!(cpu[3].is_some());
        });
    }

    #[test]
    fn the_filter_applies_three_hundred_milliseconds_after_the_last_keystroke() {
        run(async {
            let rig = Rig::capable();
            let processes = rig.processes();
            processes.start();
            settle().await;
            processes.set_filter("c".to_string());
            rig.clock.advance(Duration::from_millis(200));
            settle().await;
            processes.set_filter("car".to_string());
            rig.clock.advance(Duration::from_millis(299));
            settle().await;
            assert_eq!(rig.mock.process_opens().len(), 1);
            assert_eq!(processes.filter.get_untracked(), "car");
            assert_eq!(processes.query.get_untracked().filter, "");
            rig.clock.advance(Duration::from_millis(1));
            settle().await;
            let opens = rig.mock.process_opens();
            assert_eq!(opens.len(), 2);
            assert_eq!(opens[1].filter, "car");
            assert_eq!(opens[0].limit, PROCESS_LIMIT);
        });
    }

    #[test]
    fn a_sort_or_reverse_change_reopens_and_a_repeat_does_not() {
        run(async {
            let rig = Rig::capable();
            let processes = rig.processes();
            processes.start();
            processes.set_sort(ProcessSort::Memory);
            processes.set_sort(ProcessSort::Memory);
            processes.set_reverse(true);
            let opens = rig.mock.process_opens();
            assert_eq!(opens.len(), 3);
            assert_eq!(opens[1].sort, ProcessSort::Memory);
            assert!(opens[2].reverse);
            assert_eq!(rig.mock.open_watches(), 1);
        });
    }

    #[test]
    fn selecting_a_process_pins_it_once_and_builds_its_cpu_run() {
        run(async {
            let rig = Rig::capable();
            let processes = rig.processes();
            processes.start();
            settle().await;
            assert_eq!(
                processes.snapshot.get_untracked().map(|s| s.rows.len()),
                Some(4)
            );
            let cargo = (4211, 1_790_004_211);
            processes.select(Some(cargo));
            processes.select(Some(cargo));
            let opens = rig.mock.process_opens();
            assert_eq!(opens.len(), 2);
            assert_eq!(opens[1].pinned, Some(cargo));
            rig.mock.push_snapshot(snapshot(cargo.0, cargo.1, 50.0));
            settle().await;
            rig.mock.push_snapshot(snapshot(cargo.0, cargo.1, 60.0));
            settle().await;
            let cpu_run: Vec<_> = processes.selected_cpu.get_untracked().values().collect();
            assert_eq!(cpu_run, [Some(50.0), Some(60.0)]);
            processes.select(Some((902, 1_790_000_902)));
            assert!(processes.selected_cpu.get_untracked().is_empty());
            assert_eq!(rig.mock.process_opens().len(), 3);
        });
    }

    #[test]
    fn signals_are_single_flight_and_a_failure_is_returned() {
        run(async {
            let rig = Rig::capable();
            let gated = Gated::over(&rig.mock);
            let processes = Arc::new(ProcessesController::new(
                rig.env_over(Arc::clone(&gated) as SharedMonitorRepository),
            ));
            let sending = Arc::clone(&processes);
            let first = tokio::task::spawn_local(async move {
                sending.signal(4211, 7, ProcessSignal::Term).await
            });
            settle().await;
            assert!(processes.signalling.get_untracked());
            let second = processes.signal(4211, 7, ProcessSignal::Kill).await;
            assert_eq!(
                second,
                Err(AppFailure::Internal(SIGNAL_IN_FLIGHT.to_string()))
            );
            gated.release(1);
            assert_eq!(first.await.expect("the first send"), Ok(()));
            assert!(!processes.signalling.get_untracked());
            rig.mock
                .fail_signal(Some(AppFailure::Network("refused".to_string())));
            gated.release(1);
            let failed = processes.signal(1, 2, ProcessSignal::Kill).await;
            assert_eq!(failed, Err(AppFailure::Network("refused".to_string())));
            assert_eq!(
                rig.mock.signals(),
                [(4211, 7, ProcessSignal::Term), (1, 2, ProcessSignal::Kill)]
            );
            assert!(!processes.signalling.get_untracked());
        });
    }

    #[test]
    fn an_epoch_change_clears_the_process_snapshot_and_selection() {
        run(async {
            let rig = Rig::capable();
            let processes = rig.processes();
            processes.start();
            settle().await;
            processes.select(Some((4211, 1_790_004_211)));
            rig.mock.push_snapshot(snapshot(4211, 1_790_004_211, 50.0));
            settle().await;
            rig.land(Some(server("other:7777", true)));
            processes.observe();
            assert_eq!(processes.snapshot.get_untracked(), None);
            assert_eq!(processes.selected.get_untracked(), None);
            assert!(processes.selected_cpu.get_untracked().is_empty());
            let opens = rig.mock.process_opens();
            assert_eq!(opens.len(), 3);
            assert_eq!(opens[2].pinned, None);
        });
    }

    #[test]
    fn dispose_stops_every_stream() {
        run(async {
            let rig = Rig::capable();
            let summary = rig.summary();
            let monitor = rig.monitor();
            let processes = rig.processes();
            summary.start();
            monitor.start();
            processes.start();
            settle().await;
            assert_eq!(rig.mock.open_watches(), 3);
            dispose(&*summary);
            dispose(&*monitor);
            dispose(&*processes);
            assert_eq!(rig.mock.open_watches(), 0);
            assert_eq!(rig.mock.watch_drops(), 3);
            monitor.start();
            processes.set_sort(ProcessSort::Name);
            rig.bus.publish(WakeReason::NetworkChanged);
            settle().await;
            assert_eq!(rig.mock.open_watches(), 0);
            assert_eq!(rig.mock.vitals_opens().len(), 2);
        });
    }

    #[test]
    fn the_summary_picks_the_fullest_disk_and_survives_empty_totals() {
        let reading = VitalsSummary::of(&VitalsSample {
            cpu_percent: 5.0,
            memory: Memory::default(),
            disks: vec![
                DiskVolume {
                    mount: "/".to_string(),
                    total_bytes: 100,
                    used_bytes: 40,
                    ..DiskVolume::default()
                },
                DiskVolume {
                    mount: "/data".to_string(),
                    total_bytes: 100,
                    used_bytes: 90,
                    ..DiskVolume::default()
                },
                DiskVolume {
                    mount: "/empty".to_string(),
                    ..DiskVolume::default()
                },
            ],
            ..VitalsSample::default()
        });
        assert_eq!(reading.memory_used_fraction, 0.0);
        let fullest = reading.fullest_disk.expect("a disk");
        assert_eq!(fullest.mount, "/data");
        assert!((fullest.used_fraction - 0.9).abs() < 1e-6);
    }

    #[test]
    fn resampling_picks_the_nearest_history_slot_from_the_end() {
        let series = [Some(0.0), Some(1.0), Some(2.0), None, Some(4.0)];
        assert_eq!(
            resample(&series, 1_000, 2_000, 3),
            [Some(0.0), Some(2.0), Some(4.0)]
        );
        assert_eq!(resample(&series, 1_000, 1_000, 7)[..2], [None, None]);
        assert_eq!(resample(&series, 0, 1_000, 2), [None, None]);
    }
}
