use crate::failure::AppFailure;
use crate::features::monitor::domain::entities::{
    HistoryRange, ProcessItem, ProcessQuery, ProcessSignal, VitalsDetail, VitalsHistory, VitalsItem,
};

/// Called off the UI thread by a real stream, so it must not touch a signal itself.
pub type VitalsSink = Box<dyn Fn(VitalsItem) + Send>;

pub type ProcessSink = Box<dyn Fn(ProcessItem) + Send>;

/// Dropping it stops the watch; nothing reaches its sink afterwards.
pub struct MonitorWatch {
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl MonitorWatch {
    pub fn new(stop: impl FnOnce() + Send + 'static) -> Self {
        Self {
            stop: Some(Box::new(stop)),
        }
    }
}

impl Drop for MonitorWatch {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait MonitorRepository {
    fn watch_vitals(&self, detail: VitalsDetail, sink: VitalsSink) -> MonitorWatch;

    async fn vitals_history(&self, range: HistoryRange) -> Result<VitalsHistory, AppFailure>;

    fn watch_processes(&self, query: ProcessQuery, sink: ProcessSink) -> MonitorWatch;

    /// `Ok` means the signal was sent, not that the process exited.
    async fn signal_process(
        &self,
        pid: u32,
        start_unix_s: i64,
        signal: ProcessSignal,
    ) -> Result<(), AppFailure>;
}
