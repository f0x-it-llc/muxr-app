//! `pro`'s composition-root wiring seam (see `docs/FEATURES_ARCHITECTURE.md`'s
//! DI pattern). [`provide`] is called exactly once, from `crate::MuxrApp::init`,
//! and publishes two app-scoped singletons: the entitlement store and the one
//! [`ProController`] every gate reads. Both go through
//! [`frust::provide_context`], so a screen resolves them the same way it
//! resolves any other feature's.
//!
//! The repository is the store-backed [`PlayEntitlementRepository`] in a real
//! run, and the in-memory [`MockEntitlementRepository`] in the two cases that
//! cannot reach a store: a headless suite that flipped
//! [`use_mock_entitlement_for_tests`], and a `mock-iap` build — the desktop
//! preview of the purchase flow, since no desktop platform can sell. An
//! unopenable preferences store instead falls back to
//! [`UnavailableEntitlementRepository`] — this launch sells nothing and grants
//! nothing, rather than failing outright (`connect`'s saved-servers precedent
//! covers the failure-doesn't-block-launch shape, not the answer this store
//! gives). With the test switch on, the concrete mock is published beside its
//! trait object so a suite can script it ([`mock_entitlement`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use super::data::repositories::{
    MockEntitlementRepository, PlayEntitlementRepository, UnavailableEntitlementRepository,
};
use super::domain::repositories::EntitlementRepository;
use super::presentation::controller::ProController;

/// Type alias so the provide/expect pair can never drift apart.
pub type SharedEntitlementRepository = Arc<dyn EntitlementRepository + Send + Sync>;

/// Whether this process publishes the scriptable mock handle beside the
/// repository. Flipped once per test process, before [`provide`] runs.
static USE_MOCK_ENTITLEMENT_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Test-only; idempotent; must run before `MuxrApp::init` to have any effect.
pub fn use_mock_entitlement_for_tests() {
    USE_MOCK_ENTITLEMENT_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub fn provide() {
    let repository = provide_entitlement_repository();
    provide_context(Arc::new(ProController::new(repository)));
}

/// Publishes the entitlement store and hands it back, so the controller below
/// is built over the very same instance rather than a second lookup's.
fn provide_entitlement_repository() -> SharedEntitlementRepository {
    let scripted = USE_MOCK_ENTITLEMENT_FOR_TESTS.load(Ordering::Relaxed);
    let repository = if scripted || cfg!(feature = "mock-iap") {
        let mock = Arc::new(MockEntitlementRepository::free());
        if scripted {
            provide_context(Arc::clone(&mock));
        }
        mock as SharedEntitlementRepository
    } else {
        platform_entitlement_repository()
    };
    provide_context(Arc::clone(&repository));
    repository
}

/// The real store, over the platform's own billing service and preferences.
fn platform_entitlement_repository() -> SharedEntitlementRepository {
    match PlayEntitlementRepository::standard() {
        Ok(repository) => Arc::new(repository),
        Err(err) => {
            // Only the cache behind it failed to open; the store is
            // unavailable for this launch, so nothing is sold or granted.
            log::warn!("pro entitlement: store unavailable for this launch: {err}");
            Arc::new(UnavailableEntitlementRepository)
        }
    }
}

pub fn expect_entitlement_repository() -> SharedEntitlementRepository {
    use_context::<SharedEntitlementRepository>().expect(
        "expect_entitlement_repository: features::pro::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

pub fn expect_pro_controller() -> Arc<ProController> {
    use_context::<Arc<ProController>>().expect(
        "expect_pro_controller: features::pro::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

/// The scriptable mock behind the repository, when this process opted onto it
/// via [`use_mock_entitlement_for_tests`]. `None` in a real app run.
pub fn mock_entitlement() -> Option<Arc<MockEntitlementRepository>> {
    use_context::<Arc<MockEntitlementRepository>>()
}
