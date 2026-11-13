//! In-memory repository for headless suites: store decisions are scripted and every call is counted.
//! A poisoned lock is recovered so one panicking test cannot cascade into later ones.

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::failure::AppFailure;
use crate::features::pro::domain::entities::{
    Entitlement, PRO_PRODUCT_ID, ProOffer, PurchaseOutcome, RestoreResult,
};
use crate::features::pro::domain::repositories::EntitlementRepository;

pub struct MockEntitlementRepository {
    state: Mutex<MockState>,
}

struct MockState {
    entitlement: Entitlement,
    offer: Option<ProOffer>,
    purchase: PurchaseOutcome,
    /// One-shot: consumed by the next `refresh`, which fails instead of answering.
    next_refresh_failure: Option<AppFailure>,
    next_restore_failure: Option<AppFailure>,
    next_purchase_failure: Option<AppFailure>,
    next_offer_failure: Option<AppFailure>,
    restore_unavailable: bool,
    refresh_calls: usize,
    restore_calls: usize,
    purchase_calls: usize,
}

impl MockEntitlementRepository {
    pub fn free() -> Self {
        Self::with(Entitlement::Free)
    }

    pub fn pro() -> Self {
        Self::with(Entitlement::Pro)
    }

    fn with(entitlement: Entitlement) -> Self {
        Self {
            state: Mutex::new(MockState {
                entitlement,
                offer: Some(default_offer()),
                purchase: PurchaseOutcome::Purchased,
                next_refresh_failure: None,
                next_restore_failure: None,
                next_purchase_failure: None,
                next_offer_failure: None,
                restore_unavailable: false,
                refresh_calls: 0,
                restore_calls: 0,
                purchase_calls: 0,
            }),
        }
    }

    pub fn set_entitlement(&self, entitlement: Entitlement) {
        self.state().entitlement = entitlement;
    }

    /// `Purchased`/`Pending` also flip the stored entitlement, the store's own side effect.
    pub fn script_purchase(&self, outcome: PurchaseOutcome) {
        self.state().purchase = outcome;
    }

    pub fn script_offer(&self, offer: Option<ProOffer>) {
        self.state().offer = offer;
    }

    pub fn fail_next_refresh(&self, failure: AppFailure) {
        self.state().next_refresh_failure = Some(failure);
    }

    pub fn fail_next_restore(&self, failure: AppFailure) {
        self.state().next_restore_failure = Some(failure);
    }

    pub fn fail_next_purchase(&self, failure: AppFailure) {
        self.state().next_purchase_failure = Some(failure);
    }

    pub fn fail_next_offer(&self, failure: AppFailure) {
        self.state().next_offer_failure = Some(failure);
    }

    pub fn script_restore_unavailable(&self) {
        self.state().restore_unavailable = true;
    }

    pub fn refresh_calls(&self) -> usize {
        self.state().refresh_calls
    }

    pub fn restore_calls(&self) -> usize {
        self.state().restore_calls
    }

    pub fn purchase_calls(&self) -> usize {
        self.state().purchase_calls
    }

    fn state(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn default_offer() -> ProOffer {
    ProOffer {
        product_id: PRO_PRODUCT_ID.to_string(),
        title: "Muxr Pro".to_string(),
        description: "Unlimited servers".to_string(),
        display_price: "$4.99".to_string(),
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl EntitlementRepository for MockEntitlementRepository {
    fn cached(&self) -> Entitlement {
        self.state().entitlement
    }

    async fn refresh(&self) -> Result<Entitlement, AppFailure> {
        let mut state = self.state();
        state.refresh_calls += 1;
        match state.next_refresh_failure.take() {
            Some(failure) => Err(failure),
            None => Ok(state.entitlement),
        }
    }

    async fn offer(&self) -> Result<Option<ProOffer>, AppFailure> {
        let mut state = self.state();
        match state.next_offer_failure.take() {
            Some(failure) => Err(failure),
            None => Ok(state.offer.clone()),
        }
    }

    async fn purchase(&self) -> Result<PurchaseOutcome, AppFailure> {
        let mut state = self.state();
        state.purchase_calls += 1;
        if let Some(failure) = state.next_purchase_failure.take() {
            return Err(failure);
        }
        let outcome = state.purchase.clone();
        match outcome {
            PurchaseOutcome::Purchased => state.entitlement = Entitlement::Pro,
            PurchaseOutcome::Pending => state.entitlement = Entitlement::Pending,
            _ => {}
        }
        Ok(outcome)
    }

    async fn restore(&self) -> Result<RestoreResult, AppFailure> {
        let mut state = self.state();
        state.restore_calls += 1;
        match state.next_restore_failure.take() {
            Some(failure) => Err(failure),
            None if state.restore_unavailable => Ok(RestoreResult::Unavailable),
            None => Ok(RestoreResult::Found(state.entitlement)),
        }
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
    fn a_free_install_starts_free_and_a_pro_one_starts_pro() {
        assert_eq!(
            MockEntitlementRepository::free().cached(),
            Entitlement::Free
        );
        assert_eq!(MockEntitlementRepository::pro().cached(), Entitlement::Pro);
    }

    #[test]
    fn refresh_answers_the_stored_entitlement_and_counts_the_call() {
        let repo = MockEntitlementRepository::free();
        repo.set_entitlement(Entitlement::Pro);

        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Pro));
        assert_eq!(repo.refresh_calls(), 1);
    }

    #[test]
    fn a_scripted_refresh_failure_is_consumed_by_exactly_one_call() {
        let repo = MockEntitlementRepository::free();
        repo.fail_next_refresh(AppFailure::Network("store unreachable".to_string()));

        assert!(block_on(repo.refresh()).is_err());
        assert_eq!(block_on(repo.refresh()), Ok(Entitlement::Free));
        assert_eq!(repo.refresh_calls(), 2, "a failed call still counts");
    }

    #[test]
    fn the_seeded_offer_names_the_one_product_this_app_sells() {
        let repo = MockEntitlementRepository::free();
        let offer = block_on(repo.offer()).expect("the seeded offer").unwrap();
        assert_eq!(offer.product_id, PRO_PRODUCT_ID);
        assert_eq!(offer.title, "Muxr Pro");
    }

    #[test]
    fn a_store_that_cannot_sell_here_answers_no_offer_rather_than_failing() {
        let repo = MockEntitlementRepository::free();
        repo.script_offer(None);
        assert_eq!(block_on(repo.offer()), Ok(None));
    }

    #[test]
    fn a_purchase_defaults_to_landing_and_flips_the_stored_entitlement() {
        let repo = MockEntitlementRepository::free();

        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Purchased));
        assert_eq!(repo.cached(), Entitlement::Pro);
        assert_eq!(repo.purchase_calls(), 1);
    }

    #[test]
    fn a_scripted_pending_purchase_leaves_the_install_unentitled() {
        let repo = MockEntitlementRepository::free();
        repo.script_purchase(PurchaseOutcome::Pending);

        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Pending));
        assert_eq!(repo.cached(), Entitlement::Pending);
        assert!(!repo.cached().is_pro());
    }

    #[test]
    fn a_cancelled_purchase_changes_nothing_but_the_counter() {
        let repo = MockEntitlementRepository::free();
        repo.script_purchase(PurchaseOutcome::Cancelled);

        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Cancelled));
        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Cancelled));
        assert_eq!(repo.cached(), Entitlement::Free);
        assert_eq!(repo.purchase_calls(), 2, "the script is not one-shot");
    }

    #[test]
    fn restore_answers_the_stored_entitlement_and_counts_the_call() {
        let repo = MockEntitlementRepository::pro();

        assert_eq!(
            block_on(repo.restore()),
            Ok(RestoreResult::Found(Entitlement::Pro))
        );
        assert_eq!(repo.restore_calls(), 1);
        assert_eq!(repo.refresh_calls(), 0, "a restore is not a refresh");
    }

    #[test]
    fn a_scripted_restore_failure_is_consumed_by_exactly_one_call() {
        let repo = MockEntitlementRepository::pro();
        repo.fail_next_restore(AppFailure::Network("store unreachable".to_string()));

        assert!(block_on(repo.restore()).is_err());
        assert_eq!(
            block_on(repo.restore()),
            Ok(RestoreResult::Found(Entitlement::Pro))
        );
        assert_eq!(repo.restore_calls(), 2, "a failed call still counts");
        assert_eq!(
            repo.refresh_calls(),
            0,
            "the refresh one-shot is a separate script"
        );
    }

    #[test]
    fn a_scripted_purchase_failure_is_consumed_by_exactly_one_call() {
        let repo = MockEntitlementRepository::free();
        repo.fail_next_purchase(AppFailure::Network("store unreachable".to_string()));

        assert!(block_on(repo.purchase()).is_err());
        assert_eq!(
            repo.cached(),
            Entitlement::Free,
            "a failed call grants nothing"
        );
        assert_eq!(block_on(repo.purchase()), Ok(PurchaseOutcome::Purchased));
        assert_eq!(repo.purchase_calls(), 2, "a failed call still counts");
    }

    #[test]
    fn a_scripted_offer_failure_is_consumed_by_exactly_one_call() {
        let repo = MockEntitlementRepository::free();
        repo.fail_next_offer(AppFailure::Network("store unreachable".to_string()));

        assert!(block_on(repo.offer()).is_err());
        assert_eq!(block_on(repo.offer()), Ok(Some(default_offer())));
    }

    #[test]
    fn an_unavailable_restore_says_so_rather_than_answering_an_empty_account() {
        let repo = MockEntitlementRepository::pro();
        repo.script_restore_unavailable();

        assert_eq!(block_on(repo.restore()), Ok(RestoreResult::Unavailable));
        assert_eq!(repo.restore_calls(), 1, "the call still counts");
        assert_eq!(
            repo.cached(),
            Entitlement::Pro,
            "nothing about the account changed"
        );
    }
}
