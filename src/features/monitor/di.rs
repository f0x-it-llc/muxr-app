use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use crate::connection::expect_connection_controller;
use crate::features::monitor::data::repositories::{GrpcMonitorRepository, MockMonitorRepository};
use crate::features::monitor::domain::repositories::MonitorRepository;
use crate::features::monitor::presentation::controllers::{
    MonitorController, MonitorEnv, ProcessesController, Scheduler, VitalsSummaryController,
};
use crate::features::pro::di::expect_pro_controller;
use crate::features::pro::domain::entities::ProFeature;

pub type SharedMonitorRepository = Arc<dyn MonitorRepository + Send + Sync>;

static USE_MOCK_MONITOR_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Must run before `provide`; there is no `cfg(test)` branch because `tests/*.rs` links the normal build.
pub fn use_mock_monitor_for_tests() {
    USE_MOCK_MONITOR_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub fn provide() {
    provide_context(platform_monitor_repository());
}

fn platform_monitor_repository() -> SharedMonitorRepository {
    if USE_MOCK_MONITOR_FOR_TESTS.load(Ordering::Relaxed) {
        let mock = Arc::new(MockMonitorRepository::seeded());
        provide_context(Arc::clone(&mock));
        return mock;
    }
    Arc::new(GrpcMonitorRepository::new(expect_connection_controller()))
}

/// Panics before `provide` has run.
pub fn expect_monitor_repository() -> SharedMonitorRepository {
    use_context::<SharedMonitorRepository>().expect(
        "expect_monitor_repository: features::monitor::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

/// Panics unless `provide` ran on the mock.
pub fn expect_mock_monitor_repository() -> Arc<MockMonitorRepository> {
    use_context::<Arc<MockMonitorRepository>>().expect(
        "expect_mock_monitor_repository: provide() must run on the mock \
         (use_mock_monitor_for_tests)",
    )
}

/// Needs `provide` and `pro::di::provide` first; the permission reads Pro untracked at each open.
pub fn monitor_env() -> MonitorEnv {
    let pro = expect_pro_controller();
    MonitorEnv::new(
        expect_monitor_repository(),
        expect_connection_controller(),
        move || pro.can_use_untracked(ProFeature::ServerMonitor),
        Scheduler::ui(),
    )
}

pub fn provide_controller() -> Arc<VitalsSummaryController> {
    let controller = Arc::new(VitalsSummaryController::new(monitor_env()));
    provide_context(Arc::clone(&controller));
    controller
}

pub fn expect_vitals_summary() -> Arc<VitalsSummaryController> {
    use_context::<Arc<VitalsSummaryController>>().expect(
        "expect_vitals_summary: features::monitor::di::provide_controller() must run first \
         (MuxrApp::init calls it)",
    )
}

/// A factory for `use_controller`, run from a screen's `Component::init`.
pub fn new_monitor_controller() -> MonitorController {
    MonitorController::new(monitor_env())
}

pub fn new_processes_controller() -> ProcessesController {
    ProcessesController::new(monitor_env())
}
