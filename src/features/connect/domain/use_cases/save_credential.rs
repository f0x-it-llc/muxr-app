use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{CredentialStatus, StoreRequest};
use crate::features::connect::domain::keyring::Keyring;
use crate::features::connect::domain::repositories::CredentialService;

/// Saves a credential: [`UseCase::execute`] is the locked save,
/// [`Self::run_in_vault`] the unlocked one over the in-memory keyring.
pub struct SaveCredential {
    service: Arc<dyn CredentialService + Send + Sync>,
}

impl SaveCredential {
    pub fn new(service: Arc<dyn CredentialService + Send + Sync>) -> Self {
        Self { service }
    }

    /// Writes `keyring` plus `request`'s token as the vault behind one gate;
    /// `keyring` gains the token only when that write succeeded.
    pub async fn run_in_vault(
        &self,
        request: StoreRequest,
        keyring: &mut Keyring,
    ) -> CredentialStatus {
        self.service.store_in_vault(request, keyring).await
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
