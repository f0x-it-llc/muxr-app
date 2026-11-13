use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{CreateSessionParams, Session};
use crate::features::sessions::domain::repositories::SessionsRepository;

pub struct CreateSession {
    repo: Arc<dyn SessionsRepository + Send + Sync>,
}

impl CreateSession {
    pub fn new(repo: Arc<dyn SessionsRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for CreateSession {
    type Params = CreateSessionParams;
    type Output = Session;
    type Failure = AppFailure;

    async fn execute(&self, params: CreateSessionParams) -> Result<Session, AppFailure> {
        self.repo.create_session(params).await
    }
}
