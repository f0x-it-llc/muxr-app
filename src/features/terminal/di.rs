//! [`provide`] runs once, from `crate::MuxrApp::init`, and publishes the app-scoped
//! [`TerminalRepository`]; a headless suite calls [`use_mock_terminal_for_tests`] before it runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use crate::connection::expect_connection_controller;
use crate::features::terminal::data::repositories::{
    GrpcTerminalRepository, MockTerminalRepository,
};
use crate::features::terminal::domain::repositories::TerminalRepository;

/// Context lookup is by exact type, so provide and expect share this alias.
pub type SharedTerminalRepository = Arc<dyn TerminalRepository + Send + Sync>;

static USE_MOCK_TERMINAL_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Idempotent; takes effect only when called before [`provide`].
pub fn use_mock_terminal_for_tests() {
    USE_MOCK_TERMINAL_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub fn provide() {
    provide_context(platform_terminal_repository());
}

fn platform_terminal_repository() -> SharedTerminalRepository {
    if USE_MOCK_TERMINAL_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockTerminalRepository::seeded());
    }
    Arc::new(GrpcTerminalRepository::new(expect_connection_controller()))
}

/// Panics if called before `MuxrApp::init` has run.
pub fn expect_terminal_repository() -> SharedTerminalRepository {
    use_context::<SharedTerminalRepository>().expect(
        "expect_terminal_repository: features::terminal::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}
