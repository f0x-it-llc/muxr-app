//! `connect` domain repository contracts — the seam `presentation`/use cases
//! depend on and `data` implements.
//!
//! Two contracts, one per store of the two-store split ([`super::entities`]):
//! [`SavedServersRepository`] is the *metadata* list, [`CredentialService`] the
//! biometric-gated *secret* store behind it. Nothing joins them but a `host:port`
//! key, and no implementation of either may reach the other.
//!
//! **The metadata list is synchronous on purpose**: an async method would force the
//! controller onto `ControllerCore::run` + `spawn_local`, and pump-poll every
//! headless test, for a store whose reads are an in-memory mirror and whose writes
//! are fire-and-forget. [`CredentialService`] is async for the mirror-image reason:
//! every gated call blocks on a system prompt, so the seam has to be one a UI
//! caller cannot reach without awaiting.

use super::entities::{
    CredentialStatus, SavedServer, StoreRequest, UnlockRequest, UnlockedCredential,
};
use crate::failure::AppFailure;

/// The saved-server store the Connect screen lists, refills its form from, and
/// removes rows from. Consumed as an `Arc<dyn .. + Send + Sync>`, so nothing
/// carries a repository type parameter and the hosting controller stays
/// `Send + Sync` as `use_controller` requires.
pub trait SavedServersRepository {
    /// Every saved server, most-recently-used first.
    fn list(&self) -> Result<Vec<SavedServer>, AppFailure>;

    /// Insert `server`, or replace the existing row with the same
    /// [`SavedServer::endpoint`].
    fn save(&self, server: SavedServer) -> Result<(), AppFailure>;

    /// Drop the row whose [`SavedServer::endpoint`] is `endpoint`. Removing an
    /// endpoint that isn't stored succeeds — the post-condition already holds.
    fn remove(&self, endpoint: &str) -> Result<(), AppFailure>;
}

/// The per-server auth-token secret, behind the device's biometric gate: a gate
/// composed with a store, where the store is touched **only** once the gate has
/// passed. No ungated read exists anywhere in this app and there is no
/// plain-storage fallback.
///
/// **Every method is `async` structurally, not stylistically**: a gated call blocks
/// the calling thread until the user answers, and the plugin has no UI-thread guard
/// — it would simply freeze, silently. Async means a UI caller can only reach it
/// through an `await`, behind which the production implementation puts every
/// blocking call on `frust::spawn_blocking`.
///
/// **Infallible by construction, except the delete**: every error the gate or store
/// can raise *is* a [`CredentialStatus`]. [`Self::delete`] runs no gate, so a
/// failure there is a plain storage failure with no status to name it.
///
/// [`CredentialStatus`]: super::entities::CredentialStatus
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait CredentialService {
    /// Gate, then persist `request`'s token under its server key. Authenticating
    /// *first* proves the gate works on this device, so a stored secret is never
    /// one the user turns out to be unable to unlock.
    async fn store(&self, request: StoreRequest) -> CredentialStatus;

    /// Gate, then read the token stored for `request`'s server key. A non-success
    /// gate never touches the store; a passed gate with nothing stored answers
    /// [`NotFound`](super::entities::CredentialStatus::NotFound).
    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential;

    /// Drop the stored credential for `server_key`. **No gate**: forgetting a
    /// secret is not a use of it, and a gate here would leave a user whose
    /// enrollment changed unable to clean up. Deleting an absent key succeeds.
    async fn delete(&self, server_key: String) -> Result<(), AppFailure>;
}
