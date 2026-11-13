//! Repository contracts: [`SavedServersRepository`] is the metadata list,
//! [`CredentialService`] the biometric-gated secret store. Only a `host:port`
//! key joins them; neither implementation may reach the other.

use super::entities::{
    CredentialStatus, SavedServer, StoreRequest, UnlockRequest, UnlockedCredential, UnlockedKeyring,
};
use super::keyring::Keyring;
use crate::failure::AppFailure;

pub trait SavedServersRepository {
    /// Most-recently-used first.
    fn list(&self) -> Result<Vec<SavedServer>, AppFailure>;

    /// Inserts `server`, replacing any existing row with the same endpoint.
    fn save(&self, server: SavedServer) -> Result<(), AppFailure>;

    /// Removing an absent endpoint succeeds — the post-condition already holds.
    fn remove(&self, endpoint: &str) -> Result<(), AppFailure>;

    /// Whether this is the device's persisted list: a keyring unlock prunes the vault to it.
    fn is_persistent(&self) -> bool;
}

/// The auth-token secret, gated by the biometric prompt: no ungated read and no
/// plain-storage fallback. [`Self::delete`] runs no gate, so its failures are
/// plain storage failures.
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait CredentialService {
    /// Behind one gate, adds `request`'s token to the vault. An undecodable vault
    /// answers `Failed` and is left as is.
    async fn store(&self, request: StoreRequest) -> CredentialStatus;

    /// One gate: a not-yet-migrated per-server entry wins over the vault's entry.
    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential;

    /// Gates once. Legacy per-server entries of `endpoints` are folded into the
    /// vault; vault entries for any other endpoint are dropped.
    async fn unlock_all(&self, reason: &str, endpoints: &[String]) -> UnlockedKeyring;

    /// Writes `keyring` plus the token behind one gate; `keyring` gains the token
    /// only once that write succeeded.
    async fn store_in_vault(
        &self,
        request: StoreRequest,
        keyring: &mut Keyring,
    ) -> CredentialStatus;

    /// Rewrites the vault without `server_key` behind one gate (skipped when
    /// `keyring` never held it), then drops the legacy entry. A refused rewrite
    /// is an `Err` that leaves `keyring` and the legacy entry untouched.
    async fn delete_from_vault(
        &self,
        server_key: String,
        keyring: &mut Keyring,
    ) -> Result<(), AppFailure>;

    /// Drops the legacy entry only, ungated; an absent key succeeds. A vault
    /// entry stays until [`Self::delete_from_vault`] or the next
    /// [`Self::unlock_all`] prunes it.
    async fn delete(&self, server_key: String) -> Result<(), AppFailure>;

    /// Removes the whole vault, ungated; an absent vault succeeds. Recovery for a
    /// vault `unlock_all` cannot read (`KeyInvalidated`, `Failed`). Legacy
    /// entries are left for the next `unlock_all` to migrate.
    async fn reset_vault(&self) -> Result<(), AppFailure>;
}
