//! [`SecureCredentialStore`] — the real
//! [`CredentialService`]: one auth-token secret per server, in a
//! `frust-secure-storage` store opened behind the device's biometric gate.
//!
//! # THE THREAD RULE
//!
//! Every gated call here blocks the calling thread until the user answers the
//! system prompt, and `frust-secure-storage` has no UI-thread guard: called on
//! the frame loop it simply parks, freezing the app with nothing in any log to
//! say why. So no gated call is made anywhere but inside the
//! [`frust::spawn_blocking`] closures below — [`gated`] is the single
//! chokepoint — and the store handle is opened *inside* that closure too,
//! since a gated `open_with` can itself block or prompt.
//!
//! # The store, and its key
//!
//! One named store ([`STORE_NAME`]) holding one entry per server, keyed
//! `zelli_cred_<host>_<port>`, sanitized so a key can never carry a path or
//! namespace separator into a platform store. Opened with
//! [`AuthPolicy::Required`] and [`AuthOptions::allow_device_credential`] (the
//! device PIN/pattern/passcode satisfies the gate too) and the plugin's safe
//! `invalidate_on_enrollment` default, so a re-enrollment surfaces as
//! [`CredentialStatus::KeyInvalidated`] rather than silently trusting a new
//! fingerprint.
//!
//! [`CredentialService::delete`] is the one ungated call and opens the same
//! store without the policy: removing an entry is not a use of it, and a gate
//! there would strand a user whose enrollment just changed. On desktop a
//! gated open answers [`Unavailability::UnsupportedPlatform`], mapped to
//! [`CredentialStatus::Unavailable`] — nothing degrades to plain storage.

use frust::spawn_blocking;
use frust_secure_storage::{
    AuthOptions, AuthPolicy, PromptSpec, SecureStorage, SecureStorageError, StoreOptions,
    Unavailability,
};
use zeroize::Zeroizing;

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{
    CredentialStatus, StoreRequest, UnlockRequest, UnlockedCredential,
};
use crate::features::connect::domain::repositories::CredentialService;

/// The secure store this app's credentials live in. The plugin namespaces per
/// store name, so this one name is the whole isolation boundary.
const STORE_NAME: &str = "muxr.credentials";

/// The per-server key's prefix — the Flutter client's own, kept so the two
/// clients' stores describe the same thing by the same name.
const KEY_PREFIX: &str = "zelli_cred_";

/// The prompt's negative-button label. Android shows it only when the
/// device-credential fallback is unavailable; iOS ignores it.
const NEGATIVE_BUTTON: &str = "Cancel";

/// The prompt's subtitle — Android renders it under the title; iOS ignores it.
const PROMPT_SUBTITLE: &str = "muxr";

/// What a failed delete reports — it runs no gate, so the only thing that can
/// go wrong is the store itself.
const DELETE_FAILED: &str = "the saved credential could not be removed";

/// The real [`CredentialService`]. See the [module docs](self).
///
/// Stateless: every call opens its own store handle inside its own blocking
/// task, so there is no handle here for a gated call to be made from the
/// wrong thread.
pub struct SecureCredentialStore;

impl SecureCredentialStore {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SecureCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl CredentialService for SecureCredentialStore {
    async fn store(&self, request: StoreRequest) -> CredentialStatus {
        let key = storage_key(&request.server_key);
        let reason = request.reason;
        let token = request.token;
        let stored = gated(move || {
            let store = open_gated(&reason)?;
            // `token` is dropped (and scrubbed) at the end of this closure regardless.
            store.set(&key, token.as_str())
        })
        .await;
        match stored {
            Ok(()) => CredentialStatus::Success,
            Err(err) => status_of(&err),
        }
    }

    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential {
        let key = storage_key(&request.server_key);
        let reason = request.reason;
        let read = gated(move || {
            let store = open_gated(&reason)?;
            store.get(&key)
        })
        .await;
        match read {
            // Wrapped the moment it arrives so every copy this app owns scrubs on drop.
            Ok(Some(token)) => UnlockedCredential::recovered(Zeroizing::new(token)),
            Ok(None) => UnlockedCredential::refused(CredentialStatus::NotFound),
            Err(err) => UnlockedCredential::refused(status_of(&err)),
        }
    }

    async fn delete(&self, server_key: String) -> Result<(), AppFailure> {
        let key = storage_key(&server_key);
        // Ungated, but still off the UI thread — a Keychain/Keystore write is I/O either way.
        let removed = gated(move || {
            let store = SecureStorage::open(STORE_NAME)?;
            store.remove(&key)
        })
        .await;
        removed.map_err(|err| {
            log::warn!("credentials: delete failed: {err}");
            AppFailure::Storage(DELETE_FAILED.to_string())
        })
    }
}

/// Runs `work` on the blocking pool and awaits the answer.
///
/// The one chokepoint: a gated `get`/`set`, and the `open_with` preceding it,
/// may only be reached through here (the [module docs](self)' thread rule). A
/// join failure is reported as a storage error, mapped to
/// [`CredentialStatus::Failed`] — never as a success.
async fn gated<T>(
    work: impl FnOnce() -> Result<T, SecureStorageError> + Send + 'static,
) -> Result<T, SecureStorageError>
where
    T: Send + 'static,
{
    match spawn_blocking(work).await {
        Ok(result) => result,
        Err(join) => Err(SecureStorageError::Storage(join.to_string())),
    }
}

/// Opens the credential store behind the gate, with `reason` as the prompt's
/// rationale. Callable only from inside a [`gated`] closure, since opening a
/// gated store is itself a platform call that can block.
fn open_gated(reason: &str) -> Result<SecureStorage, SecureStorageError> {
    // `AuthOptions` and friends are `#[non_exhaustive]`, so each is built from
    // its own `Default` with only the fields this app cares about set.
    let mut prompt = PromptSpec::default();
    prompt.title = reason.to_string();
    prompt.subtitle = PROMPT_SUBTITLE.to_string();
    prompt.negative_button = NEGATIVE_BUTTON.to_string();

    let mut auth = AuthOptions::default();
    // Lets a user with a working passcode but a broken sensor still reach their own credential.
    auth.allow_device_credential = true;
    auth.prompt = prompt;
    // `invalidate_on_enrollment`/`validity` keep the plugin's safe defaults.

    let mut options = StoreOptions::default();
    options.auth = AuthPolicy::Required(auth);
    SecureStorage::open_with(STORE_NAME, options)
}

/// `zelli_cred_<server_key with [:/] → _>`. Sanitizing is not cosmetic — a raw
/// `:` or `/` crosses into a platform store's own namespace syntax, and an
/// IPv6 literal is full of colons.
fn storage_key(server_key: &str) -> String {
    let sanitized: String = server_key
        .chars()
        .map(|ch| if ch == ':' || ch == '/' { '_' } else { ch })
        .collect();
    format!("{KEY_PREFIX}{sanitized}")
}

/// The whole gate→UX mapping, in one place. `SecureStorageError` is
/// `#[non_exhaustive]`, so an unknown variant reads as
/// [`CredentialStatus::Failed`] rather than a compile break or a guess.
fn status_of(error: &SecureStorageError) -> CredentialStatus {
    match error {
        SecureStorageError::UserCanceled | SecureStorageError::SystemCanceled => {
            CredentialStatus::Canceled
        }
        SecureStorageError::LockoutTemporary | SecureStorageError::LockoutPermanent => {
            CredentialStatus::LockedOut
        }
        SecureStorageError::KeyInvalidated => CredentialStatus::KeyInvalidated,
        SecureStorageError::NotAvailable(Unavailability::NotEnrolled) => {
            CredentialStatus::NotEnrolled
        }
        SecureStorageError::NotAvailable(Unavailability::PasscodeNotSet) => {
            CredentialStatus::NoDeviceCredential
        }
        // Every other unavailability is one fact to the user: this device can't do it.
        SecureStorageError::NotAvailable(_) => CredentialStatus::Unavailable,
        // Platform handles never installed — an availability fact, not an authentication one.
        SecureStorageError::PlatformNotInitialized => CredentialStatus::Unavailable,
        SecureStorageError::AuthFailed
        | SecureStorageError::Storage(_)
        | SecureStorageError::Io(_) => CredentialStatus::Failed,
        _ => CredentialStatus::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_the_flutter_prefix_over_a_sanitized_endpoint() {
        assert_eq!(
            storage_key("100.71.31.57:50051"),
            "zelli_cred_100.71.31.57_50051"
        );
    }

    #[test]
    fn every_separator_a_platform_store_could_read_is_replaced() {
        assert_eq!(
            storage_key("fd00::1:50051"),
            "zelli_cred_fd00__1_50051",
            "each colon becomes its own underscore — never a collapsed run"
        );
        assert_eq!(storage_key("a/b:1"), "zelli_cred_a_b_1");
    }

    #[test]
    fn two_different_endpoints_never_share_a_key() {
        assert_ne!(storage_key("rig:50051"), storage_key("rig:50052"));
    }

    #[test]
    fn every_gate_error_maps_onto_its_own_branch_of_the_outcome_table() {
        let cases = [
            (SecureStorageError::UserCanceled, CredentialStatus::Canceled),
            (
                SecureStorageError::SystemCanceled,
                CredentialStatus::Canceled,
            ),
            (
                SecureStorageError::NotAvailable(Unavailability::NotEnrolled),
                CredentialStatus::NotEnrolled,
            ),
            (
                SecureStorageError::NotAvailable(Unavailability::PasscodeNotSet),
                CredentialStatus::NoDeviceCredential,
            ),
            (
                SecureStorageError::LockoutTemporary,
                CredentialStatus::LockedOut,
            ),
            (
                SecureStorageError::LockoutPermanent,
                CredentialStatus::LockedOut,
            ),
            (
                SecureStorageError::NotAvailable(Unavailability::NoHardware),
                CredentialStatus::Unavailable,
            ),
            (
                SecureStorageError::NotAvailable(Unavailability::HardwareUnavailable),
                CredentialStatus::Unavailable,
            ),
            (
                SecureStorageError::NotAvailable(Unavailability::UnsupportedPlatform),
                CredentialStatus::Unavailable,
            ),
            (
                SecureStorageError::NotAvailable(Unavailability::UnsupportedApiLevel),
                CredentialStatus::Unavailable,
            ),
            (
                SecureStorageError::NotAvailable(Unavailability::HelperMissing),
                CredentialStatus::Unavailable,
            ),
            (
                SecureStorageError::PlatformNotInitialized,
                CredentialStatus::Unavailable,
            ),
            (
                SecureStorageError::KeyInvalidated,
                CredentialStatus::KeyInvalidated,
            ),
            (SecureStorageError::AuthFailed, CredentialStatus::Failed),
            (
                SecureStorageError::Storage("boom".to_string()),
                CredentialStatus::Failed,
            ),
        ];
        for (error, expected) in cases {
            assert_eq!(status_of(&error), expected, "{error:?}");
        }
    }

    #[test]
    fn the_desktop_refusal_is_the_unavailable_branch_rather_than_a_failure() {
        let desktop = SecureStorageError::NotAvailable(Unavailability::UnsupportedPlatform);
        assert_eq!(status_of(&desktop), CredentialStatus::Unavailable);
        assert!(CredentialStatus::Unavailable.message().is_some());
    }
}
