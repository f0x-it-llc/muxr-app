//! [`UnavailableEntitlementRepository`] — the degraded-launch store: the
//! durable cache behind [`super::PlayEntitlementRepository`] could not be
//! opened, so this app sells nothing and grants nothing until it is fixed.
//! The upgrade sheet already renders `UNAVAILABLE_COPY` for `offer() == None`
//! and toasts it for [`PurchaseOutcome::Unavailable`], so no UI change is
//! needed to carry this state.

use crate::failure::AppFailure;
use crate::features::pro::domain::entities::{Entitlement, ProOffer, PurchaseOutcome};
use crate::features::pro::domain::repositories::EntitlementRepository;

/// See the [module docs](self).
pub struct UnavailableEntitlementRepository;

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl EntitlementRepository for UnavailableEntitlementRepository {
    fn cached(&self) -> Entitlement {
        Entitlement::Free
    }

    async fn refresh(&self) -> Result<Entitlement, AppFailure> {
        Ok(Entitlement::Free)
    }

    async fn offer(&self) -> Result<Option<ProOffer>, AppFailure> {
        Ok(None)
    }

    async fn purchase(&self) -> Result<PurchaseOutcome, AppFailure> {
        Ok(PurchaseOutcome::Unavailable)
    }

    async fn restore(&self) -> Result<Entitlement, AppFailure> {
        Ok(Entitlement::Free)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn cached_is_always_free() {
        assert_eq!(UnavailableEntitlementRepository.cached(), Entitlement::Free);
    }

    #[test]
    fn refresh_answers_free() {
        let repo = UnavailableEntitlementRepository;
        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Free));
    }

    #[test]
    fn offer_answers_none() {
        let repo = UnavailableEntitlementRepository;
        assert_eq!(block_on(repo.offer()), Ok(None));
    }

    #[test]
    fn purchase_answers_unavailable() {
        let repo = UnavailableEntitlementRepository;
        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Unavailable));
    }

    #[test]
    fn restore_answers_free() {
        let repo = UnavailableEntitlementRepository;
        assert_eq!(block_on(repo.restore()), Ok(Entitlement::Free));
    }

    #[test]
    fn purchase_never_changes_the_cached_answer() {
        let repo = UnavailableEntitlementRepository;
        assert_eq!(repo.cached(), Entitlement::Free);
        let _ = block_on(repo.purchase());
        assert_eq!(repo.cached(), Entitlement::Free);
    }
}
