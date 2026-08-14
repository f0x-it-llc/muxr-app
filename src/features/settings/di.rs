//! `settings`'s composition-root wiring seam.
//!
//! [`provide`] is called once, from `crate::MuxrApp::init`, and constructs +
//! `provide_context`s this feature's two app-scoped singletons — the
//! [`AuthTokensRepository`]/[`PreferencesRepository`] implementations — so
//! every mount of `/settings` resolves the *same* stores through
//! [`expect_auth_tokens_repository`]/[`expect_preferences_repository`]: a
//! token revoked on one visit stays gone, a font size picked stays picked.
//! Both are real for a production run
//! ([`GrpcAuthTokensRepository`](crate::features::settings::data::repositories::GrpcAuthTokensRepository)
//! over `muxrd`,
//! [`PrefsRepository`](crate::features::settings::data::repositories::PrefsRepository)
//! over `frust-shared-preferences`), invisible above the trait.
//!
//! **Headless-test seam**: `provide` runs from the exact same `MuxrApp::init`
//! call a real launch or a `tests/*.rs` suite (`Harness::mount`) makes — no
//! `cfg(test)` to branch on, since an integration binary links the normal
//! build. So a suite opts out **before** `provide` runs, via
//! [`use_mock_auth_tokens_for_tests`]/[`use_mock_preferences_for_tests`] —
//! `tests/support::setup()` is the one call site, so no individual test file
//! needs to know either seam exists. Without it, every suite would need a
//! live `muxrd` connection (tokens) or leak state across runs through this
//! dev machine's real preferences store.
//!
//! Each repository travels as `Arc<dyn … + Send + Sync>` — `use_controller`
//! and frust's context both require `Send + Sync + 'static` of anything
//! hosted/provided, and this keeps `SettingsController` free of two
//! repository type parameters.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use crate::connection::expect_connection_controller;
use crate::features::settings::data::repositories::{
    GrpcAuthTokensRepository, MockAuthTokensRepository, MockPreferencesRepository, PrefsRepository,
};
use crate::features::settings::domain::repositories::{
    AuthTokensRepository, PreferencesRepository,
};

/// `false` (the real store) for every real app run, flipped once per test
/// process before `provide` ever runs.
static USE_MOCK_AUTH_TOKENS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real `muxrd`-backed token store for
/// the rest of its life. Idempotent; must run **before** `provide` (i.e.
/// before `MuxrApp::init`) to have any effect.
pub fn use_mock_auth_tokens_for_tests() {
    USE_MOCK_AUTH_TOKENS_FOR_TESTS.store(true, Ordering::Relaxed);
}

static USE_MOCK_PREFERENCES_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real, platform-backed preferences
/// store. Same idempotence/ordering contract as
/// [`use_mock_auth_tokens_for_tests`].
pub fn use_mock_preferences_for_tests() {
    USE_MOCK_PREFERENCES_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// The shared token-repository handle — a type alias so the provide/expect
/// pair can never drift apart (context lookup is by exact type).
pub type SharedAuthTokensRepository = Arc<dyn AuthTokensRepository + Send + Sync>;
pub type SharedPreferencesRepository = Arc<dyn PreferencesRepository + Send + Sync>;

/// Wire this feature's dependencies into context. See the [module docs](self).
pub fn provide() {
    provide_context(platform_auth_tokens_repository());
    provide_context(platform_preferences_repository());
}

/// The real, `muxrd`-backed store unless [`use_mock_auth_tokens_for_tests`]
/// opted out. Resolves the app's one `ConnectionController` exactly once,
/// here — safe because `MuxrApp::init` already published it under the root
/// owner (see [`GrpcAuthTokensRepository`]'s own docs for why a per-call
/// lookup from inside an owner-less `spawn_local` poller would not be).
fn platform_auth_tokens_repository() -> SharedAuthTokensRepository {
    if USE_MOCK_AUTH_TOKENS_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockAuthTokensRepository::seeded());
    }
    Arc::new(GrpcAuthTokensRepository::new(expect_connection_controller()))
}

/// The real, `frust-shared-preferences`-backed store unless
/// [`use_mock_preferences_for_tests`] opted out, or the platform store
/// itself couldn't open (`PrefsError`) — either way falls back to the same
/// in-memory mock rather than taking the whole app down.
fn platform_preferences_repository() -> SharedPreferencesRepository {
    if USE_MOCK_PREFERENCES_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockPreferencesRepository::seeded());
    }
    match PrefsRepository::standard() {
        Ok(repository) => Arc::new(repository),
        Err(err) => {
            log::warn!("preferences: falling back to the in-memory store: {err}");
            Arc::new(MockPreferencesRepository::seeded())
        }
    }
}

/// Resolve the token repository [`provide`] published.
///
/// # Panics
/// Before `MuxrApp::init` has run. Every real call site (the settings
/// screen's `use_controller` factory) runs after it.
pub fn expect_auth_tokens_repository() -> SharedAuthTokensRepository {
    use_context::<SharedAuthTokensRepository>().expect(
        "expect_auth_tokens_repository: features::settings::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

/// Resolve the preferences repository [`provide`] published. Same panic
/// contract as [`expect_auth_tokens_repository`].
pub fn expect_preferences_repository() -> SharedPreferencesRepository {
    use_context::<SharedPreferencesRepository>().expect(
        "expect_preferences_repository: features::settings::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}
