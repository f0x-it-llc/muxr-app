//! [`CreateToken`] — mints a token by name (the `+ create new token` dashed
//! row, through the name-input dialog).
//!
//! The name arrives already trimmed and non-blank (`ui::dialogs`'s
//! `name_input_dialog` cannot submit anything else), so this use case adds no
//! validation of its own; the repository still rejects a blank name, which is
//! where that invariant is enforced once.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::AuthToken;
use crate::features::settings::domain::repositories::AuthTokensRepository;

/// Mints an auth token via the injected repository.
pub struct CreateToken {
    repo: Arc<dyn AuthTokensRepository + Send + Sync>,
}

impl CreateToken {
    /// Construct with the injected repository.
    pub fn new(repo: Arc<dyn AuthTokensRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for CreateToken {
    type Params = String;
    type Output = AuthToken;
    type Failure = AppFailure;

    async fn execute(&self, name: String) -> Result<AuthToken, AppFailure> {
        self.repo.create_token(name).await
    }
}
