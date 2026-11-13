use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::terminal::domain::repositories::TerminalRepository;

/// Leaves the multiplexer process running, so it is not destructive and needs no confirmation.
pub struct DetachSession {
    repo: Arc<dyn TerminalRepository + Send + Sync>,
}

impl DetachSession {
    pub fn new(repo: Arc<dyn TerminalRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for DetachSession {
    type Params = String;
    type Output = ();
    type Failure = AppFailure;

    async fn execute(&self, id: String) -> Result<(), AppFailure> {
        self.repo.detach_session(id).await
    }
}
