use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{RenameRequest, Session};
use crate::features::sessions::domain::repositories::SessionsRepository;

/// Writes through `sessions`' repository: a session's name has one owner, so the session list
/// shows the rename with no cache to invalidate.
pub struct RenameAttachedSession {
    sessions: Arc<dyn SessionsRepository + Send + Sync>,
}

impl RenameAttachedSession {
    pub fn new(sessions: Arc<dyn SessionsRepository + Send + Sync>) -> Self {
        Self { sessions }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for RenameAttachedSession {
    type Params = RenameRequest;
    type Output = Session;
    type Failure = AppFailure;

    async fn execute(&self, request: RenameRequest) -> Result<Session, AppFailure> {
        self.sessions.rename_session(request).await
    }
}
