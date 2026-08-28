//! `pro` repository implementations — one file per concrete
//! `domain::repositories` implementation, each owning its own failure-mapping
//! site into [`crate::failure::AppFailure`].

pub mod iap_entitlement_repository;
pub mod mock_entitlement_repository;

pub(crate) use iap_entitlement_repository::PlayEntitlementRepository;
pub use mock_entitlement_repository::MockEntitlementRepository;
