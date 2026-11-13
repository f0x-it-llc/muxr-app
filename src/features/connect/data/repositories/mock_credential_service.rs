//! In-memory [`CredentialService`] running the real vault algorithms over a map keyed like the platform store.
//! `prompts()` records every gated read and `writes()` every gated vault `set`; Apple prompts on reads only,
//! Android on both, so an Android flow's prompt count is `prompts().len() + writes()`.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use zeroize::Zeroizing;

use super::secure_credential_service::{
    DELETE_FAILED, FORGET_REASON, VAULT_KEY, VaultStore, VaultWrite, next_vault, read_one,
    refused_rewrite, storage_key, store_locked, unlock_vault, vault_without, write_vault,
};
use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{
    CredentialStatus, StoreRequest, UnlockRequest, UnlockedCredential, UnlockedKeyring,
};
use crate::features::connect::domain::keyring::{Keyring, KeyringVault};
use crate::features::connect::domain::repositories::CredentialService;

#[derive(Default)]
pub struct MockCredentialService {
    state: Mutex<MockState>,
}

#[derive(Default)]
struct MockState {
    /// Outcome for every gated operation unless `gate_at` scripts it.
    gate: Option<CredentialStatus>,
    /// Per-operation outcomes, by 0-based index over every gated read and write.
    gate_at: BTreeMap<usize, CredentialStatus>,
    gated_ops: usize,
    /// The platform store: the vault under `VAULT_KEY`, legacy entries under `storage_key`.
    entries: BTreeMap<String, Zeroizing<String>>,
    /// Every gated read's prompt rationale, in order.
    prompts: Vec<String>,
    writes: usize,
    /// Whether every later delete reports a storage failure.
    delete_fails: bool,
}

impl MockState {
    fn next_gate(&mut self) -> CredentialStatus {
        let index = self.gated_ops;
        self.gated_ops += 1;
        self.gate_at
            .get(&index)
            .or(self.gate.as_ref())
            .copied()
            .unwrap_or(CredentialStatus::Success)
    }

    fn vault(&self) -> Option<Keyring> {
        KeyringVault::decode(self.entries.get(VAULT_KEY)?).ok()
    }
}

impl MockCredentialService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_gate(&self, status: CredentialStatus) {
        self.state().gate = Some(status);
    }

    /// Scripts the `n`th gated operation (0-based, reads and writes in order).
    pub fn set_gate_at(&self, n: usize, status: CredentialStatus) {
        self.state().gate_at.insert(n, status);
    }

    pub fn seed(&self, server_key: &str, token: &str) {
        self.state()
            .entries
            .insert(storage_key(server_key), Zeroizing::new(token.to_string()));
    }

    pub fn seed_vault(&self, entries: &[(&str, &str)]) {
        let mut keyring = Keyring::new();
        for (endpoint, token) in entries {
            keyring.insert(*endpoint, Zeroizing::new(token.to_string()));
        }
        let encoded = Zeroizing::new(KeyringVault::encode(&keyring, |_| 0));
        self.state().entries.insert(VAULT_KEY.to_string(), encoded);
    }

    pub fn seed_vault_raw(&self, raw: &str) {
        self.state()
            .entries
            .insert(VAULT_KEY.to_string(), Zeroizing::new(raw.to_string()));
    }

    pub fn stored(&self, server_key: &str) -> Option<String> {
        let state = self.state();
        if let Some(token) = state.vault().and_then(|vault| vault.token_for(server_key)) {
            return Some(token.to_string());
        }
        let legacy = state.entries.get(&storage_key(server_key));
        legacy.map(|token| token.to_string())
    }

    pub fn legacy(&self, server_key: &str) -> Option<String> {
        let state = self.state();
        let legacy = state.entries.get(&storage_key(server_key));
        legacy.map(|token| token.to_string())
    }

    pub fn vault_endpoints(&self) -> Option<Vec<String>> {
        self.state().vault().map(|vault| vault.endpoints())
    }

    pub fn prompts(&self) -> Vec<String> {
        self.state().prompts.clone()
    }

    pub fn writes(&self) -> usize {
        self.state().writes
    }

    pub fn set_delete_failure(&self, fails: bool) {
        self.state().delete_fails = fails;
    }

    fn state(&self) -> MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn vault_with<'a>(&'a self, reason: &'a str) -> MockVault<'a> {
        MockVault { mock: self, reason }
    }
}

struct MockVault<'a> {
    mock: &'a MockCredentialService,
    reason: &'a str,
}

impl VaultStore for MockVault<'_> {
    fn get(&self, key: &str) -> Result<Option<Zeroizing<String>>, CredentialStatus> {
        let mut state = self.mock.state();
        state.prompts.push(self.reason.to_string());
        match state.next_gate() {
            CredentialStatus::Success => Ok(state.entries.get(key).cloned()),
            refused => Err(refused),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), CredentialStatus> {
        let mut state = self.mock.state();
        state.writes += 1;
        match state.next_gate() {
            CredentialStatus::Success => {
                let value = Zeroizing::new(value.to_string());
                state.entries.insert(key.to_string(), value);
                Ok(())
            }
            refused => Err(refused),
        }
    }

    fn contains(&self, key: &str) -> Result<bool, CredentialStatus> {
        Ok(self.mock.state().entries.contains_key(key))
    }

    fn remove(&self, key: &str) -> Result<(), CredentialStatus> {
        self.mock.state().entries.remove(key);
        Ok(())
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl CredentialService for MockCredentialService {
    async fn store(&self, request: StoreRequest) -> CredentialStatus {
        let vault = self.vault_with(&request.reason);
        match store_locked(&vault, request.server_key, request.token) {
            Ok(()) => CredentialStatus::Success,
            Err(status) => status,
        }
    }

    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential {
        match read_one(&self.vault_with(&request.reason), &request.server_key) {
            Ok(Some(token)) => UnlockedCredential::recovered(token),
            Ok(None) => UnlockedCredential::refused(CredentialStatus::NotFound),
            Err(status) => UnlockedCredential::refused(status),
        }
    }

    async fn unlock_all(&self, reason: &str, endpoints: &[String]) -> UnlockedKeyring {
        match unlock_vault(&self.vault_with(reason), endpoints) {
            Ok(vault) => vault.into_unlocked(),
            Err(status) => UnlockedKeyring::refused(status),
        }
    }

    async fn store_in_vault(
        &self,
        request: StoreRequest,
        keyring: &mut Keyring,
    ) -> CredentialStatus {
        let encoded = next_vault(keyring, &request.server_key, &request.token);
        let legacy = storage_key(&request.server_key);
        match write_vault(
            &self.vault_with(&request.reason),
            &encoded,
            Some(&legacy),
            VaultWrite::Store(&request.server_key),
        ) {
            Ok(()) => {
                keyring.insert(request.server_key, request.token);
                CredentialStatus::Success
            }
            Err(status) => status,
        }
    }

    async fn delete_from_vault(
        &self,
        server_key: String,
        keyring: &mut Keyring,
    ) -> Result<(), AppFailure> {
        if keyring.token_for(&server_key).is_some() {
            let encoded = vault_without(keyring, &server_key);
            let written = write_vault(
                &self.vault_with(FORGET_REASON),
                &encoded,
                None,
                VaultWrite::Evict(&server_key),
            );
            refused_rewrite(written.err().unwrap_or(CredentialStatus::Success))?;
            keyring.remove(&server_key);
        }
        self.delete(server_key).await
    }

    async fn delete(&self, server_key: String) -> Result<(), AppFailure> {
        self.remove_ungated(&storage_key(&server_key))
    }

    async fn reset_vault(&self) -> Result<(), AppFailure> {
        self.remove_ungated(VAULT_KEY)
    }
}

impl MockCredentialService {
    fn remove_ungated(&self, key: &str) -> Result<(), AppFailure> {
        let mut state = self.state();
        if state.delete_fails {
            return Err(AppFailure::Storage(DELETE_FAILED.to_string()));
        }
        state.entries.remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    fn store_request(key: &str, token: &str) -> StoreRequest {
        StoreRequest {
            server_key: key.to_string(),
            token: Zeroizing::new(token.to_string()),
            reason: format!("Save credentials for {key}"),
        }
    }

    fn unlock_request(key: &str) -> UnlockRequest {
        UnlockRequest {
            server_key: key.to_string(),
            reason: format!("Unlock {key}"),
        }
    }

    fn saved(endpoints: &[&str]) -> Vec<String> {
        endpoints.iter().map(|e| e.to_string()).collect()
    }

    fn token(keyring: &Keyring, endpoint: &str) -> Option<String> {
        keyring.token_for(endpoint).map(|token| token.to_string())
    }

    #[test]
    fn a_passing_gate_stores_and_recovers_the_same_token() {
        let service = MockCredentialService::new();
        assert_eq!(
            block_on(service.store(store_request("rig:1", "s3cr3t"))),
            CredentialStatus::Success
        );
        let unlocked = block_on(service.unlock(unlock_request("rig:1")));
        assert_eq!(unlocked.status, CredentialStatus::Success);
        assert_eq!(
            unlocked.token.as_deref().map(String::as_str),
            Some("s3cr3t")
        );
    }

    #[test]
    fn a_refused_gate_never_touches_the_store_in_either_direction() {
        let service = MockCredentialService::new();
        service.seed("rig:1", "s3cr3t");
        service.set_gate(CredentialStatus::Canceled);

        assert_eq!(
            block_on(service.store(store_request("rig:1", "replacement"))),
            CredentialStatus::Canceled
        );
        assert_eq!(
            service.stored("rig:1").as_deref(),
            Some("s3cr3t"),
            "a denied save must not overwrite what is already there"
        );

        let unlocked = block_on(service.unlock(unlock_request("rig:1")));
        assert_eq!(unlocked.status, CredentialStatus::Canceled);
        assert!(unlocked.token.is_none(), "a refused gate recovers nothing");
    }

    #[test]
    fn a_passing_gate_over_an_empty_store_is_not_found() {
        let service = MockCredentialService::new();
        let unlocked = block_on(service.unlock(unlock_request("rig:1")));
        assert_eq!(unlocked.status, CredentialStatus::NotFound);
    }

    #[test]
    fn a_delete_needs_no_gate_and_is_idempotent() {
        let service = MockCredentialService::new();
        service.seed("rig:1", "s3cr3t");
        service.set_gate(CredentialStatus::LockedOut);

        block_on(service.delete("rig:1".to_string())).expect("a delete runs no gate");
        block_on(service.delete("rig:1".to_string())).expect("…and removing nothing succeeds");

        assert_eq!(service.stored("rig:1"), None);
        assert!(
            service.prompts().is_empty(),
            "a delete raises no prompt at all"
        );
    }

    #[test]
    fn every_gated_call_records_exactly_one_prompt() {
        let service = MockCredentialService::new();
        block_on(service.store(store_request("rig:1", "s3cr3t")));
        block_on(service.unlock(unlock_request("rig:1")));

        assert_eq!(
            service.prompts(),
            vec![
                "Save credentials for rig:1".to_string(),
                "Unlock rig:1".to_string()
            ]
        );
        assert_eq!(service.writes(), 1, "only the save wrote the vault");
    }

    #[test]
    fn a_scripted_gate_answers_every_later_call_the_same_way() {
        let service = MockCredentialService::new();
        service.set_gate(CredentialStatus::LockedOut);

        for _ in 0..3 {
            assert_eq!(
                block_on(service.unlock(unlock_request("rig:1"))).status,
                CredentialStatus::LockedOut
            );
        }
        service.set_gate(CredentialStatus::Success);
        assert_eq!(
            block_on(service.unlock(unlock_request("rig:1"))).status,
            CredentialStatus::NotFound,
            "…until the script changes"
        );
    }

    #[test]
    fn a_scripted_delete_failure_is_reported_rather_than_swallowed() {
        let service = MockCredentialService::new();
        service.seed("rig:1", "s3cr3t");
        service.set_delete_failure(true);

        let failure = block_on(service.delete("rig:1".to_string()))
            .expect_err("the scripted failure surfaces");
        assert!(matches!(failure, AppFailure::Storage(_)));
    }

    #[test]
    fn unlock_all_migrates_every_legacy_entry_with_one_read_each_and_one_write() {
        let service = MockCredentialService::new();
        service.seed("a:1", "ta");
        service.seed("b:2", "tb");

        let unlocked = block_on(service.unlock_all("Unlock muxr", &saved(&["a:1", "b:2"])));
        assert_eq!(unlocked.status, CredentialStatus::Success);
        assert_eq!(unlocked.migrated, saved(&["a:1", "b:2"]));
        let keyring = unlocked.keyring.expect("a keyring on success");
        assert_eq!(token(&keyring, "b:2").as_deref(), Some("tb"));
        assert_eq!(service.prompts(), vec!["Unlock muxr".to_string(); 3]);
        assert_eq!(service.writes(), 1);
        assert_eq!(service.legacy("a:1"), None);
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1", "b:2"])));
        assert_eq!(service.stored("a:1").as_deref(), Some("ta"));
    }

    #[test]
    fn a_cancel_scripted_on_the_second_legacy_read_leaves_everything_as_it_was() {
        let service = MockCredentialService::new();
        service.seed("a:1", "ta");
        service.seed("b:2", "tb");
        service.set_gate_at(2, CredentialStatus::Canceled);

        let unlocked = block_on(service.unlock_all("Unlock muxr", &saved(&["a:1", "b:2"])));
        assert_eq!(unlocked.status, CredentialStatus::Canceled);
        assert!(unlocked.keyring.is_none());
        assert_eq!(service.vault_endpoints(), None, "no vault was written");
        assert_eq!(service.writes(), 0);
        assert_eq!(service.legacy("a:1").as_deref(), Some("ta"));
        assert_eq!(service.legacy("b:2").as_deref(), Some("tb"));
    }

    #[test]
    fn an_invalidated_legacy_read_is_unreadable_and_the_rest_still_migrate() {
        let service = MockCredentialService::new();
        service.seed("a:1", "ta");
        service.seed("b:2", "tb");
        service.set_gate_at(1, CredentialStatus::KeyInvalidated);

        let unlocked = block_on(service.unlock_all("Unlock muxr", &saved(&["a:1", "b:2"])));
        assert_eq!(unlocked.status, CredentialStatus::Success);
        assert_eq!(unlocked.unreadable, saved(&["a:1"]));
        assert_eq!(unlocked.migrated, saved(&["b:2"]));
    }

    #[test]
    fn a_corrupted_vault_fails_the_unlock_and_deletes_nothing() {
        let service = MockCredentialService::new();
        service.seed("a:1", "ta");
        service.seed_vault_raw("{\"version\":9}");

        let unlocked = block_on(service.unlock_all("Unlock muxr", &saved(&["a:1"])));
        assert_eq!(unlocked.status, CredentialStatus::Failed);
        assert_eq!(service.legacy("a:1").as_deref(), Some("ta"));
        assert_eq!(service.writes(), 0);
    }

    #[test]
    fn a_vault_entry_orphaned_by_a_locked_delete_is_pruned_at_the_next_unlock() {
        let service = MockCredentialService::new();
        service.seed_vault(&[("a:1", "ta"), ("b:2", "tb")]);
        block_on(service.delete("b:2".to_string())).expect("ungated");
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1", "b:2"])));

        let unlocked = block_on(service.unlock_all("Unlock muxr", &saved(&["a:1"])));
        assert_eq!(unlocked.keyring.unwrap().endpoints(), saved(&["a:1"]));
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1"])));
        assert_eq!(service.writes(), 1);
    }

    #[test]
    fn an_unlocked_save_writes_once_and_fills_the_keyring_only_on_success() {
        let service = MockCredentialService::new();
        let mut keyring = Keyring::new();
        keyring.insert("a:1", Zeroizing::new("ta".to_string()));

        let status = block_on(service.store_in_vault(store_request("b:2", "tb"), &mut keyring));
        assert_eq!(status, CredentialStatus::Success);
        assert_eq!(token(&keyring, "b:2").as_deref(), Some("tb"));
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1", "b:2"])));
        assert_eq!((service.prompts().len(), service.writes()), (0, 1));
        assert_eq!(service.legacy("b:2"), None, "never a legacy entry");

        service.set_gate(CredentialStatus::Canceled);
        let status = block_on(service.store_in_vault(store_request("c:3", "tc"), &mut keyring));
        assert_eq!(status, CredentialStatus::Canceled);
        assert_eq!(keyring.endpoints(), saved(&["a:1", "b:2"]));
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1", "b:2"])));
    }

    #[test]
    fn an_unlocked_delete_drops_the_keyring_vault_and_legacy_entries() {
        let service = MockCredentialService::new();
        service.seed_vault(&[("a:1", "ta"), ("b:2", "tb")]);
        service.seed("b:2", "old");
        let mut keyring = block_on(service.unlock_all("Unlock", &saved(&["a:1", "b:2"])))
            .keyring
            .expect("unlocked");
        let writes = service.writes();

        block_on(service.delete_from_vault("b:2".to_string(), &mut keyring)).expect("deleted");
        assert_eq!(keyring.endpoints(), saved(&["a:1"]));
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1"])));
        assert_eq!(service.stored("b:2"), None);
        assert_eq!(service.writes(), writes + 1);

        block_on(service.delete_from_vault("zz:9".to_string(), &mut keyring)).expect("absent");
        assert_eq!(service.writes(), writes + 1, "nothing to rewrite, no gate");
    }

    fn unlocked_keyring(entries: &[(&str, &str)]) -> Keyring {
        let mut keyring = Keyring::new();
        for (endpoint, token) in entries {
            keyring.insert(*endpoint, Zeroizing::new(token.to_string()));
        }
        keyring
    }

    #[test]
    fn an_unlocked_delete_removes_a_legacy_entry_that_is_still_there() {
        let service = MockCredentialService::new();
        service.seed_vault(&[("a:1", "ta"), ("b:2", "tb")]);
        service.seed("b:2", "old");
        let mut keyring = unlocked_keyring(&[("a:1", "ta"), ("b:2", "tb")]);

        block_on(service.delete_from_vault("b:2".to_string(), &mut keyring)).expect("deleted");
        assert_eq!(service.legacy("b:2"), None, "the legacy entry went too");
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1"])));
        assert_eq!(keyring.endpoints(), saved(&["a:1"]));

        service.seed("c:3", "legacy-only");
        block_on(service.delete_from_vault("c:3".to_string(), &mut keyring)).expect("deleted");
        assert_eq!(
            service.legacy("c:3"),
            None,
            "no vault entry, legacy still removed"
        );
        assert_eq!(
            service.writes(),
            1,
            "only the first delete rewrote the vault"
        );
    }

    #[test]
    fn a_refused_vault_rewrite_leaves_the_keyring_vault_and_legacy_entry_untouched() {
        let service = MockCredentialService::new();
        service.seed_vault(&[("a:1", "ta"), ("b:2", "tb")]);
        service.seed("b:2", "old");
        service.set_gate(CredentialStatus::Canceled);
        let mut keyring = unlocked_keyring(&[("a:1", "ta"), ("b:2", "tb")]);

        let failure = block_on(service.delete_from_vault("b:2".to_string(), &mut keyring))
            .expect_err("a refused rewrite is reported");
        assert!(matches!(failure, AppFailure::Storage(_)));
        assert_eq!(keyring.endpoints(), saved(&["a:1", "b:2"]));
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1", "b:2"])));
        assert_eq!(service.legacy("b:2").as_deref(), Some("old"));
    }

    #[test]
    fn a_reset_removes_the_vault_ungated_and_leaves_legacy_entries() {
        let service = MockCredentialService::new();
        service.seed_vault_raw("{corrupted");
        service.seed("a:1", "ta");
        service.set_gate(CredentialStatus::KeyInvalidated);

        let refused = block_on(service.unlock_all("Unlock", &saved(&["a:1"])));
        assert_eq!(refused.status, CredentialStatus::KeyInvalidated);

        block_on(service.reset_vault()).expect("a reset runs no gate");
        block_on(service.reset_vault()).expect("…and an absent vault is fine");
        assert_eq!(service.vault_endpoints(), None);
        assert_eq!(service.legacy("a:1").as_deref(), Some("ta"));
        assert_eq!(service.writes(), 0);

        service.set_gate(CredentialStatus::Success);
        let unlocked = block_on(service.unlock_all("Unlock", &saved(&["a:1"])));
        assert_eq!(unlocked.status, CredentialStatus::Success);
        assert_eq!(unlocked.migrated, saved(&["a:1"]));
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1"])));
    }

    #[test]
    fn a_reset_over_a_corrupted_vault_unlocks_to_an_empty_keyring() {
        let service = MockCredentialService::new();
        service.seed_vault_raw("{corrupted");
        let failed = block_on(service.unlock_all("Unlock", &saved(&["a:1"])));
        assert_eq!(failed.status, CredentialStatus::Failed);

        block_on(service.reset_vault()).expect("reset");
        let unlocked = block_on(service.unlock_all("Unlock", &saved(&["a:1"])));
        assert_eq!(unlocked.status, CredentialStatus::Success);
        assert!(unlocked.keyring.expect("a keyring").is_empty());
    }

    #[test]
    fn a_scripted_delete_failure_fails_a_reset_too() {
        let service = MockCredentialService::new();
        service.seed_vault(&[("a:1", "ta")]);
        service.set_delete_failure(true);

        assert!(matches!(
            block_on(service.reset_vault()),
            Err(AppFailure::Storage(_))
        ));
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1"])));
    }

    #[test]
    fn a_locked_save_rewrites_the_vault_it_read_without_a_legacy_entry() {
        let service = MockCredentialService::new();
        service.seed_vault(&[("a:1", "ta")]);

        let status = block_on(service.store(store_request("b:2", "tb")));
        assert_eq!(status, CredentialStatus::Success);
        assert_eq!(service.vault_endpoints(), Some(saved(&["a:1", "b:2"])));
        assert_eq!(service.legacy("b:2"), None);
        assert_eq!((service.prompts().len(), service.writes()), (1, 1));
    }
}
