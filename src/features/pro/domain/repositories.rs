//! Every method but `cached` is async (it reaches the billing service); `cached` is synchronous
//! so a cold start can render a gate before any store call answers.

use super::entities::{Entitlement, ProOffer, PurchaseOutcome, RestoreResult};
use crate::failure::AppFailure;

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait EntitlementRepository {
    fn cached(&self) -> Entitlement;

    /// Overwrites the cache (no merge) so a refund or revoked purchase propagates back to Free.
    async fn refresh(&self) -> Result<Entitlement, AppFailure>;

    /// `None`: the store does not know the SKU or cannot sell here; not a failure.
    async fn offer(&self) -> Result<Option<ProOffer>, AppFailure>;

    async fn purchase(&self) -> Result<PurchaseOutcome, AppFailure>;

    /// `Unavailable` (no store was asked) is never the same answer as `Found(Free)`.
    async fn restore(&self) -> Result<RestoreResult, AppFailure>;

    /// Resolves on an unsolicited purchase change (completed while not running, approved later);
    /// a store that never pushes never resolves.
    async fn store_changed(&self) {
        std::future::pending::<()>().await
    }
}
