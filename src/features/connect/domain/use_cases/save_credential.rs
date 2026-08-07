//! [`SaveCredential`] — gates and stores one server's auth token (the manual
//! save offer's accept, and the single prompt a successful QR pairing raises).
//!
//! A thin, injected-service use case with no logic of its own, for the reason
//! `settings`' own use cases record: the value of the seam is that the
//! controller drives it through `ControllerCore::run` (activity tracking, one
//! failure-emission site) rather than touching a service directly, and a real
//! rule lands here without a call site moving.
//!
//! It never fails: [`CredentialService`]'s own contract is that every gate and
//! store error *is* a [`CredentialStatus`], so this always answers `Ok` and the
//! caller branches on the status.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{CredentialStatus, StoreRequest};
use crate::features::connect::domain::repositories::CredentialService;

/// Stores a credential behind the biometric gate via the injected service.
pub struct SaveCredential {
    service: Arc<dyn CredentialService + Send + Sync>,
}

impl SaveCredential {
    /// Construct with the injected service.
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
