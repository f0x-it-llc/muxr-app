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
use super::keyring::Keyring;
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
    /// The locked save: behind one gate, reads the vault, adds `request`'s
    /// token and writes the vault back. A vault that does not decode answers
    /// [`Failed`](super::entities::CredentialStatus::Failed) and is left as is.
    async fn store(&self, request: StoreRequest) -> CredentialStatus;

    /// Single-endpoint read behind one gate: a not-yet-migrated per-server
    /// entry when one exists, otherwise the vault's entry; nothing stored
    /// answers [`NotFound`](super::entities::CredentialStatus::NotFound).
    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential;

    /// Gates once, then reads the whole keyring. `endpoints` is the saved list's
    /// endpoints: their legacy per-server entries are folded into the vault,
    /// and vault entries for any other endpoint are dropped.
    async fn unlock_all(&self, reason: &str, endpoints: &[String]) -> UnlockedKeyring;

    /// The unlocked save: writes `keyring` plus `request`'s token as the new
    /// vault behind one gate, and inserts the token into `keyring` only once
    /// that write succeeded.
    async fn store_in_vault(
        &self,
        request: StoreRequest,
        keyring: &mut Keyring,
    ) -> CredentialStatus;

    /// The unlocked delete: rewrites the vault without `server_key` behind one
    /// gate (skipped when `keyring` never held it), then drops it from
    /// `keyring` and removes its legacy entry. A refused rewrite is an `Err`
    /// that leaves `keyring` and the legacy entry untouched.
    async fn delete_from_vault(
        &self,
        server_key: String,
        keyring: &mut Keyring,
    ) -> Result<(), AppFailure>;

    /// The locked delete: drops `server_key`'s legacy entry only. No gate —
    /// forgetting a secret isn't using it, and deleting an absent key
    /// succeeds. The vault cannot be rewritten without a gate, so a vault
    /// entry for a still-saved endpoint stays until [`Self::delete_from_vault`]
    /// removes it, or until its saved row is removed and the next
    /// [`Self::unlock_all`] prunes it.
    async fn delete(&self, server_key: String) -> Result<(), AppFailure>;

    /// Removes the whole vault, ungated; an absent vault succeeds. The
    /// recovery for a vault [`Self::unlock_all`] can never read again —
    /// [`KeyInvalidated`](super::entities::CredentialStatus::KeyInvalidated)
    /// after a biometric re-enrollment, or
    /// [`Failed`](super::entities::CredentialStatus::Failed) for a blob that
    /// no longer decodes — which presentation offers on either status. Legacy
    /// per-server entries are left for the next `unlock_all` to migrate.
    async fn reset_vault(&self) -> Result<(), AppFailure>;
}
