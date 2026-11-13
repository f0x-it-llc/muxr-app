//! The `/settings` view model. A token mutation re-reads the list rather than patching it; a
//! preference save publishes the record the repository returns. A minted secret stays on
//! `pending_token_secret` until the next mint or an explicit dismissal clears it.

use std::sync::Arc;

use clean_signals::{ControllerCore, NoParams, RunOptions};
use frust::{GetUntracked, RwSignal, Set, spawn_local};
use zeroize::Zeroizing;

use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::connect::di::{Credentials, KeyringHandle, SavedServers};
use crate::features::connect::domain::entities::{CredentialStatus, SavedServer};
use crate::features::connect::presentation::controller::SAVED_LIST_UNAVAILABLE_MESSAGE;
use crate::features::connect::presentation::{KeyringUnlock, KeyringUnlockOutcome};
use crate::features::settings::domain::entities::{
    AppearancePrefs, AuthToken, CreateTokenRequest, MintedToken, PollPrefs, Preferences,
};
use crate::features::settings::domain::repositories::{
    AuthTokensRepository, PreferencesRepository,
};
use crate::features::settings::domain::use_cases::{
    CreateToken, ListTokens, LoadPreferences, RevokeToken, UpdateAppearance, UpdatePollPrefs,
};
use crate::ui::toasts::app_toasts;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SavedServerRow {
    Server { server: SavedServer, active: bool },
    AddServer,
}

pub fn saved_server_rows(saved: &[SavedServer], active: Option<&str>) -> Vec<SavedServerRow> {
    saved
        .iter()
        .map(|server| SavedServerRow::Server {
            active: active == Some(server.endpoint().as_str()),
            server: server.clone(),
        })
        .chain(std::iter::once(SavedServerRow::AddServer))
        .collect()
}

fn report(status: CredentialStatus) {
    if let Some(message) = status.message() {
        app_toasts().show(message);
    }
}

pub struct SettingsController {
    core: ControllerCore<AppFailure>,
    list_tokens: ListTokens,
    create_token: CreateToken,
    revoke_token: RevokeToken,
    load_preferences: LoadPreferences,
    update_appearance: UpdateAppearance,
    update_poll_prefs: UpdatePollPrefs,
    saved_servers: SavedServers,
    credentials: Credentials,
    keyring_unlock: KeyringUnlock,
    /// The single-flight claim of a switch's keyring unlock; the dial has the connection's own.
    pub switching: RwSignal<bool>,
    pub tokens: RwSignal<Vec<AuthToken>>,
    pub appearance: RwSignal<AppearancePrefs>,
    pub poll: RwSignal<PollPrefs>,
    pub loaded: RwSignal<bool>,
    pub pending_token_secret: RwSignal<Option<MintedToken>>,
    pub saved: RwSignal<Vec<SavedServer>>,
    /// Set only by a restore started from the `muxr pro` row.
    pub restoring: RwSignal<bool>,
}

impl SettingsController {
    pub fn new(
        tokens: Arc<dyn AuthTokensRepository + Send + Sync>,
        preferences: Arc<dyn PreferencesRepository + Send + Sync>,
        saved_servers: SavedServers,
        credentials: Credentials,
        keyring: KeyringHandle,
        connection: Arc<ConnectionController>,
    ) -> Self {
        let defaults = Preferences::defaults();
        let keyring_unlock = KeyringUnlock::new(
            Arc::clone(&saved_servers),
            Arc::clone(&credentials),
            connection,
            keyring,
        );
        Self {
            keyring_unlock,
            switching: RwSignal::new(false),
            core: ControllerCore::new(),
            list_tokens: ListTokens::new(Arc::clone(&tokens)),
            create_token: CreateToken::new(Arc::clone(&tokens)),
            revoke_token: RevokeToken::new(tokens),
            load_preferences: LoadPreferences::new(Arc::clone(&preferences)),
            update_appearance: UpdateAppearance::new(Arc::clone(&preferences)),
            update_poll_prefs: UpdatePollPrefs::new(preferences),
            saved_servers,
            credentials,
            tokens: RwSignal::new(Vec::new()),
            appearance: RwSignal::new(defaults.appearance),
            poll: RwSignal::new(defaults.poll),
            loaded: RwSignal::new(false),
            pending_token_secret: RwSignal::new(None),
            restoring: RwSignal::new(false),
            saved: RwSignal::new(Vec::new()),
        }
    }

    /// Named rather than [`AsRef`], which `Arc<Self>`'s inherent `as_ref` makes ambiguous.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    pub async fn refresh(&self) {
        self.refresh_tokens().await;
        if let Ok(prefs) = self
            .core
            .run(&self.load_preferences, NoParams, RunOptions::default())
            .await
        {
            self.appearance.set(prefs.appearance);
            self.poll.set(prefs.poll);
        }
        self.refresh_saved_servers();
        self.loaded.set(true);
    }

    /// A failure keeps the previous list and raises nothing.
    pub fn refresh_saved_servers(&self) {
        if let Ok(servers) = self.saved_servers.list() {
            self.saved.set(servers);
        }
    }

    /// A locked keyring unlocks first, and a refusal deletes nothing. The token revoke and the
    /// row clear run only once the eviction landed, so a refused vault rewrite changes nothing.
    pub async fn delete_credential(&self, endpoint: String) -> bool {
        let Some(mut server) = self.saved_row(&endpoint) else {
            return false;
        };
        if !self.keyring().is_unlocked() {
            match self.keyring_unlock.unlock().await {
                KeyringUnlockOutcome::Unlocked { .. } => {}
                KeyringUnlockOutcome::Refused(status) => {
                    report(status);
                    return false;
                }
                KeyringUnlockOutcome::ListUnavailable => {
                    app_toasts().show(SAVED_LIST_UNAVAILABLE_MESSAGE);
                    return false;
                }
                KeyringUnlockOutcome::Busy | KeyringUnlockOutcome::Superseded => return false,
            }
            self.refresh_saved_servers();
            // An unreadable credential has already lost its claim by now.
            let Some(row) = self.saved_row(&endpoint) else {
                return false;
            };
            server = row;
        }
        if let Err(failure) = self.evict_unlocked(&endpoint).await {
            self.core.failures().emit(&failure);
            return false;
        }
        if let Some(name) = server.token_name.clone() {
            self.revoke_quietly(name).await;
        }
        self.clear_credential_fields(server);
        true
    }

    /// Never prompts: locked, the ungated legacy delete runs and the next unlock prunes the vault.
    /// Unlocked, the handle drops its token regardless; a refused rewrite falls back to legacy.
    pub async fn forget_server(&self, endpoint: String) -> bool {
        let Some(server) = self.saved_row(&endpoint) else {
            return false;
        };
        if server.has_credential || self.keyring().token_for(&endpoint).is_some() {
            if let Some(name) = server.token_name.clone() {
                self.revoke_quietly(name).await;
            }
            let evicted = if self.keyring().is_unlocked() {
                let evicted = self.evict_unlocked(&endpoint).await;
                self.keyring().remove(&endpoint);
                if evicted.is_err()
                    && let Err(failure) = self.credentials.delete(endpoint.clone()).await
                {
                    log::warn!(
                        "credentials: legacy delete after a refused eviction failed: {failure}"
                    );
                }
                evicted
            } else {
                self.credentials.delete(endpoint.clone()).await
            };
            if let Err(failure) = evicted {
                self.core.failures().emit(&failure);
            }
        }
        let removed = self.saved_servers.remove(&endpoint);
        if let Err(failure) = &removed {
            self.core.failures().emit(failure);
        }
        self.refresh_saved_servers();
        removed.is_ok()
    }

    fn keyring(&self) -> &KeyringHandle {
        self.keyring_unlock.keyring()
    }

    /// Rewrites a snapshot, never under the handle's lock; the handle drops the entry after.
    async fn evict_unlocked(&self, endpoint: &str) -> Result<(), AppFailure> {
        let mut snapshot = self.keyring().snapshot();
        self.credentials
            .delete_from_vault(endpoint.to_string(), &mut snapshot)
            .await?;
        self.keyring().remove(endpoint);
        Ok(())
    }

    /// The free-tier verdict is the caller's. A row whose fresh read claims no credential is never
    /// dialed with a keyring token; the list is re-read instead.
    pub fn switch_server(self: &Arc<Self>, endpoint: String, on_switched: Box<dyn FnOnce()>) {
        if self.switching.get_untracked() {
            return;
        }
        if !self.claims_credential(&endpoint) {
            self.refresh_saved_servers();
            report(CredentialStatus::NotFound);
            return;
        }
        if self.keyring().is_unlocked() {
            self.dial_saved(&endpoint, on_switched);
            return;
        }
        self.switching.set(true);
        let controller = Arc::clone(self);
        spawn_local(async move {
            let outcome = controller.keyring_unlock.unlock().await;
            controller.switching.set(false);
            controller.refresh_saved_servers();
            match outcome {
                KeyringUnlockOutcome::Unlocked { .. } => {
                    controller.dial_saved(&endpoint, on_switched);
                }
                KeyringUnlockOutcome::Refused(status) => report(status),
                KeyringUnlockOutcome::ListUnavailable => {
                    app_toasts().show(SAVED_LIST_UNAVAILABLE_MESSAGE);
                }
                KeyringUnlockOutcome::Busy | KeyringUnlockOutcome::Superseded => {}
            }
        });
    }

    fn claims_credential(&self, endpoint: &str) -> bool {
        self.saved_row(endpoint)
            .is_some_and(|server| server.has_credential)
    }

    fn dial_saved(self: &Arc<Self>, endpoint: &str, on_switched: Box<dyn FnOnce()>) {
        let found = self
            .saved_row(endpoint)
            .filter(|server| server.has_credential)
            .and_then(|server| Some((self.keyring().token_for(endpoint)?, server)));
        let Some((token, server)) = found else {
            self.refresh_saved_servers();
            report(CredentialStatus::NotFound);
            return;
        };
        self.switch_now(&server, token, on_switched);
    }

    fn switch_now(
        &self,
        server: &SavedServer,
        token: Zeroizing<String>,
        on_switched: Box<dyn FnOnce()>,
    ) {
        self.keyring_unlock
            .switch_to_saved(server, token, move |outcome| {
                if outcome.is_ok() {
                    on_switched();
                }
            });
    }

    fn saved_row(&self, endpoint: &str) -> Option<SavedServer> {
        self.saved_servers
            .list()
            .ok()?
            .into_iter()
            .find(|server| server.endpoint() == endpoint)
    }

    fn clear_credential_fields(&self, mut server: SavedServer) {
        server.has_credential = false;
        server.token_name = None;
        if let Err(failure) = self.saved_servers.save(server) {
            self.core.failures().emit(&failure);
        }
        self.refresh_saved_servers();
    }

    /// A revoke needs a live read-write session; the delete it accompanies must not depend on one.
    async fn revoke_quietly(&self, name: String) {
        let _ = self
            .core
            .run(
                &self.revoke_token,
                name,
                RunOptions {
                    emit_failure: false,
                    ..RunOptions::default()
                },
            )
            .await;
        self.refresh_tokens().await;
    }

    pub async fn refresh_tokens(&self) {
        if let Ok(tokens) = self
            .core
            .run(&self.list_tokens, NoParams, RunOptions::default())
            .await
        {
            self.tokens.set(tokens);
        }
    }

    pub async fn create_token(&self, name: String, read_only: bool) -> bool {
        let request = CreateTokenRequest { name, read_only };
        let Ok(minted) = self
            .core
            .run(&self.create_token, request, RunOptions::default())
            .await
        else {
            return false;
        };
        self.pending_token_secret.set(Some(minted));
        self.refresh_tokens().await;
        true
    }

    pub fn clear_pending_secret(&self) {
        self.pending_token_secret.set(None);
    }

    pub async fn revoke_token(&self, name: String) -> bool {
        let revoked = self
            .core
            .run(&self.revoke_token, name, RunOptions::default())
            .await
            .is_ok();
        if revoked {
            self.refresh_tokens().await;
        }
        revoked
    }

    pub async fn set_appearance(&self, prefs: AppearancePrefs) {
        if let Ok(stored) = self
            .core
            .run(&self.update_appearance, prefs, RunOptions::default())
            .await
        {
            self.appearance.set(stored);
        }
    }

    pub async fn set_poll_prefs(&self, prefs: PollPrefs) {
        if let Ok(stored) = self
            .core
            .run(&self.update_poll_prefs, prefs, RunOptions::default())
            .await
        {
            self.poll.set(stored);
        }
    }

    /// Untracked: a press handler's ambient owner is arbitrary, and none on desktop.
    pub fn appearance_untracked(&self) -> AppearancePrefs {
        self.appearance.get_untracked()
    }

    pub fn poll_untracked(&self) -> PollPrefs {
        self.poll.get_untracked()
    }
}

impl AsRef<ControllerCore<AppFailure>> for SettingsController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::connect::data::repositories::{
        MockCredentialService, MockSavedServersRepository,
    };
    use crate::features::connect::domain::entities::TrustMode;
    use crate::features::settings::data::repositories::mock_auth_tokens_repository::MOCK_TODAY;
    use crate::features::settings::data::repositories::{
        MockAuthTokensRepository, MockPreferencesRepository,
    };
    use crate::features::settings::domain::entities::{CursorStyle, TokenScope};

    const SEEDED_COUNT: usize = 2;

    const SAVED_ENDPOINT: &str = "rig.example:50051";

    fn controller() -> SettingsController {
        controller_over(MockSavedServersRepository::with_servers(Vec::new()), None)
    }

    fn controller_over(
        servers: MockSavedServersRepository,
        credentials: Option<Arc<MockCredentialService>>,
    ) -> SettingsController {
        controller_with_keyring(servers, credentials, KeyringHandle::new())
    }

    fn controller_with_keyring(
        servers: MockSavedServersRepository,
        credentials: Option<Arc<MockCredentialService>>,
        keyring: KeyringHandle,
    ) -> SettingsController {
        SettingsController::new(
            Arc::new(MockAuthTokensRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
            Arc::new(servers),
            credentials.unwrap_or_else(|| Arc::new(MockCredentialService::new())),
            keyring,
            Arc::new(ConnectionController::detached()),
        )
    }

    fn unlocked_keyring(entries: &[(&str, &str)]) -> KeyringHandle {
        let mut keyring = crate::features::connect::domain::keyring::Keyring::new();
        for (endpoint, token) in entries {
            keyring.insert(*endpoint, Zeroizing::new(token.to_string()));
        }
        let handle = KeyringHandle::new();
        handle.publish(keyring);
        handle
    }

    fn saved_row(has_credential: bool) -> SavedServer {
        SavedServer {
            host: "rig.example".to_string(),
            port: 50051,
            trust_mode: TrustMode::System,
            label: "Rig".to_string(),
            last_used_ms: 1_700_000_000_000,
            read_only: false,
            has_credential,
            // The seeded token store's second row, so a delete has something to revoke.
            token_name: has_credential.then(|| "token_2".to_string()),
            fingerprint: None,
        }
    }

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn the_preference_signals_are_populated_before_any_load() {
        let c = controller();
        assert!(!c.loaded.get_untracked(), "nothing is loaded up front");
        assert_eq!(c.appearance_untracked(), Preferences::defaults().appearance);
        assert_eq!(c.poll_untracked(), Preferences::defaults().poll);
        assert!(c.tokens.get_untracked().is_empty(), "…but the list is");
    }

    #[test]
    fn refresh_loads_the_seeded_tokens_and_prefs_and_flips_loaded() {
        let c = controller();
        block_on(c.refresh());

        assert_eq!(c.tokens.get_untracked().len(), SEEDED_COUNT);
        assert_eq!(c.appearance_untracked(), Preferences::defaults().appearance);
        assert!(c.loaded.get_untracked());
    }

    #[test]
    fn create_round_trips_through_the_repository() {
        let c = controller();
        block_on(c.refresh());

        block_on(c.create_token("laptop".to_string(), false));

        let tokens = c.tokens.get_untracked();
        assert_eq!(tokens.len(), SEEDED_COUNT + 1);
        let minted = tokens.last().expect("the minted token");
        assert_eq!(minted.name, "laptop");
        assert_eq!(minted.scope, TokenScope::ReadWrite);
        assert_eq!(minted.created, MOCK_TODAY);
    }

    #[test]
    fn create_with_the_read_only_toggle_mints_a_read_only_token() {
        let c = controller();
        block_on(c.refresh());

        block_on(c.create_token("phone".to_string(), true));

        let tokens = c.tokens.get_untracked();
        let minted = tokens.last().expect("the minted token");
        assert_eq!(minted.name, "phone");
        assert_eq!(minted.scope, TokenScope::ReadOnly);
    }

    #[test]
    fn create_parks_the_one_time_secret_until_dismissed() {
        let c = controller();
        block_on(c.refresh());
        assert_eq!(c.pending_token_secret.get_untracked(), None);

        assert!(block_on(c.create_token("laptop".to_string(), false)));
        let pending = c
            .pending_token_secret
            .get_untracked()
            .expect("a landed mint parks its secret");
        assert_eq!(pending.name, "laptop");

        c.clear_pending_secret();
        assert_eq!(c.pending_token_secret.get_untracked(), None);
    }

    #[test]
    fn a_rejected_mint_parks_no_secret() {
        let c = controller();
        block_on(c.refresh());

        // The mock rejects a duplicate name.
        assert!(!block_on(c.create_token("token_2".to_string(), false)));
        assert_eq!(c.pending_token_secret.get_untracked(), None);
    }

    #[test]
    fn revoke_removes_the_token() {
        let c = controller();
        block_on(c.refresh());

        block_on(c.revoke_token("token_2".to_string()));

        let names: Vec<String> = c
            .tokens
            .get_untracked()
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names.len(), SEEDED_COUNT - 1);
        assert!(!names.contains(&"token_2".to_string()), "{names:?}");
    }

    #[test]
    fn a_failing_mutation_leaves_the_list_alone() {
        let c = controller();
        block_on(c.refresh());
        let before = c.tokens.get_untracked();

        // The mock maps an unknown name to `Internal`.
        let revoked = block_on(c.revoke_token("nope".to_string()));

        assert!(!revoked, "…and says so, rather than silently succeeding");
        assert_eq!(c.tokens.get_untracked(), before);
    }

    #[test]
    fn a_mutation_reports_whether_it_landed() {
        let c = controller();
        block_on(c.refresh());

        assert!(block_on(c.create_token("laptop".to_string(), false)));
        assert!(
            !block_on(c.create_token("laptop".to_string(), false)),
            "a duplicate mint is rejected by the repository"
        );
        assert!(block_on(c.revoke_token("laptop".to_string())));
        assert!(
            !block_on(c.revoke_token("laptop".to_string())),
            "…and revoking it twice is too"
        );
        assert_eq!(
            c.tokens.get_untracked().len(),
            SEEDED_COUNT,
            "the list is back where it started"
        );
    }

    #[test]
    fn refresh_loads_the_saved_servers_beside_everything_else() {
        let c = controller_over(
            MockSavedServersRepository::with_servers(vec![saved_row(true)]),
            None,
        );
        assert!(c.saved.get_untracked().is_empty(), "nothing before a load");

        block_on(c.refresh());

        let saved = c.saved.get_untracked();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].has_credential);
    }

    #[test]
    fn deleting_a_credential_drops_the_secret_revokes_the_token_and_clears_the_row() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed(SAVED_ENDPOINT, "s3cr3t");
        let c = controller_over(
            MockSavedServersRepository::with_servers(vec![saved_row(true)]),
            Some(Arc::clone(&credentials)),
        );
        block_on(c.refresh());

        assert!(block_on(c.delete_credential(SAVED_ENDPOINT.to_string())));

        assert_eq!(
            credentials.stored(SAVED_ENDPOINT),
            None,
            "the secret is gone"
        );
        let prompts = credentials.prompts();
        assert!(
            !prompts.is_empty() && prompts.iter().all(|reason| reason == "Unlock Rig"),
            "a locked delete runs the keyring unlock rather than leave its vault copy: \
             {prompts:?}"
        );
        let row = &c.saved.get_untracked()[0];
        assert!(!row.has_credential, "…and the row stops claiming one");
        assert_eq!(row.token_name, None);
        assert!(
            !c.tokens
                .get_untracked()
                .iter()
                .any(|token| token.name == "token_2"),
            "the server-side token was revoked with it"
        );
        assert_eq!(
            c.saved.get_untracked().len(),
            1,
            "the saved server itself is kept"
        );
    }

    #[test]
    fn a_refused_eviction_leaves_the_rows_credential_claim_as_it_was() {
        // A row that stops claiming a held credential would hide a secret the user cannot delete.
        let credentials = Arc::new(MockCredentialService::new());
        credentials.set_delete_failure(true);
        let c = controller_over(
            MockSavedServersRepository::with_servers(vec![saved_row(true)]),
            Some(credentials),
        );
        block_on(c.refresh());

        assert!(!block_on(c.delete_credential(SAVED_ENDPOINT.to_string())));

        assert!(c.saved.get_untracked()[0].has_credential);
    }

    #[test]
    fn an_unlocked_delete_rewrites_the_vault_once_and_drops_the_keyrings_copy() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed_vault(&[(SAVED_ENDPOINT, "s3cr3t"), ("other.example:1", "keep")]);
        let keyring = unlocked_keyring(&[(SAVED_ENDPOINT, "s3cr3t"), ("other.example:1", "keep")]);
        let c = controller_with_keyring(
            MockSavedServersRepository::with_servers(vec![saved_row(true)]),
            Some(Arc::clone(&credentials)),
            keyring.clone(),
        );
        block_on(c.refresh());

        assert!(block_on(c.delete_credential(SAVED_ENDPOINT.to_string())));

        assert_eq!(credentials.writes(), 1, "one gated vault rewrite");
        assert!(credentials.prompts().is_empty(), "…and no unlock prompt");
        assert_eq!(
            credentials.vault_endpoints(),
            Some(vec!["other.example:1".to_string()])
        );
        assert_eq!(credentials.stored(SAVED_ENDPOINT), None);
        assert_eq!(keyring.endpoints(), vec!["other.example:1"]);
        assert!(!c.saved.get_untracked()[0].has_credential);
    }

    #[test]
    fn a_refused_vault_rewrite_keeps_the_keyring_and_the_rows_claim() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed_vault(&[(SAVED_ENDPOINT, "s3cr3t")]);
        credentials.set_gate(CredentialStatus::Canceled);
        let keyring = unlocked_keyring(&[(SAVED_ENDPOINT, "s3cr3t")]);
        let c = controller_with_keyring(
            MockSavedServersRepository::with_servers(vec![saved_row(true)]),
            Some(Arc::clone(&credentials)),
            keyring.clone(),
        );
        block_on(c.refresh());

        assert!(!block_on(c.delete_credential(SAVED_ENDPOINT.to_string())));

        assert_eq!(
            credentials.stored(SAVED_ENDPOINT).as_deref(),
            Some("s3cr3t")
        );
        assert_eq!(keyring.endpoints(), vec![SAVED_ENDPOINT]);
        assert!(c.saved.get_untracked()[0].has_credential);
    }

    #[test]
    fn a_locked_forget_takes_the_ungated_delete_and_prompts_nothing() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed(SAVED_ENDPOINT, "s3cr3t");
        let c = controller_over(
            MockSavedServersRepository::with_servers(vec![saved_row(true)]),
            Some(Arc::clone(&credentials)),
        );
        block_on(c.refresh());

        assert!(block_on(c.forget_server(SAVED_ENDPOINT.to_string())));

        assert!(credentials.prompts().is_empty());
        assert_eq!(credentials.writes(), 0);
        assert_eq!(credentials.legacy(SAVED_ENDPOINT), None);
    }

    #[test]
    fn the_saved_rows_mark_the_connected_endpoint_and_close_with_add_server() {
        let rows = saved_server_rows(&[saved_row(true)], Some(SAVED_ENDPOINT));
        assert_eq!(
            rows,
            vec![
                SavedServerRow::Server {
                    server: saved_row(true),
                    active: true,
                },
                SavedServerRow::AddServer,
            ]
        );
        assert_eq!(
            saved_server_rows(&[], None),
            vec![SavedServerRow::AddServer]
        );
        assert!(matches!(
            saved_server_rows(&[saved_row(false)], Some("elsewhere:1"))[0],
            SavedServerRow::Server { active: false, .. }
        ));
    }

    #[test]
    fn forgetting_a_server_takes_its_credential_with_it() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed(SAVED_ENDPOINT, "s3cr3t");
        let c = controller_over(
            MockSavedServersRepository::with_servers(vec![saved_row(true)]),
            Some(Arc::clone(&credentials)),
        );
        block_on(c.refresh());

        assert!(block_on(c.forget_server(SAVED_ENDPOINT.to_string())));

        assert!(c.saved.get_untracked().is_empty());
        assert_eq!(
            credentials.stored(SAVED_ENDPOINT),
            None,
            "a secret filed under a forgotten endpoint would be unreachable forever"
        );
    }

    #[test]
    fn forgetting_a_server_with_no_credential_touches_no_credential_store() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed("someone.else:1", "not-this-one");
        let c = controller_over(
            MockSavedServersRepository::with_servers(vec![saved_row(false)]),
            Some(Arc::clone(&credentials)),
        );
        block_on(c.refresh());

        assert!(block_on(c.forget_server(SAVED_ENDPOINT.to_string())));

        assert!(c.saved.get_untracked().is_empty());
        assert_eq!(
            credentials.stored("someone.else:1").as_deref(),
            Some("not-this-one")
        );
    }

    #[test]
    fn deleting_or_forgetting_an_unknown_endpoint_is_a_no_op() {
        let c = controller_over(
            MockSavedServersRepository::with_servers(vec![saved_row(true)]),
            None,
        );
        block_on(c.refresh());

        assert!(!block_on(c.delete_credential("nope:1".to_string())));
        assert!(!block_on(c.forget_server("nope:1".to_string())));
        assert_eq!(c.saved.get_untracked().len(), 1);
        assert!(c.saved.get_untracked()[0].has_credential);
    }

    #[test]
    fn setting_a_preference_publishes_what_the_repository_stored() {
        let c = controller();
        block_on(c.refresh());

        let next = AppearancePrefs {
            cursor_style: CursorStyle::Bar,
            ..c.appearance_untracked()
        };
        block_on(c.set_appearance(next.clone()));
        assert_eq!(c.appearance_untracked(), next);

        let poll = PollPrefs {
            bell_notifications: false,
            layout_refresh_secs: 10,
        };
        block_on(c.set_poll_prefs(poll));
        assert_eq!(c.poll_untracked(), poll);
    }

    #[test]
    fn a_rejected_preference_write_leaves_the_signal_alone() {
        let c = controller();
        block_on(c.refresh());
        let before = c.appearance_untracked();

        block_on(c.set_appearance(AppearancePrefs {
            font_size_px: 72,
            ..before.clone()
        }));

        assert_eq!(
            c.appearance_untracked(),
            before,
            "the rejected size never reaches the row"
        );
    }

    #[test]
    fn a_preference_survives_a_reload_rather_than_snapping_back() {
        let c = controller();
        block_on(c.refresh());
        block_on(c.set_poll_prefs(PollPrefs {
            layout_refresh_secs: 5,
            bell_notifications: false,
        }));

        block_on(c.refresh());

        assert_eq!(c.poll_untracked().layout_refresh_secs, 5);
        assert!(!c.poll_untracked().bell_notifications);
    }
}
