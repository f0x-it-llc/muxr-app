//! `connect` domain repository contracts — the seam `presentation`/use cases
//! depend on and `data` implements.
//!
//! Two contracts, one per store of the two-store split ([`super::entities`]):
//! [`SavedServersRepository`] is the metadata list, [`CredentialService`] the
//! biometric-gated secret store. Only a `host:port` key joins them; no
//! implementation of either may reach the other.
//!
//! The metadata store is synchronous (an in-memory mirror needs no async
//! ceremony); [`CredentialService`] is async because every gated call blocks on
//! a system prompt.

use super::entities::{
    CredentialStatus, SavedServer, StoreRequest, UnlockRequest, UnlockedCredential, UnlockedKeyring,
};
use crate::failure::AppFailure;

/// The saved-server store the Connect screen reads and edits. Consumed as
/// `Arc<dyn .. + Send + Sync>` so the hosting controller stays `Send + Sync` as
/// `use_controller` requires.
pub trait SavedServersRepository {
    /// Most-recently-used first.
    fn list(&self) -> Result<Vec<SavedServer>, AppFailure>;

    /// Inserts `server`, replacing any existing row with the same endpoint.
    fn save(&self, server: SavedServer) -> Result<(), AppFailure>;

    /// Removing an absent endpoint succeeds — the post-condition already holds.
    fn remove(&self, endpoint: &str) -> Result<(), AppFailure>;
}

/// The per-server auth-token secret, gated by the device's biometric prompt:
/// the store is touched only once the gate passes, with no ungated read and no
/// plain-storage fallback anywhere in this app.
///
/// Every method is `async` because a gated call blocks on a system prompt — the
/// production impl puts each blocking call on `frust::spawn_blocking`.
/// [`Self::delete`] runs no gate, so its failures are plain storage failures,
/// not a [`CredentialStatus`].
///
/// [`CredentialStatus`]: super::entities::CredentialStatus
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait CredentialService {
    /// Gates, then stores `request`'s token — authenticating first guarantees
    /// the secret is unlockable later.
    async fn store(&self, request: StoreRequest) -> CredentialStatus;

    /// Legacy single-key read (pre-keyring): gates, then reads the token for
    /// `request`'s server key; nothing stored
    /// after a passed gate answers
    /// [`NotFound`](super::entities::CredentialStatus::NotFound).
    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential;

    /// Gates once, then reads the whole keyring. `endpoints` is the saved list's
    /// endpoints, used to find legacy per-server entries to migrate.
    async fn unlock_all(&self, reason: &str, endpoints: &[String]) -> UnlockedKeyring;

    /// Drops the credential for `server_key`. No gate — forgetting a secret
    /// isn't using it, and deleting an absent key succeeds.
    async fn delete(&self, server_key: String) -> Result<(), AppFailure>;
}
