use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use crate::connection::expect_connection_controller;
use crate::features::sessions::data::repositories::{
    GrpcSessionsRepository, MockSessionsRepository,
};
use crate::features::sessions::domain::repositories::SessionsRepository;

pub type SharedSessionsRepository = Arc<dyn SessionsRepository + Send + Sync>;

static USE_MOCK_SESSIONS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Must run before `provide`; there is no `cfg(test)` branch because `tests/*.rs` links the normal build.
pub fn use_mock_sessions_for_tests() {
    USE_MOCK_SESSIONS_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub fn provide() {
    provide_context(platform_sessions_repository());
}

fn platform_sessions_repository() -> SharedSessionsRepository {
    if USE_MOCK_SESSIONS_FOR_TESTS.load(Ordering::Relaxed) {
        let mock = Arc::new(MockSessionsRepository::seeded());
        provide_context(Arc::clone(&mock));
        return mock;
    }
    Arc::new(GrpcSessionsRepository::new(expect_connection_controller()))
}

/// Panics before `provide` has run.
pub fn expect_sessions_repository() -> SharedSessionsRepository {
    use_context::<SharedSessionsRepository>().expect(
        "expect_sessions_repository: features::sessions::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

/// Panics unless `provide` ran on the mock.
pub fn expect_mock_sessions_repository() -> Arc<MockSessionsRepository> {
    use_context::<Arc<MockSessionsRepository>>().expect(
        "expect_mock_sessions_repository: provide() must run on the mock \
         (use_mock_sessions_for_tests)",
    )
}
