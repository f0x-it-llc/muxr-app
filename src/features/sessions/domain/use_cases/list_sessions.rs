use std::sync::Arc;

use clean_signals::{NoParams, UseCase};

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::Session;
use crate::features::sessions::domain::repositories::SessionsRepository;

pub struct ListSessions {
    repo: Arc<dyn SessionsRepository + Send + Sync>,
}

impl ListSessions {
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
