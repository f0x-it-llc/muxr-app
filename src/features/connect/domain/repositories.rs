//! `connect` domain repository contract(s) — the seam `presentation`/use cases
//! depend on and `data` implements (framework-free, per
//! [`crate::features::connect::domain`]'s module doc).
//!
//! Two contracts, one per store of the two-store split
//! ([`super::entities`]'s module docs): [`SavedServersRepository`] is the
//! saved-server *metadata* list the Connect screen renders and refills its form
//! from, and [`CredentialService`] is the biometric-gated *secret* store behind
//! it. Nothing joins them but a `host:port` key, and no implementation of
//! either may reach the other.
//!
//! # Why the saved-server list is synchronous — and how an async swap stays additive
//!
//! Every method returns `Result<_, AppFailure>`: the real implementation
//! (`crate::features::connect::data::repositories::PrefsSavedServersRepository`)
//! reads a device store that can refuse, and the one call site already has a
//! failure arm wired to a toast. The `async` half is deliberately **not** here
//! — a `clean_signals::async_trait` method would force the controller onto
//! `ControllerCore::run` + `spawn_local`, and pump-poll every headless test, for
//! a store whose reads are an in-memory mirror and whose writes are
//! fire-and-forget (that implementation's own module docs).
//!
//! [`CredentialService`] is async for the mirror-image reason: **every one of
//! its gated calls blocks on a system biometric prompt**, so the seam has to be
//! one a UI caller cannot reach without awaiting.

use super::entities::{
    CredentialStatus, SavedServer, StoreRequest, UnlockRequest, UnlockedCredential,
};
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

/// The per-server auth-token secret, behind the device's biometric gate.
///
/// The one API the connect and settings layers use to read, write or drop a
/// stored credential — the Flutter client's `SecureCredentialService`, whose
/// shape this keeps: a gate composed with a store, where the store is touched
/// **only** once the gate has passed. There is no ungated read anywhere in this
/// app, and no plain-storage fallback: a refused gate means the secret stays
/// unread, never that it is fetched some other way.
///
/// # Why every method is `async`
///
/// A gated call **blocks the calling thread** until the user answers the system
/// prompt, and the underlying plugin has no UI-thread guard to catch a caller
/// that runs one on the frame loop — it would simply freeze, silently, for as
/// long as the prompt is up. Making this seam async is what structurally
/// prevents that: a UI caller can only reach it through an `await`, and the one
/// production implementation
/// ([`data::SecureCredentialStore`](crate::features::connect::data::repositories::SecureCredentialStore))
/// puts every blocking call on `frust::spawn_blocking` behind that await. The
/// discipline is otherwise unenforced, which is why it is stated here, at the
/// seam, as well as at the implementation.
///
/// Consumed as `Arc<dyn CredentialService + Send + Sync>` (the
/// `examples/huddle` DI convention), like every other repository handle here.
///
/// # Infallible by construction, except the delete
///
/// [`Self::store`]/[`Self::unlock`] answer with a [`CredentialStatus`] rather
/// than a `Result`: every error the gate or the store can raise — a cancel, a
/// lockout, a missing enrollment, an invalidated key, a backend failure — *is*
/// one of those statuses, so a second failure channel would only ever carry a
/// duplicate of one arm. [`Self::delete`] is the exception: it runs no gate, so
/// a failure there is a plain storage failure with no status to name it.
///
/// [`CredentialStatus`]: super::entities::CredentialStatus
#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
pub trait CredentialService {
    /// Gate, then persist `request`'s token under its server key.
    ///
    /// Authenticating *first* is deliberate (Flutter's own note): it proves the
    /// gate works on this device before a credential is committed, so a stored
    /// secret is never one the user turns out to be unable to unlock.
    async fn store(&self, request: StoreRequest) -> CredentialStatus;

    /// Gate, then read the token stored for `request`'s server key.
    ///
    /// A non-success gate never touches the store. A passed gate with nothing
    /// stored answers [`CredentialStatus::NotFound`](super::entities::CredentialStatus::NotFound).
    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential;

    /// Drop the stored credential for `server_key`. **No gate**: forgetting a
    /// secret is not a use of it, and requiring a biometric to remove one would
    /// leave a user whose enrollment changed unable to clean up. Deleting a key
    /// that isn't stored succeeds.
    async fn delete(&self, server_key: String) -> Result<(), AppFailure>;
}
