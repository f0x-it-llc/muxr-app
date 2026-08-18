//! [`RenameSession`] — renames an existing session (a card's "rename" action,
//! through the same name-input dialog `CreateSession` uses, seeded with the
//! current name).

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{RenameRequest, Session};
use crate::features::sessions::domain::repositories::SessionsRepository;

pub struct RenameSession {
    repo: Arc<dyn SessionsRepository + Send + Sync>,
}

impl RenameSession {
    pub fn new(repo: Arc<dyn SessionsRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for RenameSession {
    type Params = RenameRequest;
    type Output = Session;
    type Failure = AppFailure;

    async fn execute(&self, request: RenameRequest) -> Result<Session, AppFailure> {
        self.repo.rename_session(request).await
    }
}
