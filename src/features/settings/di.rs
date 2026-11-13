//! [`provide`] runs once from `MuxrApp::init` and publishes the app-scoped repositories,
//! so every mount of `/settings` shares one store.

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

static USE_MOCK_AUTH_TOKENS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Test-only; must run before `MuxrApp::init`.
pub fn use_mock_auth_tokens_for_tests() {
    USE_MOCK_AUTH_TOKENS_FOR_TESTS.store(true, Ordering::Relaxed);
}

static USE_MOCK_PREFERENCES_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Test-only; must run before `MuxrApp::init`.
pub fn use_mock_preferences_for_tests() {
    USE_MOCK_PREFERENCES_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub type SharedAuthTokensRepository = Arc<dyn AuthTokensRepository + Send + Sync>;
pub type SharedPreferencesRepository = Arc<dyn PreferencesRepository + Send + Sync>;

pub fn provide() {
    provide_context(platform_auth_tokens_repository());
    provide_context(platform_preferences_repository());
}

fn platform_auth_tokens_repository() -> SharedAuthTokensRepository {
    if USE_MOCK_AUTH_TOKENS_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockAuthTokensRepository::seeded());
    }
    Arc::new(GrpcAuthTokensRepository::new(expect_connection_controller()))
}

fn platform_preferences_repository() -> SharedPreferencesRepository {
    if USE_MOCK_PREFERENCES_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockPreferencesRepository::seeded());
    }
    match PrefsRepository::standard() {
        Ok(repository) => Arc::new(repository),
        Err(err) => {
            // Falls back to in-memory, so nothing persists.
            log::warn!("preferences: falling back to the in-memory store: {err}");
            Arc::new(MockPreferencesRepository::seeded())
        }
    }
}

pub fn expect_auth_tokens_repository() -> SharedAuthTokensRepository {
    use_context::<SharedAuthTokensRepository>().expect(
        "expect_auth_tokens_repository: features::settings::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

pub fn expect_preferences_repository() -> SharedPreferencesRepository {
    use_context::<SharedPreferencesRepository>().expect(
        "expect_preferences_repository: features::settings::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}
