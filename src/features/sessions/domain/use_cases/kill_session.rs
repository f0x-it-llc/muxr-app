//! [`KillSession`] — kills a session (a card's destructive "kill" action,
//! behind `ui::dialogs`'s confirm dialog).

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::sessions::domain::repositories::SessionsRepository;

/// Kills a session via the injected repository.
pub struct KillSession {
    repo: Arc<dyn SessionsRepository + Send + Sync>,
}

impl KillSession {
    /// Construct with the injected repository.
    pub fn new(repo: Arc<dyn SessionsRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for KillSession {
    type Params = String;
    type Output = ();
    type Failure = AppFailure;

    async fn execute(&self, id: String) -> Result<(), AppFailure> {
        self.repo.kill_session(id).await
    }
}
