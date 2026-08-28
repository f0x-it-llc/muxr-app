//! [`MockEntitlementRepository`] — the in-memory [`EntitlementRepository`]
//! every headless suite runs on, and the only way to walk the purchase outcome
//! table without a real store account.
//!
//! Everything a store would decide is scripted here instead
//! ([`MockEntitlementRepository::script_purchase`]/[`script_offer`]/
//! [`fail_next_refresh`]), and every store call is counted, so a suite can
//! assert "the launch refresh ran exactly once" or "the second, overlapping
//! purchase never reached the store".
//!
//! A poisoned lock is recovered rather than propagated: a panicking test must
//! not cascade into every later one through this shared state.
//!
//! [`script_offer`]: MockEntitlementRepository::script_offer
//! [`fail_next_refresh`]: MockEntitlementRepository::fail_next_refresh

use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::failure::AppFailure;
use crate::features::pro::domain::entities::{
    Entitlement, PRO_PRODUCT_ID, ProOffer, PurchaseOutcome,
};
use crate::features::pro::domain::repositories::EntitlementRepository;

/// The in-memory entitlement store. See the [module docs](self).
pub struct MockEntitlementRepository {
    state: Mutex<MockState>,
}

/// Everything the mock remembers.
struct MockState {
    entitlement: Entitlement,
    /// What [`EntitlementRepository::offer`] answers; `None` models a store
    /// that cannot sell here.
    offer: Option<ProOffer>,
    /// What [`EntitlementRepository::purchase`] answers, from here on.
    purchase: PurchaseOutcome,
    /// Consumed by the next `refresh`, which fails instead of answering.
    next_refresh_failure: Option<AppFailure>,
    refresh_calls: usize,
    restore_calls: usize,
    purchase_calls: usize,
}

impl MockEntitlementRepository {
    /// A free install: the default script (a purchase succeeds, the offer is
    /// on sale) with nothing bought yet.
    pub fn free() -> Self {
        Self::with(Entitlement::Free)
    }

    /// An install that already owns Pro.
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
                refresh_calls: 0,
                restore_calls: 0,
                purchase_calls: 0,
            }),
        }
    }

    /// Force the stored entitlement, as a store answer would have.
    pub fn set_entitlement(&self, entitlement: Entitlement) {
        self.state().entitlement = entitlement;
    }

    /// Script what [`EntitlementRepository::purchase`] answers from here on.
    /// [`PurchaseOutcome::Purchased`] also flips the stored entitlement to
    /// [`Entitlement::Pro`], and [`PurchaseOutcome::Pending`] to
    /// [`Entitlement::Pending`] — the store's own side effect, which a
    /// controller must not have to fake.
    pub fn script_purchase(&self, outcome: PurchaseOutcome) {
        self.state().purchase = outcome;
    }

    /// Script the product lookup's answer; `None` models an unknown SKU or a
    /// platform with no billing at all.
    pub fn script_offer(&self, offer: Option<ProOffer>) {
        self.state().offer = offer;
    }

    /// Make the next (and only the next) refresh fail with `failure`.
    pub fn fail_next_refresh(&self, failure: AppFailure) {
        self.state().next_refresh_failure = Some(failure);
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

/// The seeded product, as a store that knows [`PRO_PRODUCT_ID`] would report it.
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
        Ok(self.state().offer.clone())
    }

    async fn purchase(&self) -> Result<PurchaseOutcome, AppFailure> {
        let mut state = self.state();
        state.purchase_calls += 1;
        let outcome = state.purchase.clone();
        match outcome {
            PurchaseOutcome::Purchased => state.entitlement = Entitlement::Pro,
            PurchaseOutcome::Pending => state.entitlement = Entitlement::Pending,
            _ => {}
        }
        Ok(outcome)
    }

    async fn restore(&self) -> Result<Entitlement, AppFailure> {
        let mut state = self.state();
        state.restore_calls += 1;
        Ok(state.entitlement)
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

        assert_eq!(block_on(repo.restore()), Ok(Entitlement::Pro));
        assert_eq!(repo.restore_calls(), 1);
        assert_eq!(repo.refresh_calls(), 0, "a restore is not a refresh");
    }
}
