//! `pro`'s composition-root wiring seam (see `docs/FEATURES_ARCHITECTURE.md`'s
//! DI pattern). [`provide`] is called exactly once, from `crate::MuxrApp::init`,
//! and publishes two app-scoped singletons: the entitlement store and the one
//! [`ProController`] every gate reads. Both go through
//! [`frust::provide_context`], so a screen resolves them the same way it
//! resolves any other feature's.
//!
//! The repository is the [`MockEntitlementRepository`] throughout this slice —
//! the store-backed implementation lands behind this same seam, with no call
//! site moving. When the test switch is on, the concrete mock is published
//! beside its trait object too, so a suite can script it
//! ([`mock_entitlement`], the `connect` credential store's precedent).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use super::data::repositories::MockEntitlementRepository;
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
    let mock = Arc::new(MockEntitlementRepository::free());
    let repository = Arc::clone(&mock) as SharedEntitlementRepository;
    provide_context(Arc::clone(&repository));
    if USE_MOCK_ENTITLEMENT_FOR_TESTS.load(Ordering::Relaxed) {
        provide_context(mock);
    }
    repository
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
