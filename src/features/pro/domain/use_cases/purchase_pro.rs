//! [`PurchasePro`] — the buy flow. It resolves only once the store reports an
//! outcome, so a dismissed sheet is an `Ok(Cancelled)`, not an error.

use std::sync::Arc;

use clean_signals::{NoParams, UseCase};

use crate::failure::AppFailure;
use crate::features::pro::domain::entities::PurchaseOutcome;
use crate::features::pro::domain::repositories::EntitlementRepository;

pub struct PurchasePro {
    repo: Arc<dyn EntitlementRepository + Send + Sync>,
}

impl PurchasePro {
    pub fn new(repo: Arc<dyn EntitlementRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for PurchasePro {
    type Params = NoParams;
    type Output = PurchaseOutcome;
    type Failure = AppFailure;

    async fn execute(&self, _params: NoParams) -> Result<PurchaseOutcome, AppFailure> {
        self.repo.purchase().await
    }
}
