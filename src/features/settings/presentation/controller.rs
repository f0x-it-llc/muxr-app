//! The settings view models: [`SettingsController`] for the connected server's tokens,
//! [`SavedServersController`] for the device's saved servers, and [`AppSettingsController`] for
//! device-wide preferences. Only the last two serve with no server connected.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use clean_signals::{ControllerCore, NoParams, RunOptions};
use frust::{Get, GetUntracked, RwSignal, Set, spawn_local};
use zeroize::Zeroizing;

use crate::connection::ConnectionController;
use crate::failure::AppFailure;
use crate::features::connect::di::{Credentials, KeyringHandle, SavedServers};
use crate::features::connect::domain::entities::{CredentialStatus, SavedServer};
use crate::features::connect::presentation::controller::SAVED_LIST_UNAVAILABLE_MESSAGE;
use crate::features::connect::presentation::{KeyringUnlock, KeyringUnlockOutcome};
use crate::features::settings::domain::entities::{
    AppearancePrefs, AuthToken, CreateTokenRequest, MintedToken, PollPrefs, Preferences, TokenScope,
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

/// `active` marks the connected endpoint; `None` marks no row.
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

/// The token list's scope segments, in their on-screen order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScopeFilter {
    #[default]
    All,
    ReadWrite,
    ReadOnly,
}

impl ScopeFilter {
    pub const SEGMENTS: [ScopeFilter; 3] = [
        ScopeFilter::All,
        ScopeFilter::ReadWrite,
        ScopeFilter::ReadOnly,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ScopeFilter::All => "all",
            ScopeFilter::ReadWrite => TokenScope::ReadWrite.label(),
            ScopeFilter::ReadOnly => TokenScope::ReadOnly.label(),
        }
    }

    pub fn index(self) -> usize {
        Self::SEGMENTS
            .iter()
            .position(|segment| *segment == self)
            .unwrap_or(0)
    }

    fn admits(self, scope: TokenScope) -> bool {
        match self {
            ScopeFilter::All => true,
            ScopeFilter::ReadWrite => scope == TokenScope::ReadWrite,
            ScopeFilter::ReadOnly => scope == TokenScope::ReadOnly,
        }
    }
}

/// A case-insensitive substring of the name, combined with the segment; a blank query admits all.
pub fn token_shown(name: &str, scope: TokenScope, query: &str, segment: ScopeFilter) -> bool {
    let query = query.trim().to_lowercase();
    segment.admits(scope) && (query.is_empty() || name.to_lowercase().contains(&query))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRow {
    pub token: AuthToken,
    /// The token this device signed in with.
    pub this_device: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenListView {
    pub rows: Vec<TokenRow>,
    /// Per segment, in [`ScopeFilter::SEGMENTS`] order; the query never narrows them.
    pub counts: [usize; 3],
    pub total: usize,
    /// A query or a segment other than `all` is active.
    pub narrowed: bool,
}

/// The server's order, except that the row named `device_token` is pinned first.
pub fn token_list_view(
    tokens: &[AuthToken],
    query: &str,
    segment: ScopeFilter,
    device_token: Option<&str>,
) -> TokenListView {
    let counts = ScopeFilter::SEGMENTS.map(|segment| {
        tokens
            .iter()
            .filter(|token| segment.admits(token.scope))
            .count()
    });
    let mine = |token: &AuthToken| device_token == Some(token.name.as_str());
    let (pinned, rest): (Vec<&AuthToken>, Vec<&AuthToken>) =
        tokens.iter().partition(|token| mine(token));
    let rows = pinned
        .into_iter()
        .chain(rest)
        .filter(|token| token_shown(&token.name, token.scope, query, segment))
        .map(|token| TokenRow {
            token: token.clone(),
            this_device: mine(token),
        })
        .collect();
    TokenListView {
        rows,
        counts,
        total: tokens.len(),
        narrowed: !query.trim().is_empty() || segment != ScopeFilter::All,
    }
}

/// What a screen shows of the token list: only a list read under the live connection.
pub struct TokenSnapshot {
    pub tokens: Vec<AuthToken>,
    pub loaded: bool,
    pub failed: bool,
}

pub struct SettingsController {
    core: ControllerCore<AppFailure>,
    list_tokens: ListTokens,
    create_token: CreateToken,
    revoke_token: RevokeToken,
    pub tokens: RwSignal<Vec<AuthToken>>,
    pub loaded: RwSignal<bool>,
    /// The last token list read failed; a later success clears it.
    pub load_failed: RwSignal<bool>,
    /// The connection epoch [`Self::tokens`] was read under, once one has been.
    pub list_epoch: RwSignal<Option<u64>>,
    /// Bumped by every [`Self::load_for`], so a reply to a superseded read is dropped.
    generation: AtomicU64,
    /// A minted secret stays here until the next mint or an explicit dismissal clears it.
    pub pending_token_secret: RwSignal<Option<MintedToken>>,
}

impl SettingsController {
    pub fn new(tokens: Arc<dyn AuthTokensRepository + Send + Sync>) -> Self {
        Self {
            core: ControllerCore::new(),
            list_tokens: ListTokens::new(Arc::clone(&tokens)),
            create_token: CreateToken::new(Arc::clone(&tokens)),
            revoke_token: RevokeToken::new(tokens),
            tokens: RwSignal::new(Vec::new()),
            loaded: RwSignal::new(false),
            load_failed: RwSignal::new(false),
            list_epoch: RwSignal::new(None),
            generation: AtomicU64::new(0),
            pending_token_secret: RwSignal::new(None),
        }
    }

    /// Named rather than [`AsRef`], which `Arc<Self>`'s inherent `as_ref` makes ambiguous.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    pub async fn refresh(&self) {
        if self.refresh_tokens().await {
            self.loaded.set(true);
        }
    }

    /// Tracked. Under any other connection than `epoch` the list reads as still loading.
    pub fn snapshot_for(&self, epoch: u64) -> TokenSnapshot {
        if self.list_epoch.get() != Some(epoch) {
            return TokenSnapshot {
                tokens: Vec::new(),
                loaded: false,
                failed: false,
            };
        }
        TokenSnapshot {
            tokens: self.tokens.get(),
            loaded: self.loaded.get(),
            failed: self.load_failed.get(),
        }
    }

    /// Reads the list for connection `epoch`, superseding every read still in flight; a failed
    /// read for a new connection leaves no other server's token behind.
    pub async fn load_for(&self, epoch: u64) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let result = self
            .core
            .run(&self.list_tokens, NoParams, RunOptions::default())
            .await;
        if self.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        match result {
            Ok(tokens) => {
                self.tokens.set(tokens);
                self.load_failed.set(false);
            }
            Err(_) => {
                if self.list_epoch.try_get_untracked().flatten() != Some(epoch) {
                    self.tokens.set(Vec::new());
                }
                self.load_failed.set(true);
            }
        }
        self.list_epoch.set(Some(epoch));
        self.loaded.set(true);
    }

    /// `false` when a [`Self::load_for`] started meanwhile, whose answer then stands.
    pub async fn refresh_tokens(&self) -> bool {
        let generation = self.generation.load(Ordering::SeqCst);
        let result = self
            .core
            .run(&self.list_tokens, NoParams, RunOptions::default())
            .await;
        if self.generation.load(Ordering::SeqCst) != generation {
            return false;
        }
        match result {
            Ok(tokens) => {
                self.tokens.set(tokens);
                self.load_failed.set(false);
            }
            Err(_) => self.load_failed.set(true),
        }
        true
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
}

impl AsRef<ControllerCore<AppFailure>> for SettingsController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

/// The device's saved servers: switch, delete credential and forget, none of which needs a
/// connection. A row's token is revoked only over a live session to that same endpoint, since
/// the live session is the only one a revoke can reach.
pub struct SavedServersController {
    core: ControllerCore<AppFailure>,
    revoke_token: RevokeToken,
    saved_servers: SavedServers,
    credentials: Credentials,
    keyring_unlock: KeyringUnlock,
    connection: Arc<ConnectionController>,
    /// The single-flight claim of a switch's keyring unlock; the dial has the connection's own.
    pub switching: RwSignal<bool>,
    pub saved: RwSignal<Vec<SavedServer>>,
}

impl SavedServersController {
    pub fn new(
        tokens: Arc<dyn AuthTokensRepository + Send + Sync>,
        saved_servers: SavedServers,
        credentials: Credentials,
        keyring: KeyringHandle,
        connection: Arc<ConnectionController>,
    ) -> Self {
        let keyring_unlock = KeyringUnlock::new(
            Arc::clone(&saved_servers),
            Arc::clone(&credentials),
            Arc::clone(&connection),
            keyring,
        );
        Self {
            core: ControllerCore::new(),
            revoke_token: RevokeToken::new(tokens),
            saved_servers,
            credentials,
            keyring_unlock,
            connection,
            switching: RwSignal::new(false),
            saved: RwSignal::new(Vec::new()),
        }
    }

    /// Named rather than [`AsRef`], which `Arc<Self>`'s inherent `as_ref` makes ambiguous.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    /// A failure keeps the previous list and raises nothing.
    pub fn refresh_saved_servers(&self) {
        if let Ok(servers) = self.saved_servers.list() {
            self.saved.set(servers);
        }
    }

    /// Untracked: decides a revoke or a confirm's copy, never a build.
    pub fn is_connected_endpoint(&self, endpoint: &str) -> bool {
        self.connection.connected.get_untracked()
            && self
                .connection
                .server_info
                .get_untracked()
                .is_some_and(|info| info.endpoint == endpoint)
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
        self.revoke_if_connected(&server).await;
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
            self.revoke_if_connected(&server).await;
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
    async fn revoke_if_connected(&self, server: &SavedServer) {
        let Some(name) = server.token_name.clone() else {
            return;
        };
        if !self.is_connected_endpoint(&server.endpoint()) {
            return;
        }
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
    }
}

impl AsRef<ControllerCore<AppFailure>> for SavedServersController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

/// Reads and writes only the device's preference store, so it serves with no server connected.
pub struct AppSettingsController {
    core: ControllerCore<AppFailure>,
    load_preferences: LoadPreferences,
    update_appearance: UpdateAppearance,
    update_poll_prefs: UpdatePollPrefs,
    pub appearance: RwSignal<AppearancePrefs>,
    pub poll: RwSignal<PollPrefs>,
    pub loaded: RwSignal<bool>,
    /// Set only by a restore started from the `muxr pro` row.
    pub restoring: RwSignal<bool>,
}

impl AppSettingsController {
    pub fn new(preferences: Arc<dyn PreferencesRepository + Send + Sync>) -> Self {
        let defaults = Preferences::defaults();
        Self {
            core: ControllerCore::new(),
            load_preferences: LoadPreferences::new(Arc::clone(&preferences)),
            update_appearance: UpdateAppearance::new(Arc::clone(&preferences)),
            update_poll_prefs: UpdatePollPrefs::new(preferences),
            appearance: RwSignal::new(defaults.appearance),
            poll: RwSignal::new(defaults.poll),
            loaded: RwSignal::new(false),
            restoring: RwSignal::new(false),
        }
    }

    /// Named rather than [`AsRef`], which `Arc<Self>`'s inherent `as_ref` makes ambiguous.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    pub async fn refresh(&self) {
        if let Ok(prefs) = self
            .core
            .run(&self.load_preferences, NoParams, RunOptions::default())
            .await
        {
            self.appearance.set(prefs.appearance);
            self.poll.set(prefs.poll);
        }
        self.loaded.set(true);
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

impl AsRef<ControllerCore<AppFailure>> for AppSettingsController {
    fn as_ref(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ConnectTarget;
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
        SettingsController::new(Arc::new(MockAuthTokensRepository::seeded()))
    }

    struct ServersRig {
        servers: SavedServersController,
        tokens: Arc<MockAuthTokensRepository>,
    }

    fn servers_over(
        rows: Vec<SavedServer>,
        credentials: Option<Arc<MockCredentialService>>,
    ) -> ServersRig {
        servers_with(rows, credentials, KeyringHandle::new(), None)
    }

    /// `connected_to` dials the mock connection first, so its endpoint is the live one.
    fn servers_with(
        rows: Vec<SavedServer>,
        credentials: Option<Arc<MockCredentialService>>,
        keyring: KeyringHandle,
        connected_to: Option<(&str, u16)>,
    ) -> ServersRig {
        let connection = Arc::new(ConnectionController::detached());
        if let Some((host, port)) = connected_to {
            connection.connect(ConnectTarget {
                host: host.to_string(),
                port,
                trust_mode: TrustMode::System,
                label: host.to_string(),
                offer_credential: false,
                token: Zeroizing::new(String::new()),
                remember: false,
                fingerprint: None,
            });
        }
        let tokens = Arc::new(MockAuthTokensRepository::seeded());
        ServersRig {
            servers: SavedServersController::new(
                Arc::clone(&tokens) as _,
                Arc::new(MockSavedServersRepository::with_servers(rows)),
                credentials.unwrap_or_else(|| Arc::new(MockCredentialService::new())),
                keyring,
                connection,
            ),
            tokens,
        }
    }

    fn token_kept(rig: &ServersRig) -> bool {
        rig.tokens
            .snapshot()
            .iter()
            .any(|token| token.name == "token_2")
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

    fn app_controller() -> AppSettingsController {
        AppSettingsController::new(Arc::new(MockPreferencesRepository::seeded()))
    }

    #[test]
    fn the_preference_signals_are_populated_before_any_load() {
        let c = app_controller();
        assert!(!c.loaded.get_untracked(), "nothing is loaded up front");
        assert_eq!(c.appearance_untracked(), Preferences::defaults().appearance);
        assert_eq!(c.poll_untracked(), Preferences::defaults().poll);
        assert!(!controller().loaded.get_untracked());
        assert!(
            controller().tokens.get_untracked().is_empty(),
            "…nor the list"
        );
    }

    #[test]
    fn refresh_loads_the_seeded_tokens_and_flips_loaded() {
        let c = controller();
        block_on(c.refresh());

        assert_eq!(c.tokens.get_untracked().len(), SEEDED_COUNT);
        assert!(c.loaded.get_untracked());
    }

    #[test]
    fn the_app_refresh_loads_the_seeded_prefs_and_flips_loaded() {
        let c = app_controller();
        block_on(c.refresh());

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

    fn token(name: &str, scope: TokenScope) -> AuthToken {
        AuthToken {
            name: name.to_string(),
            scope,
            created: "2026-11-02".to_string(),
        }
    }

    fn names(view: &TokenListView) -> Vec<&str> {
        view.rows
            .iter()
            .map(|row| row.token.name.as_str())
            .collect()
    }

    fn fleet() -> Vec<AuthToken> {
        vec![
            token("ed-laptop", TokenScope::ReadWrite),
            token("CI-runner", TokenScope::ReadWrite),
            token("demo-viewer", TokenScope::ReadOnly),
            token("ed-iphone", TokenScope::ReadWrite),
        ]
    }

    #[test]
    fn the_filter_is_a_case_insensitive_substring_of_the_name() {
        let view = token_list_view(&fleet(), "  ci ", ScopeFilter::All, None);
        assert_eq!(names(&view), ["CI-runner"]);
        assert!(view.narrowed);
        let view = token_list_view(&fleet(), "ED-", ScopeFilter::All, None);
        assert_eq!(
            names(&view),
            ["ed-laptop", "ed-iphone"],
            "server order kept"
        );
    }

    #[test]
    fn segments_narrow_by_scope_and_combine_with_the_query() {
        let all = token_list_view(&fleet(), "", ScopeFilter::All, None);
        assert_eq!(all.counts, [4, 3, 1]);
        assert!(!all.narrowed);
        let read_only = token_list_view(&fleet(), "", ScopeFilter::ReadOnly, None);
        assert_eq!(names(&read_only), ["demo-viewer"]);
        assert!(read_only.narrowed);
        let both = token_list_view(&fleet(), "ed", ScopeFilter::ReadWrite, None);
        assert_eq!(names(&both), ["ed-laptop", "ed-iphone"]);
        assert_eq!(both.counts, [4, 3, 1], "the query never narrows the counts");
        assert!(
            token_list_view(&fleet(), "zz", ScopeFilter::All, None)
                .rows
                .is_empty()
        );
    }

    #[test]
    fn the_device_token_is_pinned_first_and_marked_and_an_unknown_one_pins_nothing() {
        let view = token_list_view(&fleet(), "", ScopeFilter::All, Some("ed-iphone"));
        assert_eq!(
            names(&view),
            ["ed-iphone", "ed-laptop", "CI-runner", "demo-viewer"]
        );
        assert!(view.rows[0].this_device);
        assert!(view.rows[1..].iter().all(|row| !row.this_device));
        for device in [None, Some("gone")] {
            let view = token_list_view(&fleet(), "", ScopeFilter::All, device);
            assert_eq!(names(&view)[0], "ed-laptop");
            assert!(view.rows.iter().all(|row| !row.this_device));
        }
    }

    #[test]
    fn a_list_shows_only_under_the_connection_it_was_read_for() {
        let c = controller();
        block_on(c.load_for(3));
        assert_eq!(c.snapshot_for(3).tokens.len(), SEEDED_COUNT);
        let other = c.snapshot_for(4);
        assert!(other.tokens.is_empty() && !other.loaded && !other.failed);
    }

    #[test]
    fn a_refresh_overtaken_by_a_reload_never_lands() {
        let c = controller();
        let (refreshed, ()) = block_on(async { tokio::join!(c.refresh_tokens(), c.load_for(2)) });
        assert!(!refreshed, "the reload started meanwhile owns the list");
        assert_eq!(c.list_epoch.get_untracked(), Some(2));
    }

    #[test]
    fn refresh_loads_the_saved_servers() {
        let rig = servers_over(vec![saved_row(true)], None);
        let c = &rig.servers;
        assert!(c.saved.get_untracked().is_empty(), "nothing before a load");

        c.refresh_saved_servers();

        let saved = c.saved.get_untracked();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].has_credential);
    }

    #[test]
    fn deleting_a_credential_drops_the_secret_and_clears_the_row() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed(SAVED_ENDPOINT, "s3cr3t");
        let rig = servers_over(vec![saved_row(true)], Some(Arc::clone(&credentials)));
        let c = &rig.servers;
        c.refresh_saved_servers();

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
        assert_eq!(
            c.saved.get_untracked().len(),
            1,
            "the saved server itself is kept"
        );
    }

    #[test]
    fn a_delete_or_forget_for_another_endpoint_never_revokes_over_the_live_session() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed(SAVED_ENDPOINT, "s3cr3t");
        let rig = servers_with(
            vec![saved_row(true)],
            Some(Arc::clone(&credentials)),
            KeyringHandle::new(),
            Some(("elsewhere.example", 7777)),
        );
        assert!(!rig.servers.is_connected_endpoint(SAVED_ENDPOINT));

        assert!(block_on(
            rig.servers.delete_credential(SAVED_ENDPOINT.to_string())
        ));
        assert!(
            token_kept(&rig),
            "the live server's token_2 is not this row's"
        );

        let rig = servers_with(
            vec![saved_row(true)],
            Some(credentials),
            KeyringHandle::new(),
            None,
        );
        assert!(block_on(
            rig.servers.forget_server(SAVED_ENDPOINT.to_string())
        ));
        assert!(
            token_kept(&rig),
            "nothing is connected, so nothing is revoked"
        );
    }

    #[test]
    fn a_delete_or_forget_on_the_connected_endpoint_revokes_its_token() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed(SAVED_ENDPOINT, "s3cr3t");
        let rig = servers_with(
            vec![saved_row(true)],
            Some(Arc::clone(&credentials)),
            KeyringHandle::new(),
            Some(("rig.example", 50051)),
        );
        assert!(rig.servers.is_connected_endpoint(SAVED_ENDPOINT));

        assert!(block_on(
            rig.servers.delete_credential(SAVED_ENDPOINT.to_string())
        ));
        assert!(
            !token_kept(&rig),
            "the server-side token was revoked with it"
        );

        credentials.seed(SAVED_ENDPOINT, "s3cr3t");
        let rig = servers_with(
            vec![saved_row(true)],
            Some(credentials),
            KeyringHandle::new(),
            Some(("rig.example", 50051)),
        );
        assert!(block_on(
            rig.servers.forget_server(SAVED_ENDPOINT.to_string())
        ));
        assert!(!token_kept(&rig));
    }

    #[test]
    fn a_refused_eviction_leaves_the_rows_credential_claim_as_it_was() {
        // A row that stops claiming a held credential would hide a secret the user cannot delete.
        let credentials = Arc::new(MockCredentialService::new());
        credentials.set_delete_failure(true);
        let rig = servers_over(vec![saved_row(true)], Some(credentials));
        let c = &rig.servers;
        c.refresh_saved_servers();

        assert!(!block_on(c.delete_credential(SAVED_ENDPOINT.to_string())));

        assert!(c.saved.get_untracked()[0].has_credential);
    }

    #[test]
    fn an_unlocked_delete_rewrites_the_vault_once_and_drops_the_keyrings_copy() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed_vault(&[(SAVED_ENDPOINT, "s3cr3t"), ("other.example:1", "keep")]);
        let keyring = unlocked_keyring(&[(SAVED_ENDPOINT, "s3cr3t"), ("other.example:1", "keep")]);
        let rig = servers_with(
            vec![saved_row(true)],
            Some(Arc::clone(&credentials)),
            keyring.clone(),
            None,
        );
        let c = &rig.servers;
        c.refresh_saved_servers();

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
        let rig = servers_with(
            vec![saved_row(true)],
            Some(Arc::clone(&credentials)),
            keyring.clone(),
            None,
        );
        let c = &rig.servers;
        c.refresh_saved_servers();

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
        let rig = servers_over(vec![saved_row(true)], Some(Arc::clone(&credentials)));

        assert!(block_on(
            rig.servers.forget_server(SAVED_ENDPOINT.to_string())
        ));

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
        assert!(matches!(
            saved_server_rows(&[saved_row(true)], None)[0],
            SavedServerRow::Server { active: false, .. }
        ));
    }

    #[test]
    fn forgetting_a_server_takes_its_credential_with_it() {
        let credentials = Arc::new(MockCredentialService::new());
        credentials.seed(SAVED_ENDPOINT, "s3cr3t");
        let rig = servers_over(vec![saved_row(true)], Some(Arc::clone(&credentials)));
        let c = &rig.servers;
        c.refresh_saved_servers();

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
        let rig = servers_over(vec![saved_row(false)], Some(Arc::clone(&credentials)));
        let c = &rig.servers;
        c.refresh_saved_servers();

        assert!(block_on(c.forget_server(SAVED_ENDPOINT.to_string())));

        assert!(c.saved.get_untracked().is_empty());
        assert_eq!(
            credentials.stored("someone.else:1").as_deref(),
            Some("not-this-one")
        );
    }

    #[test]
    fn deleting_or_forgetting_an_unknown_endpoint_is_a_no_op() {
        let rig = servers_over(vec![saved_row(true)], None);
        let c = &rig.servers;
        c.refresh_saved_servers();

        assert!(!block_on(c.delete_credential("nope:1".to_string())));
        assert!(!block_on(c.forget_server("nope:1".to_string())));
        assert_eq!(c.saved.get_untracked().len(), 1);
        assert!(c.saved.get_untracked()[0].has_credential);
    }

    #[test]
    fn setting_a_preference_publishes_what_the_repository_stored() {
        let c = app_controller();
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
        let c = app_controller();
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
        let c = app_controller();
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
