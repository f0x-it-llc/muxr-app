use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::sessions::domain::repositories::SessionsRepository;
use crate::features::terminal::domain::entities::Workspace;
use crate::features::terminal::domain::repositories::TerminalRepository;

/// Only herdr has spaces; zellij's space list is always empty, so `with_spaces: false` skips
/// that round trip.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceQuery {
    /// The session name is looked up by this id; `None` for a bare `/terminal`.
    pub session_id: Option<String>,
    pub with_spaces: bool,
}

/// The session name comes from [`SessionsRepository`], the same store a rename writes to.
pub struct LoadWorkspace {
    terminal: Arc<dyn TerminalRepository + Send + Sync>,
    sessions: Arc<dyn SessionsRepository + Send + Sync>,
}

impl LoadWorkspace {
    pub fn new(
        terminal: Arc<dyn TerminalRepository + Send + Sync>,
        sessions: Arc<dyn SessionsRepository + Send + Sync>,
    ) -> Self {
        Self { terminal, sessions }
    }

    /// A missing or killed session answers `Ok(None)`, not a failure.
    async fn session_name(&self, id: Option<String>) -> Result<Option<String>, AppFailure> {
        let Some(id) = id else {
            return Ok(None);
        };
        let sessions = self.sessions.list_sessions().await?;
        Ok(sessions
            .into_iter()
            .find(|session| session.id == id)
            .map(|session| session.name))
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl UseCase for LoadWorkspace {
    type Params = WorkspaceQuery;
    type Output = Workspace;
    type Failure = AppFailure;

    async fn execute(&self, query: WorkspaceQuery) -> Result<Workspace, AppFailure> {
        let spaces = if query.with_spaces {
            self.terminal.list_spaces().await?
        } else {
            Vec::new()
        };
        let tabs = self.terminal.list_tabs().await?;
        let lines = match tabs.iter().find(|tab| tab.active).or_else(|| tabs.first()) {
            Some(tab) => self.terminal.tab_lines(tab.key.clone()).await?,
            None => Vec::new(),
        };
        let session_name = self.session_name(query.session_id).await?;
        Ok(Workspace {
            spaces,
            tabs,
            lines,
            session_name,
        })
    }
}
