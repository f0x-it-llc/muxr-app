//! [`LoadWorkspace`] — one round trip for everything the terminal screen
//! shows: the sheet's spaces and tabs, the active tab's lines, and the
//! attached session's current name.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::sessions::domain::repositories::SessionsRepository;
use crate::features::terminal::domain::entities::Workspace;
use crate::features::terminal::domain::repositories::TerminalRepository;

/// What one load addresses — [`LoadWorkspace`]'s params. Two facts because the two reads this
/// use case makes are addressed differently: the session *name* needs an id, and the space list
/// needs to know whether this backend has spaces at all (only herdr does — zellij answers an
/// always-empty list, so its load skips that round trip). The caller decides the flag;
/// resolving a backend off a session id is `presentation`'s own job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceQuery {
    /// The attached session's id (`/terminal?session=<id>`), or `None` for a
    /// bare `/terminal` — what the session *name* is looked up by.
    pub session_id: Option<String>,
    /// Whether to read the space list at all — see the [type docs](Self).
    pub with_spaces: bool,
}

/// Loads the whole screen. `Params` is a [`WorkspaceQuery`]: which session to name, and
/// whether this backend has spaces to list.
///
/// **Two injected repositories, on purpose.** Spaces/tabs/lines come from this feature's own
/// [`TerminalRepository`]; the session *name* comes from `sessions`' [`SessionsRepository`],
/// because that is where a session's name lives and this screen's rename writes back to the
/// same store — a deliberate widening of `docs/ARCHITECTURE.md`'s slice table, strictly better
/// than a second name store that could drift out of step with the sessions list.
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

    /// The name of the session with `id`, or `None` when there is no id (a bare
    /// `/terminal`) or the session has since been killed. A *missing* session is not
    /// a failure: only the rename dialog's seed depends on it.
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
