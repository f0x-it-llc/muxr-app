//! [`RestorePurchases`] — the store's restore, followed by the same re-read
//! [`RefreshEntitlement`](super::RefreshEntitlement) performs. The two are
//! separate use cases because only one of them is user-initiated.

use std::sync::Arc;

use clean_signals::{NoParams, UseCase};

use crate::failure::AppFailure;
use crate::features::pro::domain::entities::Entitlement;
use crate::features::pro::domain::repositories::EntitlementRepository;

pub struct RestorePurchases {
    repo: Arc<dyn EntitlementRepository + Send + Sync>,
}

impl RestorePurchases {
    pub fn new(repo: Arc<dyn EntitlementRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for RestorePurchases {
    type Params = NoParams;
    type Output = Entitlement;
    type Failure = AppFailure;

    async fn execute(&self, _params: NoParams) -> Result<Entitlement, AppFailure> {
        self.repo.restore().await
    }
}
