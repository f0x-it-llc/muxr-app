use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{
    CreateSessionParams, RenameRequest, Session, SessionAgent,
};

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait SessionsRepository {
    /// Sessions in the server's own display order, never re-sorted.
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure>;

    async fn create_session(&self, params: CreateSessionParams) -> Result<Session, AppFailure>;

    async fn rename_session(&self, request: RenameRequest) -> Result<Session, AppFailure>;

    async fn kill_session(&self, id: String) -> Result<(), AppFailure>;

    /// The AI agents running in session `session_id`, most urgent first.
    /// Defaults to none, for a backend (or double) that reports no agents.
    async fn list_agents(&self, _session_id: String) -> Result<Vec<SessionAgent>, AppFailure> {
        Ok(Vec::new())
    }
}
