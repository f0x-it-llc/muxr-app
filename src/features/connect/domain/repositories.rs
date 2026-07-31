//! `connect` domain repository contract(s) — the seam `presentation`/use cases
//! depend on and `data` implements (framework-free, per
//! [`crate::features::connect::domain`]'s module doc).
//!
//! [`SavedServersRepository`] is the one contract today: the saved-server list
//! the Connect screen renders and refills its form from.
//!
//! # Why it is synchronous — and how an async swap stays additive
//!
//! Every method returns `Result<_, AppFailure>` even though the only
//! implementation (`crate::features::connect::data::repositories::MockSavedServersRepository`)
//! is in-memory and cannot fail: the *fallibility* is the part that would be
//! painful to retrofit, so it is here from the start and the one call site
//! already has a failure arm wired to a toast. The `async` half is deliberately
//! **not** here yet — a `clean_signals::async_trait` method would force the
//! controller onto `ControllerCore::run` + `spawn_local`, and pump-poll every
//! headless test, for a repository with no I/O. Swapping to `async fn` later
//! changes these three signatures and their call sites; it changes no *shape* —
//! the error type, the entity, and the method set all stay put.

use super::entities::SavedServer;
use crate::failure::AppFailure;

/// The saved-server store: what the Connect screen lists, what a future
/// "remember this server" write persists, and what a later manage-servers
/// affordance removes from.
///
/// Consumed as `Arc<dyn SavedServersRepository + Send + Sync>` (the
/// `examples/huddle` DI convention), so neither the controller nor any use case
/// carries a repository type parameter. `Send + Sync` because
/// `clean_signals_frust::use_controller` requires the hosting controller to be
/// both.
pub trait SavedServersRepository {
    /// Every saved server, most-recently-used first.
    fn list(&self) -> Result<Vec<SavedServer>, AppFailure>;

    /// Insert `server`, or replace the existing row with the same
    /// [`SavedServer::endpoint`].
    fn save(&self, server: SavedServer) -> Result<(), AppFailure>;

    /// Drop the row whose [`SavedServer::endpoint`] is `endpoint`. Removing an
    /// endpoint that isn't stored succeeds — the post-condition ("it is gone")
    /// already holds.
    fn remove(&self, endpoint: &str) -> Result<(), AppFailure>;
}
