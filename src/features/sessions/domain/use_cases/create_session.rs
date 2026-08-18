//! [`CreateSession`] — creates a session by name (the list's dashed
//! "+ new session" row and the empty state's action, both through the
//! name-input dialog).
//!
//! The name arrives already trimmed and non-blank (`ui::dialogs`'s
//! `name_input_dialog` cannot submit anything else), so this use case adds no
//! validation of its own; the repository still rejects a blank name, which is
//! where that invariant is enforced once.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::Session;
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
    type Params = String;
    type Output = Session;
    type Failure = AppFailure;

    async fn execute(&self, name: String) -> Result<Session, AppFailure> {
        self.repo.create_session(name).await
    }
}
