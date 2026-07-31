//! [`ListSessions`] — the session-list loader (and the mock poll's one read).
//!
//! Takes the [`SessionsRepository`] as `Arc<dyn SessionsRepository + Send + Sync>`
//! (huddle's `LoadChannels` shape) and delegates to it; the simulated latency
//! and the failure boundary live in the data-layer implementation.

use std::sync::Arc;

use clean_signals::{NoParams, UseCase};

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::Session;
use crate::features::sessions::domain::repositories::SessionsRepository;

/// Loads the connected server's session list via the injected repository.
pub struct ListSessions {
    repo: Arc<dyn SessionsRepository + Send + Sync>,
}

impl ListSessions {
    /// Construct with the injected repository.
    pub fn new(repo: Arc<dyn SessionsRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for ListSessions {
    type Params = NoParams;
    type Output = Vec<Session>;
    type Failure = AppFailure;

    async fn execute(&self, _params: NoParams) -> Result<Vec<Session>, AppFailure> {
        self.repo.list_sessions().await
    }
}
