//! Plain-data host vitals and process table of the connected server; wire shapes stay in
//! `data`, and an optional the host does not report is `None`.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostInfo {
    pub hostname: String,
    pub os_name: String,
    pub os_version: String,
    pub kernel_version: String,
    pub arch: String,
    pub cpu_brand: String,
    pub logical_cores: u32,
    pub physical_cores: Option<u32>,
    pub memory_total_bytes: u64,
    pub boot_unix_s: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

/// `used + cache + free == total`; `available` overlaps cache and free.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    pub total: u64,
    pub used: u64,
    pub cache: u64,
    pub free: u64,
    pub available: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DiskVolume {
    pub mount: String,
    pub fs: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetInterface {
    pub name: String,
    pub address: Option<String>,
    pub rx_bps: Option<u64>,
    pub tx_bps: Option<u64>,
    pub rx_total_bytes: u64,
    pub tx_total_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Temperature {
    pub label: String,
    pub celsius: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct VitalsSample {
    pub unix_ms: i64,
    pub cpu_percent: f32,
    pub cpu_user_percent: Option<f32>,
    pub cpu_system_percent: Option<f32>,
    pub cpu_iowait_percent: Option<f32>,
    pub core_percent: Vec<f32>,
    pub load: Option<LoadAverage>,
    pub cpu_freq_mhz: Option<u32>,
    pub memory: Memory,
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
    pub disks: Vec<DiskVolume>,
    pub disk_read_bps: Option<u64>,
    pub disk_write_bps: Option<u64>,
    pub net: Option<NetInterface>,
    pub temperatures: Vec<Temperature>,
    pub process_count: Option<u32>,
    pub running_count: Option<u32>,
}

/// `Summary` is the server's fixed 5 s cadence without per-core, temperature or full disk rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VitalsDetail {
    #[default]
    Summary,
    Full {
        interval_ms: u32,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HistoryRange {
    #[default]
    OneMinute,
    FiveMinutes,
    FifteenMinutes,
    OneHour,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SeriesKind {
    CpuTotal,
    CpuSystem,
    MemoryUsedFraction,
    NetRxBps,
    NetTxBps,
    DiskReadBps,
    DiskWriteBps,
}

/// Every series has one slot per step, oldest first; `None` is a slot without data, and a series
/// the host cannot provide is absent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VitalsHistory {
    pub step_ms: u32,
    pub end_unix_ms: i64,
    pub revision: u64,
    pub series: Vec<(SeriesKind, Vec<Option<f32>>)>,
}

impl VitalsHistory {
    pub fn series(&self, kind: SeriesKind) -> Option<&[Option<f32>]> {
        self.series
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, values)| values.as_slice())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProcessSort {
    #[default]
    Cpu,
    Memory,
    Pid,
    Name,
}

/// `pinned` is `(pid, start_unix_s)`; zero `limit`/`interval_ms` take the server defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessQuery {
    pub sort: ProcessSort,
    pub reverse: bool,
    pub filter: String,
    pub limit: u32,
    pub pinned: Option<(u32, i64)>,
    pub interval_ms: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProcessState {
    Running,
    Sleeping,
    DiskWait,
    Zombie,
    Stopped,
    Idle,
    Dead,
    #[default]
    Unknown,
}

/// Identity is `(pid, start_unix_s)`; `command` is empty when the token may not read it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessRow {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub command: String,
    pub user: String,
    pub cpu_percent: f32,
    pub memory_bytes: Option<u64>,
    pub threads: Option<u32>,
    pub start_unix_s: i64,
    pub cpu_time_ms: u64,
    pub state: ProcessState,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessSnapshot {
    pub revision: u64,
    pub unix_ms: i64,
    pub total: u32,
    pub running: u32,
    pub rows: Vec<ProcessRow>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProcessSignal {
    Term,
    Kill,
    Int,
    Hup,
}

/// `Dropped`: the stream broke and a reconnect is pending; `Live`: flowing again;
/// `Disabled`: monitoring is off and the watch has ended.
#[derive(Clone, Debug, PartialEq)]
pub enum VitalsItem {
    Host(HostInfo),
    Sample(VitalsSample),
    Live,
    Dropped,
    Disabled,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProcessItem {
    Snapshot(ProcessSnapshot),
    Live,
    Dropped,
    Disabled,
}
