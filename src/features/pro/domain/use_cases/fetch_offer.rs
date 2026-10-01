//! [`FetchOffer`] — the paywall's product lookup. `Ok(None)` is the store
//! answering "not sellable here", not a failure.

use std::sync::Arc;

use clean_signals::{NoParams, UseCase};

use crate::failure::AppFailure;
use crate::features::pro::domain::entities::ProOffer;
use crate::features::pro::domain::repositories::EntitlementRepository;

pub struct FetchOffer {
    repo: Arc<dyn EntitlementRepository + Send + Sync>,
}

impl FetchOffer {
    pub fn new(repo: Arc<dyn EntitlementRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for FetchOffer {
    type Params = NoParams;
    type Output = Option<ProOffer>;
    type Failure = AppFailure;

    async fn execute(&self, _params: NoParams) -> Result<Option<ProOffer>, AppFailure> {
        self.repo.offer().await
    }
}
