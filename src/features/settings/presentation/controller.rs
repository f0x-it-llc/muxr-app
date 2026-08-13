//! [`SettingsController`] — the `/settings` screen's view model.
//!
//! Mirrors `examples/huddle`'s controller shape (and this app's own
//! `SessionsController`): a [`ControllerCore`] embedded by composition, the
//! feature's six use cases constructed over its two injected repositories, and
//! the signals the screen renders. Hosted by the screen through
//! `clean_signals_frust::use_controller`, so it is created on route mount and
//! disposed on unmount.
//!
//! # What is a signal here, and what is not
//!
//! [`tokens`](Self::tokens), [`appearance`](Self::appearance),
//! [`poll`](Self::poll) and [`loaded`](Self::loaded) are `RwSignal`s — they are
//! observed state, and writing one is what wakes the screen's rebuild. The
//! connection facts the screen's first group prints are **not** here at all:
//! they live on [`crate::connection::ConnectionController`], because whether
//! the app is connected gates routing itself and more than one feature reads
//! it (see that module's docs).
//!
//! # The preference signals start populated, and that is deliberate
//!
//! Both preference signals are seeded with
//! [`Preferences::defaults`] rather than an `Option`: those defaults *are* what
//! the mock repository holds, so the screen's first frame paints the same rows
//! the loaded frame does instead of four blank values that fill in a beat
//! later. [`loaded`](Self::loaded) still records whether the real read has
//! landed, for a test (or a later skeleton state) that needs to know.
//!
//! # Why every mutation re-reads
//!
//! A create/revoke ends with a fresh [`refresh_tokens`](Self::refresh_tokens)
//! rather than a local patch, so the screen always renders what the store
//! actually holds — the same discipline `SessionsController` follows. A
//! preference save is the exception: the repository *returns* the stored
//! record, so the signal takes that value directly and no second round trip is
//! needed.
//!
//! # The saved-server group, and why it reaches into `connect`
//!
//! [`saved`](Self::saved) is the `saved servers` group's rows, over the
//! **`connect` feature's** two stores — the saved-server list and the
//! biometric-gated credential service — resolved through that feature's own
//! `di::expect_*` functions by the screen. This is the same cross-feature
//! widening the terminal screen already makes (`docs/ARCHITECTURE.md`'s note: a
//! page resolves a sibling's `di::expect_*` directly rather than re-routing a
//! copy through its own), and it is here because deleting a credential is *also*
//! a token revocation, which this feature owns — putting the row anywhere else
//! would mean two features touching one server-side token.
//!
//! Deleting is deliberately **ungated**: forgetting a secret is not a use of it
//! (the credential service's own contract), so no biometric prompt is raised
//! here at all. The server-side revoke that goes with it is best-effort — it is
//! attempted, its failure is not shown, and the local credential is removed
//! either way, because a token that cannot be reached must not leave a
//! credential the user asked to be rid of sitting on the device.
//!
//! # The one-time secret
//!
//! [`pending_token_secret`](Self::pending_token_secret) is the seam the
//! presentation layer's secret-reveal sheet reads from: a landed
//! [`create_token`](Self::create_token) parks the daemon's one-time
//! [`MintedToken`] there (nothing else in this controller writes it), and
//! [`clear_pending_secret`](Self::clear_pending_secret) is the reveal's own
//! "dismissed, gone forever" — the cleared `Option`'s dropped `Zeroizing`
//! secret scrubs its buffer right there. There is deliberately no
//! auto-clearing tied to a *scrim* dismissal of that sheet (`ui::sheet` hands
//! its caller no dismissal hook to hang one off):
//! the value simply sits until the next mint overwrites it or an explicit
//! dismissal clears it, never logged and never read anywhere but that one
//! sheet.

use std::sync::Arc;

use clean_signals::{ControllerCore, NoParams, RunOptions};
use frust::{GetUntracked, RwSignal, Set};

use crate::failure::AppFailure;
use crate::features::connect::di::{Credentials, SavedServers};
use crate::features::connect::domain::entities::SavedServer;
use crate::features::settings::domain::entities::{
    AppearancePrefs, AuthToken, CreateTokenRequest, MintedToken, PollPrefs, Preferences,
};
use crate::features::settings::domain::repositories::{
    AuthTokensRepository, PreferencesRepository,
};
use crate::features::settings::domain::use_cases::{
    CreateToken, ListTokens, LoadPreferences, RevokeToken, UpdateAppearance, UpdatePollPrefs,
};

/// The `/settings` screen's view model. See the [module docs](self).
pub struct SettingsController {
    core: ControllerCore<AppFailure>,
    list_tokens: ListTokens,
    create_token: CreateToken,
    revoke_token: RevokeToken,
    load_preferences: LoadPreferences,
    update_appearance: UpdateAppearance,
    update_poll_prefs: UpdatePollPrefs,
    /// The `connect` feature's saved-server list — the `saved servers` group's
    /// source, and where a deleted credential's metadata is cleared (see the
    /// [module docs](self)).
    saved_servers: SavedServers,
    /// …and its credential store, for the one ungated operation this screen
    /// performs.
    credentials: Credentials,
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
}

impl SettingsController {
    /// A fresh controller over the four injected stores (the composition root
    /// built them once — see [`crate::features::settings::di`] and, for the two
    /// from `connect`, that feature's own `di`).
    pub fn new(
        tokens: Arc<dyn AuthTokensRepository + Send + Sync>,
        preferences: Arc<dyn PreferencesRepository + Send + Sync>,
        saved_servers: SavedServers,
        credentials: Credentials,
    ) -> Self {
        let defaults = Preferences::defaults();
        Self {
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

    /// Re-read the saved-server list. Synchronous, because that store is (see
    /// its own contract) — every credential mutation below ends with one, the
    /// same re-read-rather-than-patch discipline the token list follows.
    ///
    /// A failure leaves the previous list in place and raises nothing: the
    /// group is a management affordance, and a screen that cannot list saved
    /// servers is still a screen whose other four groups work.
    pub fn refresh_saved_servers(&self) {
        if let Ok(servers) = self.saved_servers.list() {
            self.saved.set(servers);
        }
    }

    /// Delete the stored credential for `endpoint`, and the server-side token
    /// behind it.
    ///
    /// No biometric prompt (see the [module docs](self)). Order: revoke the
    /// token best-effort *first* while the session that can revoke it is still
    /// up, then drop the secret, then clear the row's credential fields —
    /// which happens even if the secure-store delete failed, since a row that
    /// claims a credential the user just deleted would offer a prompt that can
    /// only refuse.
    ///
    /// Answers whether the whole thing succeeded, which the screen's
    /// confirmation toast keys off.
    pub async fn delete_credential(&self, endpoint: String) -> bool {
        let Some(server) = self.saved_row(&endpoint) else {
            return false;
        };
        if let Some(name) = server.token_name.clone() {
            self.revoke_quietly(name).await;
        }
        let deleted = self.credentials.delete(endpoint.clone()).await;
        if let Err(failure) = &deleted {
            self.core.failures().emit(failure);
        }
        self.clear_credential_fields(server);
        deleted.is_ok()
    }

    /// Remove the saved server at `endpoint` entirely — its stored credential
    /// with it.
    ///
    /// A row's credential is part of the row: forgetting the server without
    /// forgetting the secret would leave an unreachable entry in the secure
    /// store forever, since the key it is filed under is the endpoint that just
    /// stopped being listed.
    pub async fn forget_server(&self, endpoint: String) -> bool {
        let Some(server) = self.saved_row(&endpoint) else {
            return false;
        };
        if server.has_credential {
            self.delete_credential(endpoint.clone()).await;
        }
        let removed = self.saved_servers.remove(&endpoint);
        if let Err(failure) = &removed {
            self.core.failures().emit(failure);
        }
        self.refresh_saved_servers();
        removed.is_ok()
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
    /// requested `read_only` scope, then refresh. A failed mint leaves the
    /// list untouched and surfaces through
    /// [`failures`](ControllerCore::failures) — the screen's toast bridge. A
    /// landed one parks the daemon's one-time secret on
    /// [`Self::pending_token_secret`] — see the [module docs](self).
    ///
    /// **Answers whether it landed**, which the two mutating call sites need:
    /// the screen raises its own confirmation toast afterwards, and a
    /// "Created …" beside the failure toast for the same call would be a
    /// straight lie.
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
        SettingsController::new(
            Arc::new(MockAuthTokensRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
            Arc::new(servers),
            credentials.unwrap_or_else(|| Arc::new(MockCredentialService::new())),
        )
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
        assert!(
            credentials.prompts().is_empty(),
            "a delete raises no biometric prompt"
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
    fn a_failed_secure_store_delete_still_clears_the_rows_credential_fields() {
        // Otherwise the row keeps offering a one-tap unlock for a credential
        // the user has already asked to be rid of.
        let credentials = Arc::new(MockCredentialService::new());
        credentials.set_delete_failure(true);
        let c = controller_over(
            MockSavedServersRepository::with_servers(vec![saved_row(true)]),
            Some(credentials),
        );
        block_on(c.refresh());

        assert!(!block_on(c.delete_credential(SAVED_ENDPOINT.to_string())));

        assert!(!c.saved.get_untracked()[0].has_credential);
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
