//! `pro` repository implementations — one file per concrete
//! `domain::repositories` implementation, each owning its own failure-mapping
//! site into [`crate::failure::AppFailure`].

pub mod develop_entitlement_repository;
pub mod iap_entitlement_repository;
pub mod mock_entitlement_repository;
pub mod unavailable_entitlement_repository;

pub use develop_entitlement_repository::DevelopEntitlementRepository;
pub(crate) use iap_entitlement_repository::PlayEntitlementRepository;
pub use mock_entitlement_repository::MockEntitlementRepository;
pub use unavailable_entitlement_repository::UnavailableEntitlementRepository;
