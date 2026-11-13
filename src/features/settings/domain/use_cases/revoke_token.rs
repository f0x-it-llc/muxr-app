use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::settings::domain::repositories::AuthTokensRepository;

pub struct RevokeToken {
    repo: Arc<dyn AuthTokensRepository + Send + Sync>,
}

impl RevokeToken {
    pub fn new(repo: Arc<dyn AuthTokensRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for RevokeToken {
    type Params = String;
    type Output = ();
    type Failure = AppFailure;

    async fn execute(&self, name: String) -> Result<(), AppFailure> {
        self.repo.revoke_token(name).await
    }
}
