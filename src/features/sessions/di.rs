//! `sessions`'s composition-root wiring seam (see
//! `docs/FEATURES_ARCHITECTURE.md`'s DI pattern). [`provide`] constructs the
//! one app-scoped [`SessionsRepository`] and publishes it via
//! [`frust::provide_context`]. [`use_mock_sessions_for_tests`] must flip the
//! mock switch before `provide` runs — no `cfg(test)` branch, since
//! `tests/*.rs` links the normal build. `Arc<dyn SessionsRepository + Send +
//! Sync>` satisfies `use_controller`'s `Send + Sync + 'static` bound.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use crate::connection::expect_connection_controller;
use crate::features::sessions::data::repositories::{
    GrpcSessionsRepository, MockSessionsRepository,
};
use crate::features::sessions::domain::repositories::SessionsRepository;

/// Type alias so the provide/expect pair can never drift apart.
pub type SharedSessionsRepository = Arc<dyn SessionsRepository + Send + Sync>;

/// Real repository by default; flipped once per test process before
/// [`provide`] runs.
static USE_MOCK_SESSIONS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opts the process onto the mock for its remaining life. Idempotent; must
/// run before `MuxrApp::init`.
pub fn use_mock_sessions_for_tests() {
    USE_MOCK_SESSIONS_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub fn provide() {
    provide_context(platform_sessions_repository());
}

fn platform_sessions_repository() -> SharedSessionsRepository {
    if USE_MOCK_SESSIONS_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockSessionsRepository::seeded());
    }
    Arc::new(GrpcSessionsRepository::new(expect_connection_controller()))
}

/// Resolves the repository [`provide`] published.
///
/// # Panics
///
/// Panics if called before `MuxrApp::init` has run.
pub fn expect_sessions_repository() -> SharedSessionsRepository {
    use_context::<SharedSessionsRepository>().expect(
        "expect_sessions_repository: features::sessions::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}
