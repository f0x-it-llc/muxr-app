//! [`SessionsRepository`] — list/create/rename/kill, the four ops `data`
//! implements and `domain::use_cases` depend on. Async so
//! `GrpcSessionsRepository` can genuinely await a `muxrd` round trip while
//! `MockSessionsRepository` implements the same contract with no real await.
//! Consumed as `Arc<dyn SessionsRepository + Send + Sync>`; every method
//! fails with [`AppFailure`], mapped once in `data`.

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{RenameRequest, Session};

/// Read/write access to the connected server's session list.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait SessionsRepository {
    /// Sessions in the server's own display order — never re-sorted here.
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure>;

    /// Creates a session named `name`; returns it as stored (server-minted id).
    async fn create_session(&self, name: String) -> Result<Session, AppFailure>;

    /// Renames an existing session; returns it as stored.
    async fn rename_session(&self, request: RenameRequest) -> Result<Session, AppFailure>;

    /// Kills the session with `id`, disconnecting any attached client.
    async fn kill_session(&self, id: String) -> Result<(), AppFailure>;
}
