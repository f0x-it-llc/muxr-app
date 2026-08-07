//! [`SecureCredentialStore`] — the real
//! [`CredentialService`]: one auth-token secret per server, in a
//! `frust-secure-storage` store opened behind the device's biometric gate.
//!
//! # THE THREAD RULE
//!
//! **Every gated call in this module blocks the calling thread** until the user
//! answers the system prompt, and `frust-secure-storage` has **no UI-thread
//! guard** — unlike `frust-camera`, it will not fail fast with a typed error if
//! it is called on the frame loop. It will simply park there, and the app
//! freezes for as long as the prompt is up, with nothing in any log to say why.
//!
//! So: **no gated call is made anywhere but inside the
//! [`frust::spawn_blocking`] closures below** ([`gated`] is the single
//! chokepoint every one of them goes through), and the store handle is opened
//! *inside* that closure too — a gated `open_with` is itself a call that can
//! block or prompt. The seam above this file is async
//! ([`CredentialService`]'s own docs) precisely so a UI caller cannot reach any
//! of it without an `await`; this file is what makes that await mean "off the
//! UI thread".
//!
//! # The store, and its key
//!
//! One named store ([`STORE_NAME`]) holding one entry per server, keyed
//! `zelli_cred_<host>_<port>` — the Flutter client's own key shape
//! (`credential_store.dart`), sanitized the same way (`:` and `/` become `_`)
//! so a key can never carry a path or namespace separator into a platform
//! store. Name parity, not on-disk compatibility: each backend has its own
//! encoding, and this app never reads what the Flutter client wrote.
//!
//! The store is opened with [`AuthPolicy::Required`] and
//! [`AuthOptions::allow_device_credential`] — the device PIN/pattern/passcode
//! satisfies the gate as well as a biometric, matching the Flutter gate's
//! `biometricOnly: false` — and with the plugin's safe
//! `invalidate_on_enrollment` default, so a re-enrollment surfaces as
//! [`CredentialStatus::KeyInvalidated`] rather than silently trusting a new
//! fingerprint.
//!
//! **[`CredentialService::delete`] is the one ungated call** and opens the same
//! store *without* the policy: removing an entry is not a use of it (both
//! platform backends delete by key with no authentication), and requiring a
//! gate to forget a secret would strand a user whose enrollment just changed —
//! exactly the case that most needs a cleanup path.
//!
//! # Desktop
//!
//! A gated open on Linux/Windows/macOS-preview answers
//! [`Unavailability::UnsupportedPlatform`] by design, which maps to
//! [`CredentialStatus::Unavailable`] here and reaches the user as "Biometrics
//! are not available on this device". Nothing degrades to plain storage: a
//! desktop preview simply logs in by hand.

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

/// The secure store this app's credentials live in. The plugin namespaces it
/// per store name (`frust.ss.<name>`), so this one name is the whole isolation
/// boundary against any other library's entries.
const STORE_NAME: &str = "muxr.credentials";

/// The per-server key's prefix — the Flutter client's own
/// (`credential_store.dart`), kept so the two clients' stores describe the same
/// thing by the same name.
const KEY_PREFIX: &str = "zelli_cred_";

/// The prompt's negative-button label. Android shows it only when the
/// device-credential fallback is unavailable (its helper makes the two mutually
/// exclusive); iOS provides its own Cancel affordance and ignores this.
const NEGATIVE_BUTTON: &str = "Cancel";

/// The prompt's subtitle — Android renders it under the title; iOS ignores it.
const PROMPT_SUBTITLE: &str = "muxr";

/// What [`AppFailure`] a failed *delete* reports. A delete runs no gate, so the
/// only thing that can go wrong is the store itself.
const DELETE_FAILED: &str = "the saved credential could not be removed";

/// The real [`CredentialService`]. See the [module docs](self).
///
/// Stateless: every call opens its own store handle inside its own blocking
/// task (see the thread rule above), so there is no handle here for a gated
/// call to be made through from the wrong thread.
pub struct SecureCredentialStore;

impl SecureCredentialStore {
    /// The production credential service.
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
            // `token` is dropped — and so scrubbed — at the end of this
            // closure, on the blocking pool, whatever the write answered.
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
            // The plugin hands back a plain `String`; it is wrapped the moment
            // it arrives so every copy this app owns scrubs on drop. The
            // plugin's own buffer is not addressable from here — the same
            // boundary `crate::connection`'s token-hygiene note records for
            // tonic's request frame.
            Ok(Some(token)) => UnlockedCredential::recovered(Zeroizing::new(token)),
            Ok(None) => UnlockedCredential::refused(CredentialStatus::NotFound),
            Err(err) => UnlockedCredential::refused(status_of(&err)),
        }
    }

    async fn delete(&self, server_key: String) -> Result<(), AppFailure> {
        let key = storage_key(&server_key);
        // Ungated (see the module docs), but still off the UI thread: a
        // Keychain/Keystore write is I/O whichever policy opened it.
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

/// Run `work` — every call in it a potentially blocking one — on the blocking
/// pool, and await the answer.
///
/// **The one chokepoint.** A gated `get`/`set`, and the gated `open_with` that
/// precedes it, may only be reached through here; see the [module docs](self)'
/// thread rule. A join failure (the pool shutting down under a task) is
/// reported as a storage error, which maps to
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

/// Open the credential store **behind the gate**, with `reason` as the system
/// prompt's rationale.
///
/// Callable only from inside a [`gated`] closure: opening a gated store is
/// itself a platform call that can block (Android probes its API level and
/// biometric helper here, and refuses without one).
fn open_gated(reason: &str) -> Result<SecureStorage, SecureStorageError> {
    // Both `AuthOptions` and the types under it are `#[non_exhaustive]`, so a
    // struct literal is not available outside the plugin: each is built from
    // its own `Default` and the fields this app cares about are set on it.
    let mut prompt = PromptSpec::default();
    prompt.title = reason.to_string();
    prompt.subtitle = PROMPT_SUBTITLE.to_string();
    prompt.negative_button = NEGATIVE_BUTTON.to_string();

    let mut auth = AuthOptions::default();
    // The device PIN/pattern/passcode satisfies the gate too — the Flutter
    // gate's `biometricOnly: false`, and the only reason a user with a working
    // passcode but a broken sensor can still reach their own credential.
    auth.allow_device_credential = true;
    auth.prompt = prompt;
    // `invalidate_on_enrollment` and `validity` keep the plugin's own safe
    // defaults: a re-enrollment invalidates the key (surfacing as
    // `KeyInvalidated`) and every gated call re-prompts.

    let mut options = StoreOptions::default();
    options.auth = AuthPolicy::Required(auth);
    SecureStorage::open_with(STORE_NAME, options)
}

/// The store key for `server_key` (a `host:port`): the Flutter client's
/// `zelli_cred_<serverKey with [:/] → _>`.
///
/// Sanitizing is not cosmetic — a raw `:` or `/` in a key crosses into a
/// platform store's own namespace syntax, and an IPv6 literal is full of
/// colons.
fn storage_key(server_key: &str) -> String {
    let sanitized: String = server_key
        .chars()
        .map(|ch| if ch == ':' || ch == '/' { '_' } else { ch })
        .collect();
    format!("{KEY_PREFIX}{sanitized}")
}

/// The [`CredentialStatus`] a plugin error reports as — the whole gate→UX
/// mapping, in one place.
///
/// `SecureStorageError` is `#[non_exhaustive]`: a variant this build has never
/// heard of reads as [`CredentialStatus::Failed`], the arm that says "it did not
/// work, log in by hand", rather than as a compile break or a guess.
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
        // Every other unavailability — no hardware, hardware busy, an OS too
        // old, a build missing the Android helper class, a platform with no
        // biometric primitive at all — is one fact to the user: this device
        // cannot do it.
        SecureStorageError::NotAvailable(_) => CredentialStatus::Unavailable,
        // The Android platform handles were never installed, so the store
        // cannot be reached at all on this run: an availability fact, not an
        // authentication one.
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
        // An IPv6 endpoint is the case that matters: colons everywhere.
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
        // Every desktop build opens gated and is refused right there; the user
        // must be told the device cannot do this, not that they failed a check.
        let desktop = SecureStorageError::NotAvailable(Unavailability::UnsupportedPlatform);
        assert_eq!(status_of(&desktop), CredentialStatus::Unavailable);
        assert!(CredentialStatus::Unavailable.message().is_some());
    }
}
