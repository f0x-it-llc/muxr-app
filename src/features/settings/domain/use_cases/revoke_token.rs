//! [`RevokeToken`] — revokes a token by name (a `.revoke-btn`, behind the
//! confirmation dialog).
//!
//! Destructive and irreversible on a real server, which is why the *screen*
//! puts `ui::dialogs::confirm_dialog` in front of it; this use case itself is
//! the plain delegation every other one here is.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::settings::domain::repositories::AuthTokensRepository;

/// Revokes an auth token via the injected repository.
pub struct RevokeToken {
    repo: Arc<dyn AuthTokensRepository + Send + Sync>,
}

impl RevokeToken {
    /// Construct with the injected repository.
    pub fn new(repo: Arc<dyn AuthTokensRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for RevokeToken {
    type Params = String;
    type Output = ();
    type Failure = AppFailure;

    async fn execute(&self, name: String) -> Result<(), AppFailure> {
        self.repo.revoke_token(name).await
    }
}
