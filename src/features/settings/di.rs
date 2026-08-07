//! `settings`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`, alongside
//! every other feature's own `di::provide` — feature-owned wiring, not the
//! root's own concern (mirrors `examples/huddle`'s `HuddleApp::init`, which
//! constructs and `provide_context`s each feature's repository the same way).
//!
//! It constructs this feature's two app-scoped singletons — the
//! [`AuthTokensRepository`] and [`PreferencesRepository`] implementations — and
//! publishes each under the **root owner** via [`frust::provide_context`], so
//! every mount of the `/settings` screen resolves the *same* stores through
//! [`expect_auth_tokens_repository`]/[`expect_preferences_repository`]: a token
//! revoked on one visit is still gone on the next, and a font size picked on
//! one is still picked. Both are real for a production run:
//! [`GrpcAuthTokensRepository`](crate::features::settings::data::repositories::GrpcAuthTokensRepository)
//! reaches the connected server over `muxrd`, and
//! [`PrefsRepository`](crate::features::settings::data::repositories::PrefsRepository)
//! is the `frust-shared-preferences`-backed store — both invisible to the
//! screen, the controller and the use cases (which only ever name the
//! traits).
//!
//! # The headless-test seam
//!
//! [`provide`] runs from the *exact same* `MuxrApp::init` call whether it's a
//! real app launch or a `tests/*.rs` integration suite mounting the app root
//! (`Harness::mount`, every suite's own helper) — there is no `cfg(test)` to
//! branch on here, since an integration test binary links the *normal* build
//! of this crate, never the one compiled with `--cfg test`. So a suite opts
//! out **before** `provide` runs, via [`use_mock_auth_tokens_for_tests`]/
//! [`use_mock_preferences_for_tests`]: the one call site for both is
//! `tests/support::setup()`, which every suite already calls ahead of
//! mounting anything, so no individual `tests/*.rs` file needs to know either
//! seam exists.
//!
//! - Without the tokens opt-out, every headless suite's token-group
//!   assertions would need a live `muxrd` connection the fixture doesn't
//!   answer these RPCs on yet (`src/net/testing.rs`'s own module docs).
//! - Without the preferences opt-out, every suite would read and write this
//!   dev machine's own NSUserDefaults (or, off Apple/Android, a JSON file) —
//!   leftover state from a *previous* run would make the suite
//!   order-dependent, which this app's Tests doc rules out.
//!
//! # Why `Arc<dyn …>` and not a generic
//!
//! `use_controller`'s bounds require the hosted controller to be
//! `Send + Sync + 'static`, and frust's context requires the same of anything
//! provided — so each repository travels as `Arc<dyn … + Send + Sync>`
//! (huddle's DI convention), which also keeps
//! [`SettingsController`](crate::features::settings::presentation::controller::SettingsController)
//! free of two repository type parameters.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use crate::features::settings::data::repositories::{
    GrpcAuthTokensRepository, MockAuthTokensRepository, MockPreferencesRepository, PrefsRepository,
};
use crate::features::settings::domain::repositories::{
    AuthTokensRepository, PreferencesRepository,
};

/// Set by [`use_mock_auth_tokens_for_tests`], read by
/// [`platform_auth_tokens_repository`]: `false` (the real `muxrd`-backed
/// store) for every real app run, flipped to `true` once per test process
/// before `provide` ever runs — see the [module docs](self)' headless-test
/// seam section.
static USE_MOCK_AUTH_TOKENS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real `muxrd`-backed token store for the
/// rest of its life: every later [`provide`] call constructs
/// [`MockAuthTokensRepository`] instead of
/// [`GrpcAuthTokensRepository`](crate::features::settings::data::repositories::GrpcAuthTokensRepository).
/// Idempotent, and a no-op once already set. Must run **before** `provide`
/// (i.e. before `MuxrApp::init`) to have any effect — `tests/support::setup`
/// is the one call site outside this module; see the [module docs](self).
pub fn use_mock_auth_tokens_for_tests() {
    USE_MOCK_AUTH_TOKENS_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// Set by [`use_mock_preferences_for_tests`], read by
/// [`platform_preferences_repository`]: `false` (the real store) for every
/// real app run, flipped to `true` once per test process before `provide`
/// ever runs — see the [module docs](self)' headless-test seam section.
static USE_MOCK_PREFERENCES_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real, platform-backed preferences
/// store for the rest of its life: every later [`provide`] call constructs
/// [`MockPreferencesRepository`] instead of
/// [`PrefsRepository`](crate::features::settings::data::repositories::PrefsRepository).
/// Idempotent, and a no-op once already set. Must run **before** `provide`
/// (i.e. before `MuxrApp::init`) to have any effect — `tests/support::setup`
/// is the one call site outside this module; see the [module docs](self).
pub fn use_mock_preferences_for_tests() {
    USE_MOCK_PREFERENCES_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// The shared token-repository handle this feature publishes and resolves — a
/// type alias so the provide/expect pair can never drift apart (context lookup
/// is by exact type).
pub type SharedAuthTokensRepository = Arc<dyn AuthTokensRepository + Send + Sync>;

/// The shared preferences-repository handle, for the same reason.
pub type SharedPreferencesRepository = Arc<dyn PreferencesRepository + Send + Sync>;

/// Wire this feature's own dependencies into context: the two repository
/// implementations, each constructed once and published under the root owner.
/// See the [module docs](self).
pub fn provide() {
    provide_context(platform_auth_tokens_repository());
    provide_context(platform_preferences_repository());
}

/// The [`AuthTokensRepository`] [`provide`] publishes: the real, `muxrd`-backed
/// store, unless [`use_mock_auth_tokens_for_tests`] opted this process out
/// (every headless suite). Unlike [`platform_preferences_repository`] this has
/// no fallible-construction fallback branch — [`GrpcAuthTokensRepository`] is
/// stateless (it resolves the live connection fresh on every call, see that
/// type's own module docs), so there is nothing here that can fail to
/// construct.
fn platform_auth_tokens_repository() -> SharedAuthTokensRepository {
    if USE_MOCK_AUTH_TOKENS_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockAuthTokensRepository::seeded());
    }
    Arc::new(GrpcAuthTokensRepository::new())
}

/// The [`PreferencesRepository`] [`provide`] publishes: the real
/// `frust-shared-preferences`-backed store, unless
/// [`use_mock_preferences_for_tests`] opted this process out (every headless
/// suite), or the platform store itself couldn't be opened (`PrefsError` —
/// e.g. an Android host that never ran `nativeInitPlatform`) — either way
/// this falls back to the same in-memory mock the pre-persistence era ran
/// on, rather than taking the whole app down over a preferences store.
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
///
/// Panics if called before `MuxrApp::init` has run — the same contract
/// `crate::connection::expect_connection_controller` documents. Every call
/// site (the settings screen's `use_controller` factory) runs after it, so
/// this is never hit in practice; a headless test that mounts the screen
/// without the composition root gets a loud, immediate failure rather than a
/// silently empty token list.
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
