//! `connect`'s composition-root wiring seam.
//!
//! [`provide`] is called exactly once, from `crate::MuxrApp::init`, alongside
//! every other feature's own `di::provide` — feature-owned wiring, not the
//! root's own concern (mirrors `examples/huddle`'s `HuddleApp::init`, which
//! constructs and `provide_context`s each feature's repository the same way).
//!
//! This is the **only** place `connect`'s repositories are constructed. The
//! Connect screen resolves them with `use_context` inside its own
//! `use_controller` factory (`presentation::pages`'s `ConnectScreen::init`), so
//! construction stays single-site even though the screen remounts on every
//! navigation back to `/`.
//!
//! Two contracts are published here, one per store of the two-store split
//! (`domain::entities`' module docs): [`SavedServers`], the saved-server
//! metadata list, and [`Credentials`], the biometric-gated secret store behind
//! it.
//!
//! # Why `provide_context`, not `provide_controller`
//!
//! `clean_signals_frust::provide_controller` *is* `provide_context` (see its own
//! docs) — the two differ only in intent. A repository is not a controller: it
//! owns no signals and has no `ControllerCore` to dispose, so it goes in as a
//! plain context value. `crate::connection::ConnectionController`, which does
//! own signals, uses the controller spelling.
//!
//! # The headless-test seam
//!
//! [`provide`] runs from the *exact same* `MuxrApp::init` call whether it's a
//! real app launch or a `tests/*.rs` integration suite mounting the app root —
//! there is no `cfg(test)` to branch on here, since an integration test binary
//! links the *normal* build of this crate, never the one compiled with
//! `--cfg test`. So a suite opts out **before** `provide` runs, via
//! [`use_mock_saved_servers_for_tests`]/[`use_mock_credentials_for_tests`]; the
//! one call site for both is `tests/support::setup()`, the same shape
//! `settings::di` and `crate::connection` already use.
//!
//! - Without the saved-servers opt-out, every suite would read and write this
//!   dev machine's own preference store, and leftover rows from a *previous*
//!   run would make the suite order-dependent.
//! - Without the credentials opt-out, every credential assertion would run
//!   against a real biometric gate — which off-device answers "unavailable"
//!   permanently, and on-device waits for a human. The mock is the only way to
//!   drive the gate's outcome table at all (see
//!   [`MockCredentialService`](crate::features::connect::data::repositories::MockCredentialService)).
//!
//! # Owner scope
//!
//! `MuxrApp::init` runs under the shell's **root** owner (`frust::run_with_setup`
//! wraps it; `frust::Component::init`'s own docs spell out that a root component
//! has no per-component owner of its own), so the values published here are
//! visible to the whole tree for the process's life and are never disposed.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use super::data::repositories::{
    MockCredentialService, MockSavedServersRepository, PrefsSavedServersRepository,
    SecureCredentialStore,
};
use super::domain::repositories::{CredentialService, SavedServersRepository};

/// The context type the Connect screen resolves. Named once here so the
/// `provide` and the `use_context` on the other side cannot drift — an
/// `Arc<MockSavedServersRepository>` on one side and an
/// `Arc<dyn SavedServersRepository + Send + Sync>` on the other are two
/// different context keys and would simply never find each other.
pub type SavedServers = Arc<dyn SavedServersRepository + Send + Sync>;

/// The credential service handle, for the same reason.
pub type Credentials = Arc<dyn CredentialService + Send + Sync>;

/// Set by [`use_mock_saved_servers_for_tests`], read by
/// [`platform_saved_servers`]: `false` (the real, preferences-backed store) for
/// every app run, flipped to `true` once per test process before [`provide`]
/// ever runs — see the [module docs](self)' headless-test seam.
static USE_MOCK_SAVED_SERVERS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Set by [`use_mock_credentials_for_tests`], read by
/// [`platform_credentials`], for the same reason.
static USE_MOCK_CREDENTIALS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Opt the current process out of the real, preferences-backed saved-server
/// store for the rest of its life. Idempotent, and a no-op once already set.
/// Must run **before** [`provide`] (i.e. before `MuxrApp::init`) to have any
/// effect — `tests/support::setup` is the one call site outside this module.
pub fn use_mock_saved_servers_for_tests() {
    USE_MOCK_SAVED_SERVERS_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// Opt the current process out of the real, biometric-gated credential store
/// for the rest of its life. Same contract as
/// [`use_mock_saved_servers_for_tests`].
pub fn use_mock_credentials_for_tests() {
    USE_MOCK_CREDENTIALS_FOR_TESTS.store(true, Ordering::Relaxed);
}

/// Wire this feature's own dependencies into context — see the
/// [module docs](self).
pub fn provide() {
    provide_context(platform_saved_servers());
    provide_credentials();
}

/// The [`SavedServersRepository`] [`provide`] publishes: the real store, unless
/// this process opted out (every headless suite) or the platform store itself
/// couldn't be opened — either way falling back to the in-memory store rather
/// than taking the whole app down over a saved-server list, the same fallback
/// `settings::di`'s preferences repository makes.
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

/// Publish the [`CredentialService`] [`provide`] hands out.
///
/// The mock is published **twice** — once behind [`Credentials`], once as its
/// own concrete handle — because a suite needs to *script* it (see
/// [`mock_credentials`]), and a `dyn` handle cannot be scripted. Nothing but a
/// test ever resolves the concrete key, and a real app run never publishes it.
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
/// `crate::connection::expect_connection_controller` documents. Every call site
/// runs after it, so this is never hit in practice.
pub fn expect_saved_servers() -> SavedServers {
    use_context::<SavedServers>().expect(
        "expect_saved_servers: features::connect::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

/// Resolve the credential service [`provide`] published. Same panic contract as
/// [`expect_saved_servers`].
pub fn expect_credential_service() -> Credentials {
    use_context::<Credentials>().expect(
        "expect_credential_service: features::connect::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

/// The scriptable credential mock, or `None` in a real app run (where the real
/// service is wired and no concrete handle is published).
///
/// The suite-facing half of [`use_mock_credentials_for_tests`]: a test resolves
/// this to script the gate's next answer and to read back what was stored.
pub fn mock_credentials() -> Option<Arc<MockCredentialService>> {
    use_context::<Arc<MockCredentialService>>()
}
