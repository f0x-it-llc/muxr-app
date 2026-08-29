//! [`ProController`] — the app's one entitlement view model: an embedded
//! [`ControllerCore`], four use cases over its store, every gate's signals.
//!
//! **App-scoped, not route-scoped**: built once in
//! [`crate::features::pro::di`], live for the process, because two paywalls
//! must not disagree. [`entitlement`](Self::entitlement) is seeded from the
//! store's *cached* answer, so the first frame gates on the last known truth;
//! the launch [`refresh`](Self::refresh) overwrites it — what makes a refund
//! propagate — and a failed one leaves it alone, since an unreachable store
//! must not downgrade a paying install. `busy` is single-flight, not a
//! spinner: buying and restoring share it, so a double tap opens one flow.

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

/// What a [`ProController::restore`] actually did — four exits the entitlement
/// signal alone cannot tell apart, only one of which owns nothing to restore.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    /// The store answered, and this account owns Pro — now published.
    Restored,
    /// The store answered, and there is nothing on it to restore.
    NothingToRestore,
    /// The store call failed; the payload is its user-facing message, already
    /// reported on [`ProController::core`]'s failure sink.
    Failed(String),
    /// Refused by the single-flight guard, without reaching the store.
    Busy,
}

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
    /// Whether a lookup has come back at all — what tells [`offer`](Self::offer)'s
    /// two `None`s apart, cleared at the start of each [`load_offer`](Self::load_offer).
    pub offer_settled: RwSignal<bool>,
    /// Whether a purchase or restore is in flight (see the [module docs](self)).
    pub busy: RwSignal<bool>,
    /// The last outcome the store reported, for a screen's confirmation.
    pub last_outcome: RwSignal<Option<PurchaseOutcome>>,
}

impl ProController {
    /// A fresh controller over the injected store, seeded from its cache.
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
            offer_settled: RwSignal::new(false),
            busy: RwSignal::new(false),
            last_outcome: RwSignal::new(None),
        }
    }

    /// This controller's [`ControllerCore`] — the failure sink the app's
    /// toast bridge subscribes to.
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
        self.offer_settled.set(false);
        if let Ok(offer) = self
            .core
            .run(&self.fetch_offer, NoParams, RunOptions::default())
            .await
        {
            self.offer.set(offer);
        }
        self.offer_settled.set(true);
    }

    /// Run the buy flow, publish what it produced, and answer the same
    /// outcome: one raised in flight is refused as `Cancelled` without
    /// reaching the store, and a failure reads back as `Failed` off the sink.
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

    /// Restore prior purchases, publish what they entitle this install to,
    /// and answer which exit it took — a caller reading the entitlement
    /// instead tells a paying user restoring offline that the account owns
    /// nothing. Shares [`purchase`](Self::purchase)'s single-flight guard.
    pub async fn restore(&self) -> RestoreOutcome {
        if self.busy.get_untracked() {
            return RestoreOutcome::Busy;
        }
        self.busy.set(true);
        let outcome = match self
            .core
            .run(&self.restore_purchases, NoParams, RunOptions::default())
            .await
        {
            Ok(entitlement) => {
                self.entitlement.set(entitlement);
                if entitlement.is_pro() {
                    RestoreOutcome::Restored
                } else {
                    RestoreOutcome::NothingToRestore
                }
            }
            Err(failure) => RestoreOutcome::Failed(failure.user_message()),
        };
        self.busy.set(false);
        outcome
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
