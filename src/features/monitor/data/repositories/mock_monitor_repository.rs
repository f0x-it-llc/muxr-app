//! Scriptable in-memory `MonitorRepository`: a seeded host, sample and snapshot queues a test
//! pushes into, `disabled`/`dropped` switches and a failing signal. Every call is recorded;
//! sinks run on the pushing thread, outside the lock.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::failure::AppFailure;
use crate::features::monitor::domain::entities::{
    DiskVolume, HistoryRange, HostInfo, LoadAverage, Memory, NetInterface, ProcessItem,
    ProcessQuery, ProcessRow, ProcessSignal, ProcessSnapshot, ProcessState, SeriesKind,
    Temperature, VitalsDetail, VitalsHistory, VitalsItem, VitalsSample,
};
use crate::features::monitor::domain::failures;
use crate::features::monitor::domain::repositories::{
    MonitorRepository, MonitorWatch, ProcessSink, VitalsSink,
};

type Shared<S> = Arc<Mutex<S>>;

type Sink<I> = Box<dyn Fn(I) + Send>;

struct Watcher<S> {
    id: u64,
    sink: Shared<S>,
}

#[derive(Default)]
struct State {
    host: HostInfo,
    samples: VecDeque<VitalsSample>,
    snapshots: VecDeque<ProcessSnapshot>,
    disabled: bool,
    dropped: bool,
    signal_failure: Option<AppFailure>,
    next_id: u64,
    vitals: Vec<Watcher<VitalsSink>>,
    processes: Vec<Watcher<ProcessSink>>,
    vitals_opens: Vec<VitalsDetail>,
    process_opens: Vec<ProcessQuery>,
    history_requests: Vec<HistoryRange>,
    signals: Vec<(u32, i64, ProcessSignal)>,
    watch_drops: usize,
}

pub struct MockMonitorRepository {
    state: Arc<Mutex<State>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn deliver<I: Clone>(sinks: &[Shared<Sink<I>>], items: &[I]) {
    for sink in sinks {
        let sink = sink.lock().unwrap_or_else(PoisonError::into_inner);
        items.iter().cloned().for_each(&*sink);
    }
}

fn sinks<S>(watchers: &[Watcher<S>]) -> Vec<Shared<S>> {
    watchers.iter().map(|w| Arc::clone(&w.sink)).collect()
}

impl MockMonitorRepository {
    pub fn seeded() -> Self {
        let state = State {
            host: seed_host(),
            samples: VecDeque::from([seed_sample()]),
            snapshots: VecDeque::from([seed_snapshot()]),
            ..State::default()
        };
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    pub fn set_host(&self, host: HostInfo) {
        self.state().host = host;
    }

    /// Delivered to every flowing vitals watch, else queued for the next one to open or resume.
    pub fn push_sample(&self, sample: VitalsSample) {
        let mut state = self.state();
        if state.vitals.is_empty() || state.dropped || state.disabled {
            state.samples.push_back(sample);
            return;
        }
        let targets = sinks(&state.vitals);
        drop(state);
        deliver(&targets, &[VitalsItem::Sample(sample)]);
    }

    pub fn push_snapshot(&self, snapshot: ProcessSnapshot) {
        let mut state = self.state();
        if state.processes.is_empty() || state.dropped || state.disabled {
            state.snapshots.push_back(snapshot);
            return;
        }
        let targets = sinks(&state.processes);
        drop(state);
        deliver(&targets, &[ProcessItem::Snapshot(snapshot)]);
    }

    /// Switching on ends every open watch with `Disabled`; later opens end at once.
    pub fn set_disabled(&self, disabled: bool) {
        let mut state = self.state();
        state.disabled = disabled;
        if !disabled {
            return;
        }
        let vitals = sinks(&std::mem::take(&mut state.vitals));
        let processes = sinks(&std::mem::take(&mut state.processes));
        drop(state);
        deliver(&vitals, &[VitalsItem::Disabled]);
        deliver(&processes, &[ProcessItem::Disabled]);
    }

    /// `true` sends `Dropped` to every open watch and queues pushes; `false` sends `Live` and
    /// flushes the queues.
    pub fn set_dropped(&self, dropped: bool) {
        let mut state = self.state();
        if state.dropped == dropped {
            return;
        }
        state.dropped = dropped;
        let vitals = sinks(&state.vitals);
        let processes = sinks(&state.processes);
        let (mut vitals_items, mut process_items) = if dropped {
            (vec![VitalsItem::Dropped], vec![ProcessItem::Dropped])
        } else {
            (vec![VitalsItem::Live], vec![ProcessItem::Live])
        };
        if !dropped && !vitals.is_empty() {
            vitals_items.extend(state.samples.drain(..).map(VitalsItem::Sample));
        }
        if !dropped && !processes.is_empty() {
            process_items.extend(state.snapshots.drain(..).map(ProcessItem::Snapshot));
        }
        drop(state);
        deliver(&vitals, &vitals_items);
        deliver(&processes, &process_items);
    }

    pub fn fail_signal(&self, failure: Option<AppFailure>) {
        self.state().signal_failure = failure;
    }

    pub fn vitals_opens(&self) -> Vec<VitalsDetail> {
        self.state().vitals_opens.clone()
    }

    pub fn process_opens(&self) -> Vec<ProcessQuery> {
        self.state().process_opens.clone()
    }

    pub fn history_requests(&self) -> Vec<HistoryRange> {
        self.state().history_requests.clone()
    }

    pub fn signals(&self) -> Vec<(u32, i64, ProcessSignal)> {
        self.state().signals.clone()
    }

    pub fn watch_drops(&self) -> usize {
        self.state().watch_drops
    }

    pub fn open_watches(&self) -> usize {
        let state = self.state();
        state.vitals.len() + state.processes.len()
    }

    fn stopper(&self, id: u64) -> MonitorWatch {
        let state = Arc::clone(&self.state);
        MonitorWatch::new(move || {
            let mut state = lock(&state);
            state.vitals.retain(|w| w.id != id);
            state.processes.retain(|w| w.id != id);
            state.watch_drops += 1;
        })
    }
}

impl Default for MockMonitorRepository {
    fn default() -> Self {
        Self::seeded()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl MonitorRepository for MockMonitorRepository {
    fn watch_vitals(&self, detail: VitalsDetail, sink: VitalsSink) -> MonitorWatch {
        let mut state = self.state();
        state.vitals_opens.push(detail);
        state.next_id += 1;
        let id = state.next_id;
        if state.disabled {
            drop(state);
            sink(VitalsItem::Disabled);
            return self.stopper(id);
        }
        let mut items = vec![VitalsItem::Host(state.host.clone())];
        if state.dropped {
            items.push(VitalsItem::Dropped);
        } else {
            items.extend(state.samples.drain(..).map(VitalsItem::Sample));
        }
        let sink = Arc::new(Mutex::new(sink));
        state.vitals.push(Watcher {
            id,
            sink: Arc::clone(&sink),
        });
        drop(state);
        deliver(&[sink], &items);
        self.stopper(id)
    }

    async fn vitals_history(&self, range: HistoryRange) -> Result<VitalsHistory, AppFailure> {
        let mut state = self.state();
        state.history_requests.push(range);
        if state.disabled {
            return Err(failures::monitoring_off());
        }
        Ok(seed_history(range))
    }

    fn watch_processes(&self, query: ProcessQuery, sink: ProcessSink) -> MonitorWatch {
        let mut state = self.state();
        state.process_opens.push(query);
        state.next_id += 1;
        let id = state.next_id;
        if state.disabled {
            drop(state);
            sink(ProcessItem::Disabled);
            return self.stopper(id);
        }
        let items: Vec<ProcessItem> = if state.dropped {
            vec![ProcessItem::Dropped]
        } else {
            state
                .snapshots
                .drain(..)
                .map(ProcessItem::Snapshot)
                .collect()
        };
        let sink = Arc::new(Mutex::new(sink));
        state.processes.push(Watcher {
            id,
            sink: Arc::clone(&sink),
        });
        drop(state);
        deliver(&[sink], &items);
        self.stopper(id)
    }

    async fn signal_process(
        &self,
        pid: u32,
        start_unix_s: i64,
        signal: ProcessSignal,
    ) -> Result<(), AppFailure> {
        let mut state = self.state();
        state.signals.push((pid, start_unix_s, signal));
        if state.disabled {
            return Err(failures::monitoring_off());
        }
        state.signal_failure.clone().map_or(Ok(()), Err)
    }
}

const GIB: u64 = 1 << 30;

fn seed_host() -> HostInfo {
    HostInfo {
        hostname: "workbench".to_string(),
        os_name: "Arch Linux".to_string(),
        os_version: "rolling".to_string(),
        kernel_version: "6.18.2-arch1-1".to_string(),
        arch: "x86_64".to_string(),
        cpu_brand: "AMD Ryzen 7 7840U".to_string(),
        logical_cores: 16,
        physical_cores: Some(8),
        memory_total_bytes: 32 * GIB,
        boot_unix_s: Some(1_790_000_000),
    }
}

fn seed_sample() -> VitalsSample {
    VitalsSample {
        unix_ms: 1_790_086_400_000,
        cpu_percent: 23.5,
        cpu_user_percent: Some(15.0),
        cpu_system_percent: Some(6.5),
        cpu_iowait_percent: Some(2.0),
        core_percent: vec![
            31.0, 12.0, 44.0, 8.0, 19.0, 27.0, 3.0, 60.0, 14.0, 22.0, 9.0, 35.0, 18.0, 41.0, 6.0,
            27.0,
        ],
        load: Some(LoadAverage {
            one: 1.42,
            five: 1.10,
            fifteen: 0.87,
        }),
        cpu_freq_mhz: Some(3_300),
        memory: Memory {
            total: 32 * GIB,
            used: 12 * GIB,
            cache: 9 * GIB,
            free: 11 * GIB,
            available: 19 * GIB,
        },
        swap_used_bytes: GIB / 2,
        swap_total_bytes: 8 * GIB,
        disks: vec![DiskVolume {
            mount: "/".to_string(),
            fs: "ext4".to_string(),
            total_bytes: 1_000 * GIB,
            used_bytes: 412 * GIB,
        }],
        disk_read_bps: Some(2_400_000),
        disk_write_bps: Some(880_000),
        net: Some(NetInterface {
            name: "wlan0".to_string(),
            address: Some("192.168.1.24".to_string()),
            rx_bps: Some(1_250_000),
            tx_bps: Some(96_000),
            rx_total_bytes: 48 * GIB,
            tx_total_bytes: 6 * GIB,
        }),
        temperatures: vec![
            Temperature {
                label: "Tctl".to_string(),
                celsius: 58.0,
            },
            Temperature {
                label: "nvme Composite".to_string(),
                celsius: 41.0,
            },
        ],
        process_count: Some(412),
        running_count: Some(3),
    }
}

fn row(pid: u32, name: &str, user: &str, cpu: f32, memory: u64, state: ProcessState) -> ProcessRow {
    ProcessRow {
        pid,
        parent_pid: Some(1),
        name: name.to_string(),
        command: format!("/usr/bin/{name}"),
        user: user.to_string(),
        cpu_percent: cpu,
        memory_bytes: Some(memory),
        threads: None,
        start_unix_s: 1_790_000_000 + i64::from(pid),
        cpu_time_ms: u64::from(pid) * 1_000,
        state,
    }
}

fn seed_snapshot() -> ProcessSnapshot {
    ProcessSnapshot {
        revision: 1,
        unix_ms: 1_790_086_400_000,
        total: 412,
        running: 3,
        rows: vec![
            row(4211, "cargo", "ed", 88.0, 2 * GIB, ProcessState::Running),
            row(1830, "firefox", "ed", 12.5, 3 * GIB, ProcessState::Sleeping),
            row(902, "muxrd", "ed", 1.5, 64 << 20, ProcessState::Sleeping),
            row(
                611,
                "postgres",
                "postgres",
                0.8,
                512 << 20,
                ProcessState::Idle,
            ),
        ],
    }
}

fn seed_history(range: HistoryRange) -> VitalsHistory {
    let (step_ms, slots) = match range {
        HistoryRange::OneMinute => (2_000, 30),
        HistoryRange::FiveMinutes => (5_000, 60),
        HistoryRange::FifteenMinutes => (15_000, 60),
        HistoryRange::OneHour => (60_000, 60),
    };
    let wave = |base: f32, swing: f32| -> Vec<Option<f32>> {
        (0..slots)
            .map(|i| (i > 2).then(|| base + swing * ((i as f32) * 0.4).sin()))
            .collect()
    };
    VitalsHistory {
        step_ms,
        end_unix_ms: 1_790_086_400_000,
        revision: 1,
        series: vec![
            (SeriesKind::CpuTotal, wave(30.0, 15.0)),
            (SeriesKind::CpuSystem, wave(8.0, 4.0)),
            (SeriesKind::MemoryUsedFraction, wave(0.4, 0.05)),
            (SeriesKind::NetRxBps, wave(900_000.0, 400_000.0)),
            (SeriesKind::NetTxBps, wave(90_000.0, 40_000.0)),
            (SeriesKind::DiskReadBps, wave(2_000_000.0, 1_000_000.0)),
            (SeriesKind::DiskWriteBps, wave(800_000.0, 300_000.0)),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Seen<I> = Arc<Mutex<Vec<I>>>;

    fn recorder<I: Send + 'static>() -> (Sink<I>, Seen<I>) {
        let seen: Seen<I> = Arc::default();
        let sink_seen = Arc::clone(&seen);
        (
            Box::new(move |item| sink_seen.lock().expect("seen").push(item)),
            seen,
        )
    }

    fn sample(cpu: f32) -> VitalsSample {
        VitalsSample {
            cpu_percent: cpu,
            ..VitalsSample::default()
        }
    }

    #[test]
    fn dropping_a_monitor_watch_stops_its_deliveries() {
        let mock = MockMonitorRepository::seeded();
        let (sink, seen) = recorder();
        let watch = mock.watch_vitals(VitalsDetail::Summary, sink);
        assert_eq!(
            seen.lock().expect("seen").len(),
            2,
            "host and the seeded sample"
        );
        mock.push_sample(sample(50.0));
        assert_eq!(seen.lock().expect("seen").len(), 3);
        drop(watch);
        mock.push_sample(sample(60.0));
        assert_eq!(
            seen.lock().expect("seen").len(),
            3,
            "nothing after the drop"
        );
        assert_eq!(mock.watch_drops(), 1);
        assert_eq!(mock.open_watches(), 0);
    }

    #[tokio::test]
    async fn opens_history_and_signals_are_recorded() {
        let mock = MockMonitorRepository::seeded();
        let detail = VitalsDetail::Full { interval_ms: 2_000 };
        let _vitals = mock.watch_vitals(detail, recorder().0);
        let query = ProcessQuery {
            filter: "car".to_string(),
            ..ProcessQuery::default()
        };
        let (sink, seen) = recorder();
        let _processes = mock.watch_processes(query.clone(), sink);
        assert!(
            matches!(&seen.lock().expect("seen")[0], ProcessItem::Snapshot(s) if s.rows.len() == 4)
        );
        let history = mock.vitals_history(HistoryRange::OneHour).await;
        assert_eq!(
            history
                .expect("seeded")
                .series(SeriesKind::CpuTotal)
                .map(<[_]>::len),
            Some(60)
        );
        let sent = mock.signal_process(4211, 7, ProcessSignal::Term).await;
        assert!(sent.is_ok());
        assert_eq!(mock.vitals_opens(), [detail]);
        assert_eq!(mock.process_opens(), [query]);
        assert_eq!(mock.history_requests(), [HistoryRange::OneHour]);
        assert_eq!(mock.signals(), [(4211, 7, ProcessSignal::Term)]);
    }

    #[test]
    fn dropped_then_live_queues_pushes_and_disabled_ends_every_watch() {
        let mock = MockMonitorRepository::seeded();
        let (sink, seen) = recorder();
        let _watch = mock.watch_vitals(VitalsDetail::Summary, sink);
        mock.set_dropped(true);
        mock.push_sample(sample(70.0));
        mock.set_dropped(false);
        mock.set_disabled(true);
        let items = seen.lock().expect("seen").clone();
        assert!(matches!(items[2], VitalsItem::Dropped));
        assert!(matches!(items[3], VitalsItem::Live));
        assert!(matches!(&items[4], VitalsItem::Sample(s) if s.cpu_percent == 70.0));
        assert!(matches!(items[5], VitalsItem::Disabled));
        let (sink, later) = recorder();
        let _late = mock.watch_processes(ProcessQuery::default(), sink);
        assert_eq!(*later.lock().expect("seen"), [ProcessItem::Disabled]);
    }

    #[tokio::test]
    async fn a_scripted_signal_failure_is_returned() {
        let mock = MockMonitorRepository::seeded();
        mock.fail_signal(Some(AppFailure::Network("not signalled".to_string())));
        let refused = mock.signal_process(1, 0, ProcessSignal::Kill).await;
        assert_eq!(
            refused,
            Err(AppFailure::Network("not signalled".to_string()))
        );
    }
}
