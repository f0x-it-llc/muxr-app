//! [`SecureCredentialStore`] — the real
//! [`CredentialService`]: every server's auth token in one vault entry of a
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
//! # The store, the vault, and the legacy keys
//!
//! One named store ([`STORE_NAME`]) holds one vault entry ([`VAULT_KEY`]): the
//! [`KeyringVault`] JSON of every saved server's token, so unlocking them all
//! costs one gated read (Android prompts per gated `get` and per `set`; Apple
//! never prompts on `set`). Earlier builds kept one entry per server, keyed
//! `zelli_cred_<host>_<port>` ([`storage_key`]); [`CredentialService::unlock_all`]
//! folds those into the vault and removes them, and every vault write removes
//! the written endpoint's superseded legacy entry. Opened with
//! [`AuthPolicy::Required`] and [`AuthOptions::allow_device_credential`] and
//! the plugin's safe `invalidate_on_enrollment` default, so a re-enrollment
//! surfaces as [`CredentialStatus::KeyInvalidated`].
//!
//! [`CredentialService::delete`] and [`CredentialService::reset_vault`] are the
//! ungated calls. `delete` removes the legacy entry only, since the vault
//! cannot be rewritten without a gate: a vault entry for a still-saved
//! endpoint stays until `delete_from_vault` removes it, or until the saved row
//! is removed and the next `unlock_all` prunes it. `reset_vault` removes the
//! whole vault — the only way out of one that answers `KeyInvalidated` after a
//! re-enrollment or no longer decodes. On desktop a gated open answers
//! [`Unavailability::UnsupportedPlatform`], mapped to
//! [`CredentialStatus::Unavailable`] — nothing degrades to plain storage.
//!
//! The vault algorithms run over [`VaultStore`], so the in-memory mock walks
//! exactly the same steps.

use frust::spawn_blocking;
use frust_secure_storage::{
    AuthOptions, AuthPolicy, PromptSpec, SecureStorage, SecureStorageError, StoreOptions,
    Unavailability,
};
use zeroize::Zeroizing;

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{
    CredentialStatus, StoreRequest, UnlockRequest, UnlockedCredential, UnlockedKeyring,
    epoch_millis,
};
use crate::features::connect::domain::keyring::{Keyring, KeyringVault};
use crate::features::connect::domain::repositories::CredentialService;

/// The secure store this app's credentials live in. The plugin namespaces per
/// store name, so this one name is the whole isolation boundary.
const STORE_NAME: &str = "muxr.credentials";

/// The single entry holding every saved server's token.
pub(super) const VAULT_KEY: &str = "muxr_keyring";

/// The legacy per-server key's prefix — the Flutter client's own.
const KEY_PREFIX: &str = "zelli_cred_";

/// The prompt's negative-button label. Android shows it only when the
/// device-credential fallback is unavailable; iOS ignores it.
const NEGATIVE_BUTTON: &str = "Cancel";

/// The prompt's subtitle — Android renders it under the title; iOS ignores it.
const PROMPT_SUBTITLE: &str = "muxr";

/// The prompt title of the vault rewrite an unlocked delete needs.
pub(super) const FORGET_REASON: &str = "Forget the saved credential";

/// What a failed delete reports.
pub(super) const DELETE_FAILED: &str = "the saved credential could not be removed";

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
        let StoreRequest {
            server_key,
            token,
            reason,
        } = request;
        // `token` is dropped (and scrubbed) at the end of this closure regardless.
        let stored = gated(move || {
            let store = open_gated(&reason)?;
            Ok(store_locked(&store, server_key, token))
        })
        .await;
        status_from(stored)
    }

    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential {
        let UnlockRequest { server_key, reason } = request;
        let read = gated(move || {
            let store = open_gated(&reason)?;
            Ok(read_one(&store, &server_key))
        })
        .await;
        match read {
            Ok(Ok(Some(token))) => UnlockedCredential::recovered(token),
            Ok(Ok(None)) => UnlockedCredential::refused(CredentialStatus::NotFound),
            Ok(Err(status)) => UnlockedCredential::refused(status),
            Err(err) => UnlockedCredential::refused(status_of(&err)),
        }
    }

    async fn unlock_all(&self, reason: &str, endpoints: &[String]) -> UnlockedKeyring {
        let reason = reason.to_string();
        let endpoints = endpoints.to_vec();
        let read = gated(move || {
            let store = open_gated(&reason)?;
            Ok(unlock_vault(&store, &endpoints))
        })
        .await;
        match read {
            Ok(Ok(vault)) => vault.into_unlocked(),
            Ok(Err(status)) => UnlockedKeyring::refused(status),
            Err(err) => UnlockedKeyring::refused(status_of(&err)),
        }
    }

    async fn store_in_vault(
        &self,
        request: StoreRequest,
        keyring: &mut Keyring,
    ) -> CredentialStatus {
        let StoreRequest {
            server_key,
            token,
            reason,
        } = request;
        let encoded = next_vault(keyring, &server_key, &token);
        let legacy = storage_key(&server_key);
        let logged = server_key.clone();
        // Only the encoded blob crosses into the blocking task.
        let written = gated(move || {
            let store = open_gated(&reason)?;
            Ok(write_vault(
                &store,
                &encoded,
                Some(&legacy),
                VaultWrite::Store(&logged),
            ))
        })
        .await;
        let status = status_from(written);
        if status == CredentialStatus::Success {
            keyring.insert(server_key, token);
        }
        status
    }

    async fn delete_from_vault(
        &self,
        server_key: String,
        keyring: &mut Keyring,
    ) -> Result<(), AppFailure> {
        if keyring.token_for(&server_key).is_some() {
            let encoded = vault_without(keyring, &server_key);
            let evicted = server_key.clone();
            let written = gated(move || {
                let store = open_gated(FORGET_REASON)?;
                Ok(write_vault(
                    &store,
                    &encoded,
                    None,
                    VaultWrite::Evict(&evicted),
                ))
            })
            .await;
            refused_rewrite(status_from(written))?;
            keyring.remove(&server_key);
        }
        self.delete(server_key).await
    }

    async fn delete(&self, server_key: String) -> Result<(), AppFailure> {
        remove_ungated(storage_key(&server_key)).await
    }

    async fn reset_vault(&self) -> Result<(), AppFailure> {
        remove_ungated(VAULT_KEY.to_string()).await
    }
}

/// Removes `key` without a gate — still off the UI thread, since a
/// Keychain/Keystore write is I/O either way. An absent key succeeds.
async fn remove_ungated(key: String) -> Result<(), AppFailure> {
    let removed = gated(move || {
        let store = SecureStorage::open(STORE_NAME)?;
        store.remove(&key)
    })
    .await;
    removed.map_err(|err| {
        log::warn!("credentials: an ungated remove failed: {err}");
        AppFailure::Storage(DELETE_FAILED.to_string())
    })
}

/// The four store operations the vault algorithms use. `get`/`set` are the
/// gated ones (each may prompt); `contains`/`remove` never prompt. A failure
/// arrives already mapped onto its non-success [`CredentialStatus`].
pub(super) trait VaultStore {
    fn get(&self, key: &str) -> Result<Option<Zeroizing<String>>, CredentialStatus>;
    fn set(&self, key: &str, value: &str) -> Result<(), CredentialStatus>;
    fn contains(&self, key: &str) -> Result<bool, CredentialStatus>;
    fn remove(&self, key: &str) -> Result<(), CredentialStatus>;
}

impl VaultStore for SecureStorage {
    fn get(&self, key: &str) -> Result<Option<Zeroizing<String>>, CredentialStatus> {
        // Wrapped the moment it arrives; the plugin's own buffer is not addressable from here.
        SecureStorage::get(self, key)
            .map(|value| value.map(Zeroizing::new))
            .map_err(|err| status_of(&err))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), CredentialStatus> {
        SecureStorage::set(self, key, value).map_err(|err| status_of(&err))
    }

    fn contains(&self, key: &str) -> Result<bool, CredentialStatus> {
        SecureStorage::contains(self, key).map_err(|err| status_of(&err))
    }

    fn remove(&self, key: &str) -> Result<(), CredentialStatus> {
        SecureStorage::remove(self, key).map_err(|err| status_of(&err))
    }
}

/// What [`unlock_vault`] recovered.
pub(super) struct UnlockedVault {
    pub keyring: Keyring,
    pub migrated: Vec<String>,
    pub unreadable: Vec<String>,
}

impl UnlockedVault {
    pub fn into_unlocked(self) -> UnlockedKeyring {
        UnlockedKeyring {
            status: CredentialStatus::Success,
            keyring: Some(self.keyring),
            migrated: self.migrated,
            unreadable: self.unreadable,
        }
    }
}

/// Reads and strictly decodes the vault (one gated `get`): no vault is an
/// empty keyring, an undecodable one [`CredentialStatus::Failed`].
pub(super) fn read_vault(store: &impl VaultStore) -> Result<Keyring, CredentialStatus> {
    log::info!("credentials: gated read of the vault");
    match store.get(VAULT_KEY)? {
        None => Ok(Keyring::new()),
        Some(raw) => KeyringVault::decode(&raw).map_err(|err| {
            log::warn!("credentials: {err}");
            CredentialStatus::Failed
        }),
    }
}

/// The `unlock_all` algorithm: the vault, then one gated read per saved
/// endpoint still holding a legacy entry. A legacy entry beside a vault entry
/// is stale (every vault write supersedes it) and is removed unread. Any
/// refusal but an invalidated legacy key aborts with nothing written or
/// removed; otherwise the vault is written once when anything was migrated or
/// pruned, and only then are legacy entries removed.
pub(super) fn unlock_vault(
    store: &impl VaultStore,
    endpoints: &[String],
) -> Result<UnlockedVault, CredentialStatus> {
    let mut keyring = read_vault(store)?;
    let mut migrated = Vec::new();
    let mut unreadable = Vec::new();
    let mut stale = Vec::new();
    for endpoint in endpoints {
        let key = storage_key(endpoint);
        if !store.contains(&key)? {
            continue;
        }
        log::info!("credentials: legacy entry present for {endpoint}");
        if keyring.token_for(endpoint).is_some() {
            stale.push(key);
            continue;
        }
        match store.get(&key) {
            Ok(Some(token)) => {
                keyring.insert(endpoint.clone(), token);
                migrated.push(endpoint.clone());
            }
            Ok(None) => {}
            Err(CredentialStatus::KeyInvalidated) => unreadable.push(endpoint.clone()),
            Err(status) => return Err(status),
        }
    }
    let mut pruned = Vec::new();
    for endpoint in keyring.endpoints() {
        if !endpoints.contains(&endpoint) && keyring.remove(&endpoint) {
            pruned.push(endpoint);
        }
    }
    if !migrated.is_empty() || !pruned.is_empty() {
        log::info!(
            "credentials: gated write (unlock) of the vault: migrated {migrated:?}, pruned {pruned:?}"
        );
        store.set(VAULT_KEY, &encode_vault(&keyring))?;
    } else {
        log::info!("credentials: unlock wrote nothing (no migration, no prune)");
    }
    for key in migrated.iter().map(|endpoint| storage_key(endpoint)) {
        remove_quietly(store, &key);
    }
    for key in &stale {
        remove_quietly(store, key);
    }
    Ok(UnlockedVault {
        keyring,
        migrated,
        unreadable,
    })
}

/// The locked save: a read-modify-write of the vault, never a legacy entry.
pub(super) fn store_locked(
    store: &impl VaultStore,
    server_key: String,
    token: Zeroizing<String>,
) -> Result<(), CredentialStatus> {
    let mut keyring = read_vault(store)?;
    let legacy = storage_key(&server_key);
    keyring.insert(server_key.clone(), token);
    write_vault(
        store,
        &encode_vault(&keyring),
        Some(&legacy),
        VaultWrite::Store(&server_key),
    )
}

/// Why a gated vault `set` happened, for the info log: the vault writes are
/// the only source of Android's per-write prompt.
#[derive(Clone, Copy)]
pub(super) enum VaultWrite<'a> {
    /// A credential for this endpoint was saved.
    Store(&'a str),
    /// A credential for this endpoint was evicted.
    Evict(&'a str),
}

/// One gated `set` of an encoded vault, then the quiet removal of the legacy
/// entry it supersedes. Logs the endpoint and the reason, never a token.
pub(super) fn write_vault(
    store: &impl VaultStore,
    encoded: &str,
    superseded: Option<&str>,
    why: VaultWrite<'_>,
) -> Result<(), CredentialStatus> {
    match why {
        VaultWrite::Store(endpoint) => {
            log::info!("credentials: gated write (store) of the vault for {endpoint}");
        }
        VaultWrite::Evict(endpoint) => {
            log::info!("credentials: gated write (evict) of the vault for {endpoint}");
        }
    }
    store.set(VAULT_KEY, encoded)?;
    if let Some(key) = superseded {
        remove_quietly(store, key);
    }
    Ok(())
}

/// One endpoint's token: its not-yet-migrated legacy entry when present
/// (`contains` is ungated), otherwise the vault's — one gated read either way.
pub(super) fn read_one(
    store: &impl VaultStore,
    server_key: &str,
) -> Result<Option<Zeroizing<String>>, CredentialStatus> {
    let legacy = storage_key(server_key);
    if store.contains(&legacy)? {
        log::info!("credentials: gated read (legacy entry) for {server_key}");
        return store.get(&legacy);
    }
    log::info!("credentials: gated read (single) for {server_key}");
    Ok(read_vault(store)?.token_for(server_key))
}

/// A legacy entry left behind is harmless: it is stale, and the next
/// `unlock_all` removes it unread.
fn remove_quietly(store: &impl VaultStore, key: &str) {
    if let Err(status) = store.remove(key) {
        log::warn!("credentials: a legacy entry was not removed: {status:?}");
    }
}

/// The vault blob for `keyring`, every entry stamped now.
pub(super) fn encode_vault(keyring: &Keyring) -> Zeroizing<String> {
    let now = epoch_millis();
    Zeroizing::new(KeyringVault::encode(keyring, |_| now))
}

/// The vault blob for `keyring` plus `token` under `server_key`, leaving
/// `keyring` itself untouched (a [`Keyring`] is deliberately not `Clone`).
pub(super) fn next_vault(
    keyring: &Keyring,
    server_key: &str,
    token: &Zeroizing<String>,
) -> Zeroizing<String> {
    let mut next = copy_keyring(keyring);
    next.insert(server_key, token.clone());
    encode_vault(&next)
}

/// The vault blob for `keyring` without `server_key`, leaving `keyring`
/// itself untouched until the write succeeds.
pub(super) fn vault_without(keyring: &Keyring, server_key: &str) -> Zeroizing<String> {
    let mut next = copy_keyring(keyring);
    next.remove(server_key);
    encode_vault(&next)
}

fn copy_keyring(keyring: &Keyring) -> Keyring {
    let mut copy = Keyring::new();
    for endpoint in keyring.endpoints() {
        if let Some(token) = keyring.token_for(&endpoint) {
            copy.insert(endpoint, token);
        }
    }
    copy
}

/// An unlocked delete's refused vault rewrite, as the failure it reports.
pub(super) fn refused_rewrite(status: CredentialStatus) -> Result<(), AppFailure> {
    match status {
        CredentialStatus::Success => Ok(()),
        status => {
            log::warn!("credentials: the vault rewrite was refused: {status:?}");
            Err(AppFailure::Storage(DELETE_FAILED.to_string()))
        }
    }
}

/// Folds a gated closure's two failure layers into one status.
fn status_from(
    result: Result<Result<(), CredentialStatus>, SecureStorageError>,
) -> CredentialStatus {
    match result {
        Ok(Ok(())) => CredentialStatus::Success,
        Ok(Err(status)) => status,
        Err(err) => status_of(&err),
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

/// The legacy per-server key, `zelli_cred_<server_key with [:/] → _>`. Sanitizing is not cosmetic — a raw
/// `:` or `/` crosses into a platform store's own namespace syntax, and an
/// IPv6 literal is full of colons.
pub(super) fn storage_key(server_key: &str) -> String {
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

    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;

    /// An in-memory [`VaultStore`] with per-key scripted read refusals.
    #[derive(Default)]
    struct FakeStore {
        entries: RefCell<BTreeMap<String, String>>,
        refuse_get: BTreeMap<String, CredentialStatus>,
        refuse_set: Option<CredentialStatus>,
        gets: Cell<usize>,
        sets: Cell<usize>,
    }

    impl VaultStore for FakeStore {
        fn get(&self, key: &str) -> Result<Option<Zeroizing<String>>, CredentialStatus> {
            self.gets.set(self.gets.get() + 1);
            if let Some(status) = self.refuse_get.get(key) {
                return Err(*status);
            }
            Ok(self.entries.borrow().get(key).cloned().map(Zeroizing::new))
        }

        fn set(&self, key: &str, value: &str) -> Result<(), CredentialStatus> {
            self.sets.set(self.sets.get() + 1);
            if let Some(status) = self.refuse_set {
                return Err(status);
            }
            self.entries
                .borrow_mut()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }

        fn contains(&self, key: &str) -> Result<bool, CredentialStatus> {
            Ok(self.entries.borrow().contains_key(key))
        }

        fn remove(&self, key: &str) -> Result<(), CredentialStatus> {
            self.entries.borrow_mut().remove(key);
            Ok(())
        }
    }

    impl FakeStore {
        fn with_legacy(endpoints: &[&str]) -> Self {
            let store = Self::default();
            for endpoint in endpoints {
                store
                    .entries
                    .borrow_mut()
                    .insert(storage_key(endpoint), format!("tok-{endpoint}"));
            }
            store
        }

        fn with_vault(self, entries: &[(&str, &str)]) -> Self {
            let mut keyring = Keyring::new();
            for (endpoint, token) in entries {
                keyring.insert(*endpoint, Zeroizing::new(token.to_string()));
            }
            let encoded = encode_vault(&keyring);
            self.entries
                .borrow_mut()
                .insert(VAULT_KEY.to_string(), encoded.to_string());
            self
        }

        fn vault(&self) -> Option<Keyring> {
            let raw = self.entries.borrow().get(VAULT_KEY).cloned()?;
            Some(KeyringVault::decode(&raw).expect("the written vault decodes"))
        }

        fn has(&self, key: &str) -> bool {
            self.entries.borrow().contains_key(key)
        }
    }

    fn saved(endpoints: &[&str]) -> Vec<String> {
        endpoints.iter().map(|e| e.to_string()).collect()
    }

    const A: &str = "a.example:1";
    const B: &str = "b.example:2";
    const C: &str = "c.example:3";

    #[test]
    fn the_vault_key_is_its_own_name_and_never_a_legacy_key() {
        assert_eq!(VAULT_KEY, "muxr_keyring");
        assert!(!VAULT_KEY.starts_with(KEY_PREFIX));
    }

    #[test]
    fn legacy_entries_fold_into_one_vault_and_are_removed() {
        let store = FakeStore::with_legacy(&[A, B, C]);
        let vault = unlock_vault(&store, &saved(&[A, B, C])).expect("unlocks");

        assert_eq!(vault.keyring.len(), 3);
        assert_eq!(vault.migrated, saved(&[A, B, C]));
        assert!(vault.unreadable.is_empty());
        assert_eq!(
            vault.keyring.token_for(B).as_deref().map(String::as_str),
            Some("tok-b.example:2")
        );
        for endpoint in [A, B, C] {
            assert!(
                !store.has(&storage_key(endpoint)),
                "{endpoint}'s legacy key is gone"
            );
        }
        assert_eq!(store.vault().expect("a vault was written").len(), 3);
        assert_eq!(
            store.gets.get(),
            4,
            "one vault read plus one per legacy entry"
        );
        assert_eq!(store.sets.get(), 1, "the vault is written exactly once");
    }

    #[test]
    fn a_cancel_on_the_second_legacy_read_writes_and_removes_nothing() {
        let mut store = FakeStore::with_legacy(&[A, B, C]);
        store
            .refuse_get
            .insert(storage_key(B), CredentialStatus::Canceled);

        let refused = unlock_vault(&store, &saved(&[A, B, C])).err();
        assert_eq!(refused, Some(CredentialStatus::Canceled));
        assert!(store.vault().is_none(), "no vault was written");
        assert_eq!(store.sets.get(), 0);
        for endpoint in [A, B, C] {
            assert!(
                store.has(&storage_key(endpoint)),
                "{endpoint}'s legacy key is intact"
            );
        }
    }

    #[test]
    fn an_invalidated_legacy_key_is_unreadable_while_the_rest_migrate() {
        let mut store = FakeStore::with_legacy(&[A, B]);
        store
            .refuse_get
            .insert(storage_key(A), CredentialStatus::KeyInvalidated);

        let vault = unlock_vault(&store, &saved(&[A, B])).expect("unlocks");
        assert_eq!(vault.unreadable, saved(&[A]));
        assert_eq!(vault.migrated, saved(&[B]));
        assert_eq!(vault.keyring.endpoints(), saved(&[B]));
        assert!(
            store.has(&storage_key(A)),
            "an unreadable entry is left for the caller"
        );
        assert!(!store.has(&storage_key(B)));
    }

    #[test]
    fn a_corrupted_vault_fails_and_touches_nothing() {
        let store = FakeStore::with_legacy(&[A]);
        store
            .entries
            .borrow_mut()
            .insert(VAULT_KEY.to_string(), "{not a vault".to_string());

        assert_eq!(
            unlock_vault(&store, &saved(&[A])).err(),
            Some(CredentialStatus::Failed)
        );
        assert!(store.has(&storage_key(A)));
        assert_eq!(
            store.entries.borrow().get(VAULT_KEY).map(String::as_str),
            Some("{not a vault")
        );
        assert_eq!(store.sets.get(), 0);
        assert_eq!(
            store_locked(&store, B.to_string(), Zeroizing::new("t".to_string())).err(),
            Some(CredentialStatus::Failed),
            "the locked save refuses to overwrite an unreadable vault too"
        );
        assert_eq!(store.sets.get(), 0);
    }

    #[test]
    fn a_vault_entry_for_an_endpoint_no_longer_saved_is_pruned() {
        let store = FakeStore::default().with_vault(&[(A, "ta"), (B, "tb")]);
        let vault = unlock_vault(&store, &saved(&[A])).expect("unlocks");

        assert_eq!(vault.keyring.endpoints(), saved(&[A]));
        assert!(vault.migrated.is_empty());
        assert_eq!(store.vault().unwrap().endpoints(), saved(&[A]));
        assert_eq!(store.sets.get(), 1);
    }

    #[test]
    fn nothing_to_migrate_or_prune_is_one_read_and_no_write() {
        let store = FakeStore::default().with_vault(&[(A, "ta")]);
        let vault = unlock_vault(&store, &saved(&[A, B])).expect("unlocks");

        assert_eq!(vault.keyring.len(), 1);
        assert_eq!((store.gets.get(), store.sets.get()), (1, 0));
        let empty = FakeStore::default();
        let vault = unlock_vault(&empty, &saved(&[A])).expect("no vault is an empty keyring");
        assert!(vault.keyring.is_empty());
        assert!(empty.vault().is_none());
    }

    #[test]
    fn a_legacy_entry_beside_its_vault_entry_is_stale_and_removed_unread() {
        let store = FakeStore::with_legacy(&[A]).with_vault(&[(A, "newer")]);
        let vault = unlock_vault(&store, &saved(&[A])).expect("unlocks");

        assert_eq!(
            vault.keyring.token_for(A).as_deref().map(String::as_str),
            Some("newer")
        );
        assert!(vault.migrated.is_empty());
        assert!(!store.has(&storage_key(A)));
        assert_eq!(store.gets.get(), 1, "the stale entry is never read");
    }

    #[test]
    fn a_refused_vault_write_keeps_every_legacy_entry() {
        let mut store = FakeStore::with_legacy(&[A]);
        store.refuse_set = Some(CredentialStatus::Canceled);

        assert_eq!(
            unlock_vault(&store, &saved(&[A])).err(),
            Some(CredentialStatus::Canceled)
        );
        assert!(store.has(&storage_key(A)));
    }

    #[test]
    fn the_locked_save_read_modify_writes_the_vault_and_supersedes_the_legacy_entry() {
        let store = FakeStore::with_legacy(&[B]).with_vault(&[(A, "ta")]);
        store_locked(&store, B.to_string(), Zeroizing::new("tb".to_string())).expect("saves");

        let vault = store.vault().unwrap();
        assert_eq!(vault.endpoints(), saved(&[A, B]));
        assert_eq!(
            vault.token_for(B).as_deref().map(String::as_str),
            Some("tb")
        );
        assert!(
            !store.has(&storage_key(B)),
            "the superseded legacy entry is gone"
        );
        assert_eq!(store.entries.borrow().len(), 1, "no legacy key was written");
    }

    #[test]
    fn a_single_read_takes_the_legacy_entry_first_then_the_vault() {
        let store = FakeStore::with_legacy(&[A]).with_vault(&[(B, "tb")]);
        assert_eq!(
            read_one(&store, A).unwrap().as_deref().map(String::as_str),
            Some("tok-a.example:1")
        );
        assert_eq!(
            read_one(&store, B).unwrap().as_deref().map(String::as_str),
            Some("tb")
        );
        assert_eq!(read_one(&store, C).unwrap(), None);
        assert_eq!(store.gets.get(), 3, "one gated read per lookup");
    }

    #[test]
    fn the_next_vault_adds_the_entry_without_touching_the_keyring() {
        let mut keyring = Keyring::new();
        keyring.insert(A, Zeroizing::new("ta".to_string()));
        let encoded = next_vault(&keyring, B, &Zeroizing::new("tb".to_string()));

        assert_eq!(keyring.len(), 1);
        let next = KeyringVault::decode(&encoded).unwrap();
        assert_eq!(next.endpoints(), saved(&[A, B]));
    }

    #[test]
    fn an_invalidated_vault_read_surfaces_as_key_invalidated_rather_than_failed() {
        let mut store = FakeStore::with_legacy(&[A]).with_vault(&[(A, "ta")]);
        store
            .refuse_get
            .insert(VAULT_KEY.to_string(), CredentialStatus::KeyInvalidated);

        assert_eq!(
            unlock_vault(&store, &saved(&[A])).err(),
            Some(CredentialStatus::KeyInvalidated)
        );
        assert!(store.has(&storage_key(A)) && store.has(VAULT_KEY));
    }

    #[test]
    fn the_vault_without_an_entry_leaves_the_keyring_untouched() {
        let mut keyring = Keyring::new();
        keyring.insert(A, Zeroizing::new("ta".to_string()));
        keyring.insert(B, Zeroizing::new("tb".to_string()));
        let encoded = vault_without(&keyring, B);

        assert_eq!(keyring.len(), 2);
        assert_eq!(
            KeyringVault::decode(&encoded).unwrap().endpoints(),
            saved(&[A])
        );
    }

    #[test]
    fn a_refused_rewrite_is_a_storage_failure() {
        assert!(refused_rewrite(CredentialStatus::Success).is_ok());
        assert!(matches!(
            refused_rewrite(CredentialStatus::Canceled),
            Err(AppFailure::Storage(_))
        ));
    }
}
