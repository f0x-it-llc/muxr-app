//! [`CreateToken`] — mints a token by name and read-only toggle (the `+
//! create new token` dashed row, through the create dialog). The name
//! arrives already trimmed and non-blank, so this adds no validation of its
//! own; the repository still rejects a blank name, where that invariant is
//! enforced once.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{CreateTokenRequest, MintedToken};
use crate::features::settings::domain::repositories::AuthTokensRepository;

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
