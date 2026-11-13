//! Called once from `crate::MuxrApp::init`, the only place `connect`'s repositories are built.
//! Publishes the app-scoped [`KeyringHandle`]: a shared `Mutex<Keyring>` plus `unlocked` and
//! `generation` signals. Tokens never sit inside a signal.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use frust::{GetUntracked, RwSignal, Set, Update, provide_context, use_context};
use zeroize::Zeroizing;

use crate::connection::KeyringLock;

use super::data::repositories::{
    MockCredentialService, MockSavedServersRepository, PrefsSavedServersRepository,
    SecureCredentialStore,
};
use super::domain::keyring::Keyring;
use super::domain::repositories::{CredentialService, SavedServersRepository};

pub type SavedServers = Arc<dyn SavedServersRepository + Send + Sync>;

pub type Credentials = Arc<dyn CredentialService + Send + Sync>;

static USE_MOCK_SAVED_SERVERS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

static USE_MOCK_CREDENTIALS_FOR_TESTS: AtomicBool = AtomicBool::new(false);

pub fn use_mock_saved_servers_for_tests() {
    USE_MOCK_SAVED_SERVERS_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub fn use_mock_credentials_for_tests() {
    USE_MOCK_CREDENTIALS_FOR_TESTS.store(true, Ordering::Relaxed);
}

pub fn provide() {
    provide_context(platform_saved_servers());
    provide_credentials();
    provide_context(KeyringHandle::new());
}

fn platform_saved_servers() -> SavedServers {
    if USE_MOCK_SAVED_SERVERS_FOR_TESTS.load(Ordering::Relaxed) {
        return Arc::new(MockSavedServersRepository::seeded());
    }
    match PrefsSavedServersRepository::standard() {
        Ok(repository) => Arc::new(repository),
        Err(err) => {
            log::warn!("saved servers: falling back to the in-memory store: {err}");
            // Not persistent: its demo rows must never drive a keyring prune.
            Arc::new(MockSavedServersRepository::seeded().not_persistent())
        }
    }
}

fn provide_credentials() {
    if USE_MOCK_CREDENTIALS_FOR_TESTS.load(Ordering::Relaxed) {
        let mock = Arc::new(MockCredentialService::new());
        provide_context(Arc::clone(&mock) as Credentials);
        provide_context(mock);
        return;
    }
    provide_context(Arc::new(SecureCredentialStore::new()) as Credentials);
}

pub fn expect_saved_servers() -> SavedServers {
    use_context::<SavedServers>().expect(
        "expect_saved_servers: features::connect::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

pub fn saved_servers() -> Option<SavedServers> {
    use_context::<SavedServers>()
}

pub fn expect_credential_service() -> Credentials {
    use_context::<Credentials>().expect(
        "expect_credential_service: features::connect::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

pub fn mock_credentials() -> Option<Arc<MockCredentialService>> {
    use_context::<Arc<MockCredentialService>>()
}

#[derive(Clone)]
pub struct KeyringHandle {
    keyring: Arc<Mutex<Keyring>>,
    pub unlocked: RwSignal<bool>,
    pub generation: RwSignal<u64>,
    /// Bumped only by [`Self::lock`]; an unlock compares it across its await to detect an overtaking lock.
    lock_epoch: Arc<AtomicU64>,
    unlocking: Arc<AtomicBool>,
}

/// App-wide unlock single-flight, released on drop.
#[must_use = "dropping the ticket releases the single-flight"]
pub struct UnlockTicket {
    unlocking: Arc<AtomicBool>,
}

impl Drop for UnlockTicket {
    fn drop(&mut self) {
        self.unlocking.store(false, Ordering::SeqCst);
    }
}

impl KeyringHandle {
    pub fn new() -> Self {
        Self {
            keyring: Arc::new(Mutex::new(Keyring::new())),
            unlocked: RwSignal::new(false),
            generation: RwSignal::new(0),
            lock_epoch: Arc::new(AtomicU64::new(0)),
            unlocking: Arc::new(AtomicBool::new(false)),
        }
    }

    /// `None` while another unlock is in flight (no second prompt may be raised).
    pub fn begin_unlock(&self) -> Option<UnlockTicket> {
        self.unlocking
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| UnlockTicket {
                unlocking: Arc::clone(&self.unlocking),
            })
    }

    pub fn unlock_in_flight(&self) -> bool {
        self.unlocking.load(Ordering::SeqCst)
    }

    pub fn lock_epoch(&self) -> u64 {
        self.lock_epoch.load(Ordering::SeqCst)
    }

    fn guard(&self) -> MutexGuard<'_, Keyring> {
        self.keyring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self) {
        self.generation.update(|generation| *generation += 1);
    }

    pub fn publish(&self, keyring: Keyring) {
        let previous = std::mem::replace(&mut *self.guard(), keyring);
        drop(previous);
        self.unlocked.set(true);
        self.bump();
    }

    pub fn insert(&self, endpoint: impl Into<String>, token: Zeroizing<String>) {
        self.guard().insert(endpoint, token);
        self.bump();
    }

    pub fn remove(&self, endpoint: &str) -> bool {
        let removed = self.guard().remove(endpoint);
        if removed {
            self.bump();
        }
        removed
    }

    pub fn token_for(&self, endpoint: &str) -> Option<Zeroizing<String>> {
        self.guard().token_for(endpoint)
    }

    pub fn endpoints(&self) -> Vec<String> {
        self.guard().endpoints()
    }

    /// A token-by-token copy for async credential calls: they hold `&mut Keyring` across an
    /// await, so the handle's mutex must not be held. Apply the outcome back only on success.
    pub fn snapshot(&self) -> Keyring {
        let held = self.guard();
        let mut copy = Keyring::new();
        for endpoint in held.endpoints() {
            if let Some(token) = held.token_for(&endpoint) {
                copy.insert(endpoint, token);
            }
        }
        copy
    }

    pub fn is_unlocked(&self) -> bool {
        self.unlocked.get_untracked()
    }

    pub fn lock(&self) {
        self.lock_epoch.fetch_add(1, Ordering::SeqCst);
        self.guard().lock();
        self.unlocked.set(false);
        self.bump();
    }
}

impl Default for KeyringHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyringLock for KeyringHandle {
    fn lock_keyring(&self) {
        self.lock();
    }
}

pub fn expect_keyring() -> KeyringHandle {
    use_context::<KeyringHandle>().expect(
        "expect_keyring: features::connect::di::provide() must run first \
         (MuxrApp::init calls it)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(value: &str) -> Zeroizing<String> {
        Zeroizing::new(value.to_string())
    }

    #[test]
    fn publish_unlocks_and_every_clone_shares_one_keyring() {
        let handle = KeyringHandle::new();
        assert!(!handle.is_unlocked());
        let mut keyring = Keyring::new();
        keyring.insert("a.example:1", token("tok-a"));
        handle.publish(keyring);

        let clone = handle.clone();
        clone.insert("b.example:2", token("tok-b"));
        assert!(handle.is_unlocked());
        assert_eq!(handle.endpoints(), vec!["a.example:1", "b.example:2"]);
        assert_eq!(handle.token_for("b.example:2").unwrap().as_str(), "tok-b");
        assert_eq!(handle.generation.get_untracked(), 2);
    }

    #[test]
    fn remove_bumps_the_generation_only_when_a_token_went() {
        let handle = KeyringHandle::new();
        handle.insert("a.example:1", token("tok-a"));
        assert!(handle.remove("a.example:1"));
        assert!(!handle.remove("a.example:1"));
        assert_eq!(handle.generation.get_untracked(), 2);
    }

    #[test]
    fn a_snapshot_is_an_independent_copy_that_still_redacts() {
        let handle = KeyringHandle::new();
        handle.insert("a.example:1", token("tok-a"));
        let mut copy = handle.snapshot();
        assert_eq!(copy.token_for("a.example:1").unwrap().as_str(), "tok-a");

        copy.insert("b.example:2", token("tok-b"));
        copy.remove("a.example:1");
        assert_eq!(
            handle.endpoints(),
            vec!["a.example:1"],
            "the handle is untouched"
        );
        handle.lock();
        assert!(
            copy.token_for("b.example:2").is_some(),
            "…and so is the copy"
        );

        let debug = format!("{copy:?}");
        assert!(
            debug.contains("b.example:2") && !debug.contains("tok-b"),
            "{debug}"
        );
    }

    #[test]
    fn lock_empties_the_keyring_and_marks_it_locked() {
        let handle = KeyringHandle::new();
        let mut keyring = Keyring::new();
        keyring.insert("a.example:1", token("tok-a"));
        handle.publish(keyring);

        handle.lock();
        assert!(!handle.is_unlocked());
        assert!(handle.endpoints().is_empty());
        assert!(handle.token_for("a.example:1").is_none());
    }

    #[test]
    fn begin_unlock_is_single_flight_and_the_lock_epoch_counts_only_locks() {
        let handle = KeyringHandle::new();
        let clone = handle.clone();
        let ticket = handle.begin_unlock().expect("first ticket");
        assert!(clone.unlock_in_flight());
        assert!(clone.begin_unlock().is_none(), "no second unlock");
        drop(ticket);
        assert!(!handle.unlock_in_flight());
        assert!(clone.begin_unlock().is_some(), "released on drop");

        assert_eq!(handle.lock_epoch(), 0);
        handle.insert("a.example:1", token("tok-a"));
        handle.publish(Keyring::new());
        assert_eq!(
            handle.lock_epoch(),
            0,
            "inserts and publishes are not locks"
        );
        clone.lock();
        assert_eq!(handle.lock_epoch(), 1);
    }
}
