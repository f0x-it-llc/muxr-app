//! [`MockCredentialService`] — the in-memory [`CredentialService`] every
//! headless suite runs on, and the only way to exercise the gate's outcome table
//! without a device.
//!
//! The real service ([`super::secure_credential_service`]) answers whatever the
//! platform's biometric gate says, which off-device is always "unavailable" and
//! on-device is whatever a human does to a prompt. Neither is a test. This one
//! answers a **scripted** [`CredentialStatus`], so a suite can drive every
//! branch of the outcome table — a cancel, a lockout, an invalidated key — in
//! order, in one process, with no hardware at all.
//!
//! It also records the prompts it was asked to raise ([`Self::prompts`]),
//! because "exactly one biometric prompt per successful pairing" is a contract
//! this app makes and a count is the only way to check it.
//!
//! Selected by `di`'s own headless-test seam
//! (`use_mock_credentials_for_tests`), never in a real app run. A poisoned lock
//! is recovered rather than propagated, for the reason
//! [`super::mock_saved_servers`] records.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use zeroize::Zeroizing;

use crate::failure::AppFailure;
use crate::features::connect::domain::entities::{
    CredentialStatus, StoreRequest, UnlockRequest, UnlockedCredential,
};
use crate::features::connect::domain::repositories::CredentialService;

/// What a scripted delete failure reports.
const DELETE_FAILED: &str = "the saved credential could not be removed";

/// The in-memory credential service. See the [module docs](self).
#[derive(Default)]
pub struct MockCredentialService {
    state: Mutex<MockState>,
}

/// Everything the mock remembers.
#[derive(Default)]
struct MockState {
    /// The outcome every gated call answers with. `Success` lets the call
    /// through to the store below.
    gate: Option<CredentialStatus>,
    /// The stored secrets, by server key.
    secrets: BTreeMap<String, Zeroizing<String>>,
    /// Every prompt rationale a gated call was asked to raise, in order.
    prompts: Vec<String>,
    /// Whether the next (and every later) delete reports a storage failure.
    delete_fails: bool,
}

impl MockCredentialService {
    /// A service whose gate always passes and whose store is empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Script `status` as the answer to every later gated call.
    /// [`CredentialStatus::Success`] restores the pass-through behaviour.
    pub fn set_gate(&self, status: CredentialStatus) {
        self.state().gate = Some(status);
    }

    /// Pre-store `token` for `server_key`, as a successful save would have.
    pub fn seed(&self, server_key: &str, token: &str) {
        self.state()
            .secrets
            .insert(server_key.to_string(), Zeroizing::new(token.to_string()));
    }

    /// The token stored for `server_key`, if any — how a suite checks that a
    /// save actually wrote (and a delete actually removed) the secret.
    pub fn stored(&self, server_key: &str) -> Option<String> {
        self.state()
            .secrets
            .get(server_key)
            .map(|token| token.to_string())
    }

    /// Every prompt rationale raised so far, in order.
    pub fn prompts(&self) -> Vec<String> {
        self.state().prompts.clone()
    }

    /// Make every later delete fail with a storage error.
    pub fn set_delete_failure(&self, fails: bool) {
        self.state().delete_fails = fails;
    }

    /// The state, recovering a poisoned lock.
    fn state(&self) -> std::sync::MutexGuard<'_, MockState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record `reason` as a raised prompt and answer what the gate is scripted
    /// to say.
    fn run_gate(&self, reason: &str) -> CredentialStatus {
        let mut state = self.state();
        state.prompts.push(reason.to_string());
        state.gate.unwrap_or(CredentialStatus::Success)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl CredentialService for MockCredentialService {
    async fn store(&self, request: StoreRequest) -> CredentialStatus {
        let gate = self.run_gate(&request.reason);
        if gate != CredentialStatus::Success {
            return gate;
        }
        self.state()
            .secrets
            .insert(request.server_key, request.token);
        CredentialStatus::Success
    }

    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential {
        let gate = self.run_gate(&request.reason);
        if gate != CredentialStatus::Success {
            return UnlockedCredential::refused(gate);
        }
        match self.state().secrets.get(&request.server_key) {
            Some(token) => UnlockedCredential::recovered(token.clone()),
            None => UnlockedCredential::refused(CredentialStatus::NotFound),
        }
    }

    async fn delete(&self, server_key: String) -> Result<(), AppFailure> {
        let mut state = self.state();
        if state.delete_fails {
            return Err(AppFailure::Storage(DELETE_FAILED.to_string()));
        }
        state.secrets.remove(&server_key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive one call to completion on a current-thread runtime — the shape
    /// every async unit test in this app uses.
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
}
