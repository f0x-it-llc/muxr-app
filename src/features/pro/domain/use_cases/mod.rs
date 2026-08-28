//! `pro` use cases — one file per `<verb><noun>` action over
//! [`EntitlementRepository`](super::repositories::EntitlementRepository), each
//! a thin injected-repository [`UseCase`](clean_signals::UseCase) whose value
//! is the seam:
//! [`ProController`](crate::features::pro::presentation::controller::ProController)
//! drives every store call through `ControllerCore::run` (activity tracking,
//! one failure-emission site) rather than touching the repository directly.
//! Never call `execute` directly from a controller.
//!
//! [`check_server_limit`] is the exception — a plain function, like `connect`'s
//! two parses: the free-tier rule is a pure synchronous verdict with no I/O,
//! and wrapping it would buy an `await` in every call site and no behavior.

pub mod check_server_limit;
pub mod fetch_offer;
pub mod purchase_pro;
pub mod refresh_entitlement;
pub mod restore_purchases;

pub use check_server_limit::check_server_limit;
pub use fetch_offer::FetchOffer;
pub use purchase_pro::PurchasePro;
pub use refresh_entitlement::RefreshEntitlement;
pub use restore_purchases::RestorePurchases;
