//! [`DetachSession`] — leaves the attached session running and drops this
//! client's attachment (the overflow menu's third row).

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::terminal::domain::repositories::TerminalRepository;

/// Detaches from a session via the injected repository. `Params` is the
/// session id; there is nothing to hand back.
///
/// Distinct from the sessions screen's `KillSession` on purpose: detaching
/// leaves the multiplexer process up (the session stays `running` in the
/// list), which is why it is not destructive and carries no confirmation.
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
