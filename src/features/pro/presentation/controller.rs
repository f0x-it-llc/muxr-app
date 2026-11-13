//! App-scoped, not route-scoped: two paywalls must not disagree. `entitlement` is seeded from the store's cached answer;
//! the launch refresh overwrites it (so a refund propagates) and a failed one leaves it alone, so an unreachable
//! store never downgrades a paying install. `busy` is single-flight: buying and restoring share it.

use std::sync::Arc;

use clean_signals::{ControllerCore, Failure, NoParams, RunOptions};
use frust::{GetUntracked, RwSignal, Set};

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::SavedServer;
use crate::features::pro::di::SharedEntitlementRepository;
use crate::features::pro::domain::entities::{
    Entitlement, ProOffer, PurchaseOutcome, RestoreResult, ServerLimitVerdict,
};
use crate::features::pro::domain::use_cases::check_server_limit::check_new_server_limit;
use crate::features::pro::domain::use_cases::{
    FetchOffer, PurchasePro, RefreshEntitlement, RestorePurchases, check_server_limit,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    Restored,
    Pending,
    NothingToRestore,
    Unavailable,
    Failed(String),
    Busy,
}

pub struct ProController {
    core: ControllerCore<AppFailure>,
    refresh_entitlement: RefreshEntitlement,
    fetch_offer: FetchOffer,
    purchase_pro: PurchasePro,
    restore_purchases: RestorePurchases,
    store: SharedEntitlementRepository,
    pub entitlement: RwSignal<Entitlement>,
    /// `None` covers both "not asked yet" and "this platform cannot sell it".
    pub offer: RwSignal<Option<ProOffer>>,
    pub offer_settled: RwSignal<bool>,
    pub busy: RwSignal<bool>,
    pub last_outcome: RwSignal<Option<PurchaseOutcome>>,
}

fn quietly() -> RunOptions<AppFailure> {
    RunOptions {
        emit_failure: false,
        ..RunOptions::default()
    }
}

impl ProController {
    pub fn new(repository: SharedEntitlementRepository) -> Self {
        let cached = repository.cached();
        Self {
            core: ControllerCore::new(),
            refresh_entitlement: RefreshEntitlement::new(Arc::clone(&repository)),
            fetch_offer: FetchOffer::new(Arc::clone(&repository)),
            purchase_pro: PurchasePro::new(Arc::clone(&repository)),
            restore_purchases: RestorePurchases::new(Arc::clone(&repository)),
            store: repository,
            entitlement: RwSignal::new(cached),
            offer: RwSignal::new(None),
            offer_settled: RwSignal::new(false),
            busy: RwSignal::new(false),
            last_outcome: RwSignal::new(None),
        }
    }

    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    /// Never user-initiated, so a failure is logged, not announced.
    pub async fn refresh(&self) {
        match self
            .core
            .run(&self.refresh_entitlement, NoParams, quietly())
            .await
        {
            Ok(entitlement) => self.entitlement.set(entitlement),
            Err(failure) => log::warn!("pro: entitlement refresh failed: {failure}"),
        }
    }

    pub async fn follow_store_changes(&self) {
        loop {
            self.store.store_changed().await;
            if !self.busy.get_untracked() {
                self.refresh().await;
            }
        }
    }

    /// `None` publishes as itself, and a failed lookup settles the same way.
    pub async fn load_offer(&self) {
        self.offer_settled.set(false);
        match self.core.run(&self.fetch_offer, NoParams, quietly()).await {
            Ok(offer) => self.offer.set(offer),
            Err(failure) => log::warn!("pro: offer lookup failed: {failure}"),
        }
        self.offer_settled.set(true);
    }

    /// A buy raised while one is in flight is refused as `Cancelled` without reaching the store;
    /// a failure stays off the failure sink so one store failure is one toast.
    pub async fn purchase(&self) -> PurchaseOutcome {
        if self.busy.get_untracked() {
            return PurchaseOutcome::Cancelled;
        }
        self.busy.set(true);
        let outcome = match self.core.run(&self.purchase_pro, NoParams, quietly()).await {
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

    /// Read off the entitlement alone, an offline restore would tell a paying user the account owns nothing.
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
            Ok(RestoreResult::Found(entitlement)) => {
                self.entitlement.set(entitlement);
                match entitlement {
                    Entitlement::Pro => RestoreOutcome::Restored,
                    Entitlement::Pending => RestoreOutcome::Pending,
                    Entitlement::Free => RestoreOutcome::NothingToRestore,
                }
            }
            Ok(RestoreResult::Unavailable) => RestoreOutcome::Unavailable,
            Err(failure) => RestoreOutcome::Failed(failure.user_message()),
        };
        self.busy.set(false);
        outcome
    }

    /// Untracked: a press handler has no ambient owner on desktop for a tracked read.
    pub fn is_pro_untracked(&self) -> bool {
        self.entitlement.get_untracked().is_pro()
    }

    pub fn verdict_for(&self, saved: &[SavedServer], endpoint: &str) -> ServerLimitVerdict {
        check_server_limit(self.entitlement.get_untracked(), saved, endpoint)
    }

    pub fn new_server_verdict(&self, saved: &[SavedServer]) -> ServerLimitVerdict {
        let entitlement = self.entitlement.get_untracked();
        check_new_server_limit(entitlement, saved)
    }
}
