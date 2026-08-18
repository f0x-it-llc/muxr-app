//! [`RenameAttachedSession`] — renames the session this screen is attached to,
//! through the **`sessions`** feature's own repository.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{RenameRequest, Session};
use crate::features::sessions::domain::repositories::SessionsRepository;

/// Renames a session via the injected sessions repository.
///
/// A session's name has exactly one owner, and it is not this feature: the `/sessions` list
/// renders it, its own `RenameSession` use case writes it, and its mock store holds it. So the
/// terminal's overflow-menu rename injects `Arc<dyn SessionsRepository>` and delegates — which
/// is why a rename performed here is already visible in the session list on pop back, with no
/// cache to invalidate and no second write path to keep in step.
///
/// Named `RenameAttachedSession` rather than `RenameSession` so the two stay distinguishable at
/// a glance in a stack trace or import list; the behaviour is identical.
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
