//! The `pro` domain's one repository contract — the seam `presentation`/use
//! cases depend on and `data` implements over a platform store.
//!
//! Every method but [`EntitlementRepository::cached`] is `async` because it
//! reaches the platform billing service; `cached` is deliberately synchronous,
//! since a cold start has to render a gate before any store call can answer.

use super::entities::{Entitlement, ProOffer, PurchaseOutcome};
use crate::failure::AppFailure;

/// This install's entitlement, as the platform store reports it. Consumed as
/// `Arc<dyn .. + Send + Sync>` so the app-scoped controller holding it stays
/// `Send + Sync`.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait EntitlementRepository {
    /// The last persisted answer, with no store call and no `await` — what a
    /// cold start and an offline launch gate on.
    fn cached(&self) -> Entitlement;

    /// Ask the store which purchases this account owns, and **overwrite** the
    /// cache with the answer. Overwriting rather than merging is what lets a
    /// refund or a revoked purchase propagate back to Free.
    async fn refresh(&self) -> Result<Entitlement, AppFailure>;

    /// The store's own description of the product. `None` means the store
    /// does not know the SKU, or cannot sell on this platform at all — not a
    /// failure, and the paywall renders its unavailable state from it.
    async fn offer(&self) -> Result<Option<ProOffer>, AppFailure>;

    /// Run the purchase flow, resolving only once the store reports an
    /// outcome — a dismissed sheet included.
    async fn purchase(&self) -> Result<PurchaseOutcome, AppFailure>;

    /// Restore prior purchases on this account, then re-read the entitlement
    /// the same way [`Self::refresh`] does.
    async fn restore(&self) -> Result<Entitlement, AppFailure>;
}
