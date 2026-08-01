//! [`LoadWorkspace`] — one round trip for everything the terminal screen
//! shows: the sheet's spaces and tabs, the active tab's lines, and the
//! attached session's current name.

use std::sync::Arc;

use clean_signals::UseCase;

use crate::failure::AppFailure;
use crate::features::sessions::domain::repositories::SessionsRepository;
use crate::features::terminal::domain::entities::Workspace;
use crate::features::terminal::domain::repositories::TerminalRepository;

/// Loads the whole screen. `Params` is the attached session's id (from
/// `/terminal?session=<id>`), or `None` for a bare `/terminal`.
///
/// **Two injected repositories, on purpose.** Spaces/tabs/lines come from this
/// feature's own [`TerminalRepository`]; the session *name* comes from the
/// `sessions` feature's [`SessionsRepository`], because that is where a
/// session's name lives and the rename this screen offers writes back to the
/// very same store (see
/// [`RenameAttachedSession`](super::RenameAttachedSession)). Reaching a
/// sibling feature's **domain** contract is a deliberate widening of
/// `docs/ARCHITECTURE.md`'s slice table — the same shape as `connect`'s
/// re-export of `crate::connection::TrustMode`, and strictly better than a
/// second name store that could drift out of step with the sessions list.
pub struct LoadWorkspace {
    terminal: Arc<dyn TerminalRepository + Send + Sync>,
    sessions: Arc<dyn SessionsRepository + Send + Sync>,
}

impl LoadWorkspace {
    /// Construct with both injected repositories.
    pub fn new(
        terminal: Arc<dyn TerminalRepository + Send + Sync>,
        sessions: Arc<dyn SessionsRepository + Send + Sync>,
    ) -> Self {
        Self { terminal, sessions }
    }

    /// The name of the session with `id`, or `None` when there is no id (a
    /// bare `/terminal`) or the session has since been killed. A *missing*
    /// session is not a failure: the screen's mock content does not depend on
    /// it, only the rename dialog's seed does.
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
    type Params = Option<String>;
    type Output = Workspace;
    type Failure = AppFailure;

    async fn execute(&self, session_id: Option<String>) -> Result<Workspace, AppFailure> {
        let spaces = self.terminal.list_spaces().await?;
        let tabs = self.terminal.list_tabs().await?;
        let lines = match tabs.iter().find(|tab| tab.active).or_else(|| tabs.first()) {
            Some(tab) => self.terminal.tab_lines(tab.key.clone()).await?,
            None => Vec::new(),
        };
        let session_name = self.session_name(session_id).await?;
        Ok(Workspace {
            spaces,
            tabs,
            lines,
            session_name,
        })
    }
}
