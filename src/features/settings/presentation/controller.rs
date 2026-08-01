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

use std::sync::Arc;

use clean_signals::{ControllerCore, NoParams, RunOptions};
use frust::{GetUntracked, RwSignal, Set};

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AppearancePrefs, AuthToken, PollPrefs, Preferences,
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
    /// The token set as last read from the repository — the `auth tokens`
    /// group's rows.
    pub tokens: RwSignal<Vec<AuthToken>>,
    /// The `terminal appearance` group's three values.
    pub appearance: RwSignal<AppearancePrefs>,
    /// The `session poll` group's two values.
    pub poll: RwSignal<PollPrefs>,
    /// Whether the first load has completed (see the [module docs](self)).
    pub loaded: RwSignal<bool>,
}

impl SettingsController {
    /// A fresh controller over the two injected repositories (the composition
    /// root built them once — see [`crate::features::settings::di`]).
    pub fn new(
        tokens: Arc<dyn AuthTokensRepository + Send + Sync>,
        preferences: Arc<dyn PreferencesRepository + Send + Sync>,
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
            tokens: RwSignal::new(Vec::new()),
            appearance: RwSignal::new(defaults.appearance),
            poll: RwSignal::new(defaults.poll),
            loaded: RwSignal::new(false),
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
        self.loaded.set(true);
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

    /// Mint a token named `name` (already trimmed by the dialog), then
    /// refresh. A failed mint leaves the list untouched and surfaces through
    /// [`failures`](ControllerCore::failures) — the screen's toast bridge.
    ///
    /// **Answers whether it landed**, which the two mutating call sites need:
    /// the screen raises its own confirmation toast afterwards, and a
    /// "Created …" beside the failure toast for the same call would be a
    /// straight lie.
    pub async fn create_token(&self, name: String) -> bool {
        let created = self
            .core
            .run(&self.create_token, name, RunOptions::default())
            .await
            .is_ok();
        if created {
            self.refresh_tokens().await;
        }
        created
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
    use crate::features::settings::data::repositories::mock_auth_tokens_repository::MOCK_TODAY;
    use crate::features::settings::data::repositories::{
        MockAuthTokensRepository, MockPreferencesRepository,
    };
    use crate::features::settings::domain::entities::{CursorStyle, TokenScope};

    /// How many tokens the mock repository seeds (the redesign's two rows).
    const SEEDED_COUNT: usize = 2;

    /// The test-wiring composition root: the two mocks behind their domain
    /// traits. `#[cfg(test)]`-only wiring, so `di.rs` stays the only production
    /// site where presentation meets `data` (huddle's convention).
    fn controller() -> SettingsController {
        SettingsController::new(
            Arc::new(MockAuthTokensRepository::seeded()),
            Arc::new(MockPreferencesRepository::seeded()),
        )
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

        block_on(c.create_token("laptop".to_string()));

        let tokens = c.tokens.get_untracked();
        assert_eq!(tokens.len(), SEEDED_COUNT + 1);
        let minted = tokens.last().expect("the minted token");
        assert_eq!(minted.name, "laptop");
        assert_eq!(minted.scope, TokenScope::ReadWrite);
        assert_eq!(minted.created, MOCK_TODAY);
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

        assert!(block_on(c.create_token("laptop".to_string())));
        assert!(
            !block_on(c.create_token("laptop".to_string())),
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
