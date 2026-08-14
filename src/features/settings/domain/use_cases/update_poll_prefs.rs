//! [`UpdatePollPrefs`] — persists the `session poll` group after the layout
//! refresh picker lands, or the bell switch flips.
//!
//! Whole-group `Params`, for the same reason
//! [`UpdateAppearance`](super::UpdateAppearance) takes one.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::PollPrefs;
use crate::features::settings::domain::repositories::PreferencesRepository;

pub struct UpdatePollPrefs {
    repo: Arc<dyn PreferencesRepository + Send + Sync>,
}

impl UpdatePollPrefs {
    pub fn new(repo: Arc<dyn PreferencesRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for UpdatePollPrefs {
    type Params = PollPrefs;
    type Output = PollPrefs;
    type Failure = AppFailure;

    async fn execute(&self, prefs: PollPrefs) -> Result<PollPrefs, AppFailure> {
        self.repo.save_poll_prefs(prefs).await
    }
}
