//! `sessions`'s composition-root wiring seam.
//!
//! [`provide`], called once from `crate::MuxrApp::init` alongside every other
//! feature's own `di::provide`, constructs this feature's one app-scoped
//! [`SessionsRepository`] and publishes it under the root owner via
//! [`frust::provide_context`], so every `/sessions` mount resolves the *same*
//! store through [`expect_sessions_repository`]. Production builds
//! [`GrpcSessionsRepository`] over the app's [`ConnectionController`]
//! (already published by the time this runs).
//!
//! # The headless-test seam
//!
//! `provide` runs identically for a real launch or a `tests/*.rs` suite
//! (`Harness::mount`) — no `cfg(test)` to branch on, since an integration
//! test binary links the normal build. So a suite opts out **before**
//! `provide` runs, via [`use_mock_sessions_for_tests`] (`tests/support::setup`
//! is the one call site) — the same seam shape
//! `settings::di::use_mock_preferences_for_tests` and
//! `connection::use_mock_connection_for_tests` use.
//!
//! # Why `Arc<dyn …>` and not a generic
//!
//! `use_controller`'s bounds require `Send + Sync + 'static`, so the
//! repository travels as `Arc<dyn SessionsRepository + Send + Sync>`
//! (huddle's DI convention), keeping `SessionsController` free of a
//! repository type parameter.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use crate::connection::expect_connection_controller;
use crate::features::sessions::data::repositories::{
    GrpcSessionsRepository, MockSessionsRepository,
};
use crate::features::sessions::domain::repositories::SessionsRepository;

/// The shared repository handle this feature publishes and resolves — a type
/// alias so the provide/expect pair can never drift apart.
pub type SharedSessionsRepository = Arc<dyn SessionsRepository + Send + Sync>;

/// `false` (the real repository) for every app run; flipped once per test
/// process, before [`provide`] ever runs.
static USE_MOCK_SESSIONS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real, `muxrd`-backed repository for the
/// rest of its life. Idempotent; must run **before** `MuxrApp::init`.
pub fn use_mock_sessions_for_tests() {
    USE_MOCK_SESSIONS_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// Wire this feature's dependencies into context. See the [module docs](self).
pub fn provide() {
    provide_context(platform_sessions_repository());
}

/// The [`SessionsRepository`] [`provide`] publishes.
fn platform_sessions_repository() -> SharedSessionsRepository {
    if USE_MOCK_SESSIONS_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockSessionsRepository::seeded());
    }
    Arc::new(GrpcSessionsRepository::new(expect_connection_controller()))
}

/// Resolve the repository [`provide`] published.
///
/// # Panics
///
/// Panics if called before `MuxrApp::init` has run. Every real call site
/// (the sessions screen's `use_controller` factory) runs after it.
pub fn expect_sessions_repository() -> SharedSessionsRepository {
    use_context::<SharedSessionsRepository>().expect(
        "expect_sessions_repository: features::sessions::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}
