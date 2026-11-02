//! [`SettingsController`] — the `/settings` screen's view model. A
//! [`ControllerCore`] embedded by composition, the feature's six use cases
//! over its two injected repositories, and the signals the screen renders.
//! Hosted through `clean_signals_frust::use_controller`, created on route
//! mount and disposed on unmount.
//!
//! The connection facts the first group prints live on
//! [`crate::connection::ConnectionController`] instead, since more than one
//! feature reads them. Both preference signals start seeded with
//! [`Preferences::defaults`] rather than an `Option`, so the first frame
//! paints the same rows the loaded frame does; [`loaded`](Self::loaded)
//! still records whether the real read has landed.
//!
//! **Every mutation re-reads**: a create/revoke ends with a fresh
//! [`refresh_tokens`](Self::refresh_tokens) rather than a local patch. A
//! preference save is the exception — the repository *returns* the stored
//! record, so the signal takes that value directly.
//!
//! **`saved`** is the `saved servers` group's rows, over `connect`'s own two
//! stores, resolved through that feature's own `di::expect_*` (the same
//! cross-feature widening the terminal screen makes). It lives here because
//! deleting a credential is *also* a token revocation, which this feature
//! owns. The token revoke that rides along is best-effort — its failure
//! unshown — but the secret itself leaves through the keyring: with the
//! keyring unlocked, one gated vault rewrite without it, then the handle
//! drops it; locked, a forget takes the ungated legacy delete (the row goes
//! too, so the next unlock prunes the vault's copy), while a credential
//! delete keeps the row and so unlocks first rather than claim a vault copy
//! it cannot reach. A refused rewrite leaves the row's credential claim as it
//! was.
//!
//! **Switching servers**: [`switch_server`](SettingsController::switch_server)
//! dials a saved row with its stored token through `connect`'s page-agnostic
//! [`KeyringUnlock`] — from the unlocked keyring, else after its one
//! `unlock_all` prompt — moving the live connection with `switch_to`.
//!
//! **The one-time secret**: [`pending_token_secret`](Self::pending_token_secret)
//! is what the reveal sheet reads — a landed [`create_token`](Self::create_token)
//! parks the daemon's one-time [`MintedToken`] there, and
//! [`clear_pending_secret`](Self::clear_pending_secret) is the reveal's own
//! "dismissed, gone forever" (the cleared `Option`'s dropped `Zeroizing`
//! secret scrubs its buffer right there). No auto-clear on a scrim dismissal:
//! the value sits until the next mint overwrites it or an explicit dismissal
//! clears it.

use std::sync::Arc;

use clean_signals::{ControllerCore, NoParams, RunOptions};
use frust::{GetUntracked, RwSignal, Set, spawn_local};
use zeroize::Zeroizing;

use crate::connection::{ConnectionController, TrustMode};
use crate::failure::AppFailure;
use crate::features::connect::di::{Credentials, KeyringHandle, SavedServers};
use crate::features::connect::domain::entities::{CredentialStatus, SavedServer};
use crate::features::connect::presentation::controller::ConfirmPrompt;
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

/// One row of the `saved servers` group, as the screen renders it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SavedServerRow {
    /// A remembered server; `active` while it is the connected endpoint.
    Server { server: SavedServer, active: bool },
    /// The trailing `+ add server` row.
    AddServer,
}

/// `saved`'s rows, the connected `active` endpoint marked, closed by the
/// `+ add server` row.
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

/// Toast `status`'s message, when it has one to say.
fn report(status: CredentialStatus) {
    if let Some(message) = status.message() {
        app_toasts().show(message);
    }
}

/// The `/settings` screen's view model. See the [module docs](self).
pub struct SettingsController {
    core: ControllerCore<AppFailure>,
    list_tokens: ListTokens,
    create_token: CreateToken,
    revoke_token: RevokeToken,
    load_preferences: LoadPreferences,
    update_appearance: UpdateAppearance,
    update_poll_prefs: UpdatePollPrefs,
    /// The `connect` feature's saved-server list (see the [module docs](self)).
    saved_servers: SavedServers,
    /// …and its credential store.
    credentials: Credentials,
    /// `connect`'s keyring seam: the unlock, the handle and the switch.
    keyring_unlock: KeyringUnlock,
    /// Whether a switch's keyring unlock is in flight — its single-flight
    /// claim (the dial itself is guarded by the connection's own).
    pub switching: RwSignal<bool>,
    /// The token set as last read from the repository — the `auth tokens`
    /// group's rows.
    pub tokens: RwSignal<Vec<AuthToken>>,
    /// The `terminal appearance` group's three values.
    pub appearance: RwSignal<AppearancePrefs>,
    /// The `session poll` group's two values.
    pub poll: RwSignal<PollPrefs>,
    /// Whether the first load has completed (see the [module docs](self)).
    pub loaded: RwSignal<bool>,
    /// The most recently minted token's one-time secret — see the
    /// [module docs](self)' "one-time secret" section.
    pub pending_token_secret: RwSignal<Option<MintedToken>>,
    /// The saved servers this device remembers — the `saved servers` group's
    /// rows, each with whether a credential is stored for it.
    pub saved: RwSignal<Vec<SavedServer>>,
    /// Whether a restore started from the `muxr pro` restore row is in flight
    /// (a purchase running elsewhere does not set it).
    pub restoring: RwSignal<bool>,
}

impl SettingsController {
    /// A fresh controller over the four injected stores — built once by the
    /// composition root (see [`crate::features::settings::di`] and, for the
    /// two from `connect`, that feature's own `di`) — plus the app-scoped
    /// keyring and connection a switch moves.
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

    /// This controller's [`ControllerCore`] — the failure sink the screen
    /// subscribes its toast bridge to (`use_failure_listener`). A named
    /// accessor rather than the [`AsRef`] impl below because
    /// `Arc<SettingsController>` has its own inherent `as_ref`, which would
    /// make the bare call ambiguous at every site.
    pub fn core(&self) -> &ControllerCore<AppFailure> {
        &self.core
    }

    /// Load everything this screen renders from its own repositories: the
    /// token list and both preference groups. The screen's `init` fires one of
    /// these.
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

    /// Re-read the saved-server list — the same re-read-rather-than-patch
    /// discipline the token list follows; every credential mutation below
    /// ends with one. A failure leaves the previous list in place and raises
    /// nothing: a screen that cannot list saved servers is still a screen
    /// whose other four groups work.
    pub fn refresh_saved_servers(&self) {
        if let Ok(servers) = self.saved_servers.list() {
            self.saved.set(servers);
        }
    }

    /// Delete the stored credential for `endpoint`, and the server-side token
    /// behind it, keeping the saved server. A locked keyring is unlocked
    /// first (one prompt; a refusal toasts and deletes nothing — see the
    /// [module docs](self)). Order: evict the secret, then revoke the token
    /// best-effort and clear the row's credential fields — both only once the
    /// eviction landed, so a refused vault rewrite leaves the credential, its
    /// token and the row's claim exactly as they were.
    /// Answers whether the whole thing succeeded, which the screen's
    /// confirmation toast keys off.
    pub async fn delete_credential(&self, endpoint: String) -> bool {
        let Some(mut server) = self.saved_row(&endpoint) else {
            return false;
        };
        if !self.keyring().is_unlocked() {
            let saved = self.saved_servers.list().unwrap_or_default();
            if let KeyringUnlockOutcome::Refused(status) = self.keyring_unlock.unlock(&saved).await
            {
                report(status);
                return false;
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

    /// Remove the saved server at `endpoint` entirely — its stored credential
    /// with it, since it's filed under this same endpoint key and would
    /// otherwise be unreachable forever once the row stops being listed.
    /// Never prompts to unlock: locked, the ungated legacy delete runs and
    /// the vault's copy is pruned by the next unlock, the row being gone. A
    /// failed eviction is emitted and the row still goes.
    pub async fn forget_server(&self, endpoint: String) -> bool {
        let Some(server) = self.saved_row(&endpoint) else {
            return false;
        };
        if server.has_credential || self.keyring().token_for(&endpoint).is_some() {
            if let Some(name) = server.token_name.clone() {
                self.revoke_quietly(name).await;
            }
            let evicted = if self.keyring().is_unlocked() {
                self.evict_unlocked(&endpoint).await
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

    /// The app-scoped keyring handle.
    fn keyring(&self) -> &KeyringHandle {
        self.keyring_unlock.keyring()
    }

    /// The unlocked eviction: one gated vault rewrite without `endpoint`
    /// (over a snapshot, never the handle's own lock), then the handle drops
    /// it too — only once the rewrite landed.
    async fn evict_unlocked(&self, endpoint: &str) -> Result<(), AppFailure> {
        let mut snapshot = self.keyring().snapshot();
        self.credentials
            .delete_from_vault(endpoint.to_string(), &mut snapshot)
            .await?;
        self.keyring().remove(endpoint);
        Ok(())
    }

    /// Move the live connection to the saved server at `endpoint` with its
    /// stored token, then run `on_switched`. Unlocked, the token comes
    /// straight off the keyring; locked, one `unlock_all` prompt publishes
    /// it first (a refusal toasts; a cancel is silent). A
    /// [`TrustMode::NoVerification`] row asks `confirm` before its token is
    /// sent. The free-tier verdict is the caller's, ahead of this. Ignored
    /// while a switch's unlock is already in flight.
    pub fn switch_server(
        self: &Arc<Self>,
        endpoint: String,
        confirm: ConfirmPrompt,
        on_switched: Box<dyn FnOnce()>,
    ) {
        if self.switching.get_untracked() {
            return;
        }
        let Some(server) = self.saved_row(&endpoint) else {
            return;
        };
        if self.keyring().is_unlocked() {
            self.dial_saved(server, confirm, on_switched);
            return;
        }
        self.switching.set(true);
        let controller = Arc::clone(self);
        spawn_local(async move {
            let saved = controller.saved_servers.list().unwrap_or_default();
            let outcome = controller.keyring_unlock.unlock(&saved).await;
            controller.switching.set(false);
            controller.refresh_saved_servers();
            match outcome {
                KeyringUnlockOutcome::Unlocked { .. } => {
                    controller.dial_saved(server, confirm, on_switched);
                }
                KeyringUnlockOutcome::Refused(status) => report(status),
            }
        });
    }

    /// [`Self::switch_server`]'s dial, the keyring already open.
    fn dial_saved(
        self: &Arc<Self>,
        server: SavedServer,
        confirm: ConfirmPrompt,
        on_switched: Box<dyn FnOnce()>,
    ) {
        let Some(token) = self.keyring().token_for(&server.endpoint()) else {
            report(CredentialStatus::NotFound);
            return;
        };
        if server.trust_mode != TrustMode::NoVerification {
            self.switch_now(&server, token, on_switched);
            return;
        }
        let controller = Arc::clone(self);
        let row = server.clone();
        confirm(
            server,
            Box::new(move || controller.switch_now(&row, token, on_switched)),
        );
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

    /// The saved row for `endpoint`, as last read from the store.
    fn saved_row(&self, endpoint: &str) -> Option<SavedServer> {
        self.saved_servers
            .list()
            .ok()?
            .into_iter()
            .find(|server| server.endpoint() == endpoint)
    }

    /// Write `server` back with no credential on it, and re-read the list.
    fn clear_credential_fields(&self, mut server: SavedServer) {
        server.has_credential = false;
        server.token_name = None;
        if let Err(failure) = self.saved_servers.save(server) {
            self.core.failures().emit(&failure);
        }
        self.refresh_saved_servers();
    }

    /// Revoke `name` without reporting a failure — the best-effort half of a
    /// credential delete (see the [module docs](self)). A revoke needs a live,
    /// read-write session; the delete it accompanies must not depend on one.
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

    /// Re-read the token list. Every token mutation ends with one of these.
    pub async fn refresh_tokens(&self) {
        if let Ok(tokens) = self
            .core
            .run(&self.list_tokens, NoParams, RunOptions::default())
            .await
        {
            self.tokens.set(tokens);
        }
    }

    /// Mint a token named `name` (already trimmed by the dialog) with the
    /// requested `read_only` scope, then refresh. A landed mint parks the
    /// daemon's one-time secret on [`Self::pending_token_secret`] (see the
    /// [module docs](self)); a failed one leaves the list untouched and
    /// surfaces through [`failures`](ControllerCore::failures). Answers
    /// whether it landed, since a "Created …" toast beside a failure toast
    /// for the same call would be a straight lie.
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

    /// Dismiss the pending secret reveal — "gone forever" from here on. See
    /// the [module docs](self)' "one-time secret" section.
    pub fn clear_pending_secret(&self) {
        self.pending_token_secret.set(None);
    }

    /// Revoke the token called `name`, then refresh. Answers whether it
    /// landed — see [`create_token`](Self::create_token).
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

    /// Persist a new `terminal appearance` record and publish it. The
    /// repository's own returned value is what lands in the signal, so the
    /// screen shows what was stored rather than what was asked for.
    pub async fn set_appearance(&self, prefs: AppearancePrefs) {
        if let Ok(stored) = self
            .core
            .run(&self.update_appearance, prefs, RunOptions::default())
            .await
        {
            self.appearance.set(stored);
        }
    }

    /// Persist a new `session poll` record and publish it.
    pub async fn set_poll_prefs(&self, prefs: PollPrefs) {
        if let Ok(stored) = self
            .core
            .run(&self.update_poll_prefs, prefs, RunOptions::default())
            .await
        {
            self.poll.set(stored);
        }
    }

    /// The current appearance record, read **without** tracking — what a press
    /// handler builds its "next" record from (a tracked read there would
    /// subscribe whatever owner happens to be ambient, which on desktop is
    /// none at all).
    pub fn appearance_untracked(&self) -> AppearancePrefs {
        self.appearance.get_untracked()
    }

    /// The current poll record, read without tracking. See
    /// [`appearance_untracked`](Self::appearance_untracked).
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

    /// How many tokens the mock repository seeds (the redesign's two rows).
    const SEEDED_COUNT: usize = 2;

    /// The endpoint the credential cases operate on.
    const SAVED_ENDPOINT: &str = "rig.example:50051";

    /// The test-wiring composition root: the four mocks behind their domain
    /// traits. `#[cfg(test)]`-only wiring, so `di.rs` stays the only production
    /// site where presentation meets `data` (huddle's convention).
    fn controller() -> SettingsController {
        controller_over(MockSavedServersRepository::with_servers(Vec::new()), None)
    }

    /// A controller over an explicit saved-server store, and optionally a
    /// credential service the caller keeps a handle on.
    fn controller_over(
        servers: MockSavedServersRepository,
        credentials: Option<Arc<MockCredentialService>>,
    ) -> SettingsController {
        controller_with_keyring(servers, credentials, KeyringHandle::new())
    }

    /// [`controller_over`] sharing `keyring` (locked unless the caller
    /// publishes into it).
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

    /// A keyring unlocked over exactly `entries`.
    fn unlocked_keyring(entries: &[(&str, &str)]) -> KeyringHandle {
        let mut keyring = crate::features::connect::domain::keyring::Keyring::new();
        for (endpoint, token) in entries {
            keyring.insert(*endpoint, Zeroizing::new(token.to_string()));
        }
        let handle = KeyringHandle::new();
        handle.publish(keyring);
        handle
    }

    /// A saved row for [`SAVED_ENDPOINT`], with or without a stored credential.
    fn saved_row(has_credential: bool) -> SavedServer {
        SavedServer {
            host: "rig.example".to_string(),
            port: 50051,
            trust_mode: TrustMode::System,
            label: "Rig".to_string(),
            last_used_ms: 1_700_000_000_000,
            read_only: false,
            has_credential,
            // The seeded token store's second row — so a delete has something
            // real to revoke.
            token_name: has_credential.then(|| "token_2".to_string()),
            fingerprint: None,
        }
    }

    /// Drive a controller call to completion on a current-thread runtime.
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

        // The mock rejects a duplicate name — the one reachable failure here.
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

        // No such token — the mock maps an unknown name to `Internal`.
        let revoked = block_on(c.revoke_token("nope".to_string()));

        assert!(!revoked, "…and says so, rather than silently succeeding");
        assert_eq!(c.tokens.get_untracked(), before);
    }

    #[test]
    fn a_mutation_reports_whether_it_landed() {
        // What the screen's confirmation toasts key off: a "Created …" raised
        // beside a failure toast for the same call would contradict it.
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

    // -----------------------------------------------------------------------
    // Saved servers and their credentials
    // -----------------------------------------------------------------------

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
        // A row that stopped claiming a credential the store still holds
        // would hide a secret the user can no longer reach to delete.
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
        // The whole point of the round trip: the picker writes through the
        // repository, so a later `refresh` reads the picked value back rather
        // than the seed.
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
