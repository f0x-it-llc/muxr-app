//! Composition-root wiring: `provide` runs once from `MuxrApp::init` and publishes the entitlement store and the one `ProController`.
//! Real runs use the store-backed repository; tests and `mock-iap` use the mock; an unopenable
//! preferences store falls back to the unavailable repository.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use frust::{provide_context, use_context};

use super::data::repositories::{
    DevelopEntitlementRepository, MockEntitlementRepository, PlayEntitlementRepository,
    UnavailableEntitlementRepository,
};
use super::domain::repositories::EntitlementRepository;
use super::presentation::controller::ProController;

pub type SharedEntitlementRepository = Arc<dyn EntitlementRepository + Send + Sync>;

/// Whether this process publishes the scriptable mock handle beside the repository.
static USE_MOCK_ENTITLEMENT_FOR_TESTS: AtomicBool = AtomicBool::new(false);

/// Test-only; must run before `MuxrApp::init` to have any effect.
pub fn use_mock_entitlement_for_tests() {
    USE_MOCK_ENTITLEMENT_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub fn provide() {
    let repository = provide_entitlement_repository();
    provide_context(Arc::new(ProController::new(repository)));
}

/// Hands the store back so the controller is built over the same instance.
fn provide_entitlement_repository() -> SharedEntitlementRepository {
    let scripted = USE_MOCK_ENTITLEMENT_FOR_TESTS.load(Ordering::Relaxed);
    let source = select_source(scripted, cfg!(feature = "mock-iap"), flavor_marker());
    let repository = match source {
        EntitlementSource::Mock => {
            let mock = Arc::new(MockEntitlementRepository::free());
            if scripted {
                provide_context(Arc::clone(&mock));
            }
            mock as SharedEntitlementRepository
        }
        EntitlementSource::Develop => {
            log::info!("pro entitlement: develop build, every Pro feature is unlocked");
            Arc::new(DevelopEntitlementRepository)
        }
        EntitlementSource::Platform => platform_entitlement_repository(),
    };
    provide_context(Arc::clone(&repository));
    repository
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntitlementSource {
    Mock,
    Develop,
    Platform,
}

fn flavor_marker() -> Option<&'static str> {
    option_env!("MUXR_FLAVOR")
}

pub fn is_develop_marker(marker: Option<&str>) -> bool {
    marker == Some("develop")
}

pub fn is_develop_build() -> bool {
    is_develop_marker(flavor_marker())
}

/// Tests and `mock-iap` take precedence over the develop marker, which beats the real store.
pub fn select_source(scripted: bool, mock_iap: bool, marker: Option<&str>) -> EntitlementSource {
    if scripted || mock_iap {
        EntitlementSource::Mock
    } else if is_develop_marker(marker) {
        EntitlementSource::Develop
    } else {
        EntitlementSource::Platform
    }
}

fn platform_entitlement_repository() -> SharedEntitlementRepository {
    match PlayEntitlementRepository::standard() {
        Ok(repository) => Arc::new(repository),
        Err(err) => {
            // Only the cache failed to open: nothing is sold or granted this launch.
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

pub fn mock_entitlement() -> Option<Arc<MockEntitlementRepository>> {
    use_context::<Arc<MockEntitlementRepository>>()
}
