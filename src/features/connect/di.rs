//! `connect`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`, and is the
//! **only** place `connect`'s repositories are constructed — the screen resolves
//! them with `use_context`, so construction stays single-site even though the
//! screen remounts on every navigation back to `/`. Two contracts, one per store
//! of the two-store split: [`SavedServers`] and [`Credentials`]. A repository owns
//! no signals and has no `ControllerCore` to dispose, so it goes in as a plain
//! context value rather than a controller.
//!
//! **The headless-test seam.** [`provide`] runs from the same `MuxrApp::init`
//! whether it is a real launch or a suite mounting the app root — an integration
//! binary links the *normal* build of this crate, so there is no `cfg(test)` to
//! branch on. A suite opts out **before** `provide` runs, from the one call site
//! both seams share (`tests/support::setup`): without the saved-servers opt-out
//! every suite would read and write this machine's own preference store (and go
//! order-dependent on leftovers), and without the credentials one every credential
//! assertion would run against a real gate, which off-device answers "unavailable"
//! permanently and on-device waits for a human.
//!
//! `MuxrApp::init` runs under the shell's **root** owner, so what is published here
//! is visible to the whole tree for the process's life and never disposed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use super::data::repositories::{
    MockCredentialService, MockSavedServersRepository, PrefsSavedServersRepository,
    SecureCredentialStore,
};
use super::domain::repositories::{CredentialService, SavedServersRepository};

/// The context type the Connect screen resolves. Named once so the `provide` and
/// the `use_context` cannot drift — a concrete `Arc<..>` and an `Arc<dyn ..>` are
/// two different context keys and would never find each other.
pub type SavedServers = Arc<dyn SavedServersRepository + Send + Sync>;

/// The credential service handle, for the same reason.
pub type Credentials = Arc<dyn CredentialService + Send + Sync>;

/// Set by [`use_mock_saved_servers_for_tests`] — the [module docs](self)' seam.
static USE_MOCK_SAVED_SERVERS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Set by [`use_mock_credentials_for_tests`], for the same reason.
static USE_MOCK_CREDENTIALS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt this process out of the real saved-server store for the rest of its life.
/// Idempotent, and must run **before** [`provide`] to have any effect.
pub fn use_mock_saved_servers_for_tests() {
    USE_MOCK_SAVED_SERVERS_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// Opt this process out of the real credential store. Same contract as
/// [`use_mock_saved_servers_for_tests`].
pub fn use_mock_credentials_for_tests() {
    USE_MOCK_CREDENTIALS_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// Wire this feature's own dependencies into context — see the [module docs](self).
pub fn provide() {
    provide_context(platform_saved_servers());
    provide_credentials();
}

/// The [`SavedServersRepository`] [`provide`] publishes: the real store, unless
/// this process opted out or the platform store couldn't be opened — either way
/// falling back to memory rather than taking the app down over a list.
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

/// Publish the [`CredentialService`] [`provide`] hands out. The mock goes in
/// **twice** — behind [`Credentials`] and as its own concrete handle — because a
/// suite needs to *script* it and a `dyn` handle cannot be scripted. A real run
/// never publishes the concrete key.
fn provide_credentials() {
    if USE_MOCK_CREDENTIALS_FOR_TESTS.load(Ordering::Relaxed) {
        let mock = Arc::new(MockCredentialService::new());
        provide_context(Arc::clone(&mock) as Credentials);
        provide_context(mock);
        return;
    }
    provide_context(Arc::new(SecureCredentialStore::new()) as Credentials);
}

/// Resolve the saved-server repository [`provide`] published.
///
/// # Panics
///
/// Panics if called before `MuxrApp::init` has run — the same contract
/// `crate::connection::expect_connection_controller` documents.
pub fn expect_saved_servers() -> SavedServers {
    use_context::<SavedServers>().expect(
        "expect_saved_servers: features::connect::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

/// Resolve the saved-server repository if [`provide`] has published one, else
/// `None` — the soft twin of [`expect_saved_servers`], for the **one** caller that
/// runs outside a screen (the cold-start splash's snapshot). A splash is
/// decorative, and rendering it with no rows beats taking a launch down.
pub fn saved_servers() -> Option<SavedServers> {
    use_context::<SavedServers>()
}

/// Resolve the credential service [`provide`] published. Same panic contract as
/// [`expect_saved_servers`].
pub fn expect_credential_service() -> Credentials {
    use_context::<Credentials>().expect(
        "expect_credential_service: features::connect::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

/// The scriptable credential mock, or `None` in a real app run: a test resolves
/// this to script the gate's next answer and read back what was stored.
pub fn mock_credentials() -> Option<Arc<MockCredentialService>> {
    use_context::<Arc<MockCredentialService>>()
}
