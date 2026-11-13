//! Develop builds (app id `com.it.f0x.muxr.dev`) can never sell, so this repository owns Pro and contacts no store.

use crate::failure::AppFailure;
use crate::features::pro::domain::entities::{
    Entitlement, ProOffer, PurchaseOutcome, RestoreResult,
};
use crate::features::pro::domain::repositories::EntitlementRepository;

pub struct DevelopEntitlementRepository;

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl EntitlementRepository for DevelopEntitlementRepository {
    fn cached(&self) -> Entitlement {
        Entitlement::Pro
    }

    async fn refresh(&self) -> Result<Entitlement, AppFailure> {
        Ok(Entitlement::Pro)
    }

    async fn offer(&self) -> Result<Option<ProOffer>, AppFailure> {
        Ok(None)
    }

    async fn purchase(&self) -> Result<PurchaseOutcome, AppFailure> {
        Ok(PurchaseOutcome::Purchased)
    }

    async fn restore(&self) -> Result<RestoreResult, AppFailure> {
        Ok(RestoreResult::Found(Entitlement::Pro))
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
    fn develop_repository_reports_pro_without_a_store() {
        let repo = DevelopEntitlementRepository;
        assert_eq!(repo.cached(), Entitlement::Pro);
        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pro));
        assert_eq!(block_on(repo.offer()), Ok(None));
        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Purchased));
        assert_eq!(
            block_on(repo.restore()),
            Ok(RestoreResult::Found(Entitlement::Pro))
        );
    }
}
