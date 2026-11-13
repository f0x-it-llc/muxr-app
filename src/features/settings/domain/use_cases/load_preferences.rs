use std::sync::Arc;

use clean_signals::{NoParams, UseCase};

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::Preferences;
use crate::features::settings::domain::repositories::PreferencesRepository;

pub struct LoadPreferences {
    repo: Arc<dyn PreferencesRepository + Send + Sync>,
}

impl LoadPreferences {
    pub fn new(repo: Arc<dyn PreferencesRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for LoadPreferences {
    type Params = NoParams;
    type Output = Preferences;
    type Failure = AppFailure;

    async fn execute(&self, _params: NoParams) -> Result<Preferences, AppFailure> {
        self.repo.load_preferences().await
    }
}
