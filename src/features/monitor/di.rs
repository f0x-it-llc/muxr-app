use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use crate::connection::expect_connection_controller;
use crate::features::monitor::data::repositories::{GrpcMonitorRepository, MockMonitorRepository};
use crate::features::monitor::domain::repositories::MonitorRepository;

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
