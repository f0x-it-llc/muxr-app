//! [`provide`] is called exactly once, from `crate::MuxrApp::init` — the only
//! place `connect`'s repositories are constructed; the screen resolves them with
//! `use_context`, so construction stays single-site across remounts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use super::data::repositories::{
    MockCredentialService, MockSavedServersRepository, PrefsSavedServersRepository,
    SecureCredentialStore,
};
use super::domain::repositories::{CredentialService, SavedServersRepository};

pub type SavedServers = Arc<dyn SavedServersRepository + Send + Sync>;

pub type Credentials = Arc<dyn CredentialService + Send + Sync>;

static USE_MOCK_SAVED_SERVERS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

static USE_MOCK_CREDENTIALS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

pub fn use_mock_saved_servers_for_tests() {
    USE_MOCK_SAVED_SERVERS_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub fn use_mock_credentials_for_tests() {
    USE_MOCK_CREDENTIALS_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub fn provide() {
    provide_context(platform_saved_servers());
    provide_credentials();
}

fn platform_saved_servers() -> SavedServers {
    if USE_MOCK_SAVED_SERVERS_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockSavedServersRepository::seeded());
    }
    match PrefsSavedServersRepository::standard() {
        Ok(repository) => Arc::new(repository),
        Err(err) => {
            log::warn!("saved servers: falling back to the in-memory store: {err}");
            Arc::new(MockSavedServersRepository::seeded())
        }
    }
}

fn provide_credentials() {
    if USE_MOCK_CREDENTIALS_FOR_TESTS.load(Ordering::Relaxed) {
        let mock = Arc::new(MockCredentialService::new());
        provide_context(Arc::clone(&mock) as Credentials);
        provide_context(mock);
        return;
    }
    provide_context(Arc::new(SecureCredentialStore::new()) as Credentials);
}

pub fn expect_saved_servers() -> SavedServers {
    use_context::<SavedServers>().expect(
        "expect_saved_servers: features::connect::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

pub fn saved_servers() -> Option<SavedServers> {
    use_context::<SavedServers>()
}

pub fn expect_credential_service() -> Credentials {
    use_context::<Credentials>().expect(
        "expect_credential_service: features::connect::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

pub fn mock_credentials() -> Option<Arc<MockCredentialService>> {
    use_context::<Arc<MockCredentialService>>()
}
