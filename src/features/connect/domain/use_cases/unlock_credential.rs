//! [`UnlockCredential`] — gates and reads one server's stored auth token (the
//! saved-row tap's first step).
//!
//! [`SaveCredential`](super::save_credential::SaveCredential)'s mirror image,
//! and infallible for the same reason: every refusal is an
//! [`UnlockedCredential::status`], so the caller matches the outcome rather than
//! a failure.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{UnlockRequest, UnlockedCredential};
use crate::features::connect::domain::repositories::CredentialService;

/// Recovers a stored credential from behind the biometric gate via the injected
/// service.
pub struct UnlockCredential {
    service: Arc<dyn CredentialService + Send + Sync>,
}

impl UnlockCredential {
    /// Construct with the injected service.
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
