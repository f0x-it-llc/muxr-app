use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{CredentialStatus, StoreRequest};
use crate::features::connect::domain::repositories::CredentialService;

pub struct SaveCredential {
    service: Arc<dyn CredentialService + Send + Sync>,
}

impl SaveCredential {
    pub fn new(service: Arc<dyn CredentialService + Send + Sync>) -> Self {
        Self { service }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for SaveCredential {
    type Params = StoreRequest;
    type Output = CredentialStatus;
    type Failure = AppFailure;

    async fn execute(&self, request: StoreRequest) -> Result<CredentialStatus, AppFailure> {
        Ok(self.service.store(request).await)
    }
}
