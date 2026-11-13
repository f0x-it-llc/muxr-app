use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::terminal::domain::entities::TermLine;
use crate::features::terminal::domain::repositories::TerminalRepository;

pub struct SwitchTab {
    repo: Arc<dyn TerminalRepository + Send + Sync>,
}

impl SwitchTab {
    pub fn new(repo: Arc<dyn TerminalRepository + Send + Sync>) -> Self {
        Self { repo }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for SwitchTab {
    type Params = String;
    type Output = Vec<TermLine>;
    type Failure = AppFailure;

    async fn execute(&self, key: String) -> Result<Vec<TermLine>, AppFailure> {
        self.repo.tab_lines(key).await
    }
}
