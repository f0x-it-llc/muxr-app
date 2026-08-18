use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{UnlockRequest, UnlockedCredential};
use crate::features::connect::domain::repositories::CredentialService;

pub struct UnlockCredential {
    service: Arc<dyn CredentialService + Send + Sync>,
}

impl UnlockCredential {
    pub fn new(service: Arc<dyn CredentialService + Send + Sync>) -> Self {
        Self { service }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for UnlockCredential {
    type Params = UnlockRequest;
    type Output = UnlockedCredential;
    type Failure = AppFailure;

    async fn execute(&self, request: UnlockRequest) -> Result<UnlockedCredential, AppFailure> {
        Ok(self.service.unlock(request).await)
    }
}
