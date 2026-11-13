//! Degraded launch: the entitlement cache could not be opened, so nothing is sold or granted.
//! A restore here reached no store and must never read as "this account owns nothing".

use crate::failure::AppFailure;
use crate::features::pro::domain::entities::{
    Entitlement, ProOffer, PurchaseOutcome, RestoreResult,
};
use crate::features::pro::domain::repositories::EntitlementRepository;

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

    async fn restore(&self) -> Result<RestoreResult, AppFailure> {
        Ok(RestoreResult::Unavailable)
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
    fn restore_answers_unavailable_never_an_empty_account() {
        let repo = UnavailableEntitlementRepository;
        assert_eq!(block_on(repo.restore()), Ok(RestoreResult::Unavailable));
    }

    #[test]
    fn purchase_never_changes_the_cached_answer() {
        let repo = UnavailableEntitlementRepository;
        assert_eq!(repo.cached(), Entitlement::Free);
        let _ = block_on(repo.purchase());
        assert_eq!(repo.cached(), Entitlement::Free);
    }
}
