//! `sessions`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`, alongside
//! every other feature's own `di::provide` — feature-owned wiring, not the
//! root's own concern (mirrors `examples/huddle`'s `HuddleApp::init`, which
//! constructs and `provide_context`s each feature's repository the same way).
//!
//! It constructs this feature's one app-scoped singleton — the
//! [`SessionsRepository`] implementation — and publishes it under the **root
//! owner** via [`frust::provide_context`], so every mount of the `/sessions`
//! screen resolves the *same* store through [`expect_sessions_repository`] and
//! a session created on one visit is still there on the next. Production
//! builds [`GrpcSessionsRepository`], the real `muxrd`-backed implementation,
//! over the app's one [`ConnectionController`] (`crate::MuxrApp::init`
//! publishes it *before* this feature's own `provide` runs, so
//! `expect_connection_controller` is always safe here).
//!
//! # The headless-test seam
//!
//! [`provide`] runs from the *exact same* `MuxrApp::init` call whether it's a
//! real app launch or a `tests/*.rs` integration suite mounting the app root
//! (`Harness::mount`, every suite's own helper) — there is no `cfg(test)` to
//! branch on here, since an integration test binary links the *normal* build
//! of this crate, never the one compiled with `--cfg test`. So a suite opts
//! out **before** `provide` runs, via [`use_mock_sessions_for_tests`]: the
//! one call site is `tests/support::setup()`, which every suite already calls
//! ahead of mounting anything, so no individual `tests/*.rs` file needs to
//! know this seam exists. Without it, every headless suite would try to route
//! its session list through a real `muxrd` connection that
//! `connection::use_mock_connection_for_tests` has already made mock-only —
//! [`GrpcSessionsRepository::client`] would fail every call with "not
//! connected" instead of the seeded facts each suite asserts on. The exact
//! same seam shape as `crate::features::settings::di::use_mock_preferences_for_tests`
//! and `crate::connection::use_mock_connection_for_tests`.
//!
//! # Why `Arc<dyn …>` and not a generic
//!
//! `use_controller`'s bounds require the hosted controller to be
//! `Send + Sync + 'static`, and frust's context requires the same of anything
//! provided — so the repository travels as
//! `Arc<dyn SessionsRepository + Send + Sync>` (huddle's DI convention), which
//! also keeps `SessionsController` free of a repository type parameter.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use crate::connection::expect_connection_controller;
use crate::features::sessions::data::repositories::{
    GrpcSessionsRepository, MockSessionsRepository,
};
use crate::features::sessions::domain::repositories::SessionsRepository;

/// The shared repository handle this feature publishes and resolves — a type
/// alias so the provide/expect pair can never drift apart (context lookup is
/// by exact type).
pub type SharedSessionsRepository = Arc<dyn SessionsRepository + Send + Sync>;

/// Set by [`use_mock_sessions_for_tests`], read by
/// [`platform_sessions_repository`]: `false` (the real `muxrd`-backed
/// repository) for every real app run, flipped to `true` once per test
/// process before [`provide`] ever runs — see the [module docs](self)'
/// headless-test seam section.
static USE_MOCK_SESSIONS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real, `muxrd`-backed repository for the
/// rest of its life: every later [`provide`] call constructs
/// [`MockSessionsRepository`] instead of [`GrpcSessionsRepository`].
/// Idempotent, and a no-op once already set. Must run **before** `provide`
/// (i.e. before `MuxrApp::init`) to have any effect — `tests/support::setup`
/// is the one call site outside this module; see the [module docs](self).
pub fn use_mock_sessions_for_tests() {
    USE_MOCK_SESSIONS_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// Wire this feature's own dependencies into context: the one
/// [`SessionsRepository`] implementation, constructed once and published under
/// the root owner. See the [module docs](self).
pub fn provide() {
    provide_context(platform_sessions_repository());
}

/// The [`SessionsRepository`] [`provide`] publishes: the real, `muxrd`-backed
/// [`GrpcSessionsRepository`], unless [`use_mock_sessions_for_tests`] opted
/// this process out (every headless suite).
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
/// Panics if called before `MuxrApp::init` has run — the same contract
/// `crate::connection::expect_connection_controller` documents. Every call
/// site (the sessions screen's `use_controller` factory) runs after it, so
/// this is never hit in practice; a headless test that mounts the screen
/// without the composition root gets a loud, immediate failure rather than a
/// silently empty list.
pub fn expect_sessions_repository() -> SharedSessionsRepository {
    use_context::<SharedSessionsRepository>().expect(
        "expect_sessions_repository: features::sessions::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}
