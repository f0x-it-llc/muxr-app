//! `sessions` domain repository contract — the seam this feature's use cases
//! depend on and `data` implements (framework-free, per
//! `crate::features::sessions::domain`'s module doc).
//!
//! [`SessionsRepository`] is the whole contract: list, create, rename, kill —
//! exactly the four operations `docs/redesign.html`'s sessions block affords,
//! and nothing speculative.
//!
//! # Why it is already async
//!
//! Today's only implementation is in-memory (`data`'s
//! `MockSessionsRepository`), which needs no `await` at all. The trait is
//! `async` regardless so the real `muxrd` gRPC client can be dropped in behind
//! it **additively** — the controller, its use cases and the screen already
//! drive every call through `ControllerCore::run`'s async path, so the swap
//! changes one `impl` block and nothing above it. The dual `cfg_attr` is the
//! same pair every async trait in this codebase and `examples/huddle` uses
//! (`wasm32`'s single-threaded event loop can't require `Send` futures).
//!
//! Consumed as `Arc<dyn SessionsRepository + Send + Sync>` (huddle's DI
//! convention), so neither the use cases nor the controller carry a repository
//! type parameter.
//!
//! # Failure mapping
//!
//! Every method fails with the app-wide [`AppFailure`] vocabulary — the data
//! layer owns the single mapping site into it, so nothing above ever sees a
//! transport error type.

use crate::failure::AppFailure;
use crate::features::sessions::domain::entities::{RenameRequest, Session};

/// Read/write access to the connected server's session list.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait SessionsRepository {
    /// Every session the connected server currently knows about, in display
    /// order (the daemon's own ordering — this layer never re-sorts).
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure>;

    /// Create a session named `name`, returning it as stored (with the id and
    /// creation label the server minted).
    async fn create_session(&self, name: String) -> Result<Session, AppFailure>;

    /// Rename an existing session, returning it as stored.
    async fn rename_session(&self, request: RenameRequest) -> Result<Session, AppFailure>;

    /// Kill the session with `id`, disconnecting any attached client.
    async fn kill_session(&self, id: String) -> Result<(), AppFailure>;
}
