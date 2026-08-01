//! [`ListTokens`] — the auth-token list loader (the screen's first load, and
//! the reload every mutation ends with).
//!
//! Takes the [`AuthTokensRepository`] as `Arc<dyn … + Send + Sync>` (the
//! `sessions` feature's `ListSessions` shape) and delegates to it; the
//! simulated latency and the failure boundary live in the data-layer
//! implementation.

use std::sync::Arc;

use clean_signals::{NoParams, UseCase};

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::AuthToken;
use crate::features::settings::domain::repositories::AuthTokensRepository;

/// Loads the connected server's auth tokens via the injected repository.
pub struct ListTokens {
    repo: Arc<dyn AuthTokensRepository + Send + Sync>,
}

impl ListTokens {
    /// Construct with the injected repository.
    pub fn new(repo: Arc<dyn AuthTokensRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for ListTokens {
    type Params = NoParams;
    type Output = Vec<AuthToken>;
    type Failure = AppFailure;

    async fn execute(&self, _params: NoParams) -> Result<Vec<AuthToken>, AppFailure> {
        self.repo.list_tokens().await
    }
}
