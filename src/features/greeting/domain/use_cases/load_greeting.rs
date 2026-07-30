//! [`LoadGreeting`] — the async greeting loader.
//!
//! Takes the [`GreetingRepository`] as `Arc<dyn GreetingRepository + Send +
//! Sync>` (the team-demo `LoadTeam` shape) and delegates to it. Never call
//! [`UseCase::execute`] directly from a controller — always drive it
//! through `ControllerCore::run`/`run_into` (see
//! `presentation::controllers::GreetingController::load`).

use std::sync::Arc;

use clean_signals::{NoParams, UseCase};

use crate::failure::AppFailure;
use crate::features::greeting::domain::repositories::GreetingRepository;

/// Loads the greeting via the injected [`GreetingRepository`].
pub struct LoadGreeting {
    repo: Arc<dyn GreetingRepository + Send + Sync>,
}

impl LoadGreeting {
    /// Construct with the injected repository.
    pub fn new(repo: Arc<dyn GreetingRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for LoadGreeting {
    type Params = NoParams;
    type Output = String;
    type Failure = AppFailure;

    async fn execute(&self, _params: NoParams) -> Result<String, AppFailure> {
        self.repo.load_greeting().await
    }
}