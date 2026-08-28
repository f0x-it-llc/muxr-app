//! `pro` repository implementations — one file per concrete
//! `domain::repositories` implementation, each owning its own failure-mapping
//! site into [`crate::failure::AppFailure`].

pub mod mock_entitlement_repository;

pub use mock_entitlement_repository::MockEntitlementRepository;
