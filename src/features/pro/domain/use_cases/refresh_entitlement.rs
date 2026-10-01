//! [`RefreshEntitlement`] — the store's owned-purchases query, whose answer
//! overwrites the cache (see the repository contract).

use std::sync::Arc;

use clean_signals::{NoParams, UseCase};

use crate::failure::AppFailure;
use crate::features::pro::domain::entities::Entitlement;
use crate::features::pro::domain::repositories::EntitlementRepository;

pub struct RefreshEntitlement {
    repo: Arc<dyn EntitlementRepository + Send + Sync>,
}

impl RefreshEntitlement {
    pub fn new(repo: Arc<dyn EntitlementRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for RefreshEntitlement {
    type Params = NoParams;
    type Output = Entitlement;
    type Failure = AppFailure;

    async fn execute(&self, _params: NoParams) -> Result<Entitlement, AppFailure> {
        self.repo.refresh().await
    }
}
