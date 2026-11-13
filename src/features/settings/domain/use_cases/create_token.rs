use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{CreateTokenRequest, MintedToken};
use crate::features::settings::domain::repositories::AuthTokensRepository;

/// The name arrives trimmed and non-blank; the repository enforces non-blank.
pub struct CreateToken {
    repo: Arc<dyn AuthTokensRepository + Send + Sync>,
}

impl CreateToken {
    pub fn new(repo: Arc<dyn AuthTokensRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for CreateToken {
    type Params = CreateTokenRequest;
    type Output = MintedToken;
    type Failure = AppFailure;

    async fn execute(&self, request: CreateTokenRequest) -> Result<MintedToken, AppFailure> {
        self.repo.create_token(request).await
    }
}
