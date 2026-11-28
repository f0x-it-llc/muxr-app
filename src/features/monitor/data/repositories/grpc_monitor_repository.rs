//! `muxrd`-backed `MonitorRepository`. A watch reconnects on [`RECONNECT_BACKOFF`] until its handle
//! drops; `FAILED_PRECONDITION` or a `disabled` event ends it with `Disabled`, `UNAUTHENTICATED`
//! fires the session-expired hook and ends it. Every wire-to-entity mapping lives here.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio_stream::{Stream, StreamExt};
use tonic::{Code, Status};

use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::monitor::domain::entities::{
    DiskVolume, HistoryRange, HostInfo, LoadAverage, Memory, NetInterface, ProcessItem,
    ProcessQuery, ProcessRow, ProcessSignal, ProcessSnapshot, ProcessSort, ProcessState,
    SeriesKind, Temperature, VitalsDetail, VitalsHistory, VitalsItem, VitalsSample,
};
use crate::features::monitor::domain::failures;
use crate::features::monitor::domain::repositories::{
    MonitorRepository, MonitorWatch, ProcessSink, VitalsSink,
};
use crate::net::proto as wire;
use crate::net::pump::{Nap, Stop, Winner, race};
use crate::net::{MuxrClient, error};

const NOT_CONNECTED: &str = "not connected to a server";

/// Retries past the end stay at the last step until the handle drops.
pub const RECONNECT_BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

pub struct GrpcMonitorRepository {
    connection: Arc<ConnectionController>,
}

impl GrpcMonitorRepository {
    pub fn new(connection: Arc<ConnectionController>) -> Self {
        Self { connection }
    }

    fn client(&self) -> Result<MuxrClient, AppFailure> {
        self.connection
            .client()
            .ok_or_else(|| AppFailure::Network(NOT_CONNECTED.to_string()))
    }

    fn map_status(&self, client: &MuxrClient, status: Status) -> AppFailure {
        if status.code() == Code::Unauthenticated {
            self.connection.on_session_expired_for(client);
        }
        status_failure(&status)
    }

    fn link<Q>(&self, request: Q) -> Link<Q> {
        Link {
            connection: Arc::clone(&self.connection),
            last: Mutex::new(None),
            bound: Mutex::new(None),
            request,
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl MonitorRepository for GrpcMonitorRepository {
    fn watch_vitals(&self, detail: VitalsDetail, sink: VitalsSink) -> MonitorWatch {
        let source = VitalsSource(self.link(vitals_request(detail)));
        let (watch, pump) = spawn_parts(source, sink, Nap::Frust, RECONNECT_BACKOFF.to_vec());
        frust::spawn(pump);
        watch
    }

    async fn vitals_history(&self, range: HistoryRange) -> Result<VitalsHistory, AppFailure> {
        let client = self.client()?;
        let history = client
            .authed()
            .get_vitals_history(wire::VitalsHistoryRequest {
                range: history_range(range) as i32,
            })
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        Ok(map_history(history))
    }

    fn watch_processes(&self, query: ProcessQuery, sink: ProcessSink) -> MonitorWatch {
        let source = ProcessSource(self.link(processes_request(&query)));
        let (watch, pump) = spawn_parts(source, sink, Nap::Frust, RECONNECT_BACKOFF.to_vec());
        frust::spawn(pump);
        watch
    }

    async fn signal_process(
        &self,
        pid: u32,
        start_unix_s: i64,
        signal: ProcessSignal,
    ) -> Result<(), AppFailure> {
        let client = self.client()?;
        let ack = client
            .authed()
            .signal_process(wire::SignalProcessRequest {
                pid,
                start_unix_s,
                signal: process_signal(signal) as i32,
            })
            .await
            .map_err(|status| self.map_status(&client, status))?
            .into_inner();
        signal_ack(&ack)
    }
}

fn status_failure(status: &Status) -> AppFailure {
    match status.code() {
        Code::FailedPrecondition => failures::monitoring_off(),
        Code::PermissionDenied => failures::read_only(status.message()),
        Code::InvalidArgument => failures::invalid_request(status.message()),
        _ => error::from_status(status),
    }
}

/// A refused ack is the server's answer, not a transport fault, so it never retries.
fn signal_ack(ack: &wire::ActionAck) -> Result<(), AppFailure> {
    if ack.ok {
        return Ok(());
    }
    let reason = if ack.error.is_empty() {
        "no reason given"
    } else {
        &ack.error
    };
    Err(failures::signal_refused(reason))
}

type WireStream<E> = Pin<Box<dyn Stream<Item = Result<E, Status>> + Send>>;

enum Opened<E> {
    Stream(WireStream<E>),
    Offline,
    /// The connection now holds another client than the one the watch began on.
    Switched,
    Refused(Status),
}

trait WatchSource: Send + Sync + 'static {
    type Event: Send + 'static;

    fn open(&self) -> impl Future<Output = Opened<Self::Event>> + Send;

    fn expired(&self);
}

enum Mapped<I> {
    Items(Vec<I>),
    Disabled,
}

trait WatchItem: Sized + Send + 'static {
    type Event;
    const LIVE: Self;
    const DROPPED: Self;
    const DISABLED: Self;

    fn map(event: Self::Event) -> Mapped<Self>;
}

impl WatchItem for VitalsItem {
    type Event = wire::VitalsEvent;
    const LIVE: Self = VitalsItem::Live;
    const DROPPED: Self = VitalsItem::Dropped;
    const DISABLED: Self = VitalsItem::Disabled;

    fn map(event: wire::VitalsEvent) -> Mapped<Self> {
        if event.disabled {
            return Mapped::Disabled;
        }
        let host = event.host.map(|host| VitalsItem::Host(map_host(host)));
        let sample = event
            .sample
            .map(|sample| VitalsItem::Sample(map_sample(sample)));
        Mapped::Items(host.into_iter().chain(sample).collect())
    }
}

impl WatchItem for ProcessItem {
    type Event = wire::ProcessSnapshot;
    const LIVE: Self = ProcessItem::Live;
    const DROPPED: Self = ProcessItem::Dropped;
    const DISABLED: Self = ProcessItem::Disabled;

    fn map(snapshot: wire::ProcessSnapshot) -> Mapped<Self> {
        if snapshot.disabled {
            return Mapped::Disabled;
        }
        Mapped::Items(vec![ProcessItem::Snapshot(map_snapshot(snapshot))])
    }
}

/// Remembers the client of the last open, so an expiry is reported against the one that failed,
/// and the client the watch began on, so a reopen never lands on another server.
struct Link<Q> {
    connection: Arc<ConnectionController>,
    last: Mutex<Option<MuxrClient>>,
    bound: Mutex<Option<MuxrClient>>,
    request: Q,
}

enum Reach {
    Client(MuxrClient),
    Offline,
    Switched,
}

impl<Q> Link<Q> {
    fn client(&self) -> Reach {
        let current = self.connection.client();
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = current.clone();
        let Some(client) = current else {
            return Reach::Offline;
        };
        let mut bound = self.bound.lock().unwrap_or_else(PoisonError::into_inner);
        match bound.as_ref() {
            Some(first) if !first.is_same(&client) => Reach::Switched,
            Some(_) => Reach::Client(client),
            None => {
                *bound = Some(client.clone());
                Reach::Client(client)
            }
        }
    }

    fn expired(&self) {
        if let Some(client) = self
            .last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            self.connection.on_session_expired_for(client);
        }
    }
}

struct VitalsSource(Link<wire::WatchVitalsRequest>);

impl WatchSource for VitalsSource {
    type Event = wire::VitalsEvent;

    async fn open(&self) -> Opened<wire::VitalsEvent> {
        let client = match self.0.client() {
            Reach::Client(client) => client,
            Reach::Offline => return Opened::Offline,
            Reach::Switched => return Opened::Switched,
        };
        match client.authed().watch_vitals(self.0.request).await {
            Ok(response) => Opened::Stream(Box::pin(response.into_inner())),
            Err(status) => Opened::Refused(status),
        }
    }

    fn expired(&self) {
        self.0.expired();
    }
}

struct ProcessSource(Link<wire::WatchProcessesRequest>);

impl WatchSource for ProcessSource {
    type Event = wire::ProcessSnapshot;

    async fn open(&self) -> Opened<wire::ProcessSnapshot> {
        let client = match self.0.client() {
            Reach::Client(client) => client,
            Reach::Offline => return Opened::Offline,
            Reach::Switched => return Opened::Switched,
        };
        match client
            .authed()
            .watch_processes(self.0.request.clone())
            .await
        {
            Ok(response) => Opened::Stream(Box::pin(response.into_inner())),
            Err(status) => Opened::Refused(status),
        }
    }

    fn expired(&self) {
        self.0.expired();
    }
}

enum End {
    Dropped { delivered: bool },
    Disabled,
    Expired,
    Switched,
}

fn stream_end(status: &Status, delivered: bool) -> End {
    match status.code() {
        Code::FailedPrecondition => End::Disabled,
        Code::Unauthenticated => End::Expired,
        _ => End::Dropped { delivered },
    }
}

fn spawn_parts<S, I>(
    source: S,
    sink: Box<dyn Fn(I) + Send>,
    nap: Nap,
    schedule: Vec<Duration>,
) -> (MonitorWatch, impl Future<Output = ()> + Send + 'static)
where
    S: WatchSource,
    I: WatchItem<Event = S::Event>,
{
    let stop = Arc::new(Stop::default());
    let handle_stop = Arc::clone(&stop);
    let watch = MonitorWatch::new(move || handle_stop.stop());
    let pump = Pump {
        source,
        sink: Mutex::new(sink),
        stop,
        nap,
        schedule,
    };
    (watch, pump.run())
}

/// The sink's `Mutex` lets a borrow cross an `await`.
struct Pump<S, I> {
    source: S,
    sink: Mutex<Box<dyn Fn(I) + Send>>,
    stop: Arc<Stop>,
    nap: Nap,
    schedule: Vec<Duration>,
}

impl<S, I> Pump<S, I>
where
    S: WatchSource,
    I: WatchItem<Event = S::Event>,
{
    fn emit(&self, item: I) {
        if !self.stop.is_stopped() {
            (self.sink.lock().unwrap_or_else(PoisonError::into_inner))(item);
        }
    }

    async fn run(self) {
        let mut step = 0;
        let mut dropped = false;
        loop {
            let end = match race(self.stop.stopped(), self.once(&mut dropped)).await {
                Winner::First(()) => return,
                Winner::Second(end) => end,
            };
            match end {
                End::Disabled => return self.emit(I::DISABLED),
                End::Expired => return self.source.expired(),
                End::Switched => {
                    if !dropped {
                        self.emit(I::DROPPED);
                    }
                    return;
                }
                End::Dropped { delivered: true } => step = 0,
                End::Dropped { delivered: false } => {}
            }
            if !dropped {
                dropped = true;
                self.emit(I::DROPPED);
            }
            let Some(&delay) = self.schedule.get(step).or(self.schedule.last()) else {
                return;
            };
            step += 1;
            if let Winner::First(()) = race(self.stop.stopped(), self.nap.sleep(delay)).await {
                return;
            }
        }
    }

    async fn once(&self, dropped: &mut bool) -> End {
        let mut stream = match self.source.open().await {
            Opened::Stream(stream) => stream,
            Opened::Offline => return End::Dropped { delivered: false },
            Opened::Switched => return End::Switched,
            Opened::Refused(status) => return stream_end(&status, false),
        };
        let mut delivered = false;
        while let Some(next) = stream.next().await {
            let event = match next {
                Ok(event) => event,
                Err(status) => return stream_end(&status, delivered),
            };
            delivered = true;
            if std::mem::take(dropped) {
                self.emit(I::LIVE);
            }
            match I::map(event) {
                Mapped::Disabled => return End::Disabled,
                Mapped::Items(items) => items.into_iter().for_each(|item| self.emit(item)),
            }
        }
        End::Dropped { delivered }
    }
}

fn vitals_request(detail: VitalsDetail) -> wire::WatchVitalsRequest {
    match detail {
        VitalsDetail::Summary => wire::WatchVitalsRequest {
            detail: wire::VitalsDetail::Summary as i32,
            interval_ms: 0,
        },
        VitalsDetail::Full { interval_ms } => wire::WatchVitalsRequest {
            detail: wire::VitalsDetail::Full as i32,
            interval_ms,
        },
    }
}

fn history_range(range: HistoryRange) -> wire::HistoryRange {
    match range {
        HistoryRange::OneMinute => wire::HistoryRange::HistoryRange1m,
        HistoryRange::FiveMinutes => wire::HistoryRange::HistoryRange5m,
        HistoryRange::FifteenMinutes => wire::HistoryRange::HistoryRange15m,
        HistoryRange::OneHour => wire::HistoryRange::HistoryRange1h,
    }
}

fn processes_request(query: &ProcessQuery) -> wire::WatchProcessesRequest {
    let sort = match query.sort {
        ProcessSort::Cpu => wire::ProcessSort::Cpu,
        ProcessSort::Memory => wire::ProcessSort::Memory,
        ProcessSort::Pid => wire::ProcessSort::Pid,
        ProcessSort::Name => wire::ProcessSort::Name,
    };
    let (pinned_pid, pinned_start_unix_s) = query.pinned.unwrap_or((0, 0));
    wire::WatchProcessesRequest {
        sort: sort as i32,
        reverse: query.reverse,
        filter: query.filter.clone(),
        limit: query.limit,
        pinned_pid,
        pinned_start_unix_s,
        interval_ms: query.interval_ms,
    }
}

fn process_signal(signal: ProcessSignal) -> wire::ProcessSignal {
    match signal {
        ProcessSignal::Term => wire::ProcessSignal::Term,
        ProcessSignal::Kill => wire::ProcessSignal::Kill,
        ProcessSignal::Int => wire::ProcessSignal::Int,
        ProcessSignal::Hup => wire::ProcessSignal::Hup,
    }
}

fn map_host(host: wire::HostInfo) -> HostInfo {
    HostInfo {
        hostname: host.hostname,
        os_name: host.os_name,
        os_version: host.os_version,
        kernel_version: host.kernel_version,
        arch: host.arch,
        cpu_brand: host.cpu_brand,
        logical_cores: host.logical_cores,
        physical_cores: host.physical_cores,
        memory_total_bytes: host.memory_total_bytes,
        boot_unix_s: (host.boot_unix_s != 0).then_some(host.boot_unix_s),
    }
}

fn map_sample(sample: wire::VitalsSample) -> VitalsSample {
    let memory = sample.memory.unwrap_or_default();
    VitalsSample {
        unix_ms: sample.unix_ms,
        cpu_percent: sample.cpu_percent,
        cpu_user_percent: sample.cpu_user_percent,
        cpu_system_percent: sample.cpu_system_percent,
        cpu_iowait_percent: sample.cpu_iowait_percent,
        core_percent: sample.core_percent,
        load: sample.load.map(|load| LoadAverage {
            one: load.load_1,
            five: load.load_5,
            fifteen: load.load_15,
        }),
        cpu_freq_mhz: sample.cpu_freq_mhz,
        memory: Memory {
            total: memory.total_bytes,
            used: memory.used_bytes,
            cache: memory.cache_bytes,
            free: memory.free_bytes,
            available: memory.available_bytes,
        },
        swap_used_bytes: sample.swap_used_bytes,
        swap_total_bytes: sample.swap_total_bytes,
        disks: sample
            .disks
            .into_iter()
            .map(|disk| DiskVolume {
                mount: disk.mount,
                fs: disk.fs,
                total_bytes: disk.total_bytes,
                used_bytes: disk.used_bytes,
            })
            .collect(),
        disk_read_bps: sample.disk_read_bps,
        disk_write_bps: sample.disk_write_bps,
        net: sample.net.map(|net| NetInterface {
            name: net.name,
            address: net.address,
            rx_bps: net.rx_bps,
            tx_bps: net.tx_bps,
            rx_total_bytes: net.rx_total_bytes,
            tx_total_bytes: net.tx_total_bytes,
        }),
        temperatures: sample
            .temperatures
            .into_iter()
            .map(|t| Temperature {
                label: t.label,
                celsius: t.celsius,
            })
            .collect(),
        process_count: sample.process_count,
        running_count: sample.running_count,
    }
}

/// A series of an unspecified or unknown kind is dropped rather than mislabelled.
fn series_kind(kind: i32) -> Option<SeriesKind> {
    match wire::SeriesKind::try_from(kind).ok()? {
        wire::SeriesKind::Unspecified => None,
        wire::SeriesKind::CpuTotal => Some(SeriesKind::CpuTotal),
        wire::SeriesKind::CpuSystem => Some(SeriesKind::CpuSystem),
        wire::SeriesKind::MemoryUsedFraction => Some(SeriesKind::MemoryUsedFraction),
        wire::SeriesKind::NetRxBps => Some(SeriesKind::NetRxBps),
        wire::SeriesKind::NetTxBps => Some(SeriesKind::NetTxBps),
        wire::SeriesKind::DiskReadBps => Some(SeriesKind::DiskReadBps),
        wire::SeriesKind::DiskWriteBps => Some(SeriesKind::DiskWriteBps),
    }
}

fn map_history(history: wire::VitalsHistory) -> VitalsHistory {
    VitalsHistory {
        step_ms: history.step_ms,
        end_unix_ms: history.end_unix_ms,
        revision: history.revision,
        series: history
            .series
            .into_iter()
            .filter_map(|series| {
                let values = series
                    .values
                    .into_iter()
                    .map(|v| (!v.is_nan()).then_some(v))
                    .collect();
                Some((series_kind(series.kind)?, values))
            })
            .collect(),
    }
}

fn process_state(state: i32) -> ProcessState {
    match wire::ProcessState::try_from(state) {
        Ok(wire::ProcessState::Running) => ProcessState::Running,
        Ok(wire::ProcessState::Sleeping) => ProcessState::Sleeping,
        Ok(wire::ProcessState::DiskWait) => ProcessState::DiskWait,
        Ok(wire::ProcessState::Zombie) => ProcessState::Zombie,
        Ok(wire::ProcessState::Stopped) => ProcessState::Stopped,
        Ok(wire::ProcessState::Idle) => ProcessState::Idle,
        Ok(wire::ProcessState::Dead) => ProcessState::Dead,
        Ok(wire::ProcessState::Unspecified | wire::ProcessState::Unknown) | Err(_) => {
            ProcessState::Unknown
        }
    }
}

fn map_snapshot(snapshot: wire::ProcessSnapshot) -> ProcessSnapshot {
    ProcessSnapshot {
        revision: snapshot.revision,
        unix_ms: snapshot.unix_ms,
        total: snapshot.total,
        running: snapshot.running,
        rows: snapshot
            .rows
            .into_iter()
            .map(|row| ProcessRow {
                pid: row.pid,
                parent_pid: row.parent_pid,
                name: row.name,
                command: row.command,
                user: row.user,
                cpu_percent: row.cpu_percent,
                memory_bytes: row.memory_bytes,
                threads: row.threads,
                start_unix_s: row.start_unix_s,
                cpu_time_ms: row.cpu_time_ms,
                state: process_state(row.state),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use clean_signals::Failure;

    use super::*;
    use crate::features::monitor::domain::failures::{MonitorFailureKind, monitor_failure_kind};
    use crate::net::testing::wait_until;

    const TEST_BACKOFF: [Duration; 2] = [Duration::from_millis(5), Duration::from_millis(5)];

    #[derive(Default)]
    struct MockScript {
        attempts: Mutex<VecDeque<Opened<wire::VitalsEvent>>>,
        opens: AtomicUsize,
        expired: AtomicUsize,
    }

    impl MockScript {
        fn new(attempts: Vec<Opened<wire::VitalsEvent>>) -> Arc<Self> {
            Arc::new(Self {
                attempts: Mutex::new(attempts.into()),
                ..Self::default()
            })
        }
    }

    impl WatchSource for Arc<MockScript> {
        type Event = wire::VitalsEvent;

        async fn open(&self) -> Opened<wire::VitalsEvent> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            let next = self.attempts.lock().expect("script").pop_front();
            next.unwrap_or_else(|| Opened::Stream(Box::pin(tokio_stream::pending())))
        }

        fn expired(&self) {
            self.expired.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn stream(
        events: Vec<Result<wire::VitalsEvent, Status>>,
        then_hang: bool,
    ) -> Opened<wire::VitalsEvent> {
        let events = tokio_stream::iter(events);
        if then_hang {
            Opened::Stream(Box::pin(events.chain(tokio_stream::pending())))
        } else {
            Opened::Stream(Box::pin(events))
        }
    }

    fn sample_event(cpu: f32, host: bool) -> wire::VitalsEvent {
        wire::VitalsEvent {
            revision: 1,
            host: host.then(|| wire::HostInfo {
                hostname: "workbench".to_string(),
                logical_cores: 8,
                ..Default::default()
            }),
            sample: Some(wire::VitalsSample {
                cpu_percent: cpu,
                ..Default::default()
            }),
            disabled: false,
            lagged: 3,
        }
    }

    fn disabled_event() -> wire::VitalsEvent {
        wire::VitalsEvent {
            disabled: true,
            ..Default::default()
        }
    }

    type Seen = Arc<Mutex<Vec<VitalsItem>>>;

    fn pump(script: &Arc<MockScript>) -> (MonitorWatch, tokio::task::JoinHandle<()>, Seen) {
        let seen: Seen = Arc::default();
        let sink_seen = Arc::clone(&seen);
        let sink: VitalsSink = Box::new(move |item| sink_seen.lock().expect("seen").push(item));
        let (watch, pump) =
            spawn_parts(Arc::clone(script), sink, Nap::Tokio, TEST_BACKOFF.to_vec());
        (watch, tokio::spawn(pump), seen)
    }

    fn kinds(seen: &Seen) -> Vec<&'static str> {
        seen.lock()
            .expect("seen")
            .iter()
            .map(|item| match item {
                VitalsItem::Host(_) => "host",
                VitalsItem::Sample(_) => "sample",
                VitalsItem::Live => "live",
                VitalsItem::Dropped => "dropped",
                VitalsItem::Disabled => "disabled",
            })
            .collect()
    }

    async fn finished(task: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the pump ends")
            .expect("the pump does not panic");
    }

    #[tokio::test]
    async fn a_disabled_event_ends_the_watch_with_disabled_and_never_reopens() {
        let script = MockScript::new(vec![stream(
            vec![Ok(sample_event(12.5, true)), Ok(disabled_event())],
            true,
        )]);
        let (_watch, task, seen) = pump(&script);
        finished(task).await;
        assert_eq!(kinds(&seen), ["host", "sample", "disabled"]);
        assert_eq!(script.opens.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_precondition_on_open_reads_disabled_without_a_retry() {
        let script = MockScript::new(vec![Opened::Refused(Status::failed_precondition("off"))]);
        let (_watch, task, seen) = pump(&script);
        finished(task).await;
        assert_eq!(kinds(&seen), ["disabled"]);
        assert_eq!(script.opens.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn resource_exhausted_backs_off_and_the_next_stream_reports_live() {
        let script = MockScript::new(vec![
            Opened::Refused(Status::resource_exhausted("16 streams open")),
            stream(vec![Ok(sample_event(40.0, true))], true),
        ]);
        let (watch, task, seen) = pump(&script);
        wait_until("the retry delivers", || {
            seen.lock().expect("seen").len() == 4
        })
        .await;
        assert_eq!(kinds(&seen), ["dropped", "live", "host", "sample"]);
        drop(watch);
        finished(task).await;
        assert_eq!(script.opens.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_broken_stream_reports_dropped_once_across_failed_retries() {
        let script = MockScript::new(vec![
            stream(
                vec![
                    Ok(sample_event(1.0, true)),
                    Err(Status::unavailable("reset")),
                ],
                false,
            ),
            Opened::Offline,
            Opened::Refused(Status::unavailable("refused")),
            stream(vec![Ok(sample_event(2.0, false))], true),
        ]);
        let (watch, task, seen) = pump(&script);
        wait_until("the fourth open delivers", || {
            seen.lock().expect("seen").len() == 5
        })
        .await;
        assert_eq!(
            kinds(&seen),
            ["host", "sample", "dropped", "live", "sample"]
        );
        drop(watch);
        finished(task).await;
    }

    #[tokio::test]
    async fn unauthenticated_fires_the_session_expired_hook_and_ends() {
        let script = MockScript::new(vec![Opened::Refused(Status::unauthenticated("expired"))]);
        let (_watch, task, seen) = pump(&script);
        finished(task).await;
        assert!(kinds(&seen).is_empty());
        assert_eq!(script.expired.load(Ordering::SeqCst), 1);
        assert_eq!(script.opens.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dropping_the_handle_ends_the_pump_mid_stream() {
        let script = MockScript::new(vec![stream(vec![Ok(sample_event(5.0, true))], true)]);
        let (watch, task, seen) = pump(&script);
        wait_until("the stream delivers", || {
            seen.lock().expect("seen").len() == 2
        })
        .await;
        drop(watch);
        finished(task).await;
        assert_eq!(kinds(&seen), ["host", "sample"]);
    }

    #[test]
    fn a_vitals_event_yields_host_then_sample_and_ignores_lagged() {
        let Mapped::Items(items) = VitalsItem::map(sample_event(33.0, true)) else {
            panic!("a sample event is not disabled");
        };
        assert!(matches!(&items[0], VitalsItem::Host(host) if host.hostname == "workbench"));
        assert!(matches!(&items[1], VitalsItem::Sample(s) if s.cpu_percent == 33.0));
        assert_eq!(items.len(), 2);
        let disabled = wire::ProcessSnapshot {
            disabled: true,
            ..Default::default()
        };
        assert!(matches!(ProcessItem::map(disabled), Mapped::Disabled));
    }

    #[test]
    fn absent_optional_sample_and_host_fields_read_none() {
        let sample = map_sample(wire::VitalsSample::default());
        assert_eq!(sample.cpu_user_percent, None);
        assert_eq!(sample.cpu_system_percent, None);
        assert_eq!(sample.cpu_iowait_percent, None);
        assert_eq!(sample.load, None);
        assert_eq!(sample.cpu_freq_mhz, None);
        assert_eq!(sample.memory, Memory::default());
        assert_eq!(sample.disk_read_bps, None);
        assert_eq!(sample.disk_write_bps, None);
        assert_eq!(sample.net, None);
        assert_eq!(sample.process_count, None);
        assert_eq!(sample.running_count, None);
        let host = map_host(wire::HostInfo::default());
        assert_eq!(host.physical_cores, None);
        assert_eq!(host.boot_unix_s, None);
    }

    #[test]
    fn a_full_sample_maps_every_field() {
        let wire_sample = wire::VitalsSample {
            unix_ms: 1_700,
            cpu_percent: 50.0,
            cpu_user_percent: Some(30.0),
            cpu_system_percent: Some(15.0),
            cpu_iowait_percent: Some(5.0),
            core_percent: vec![10.0, 90.0],
            load: Some(wire::LoadAverage {
                load_1: 1.5,
                load_5: 1.0,
                load_15: 0.5,
            }),
            cpu_freq_mhz: Some(3_400),
            memory: Some(wire::HostMemory {
                total_bytes: 100,
                used_bytes: 50,
                cache_bytes: 30,
                free_bytes: 20,
                available_bytes: 45,
            }),
            swap_used_bytes: 7,
            swap_total_bytes: 8,
            disks: vec![wire::DiskVolume {
                mount: "/".to_string(),
                fs: "ext4".to_string(),
                total_bytes: 10,
                used_bytes: 4,
            }],
            disk_read_bps: Some(11),
            disk_write_bps: Some(12),
            net: Some(wire::NetInterface {
                name: "enp5s0".to_string(),
                address: Some("10.0.0.2".to_string()),
                rx_bps: Some(13),
                tx_bps: None,
                rx_total_bytes: 14,
                tx_total_bytes: 15,
            }),
            temperatures: vec![wire::HostTemperature {
                label: "Package id 0".to_string(),
                celsius: 61.0,
            }],
            process_count: Some(300),
            running_count: Some(2),
        };
        let sample = map_sample(wire_sample);
        assert_eq!(sample.unix_ms, 1_700);
        assert_eq!(sample.cpu_user_percent, Some(30.0));
        assert_eq!(sample.core_percent, [10.0, 90.0]);
        assert_eq!(sample.load.map(|l| l.fifteen), Some(0.5));
        assert_eq!(sample.cpu_freq_mhz, Some(3_400));
        assert_eq!(
            sample.memory,
            Memory {
                total: 100,
                used: 50,
                cache: 30,
                free: 20,
                available: 45
            }
        );
        assert_eq!((sample.swap_used_bytes, sample.swap_total_bytes), (7, 8));
        assert_eq!(sample.disks[0].mount, "/");
        assert_eq!(
            (sample.disk_read_bps, sample.disk_write_bps),
            (Some(11), Some(12))
        );
        let net = sample.net.expect("net is set");
        assert_eq!(net.address.as_deref(), Some("10.0.0.2"));
        assert_eq!((net.rx_bps, net.tx_bps), (Some(13), None));
        assert_eq!(sample.temperatures[0].celsius, 61.0);
        assert_eq!(
            (sample.process_count, sample.running_count),
            (Some(300), Some(2))
        );
    }

    #[test]
    fn history_nan_slots_read_none_and_absent_series_stay_absent() {
        let history = map_history(wire::VitalsHistory {
            step_ms: 2_000,
            end_unix_ms: 60_000,
            revision: 9,
            series: vec![
                wire::HistorySeries {
                    kind: wire::SeriesKind::CpuTotal as i32,
                    values: vec![f32::NAN, 12.0, f32::NAN, 40.0],
                },
                wire::HistorySeries {
                    kind: wire::SeriesKind::Unspecified as i32,
                    values: vec![1.0],
                },
                wire::HistorySeries {
                    kind: 99,
                    values: vec![1.0],
                },
            ],
        });
        assert_eq!(
            (history.step_ms, history.end_unix_ms, history.revision),
            (2_000, 60_000, 9)
        );
        assert_eq!(
            history.series(SeriesKind::CpuTotal),
            Some(&[None, Some(12.0), None, Some(40.0)][..])
        );
        assert_eq!(history.series(SeriesKind::CpuSystem), None);
        assert_eq!(
            history.series.len(),
            1,
            "unspecified and unknown kinds are dropped"
        );
    }

    #[test]
    fn unspecified_and_unknown_process_states_read_unknown() {
        assert_eq!(
            process_state(wire::ProcessState::Unspecified as i32),
            ProcessState::Unknown
        );
        assert_eq!(process_state(99), ProcessState::Unknown);
        assert_eq!(
            process_state(wire::ProcessState::DiskWait as i32),
            ProcessState::DiskWait
        );
        let snapshot = map_snapshot(wire::ProcessSnapshot {
            revision: 4,
            unix_ms: 5,
            total: 300,
            running: 3,
            rows: vec![wire::ProcessRow {
                pid: 42,
                name: "cargo".to_string(),
                start_unix_s: 1_000,
                state: wire::ProcessState::Running as i32,
                ..Default::default()
            }],
            disabled: false,
        });
        let row = &snapshot.rows[0];
        assert_eq!(
            (row.pid, row.start_unix_s, row.state),
            (42, 1_000, ProcessState::Running)
        );
        assert_eq!(
            (row.parent_pid, row.memory_bytes, row.threads),
            (None, None, None)
        );
        assert_eq!((snapshot.total, snapshot.running), (300, 3));
    }

    #[test]
    fn queries_build_their_wire_requests() {
        let full = vitals_request(VitalsDetail::Full { interval_ms: 2_000 });
        assert_eq!(full.detail, wire::VitalsDetail::Full as i32);
        assert_eq!(full.interval_ms, 2_000);
        assert_eq!(vitals_request(VitalsDetail::Summary).interval_ms, 0);
        let request = processes_request(&ProcessQuery {
            sort: ProcessSort::Memory,
            reverse: true,
            filter: "car".to_string(),
            limit: 20,
            pinned: Some((42, 1_000)),
            interval_ms: 3_000,
        });
        assert_eq!(request.sort, wire::ProcessSort::Memory as i32);
        assert_eq!(
            (request.pinned_pid, request.pinned_start_unix_s),
            (42, 1_000)
        );
        assert_eq!(processes_request(&ProcessQuery::default()).pinned_pid, 0);
        assert_eq!(
            history_range(HistoryRange::OneHour),
            wire::HistoryRange::HistoryRange1h
        );
        assert_eq!(process_signal(ProcessSignal::Hup), wire::ProcessSignal::Hup);
    }

    #[test]
    fn signal_refusals_map_to_distinct_failures() {
        let kind = |status: Status| monitor_failure_kind(&status_failure(&status));
        assert_eq!(
            kind(Status::permission_denied("read-only token")),
            MonitorFailureKind::ReadOnly
        );
        assert_eq!(
            kind(Status::failed_precondition("monitoring off")),
            MonitorFailureKind::MonitoringOff
        );
        assert_eq!(
            kind(Status::invalid_argument("pid 0")),
            MonitorFailureKind::InvalidRequest
        );
        assert!(matches!(
            status_failure(&Status::unauthenticated("expired")),
            AppFailure::Auth(_)
        ));
        assert!(status_failure(&Status::unavailable("down")).is_retryable());
    }

    #[test]
    fn read_only_and_off_are_internal_and_not_retryable() {
        for status in [
            Status::permission_denied("read-only token"),
            Status::failed_precondition("off"),
            Status::invalid_argument("pid 0"),
        ] {
            let failure = status_failure(&status);
            assert!(matches!(failure, AppFailure::Internal(_)), "{failure}");
            assert!(!failure.is_retryable());
        }
    }

    #[test]
    fn a_refused_ack_is_a_non_retryable_refusal_with_the_server_text() {
        let refused = wire::ActionAck {
            ok: false,
            error: "no such process".to_string(),
            ..Default::default()
        };
        let failure = signal_ack(&refused).expect_err("refused");
        assert!(!failure.is_retryable());
        assert_eq!(
            monitor_failure_kind(&failure),
            MonitorFailureKind::Refused("no such process".to_string())
        );
        let ok = wire::ActionAck {
            ok: true,
            ..Default::default()
        };
        assert!(signal_ack(&ok).is_ok());
    }

    #[tokio::test]
    async fn a_reopen_on_another_client_ends_the_pump_with_dropped() {
        let script = MockScript::new(vec![
            Opened::Refused(Status::unavailable("reset")),
            Opened::Switched,
        ]);
        let (_watch, task, seen) = pump(&script);
        finished(task).await;
        assert_eq!(kinds(&seen), ["dropped"]);
        assert_eq!(script.opens.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_link_binds_to_its_first_client_and_refuses_another() {
        let fixture = crate::net::testing::MuxrdFixture::start().await;
        let first = fixture.connect_pinned().await;
        let other = fixture.connect_pinned().await;
        let link = Link {
            connection: ConnectionController::with_live_client_for_tests(first.clone()),
            last: Mutex::new(None),
            bound: Mutex::new(None),
            request: (),
        };
        assert!(matches!(link.client(), Reach::Client(c) if c.is_same(&first)));
        assert!(matches!(link.client(), Reach::Client(_)));
        *link.bound.lock().expect("bound") = Some(other);
        assert!(matches!(link.client(), Reach::Switched));
    }

    #[tokio::test]
    async fn one_shot_calls_fail_as_network_while_disconnected() {
        let repo = GrpcMonitorRepository::new(Arc::new(ConnectionController::detached()));
        let history = repo.vitals_history(HistoryRange::OneMinute).await;
        assert!(matches!(history, Err(AppFailure::Network(_))));
        let signal = repo.signal_process(42, 1_000, ProcessSignal::Term).await;
        assert!(matches!(signal, Err(AppFailure::Network(_))));
    }
}
