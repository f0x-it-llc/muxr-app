//! [`UpdateAppearance`] — persists the `terminal appearance` group after a
//! picker sheet lands on a new font family / font size / cursor style.
//!
//! Takes the **whole** group rather than one changed field: the screen already
//! holds the current [`AppearancePrefs`] in a signal, so building the next
//! value there is one struct update, and a per-field API would need three
//! repository methods (and three use cases) that differ only in which line
//! they assign.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::AppearancePrefs;
use crate::features::settings::domain::repositories::PreferencesRepository;

pub struct UpdateAppearance {
    repo: Arc<dyn PreferencesRepository + Send + Sync>,
}

impl UpdateAppearance {
    pub fn new(repo: Arc<dyn PreferencesRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for UpdateAppearance {
    type Params = AppearancePrefs;
    type Output = AppearancePrefs;
    type Failure = AppFailure;

    async fn execute(&self, prefs: AppearancePrefs) -> Result<AppearancePrefs, AppFailure> {
        self.repo.save_appearance(prefs).await
    }
}
