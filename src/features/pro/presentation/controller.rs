//! [`ProController`] — the app's one entitlement view model: a
//! [`ControllerCore`] embedded by composition, this feature's four use cases
//! over its injected repository, and the signals every gate reads.
//!
//! **App-scoped, not route-scoped**: built once in
//! [`crate::features::pro::di`], live for the process, because two paywalls
//! must not be able to disagree. [`entitlement`](Self::entitlement) is seeded
//! from the store's *cached* answer, so the first frame gates on the last
//! known truth rather than on Free-until-proven; the launch
//! [`refresh`](Self::refresh) overwrites it, which is what makes a refund
//! propagate, and a failed one leaves it alone — an unreachable store must not
//! downgrade a paying install. `busy` is single-flight, not a spinner: buying
//! and restoring share it, so a double tap can never open two store flows.

use std::sync::Arc;

use clean_signals::{ControllerCore, Failure, NoParams, RunOptions};
use frust::{GetUntracked, RwSignal, Set};

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::SavedServer;
use crate::features::pro::di::SharedEntitlementRepository;
use crate::features::pro::domain::entities::{
    Entitlement, ProOffer, PurchaseOutcome, ServerLimitVerdict,
};
use crate::features::pro::domain::use_cases::{
    FetchOffer, PurchasePro, RefreshEntitlement, RestorePurchases, check_server_limit,
};

/// The app's entitlement view model. See the [module docs](self).
pub struct ProController {
    core: ControllerCore<AppFailure>,
    refresh_entitlement: RefreshEntitlement,
    fetch_offer: FetchOffer,
    purchase_pro: PurchasePro,
    restore_purchases: RestorePurchases,
    /// What this install is entitled to right now — every gate's input.
    pub entitlement: RwSignal<Entitlement>,
    /// The store's product description; `None` covers both "not asked yet"
    /// and "this platform cannot sell it".
    pub offer: RwSignal<Option<ProOffer>>,
    /// Whether a purchase or restore is in flight (see the [module docs](self)).
    pub busy: RwSignal<bool>,
    /// The last outcome the store reported, for a screen's confirmation.
    pub last_outcome: RwSignal<Option<PurchaseOutcome>>,
}

impl ProController {
    /// A fresh controller over the injected store, seeded from its cached
    /// entitlement — built once by the composition root.
    pub fn new(repository: SharedEntitlementRepository) -> Self {
        let cached = repository.cached();
        Self {
            core: ControllerCore::new(),
            refresh_entitlement: RefreshEntitlement::new(Arc::clone(&repository)),
            fetch_offer: FetchOffer::new(Arc::clone(&repository)),
            purchase_pro: PurchasePro::new(Arc::clone(&repository)),
            restore_purchases: RestorePurchases::new(repository),
            entitlement: RwSignal::new(cached),
            offer: RwSignal::new(None),
            busy: RwSignal::new(false),
            last_outcome: RwSignal::new(None),
        }
    }

    /// This controller's [`ControllerCore`] — the failure sink a screen
    /// subscribes its toast bridge to.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    /// Ask the store what this account owns and publish it.
    pub async fn refresh(&self) {
        if let Ok(entitlement) = self
            .core
            .run(&self.refresh_entitlement, NoParams, RunOptions::default())
            .await
        {
            self.entitlement.set(entitlement);
        }
    }

    /// Look the product up for the paywall; `None` publishes as itself, since
    /// "cannot be sold here" is an answer, not a failure to retry.
    pub async fn load_offer(&self) {
        if let Ok(offer) = self
            .core
            .run(&self.fetch_offer, NoParams, RunOptions::default())
            .await
        {
            self.offer.set(offer);
        }
    }

    /// Run the buy flow, publish what it produced, and answer the caller the
    /// same outcome. A call raised while one is in flight is refused as
    /// `Cancelled` without reaching the store; a repository failure reports on
    /// the sink and reads back as `Failed`, user-facing message only.
    pub async fn purchase(&self) -> PurchaseOutcome {
        if self.busy.get_untracked() {
            return PurchaseOutcome::Cancelled;
        }
        self.busy.set(true);
        let outcome = match self
            .core
            .run(&self.purchase_pro, NoParams, RunOptions::default())
            .await
        {
            Ok(outcome) => outcome,
            Err(failure) => PurchaseOutcome::Failed(failure.user_message()),
        };
        match outcome {
            PurchaseOutcome::Purchased => self.entitlement.set(Entitlement::Pro),
            PurchaseOutcome::Pending => self.entitlement.set(Entitlement::Pending),
            _ => {}
        }
        self.last_outcome.set(Some(outcome.clone()));
        self.busy.set(false);
        outcome
    }

    /// Restore prior purchases and publish what they entitle this install to.
    /// Shares [`purchase`](Self::purchase)'s single-flight guard.
    pub async fn restore(&self) {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        if let Ok(entitlement) = self
            .core
            .run(&self.restore_purchases, NoParams, RunOptions::default())
            .await
        {
            self.entitlement.set(entitlement);
        }
        self.busy.set(false);
    }

    /// Whether Pro is unlocked, read **without** tracking — a press handler
    /// has no ambient owner on desktop for a tracked read to subscribe.
    pub fn is_pro_untracked(&self) -> bool {
        self.entitlement.get_untracked().is_pro()
    }

    /// Whether this install may connect to `endpoint` while `saved` is
    /// remembered. Untracked, like [`is_pro_untracked`](Self::is_pro_untracked).
    pub fn verdict_for(&self, saved: &[SavedServer], endpoint: &str) -> ServerLimitVerdict {
        check_server_limit(self.entitlement.get_untracked(), saved, endpoint)
    }
}
