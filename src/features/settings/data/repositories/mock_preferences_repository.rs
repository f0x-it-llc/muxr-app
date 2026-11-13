//! In-memory [`PreferencesRepository`] for headless tests, seeded from [`Preferences::defaults`].

use std::sync::Mutex;
use std::time::Duration;

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle, PollPrefs, Preferences,
};
use crate::features::settings::domain::repositories::PreferencesRepository;

/// Keeps the async path real instead of accidentally synchronous.
const MOCK_LATENCY: Duration = Duration::from_millis(20);

pub struct MockPreferencesRepository {
    prefs: Mutex<Preferences>,
}

impl MockPreferencesRepository {
    pub fn seeded() -> Self {
        Self {
            prefs: Mutex::new(Preferences::defaults()),
        }
    }

    pub fn snapshot(&self) -> Preferences {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Preferences> {
        self.prefs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for MockPreferencesRepository {
    fn default() -> Self {
        Self::seeded()
    }
}

/// Only an empty font family is refused; faces are a runtime fact.
fn validate_appearance(prefs: &AppearancePrefs) -> Result<(), AppFailure> {
    if prefs.font_family.trim().is_empty() {
        return Err(AppFailure::Internal(
            "save_appearance: empty font family".to_string(),
        ));
    }
    if !AppearancePrefs::FONT_SIZES_PX.contains(&prefs.font_size_px) {
        return Err(AppFailure::Internal(format!(
            "save_appearance: unsupported font size {}",
            prefs.font_size_px
        )));
    }
    // Exhaustive so a variant added without being offered fails to compile.
    match prefs.cursor_style {
        CursorStyle::Block | CursorStyle::Underline | CursorStyle::Bar => Ok(()),
    }
}

fn validate_poll(prefs: &PollPrefs) -> Result<(), AppFailure> {
    if !PollPrefs::REFRESH_CHOICES_SECS.contains(&prefs.layout_refresh_secs) {
        return Err(AppFailure::Internal(format!(
            "save_poll_prefs: unsupported refresh interval {}s",
            prefs.layout_refresh_secs
        )));
    }
    Ok(())
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl PreferencesRepository for MockPreferencesRepository {
    async fn load_preferences(&self) -> Result<Preferences, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        Ok(self.snapshot())
    }

    async fn save_appearance(&self, prefs: AppearancePrefs) -> Result<AppearancePrefs, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        validate_appearance(&prefs)?;
        self.lock().appearance = prefs.clone();
        Ok(prefs)
    }

    async fn save_poll_prefs(&self, prefs: PollPrefs) -> Result<PollPrefs, AppFailure> {
        clean_signals::time::sleep(MOCK_LATENCY).await;
        validate_poll(&prefs)?;
        self.lock().poll = prefs;
        Ok(prefs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("a current-thread runtime")
            .block_on(fut)
    }

    #[test]
    fn the_seed_is_the_shipped_defaults() {
        let repo = MockPreferencesRepository::seeded();
        let loaded = block_on(repo.load_preferences()).expect("the mock never fails to load");

        assert_eq!(loaded, Preferences::defaults());
    }

    #[test]
    fn saving_appearance_round_trips_and_leaves_the_poll_group_alone() {
        let repo = MockPreferencesRepository::seeded();
        let next = AppearancePrefs {
            font_family: "Space Mono".to_string(),
            font_size_px: 14,
            cursor_style: CursorStyle::Bar,
        };

        let stored = block_on(repo.save_appearance(next.clone())).expect("save");

        assert_eq!(stored, next);
        let loaded = block_on(repo.load_preferences()).expect("load");
        assert_eq!(loaded.appearance, next);
        assert_eq!(loaded.poll, Preferences::defaults().poll);
    }

    #[test]
    fn saving_poll_prefs_round_trips_and_leaves_appearance_alone() {
        let repo = MockPreferencesRepository::seeded();
        let next = PollPrefs {
            layout_refresh_secs: 10,
            bell_notifications: false,
        };

        let stored = block_on(repo.save_poll_prefs(next)).expect("save");

        assert_eq!(stored, next);
        let loaded = block_on(repo.load_preferences()).expect("load");
        assert_eq!(loaded.poll, next);
        assert_eq!(loaded.appearance, Preferences::defaults().appearance);
    }

    #[test]
    fn a_downloaded_font_family_survives_a_save_and_load_round_trip() {
        let repo = MockPreferencesRepository::seeded();
        let downloaded = AppearancePrefs {
            font_family: "FiraCode Nerd Font Mono".to_string(),
            ..Preferences::defaults().appearance
        };

        let stored = block_on(repo.save_appearance(downloaded.clone())).expect("accepted");

        assert_eq!(stored, downloaded);
        let loaded = block_on(repo.load_preferences()).expect("load");
        assert_eq!(
            loaded.appearance.font_family, "FiraCode Nerd Font Mono",
            "the user's downloaded font survives the round trip"
        );
    }

    #[test]
    fn a_value_outside_the_domains_own_choice_set_is_rejected() {
        let repo = MockPreferencesRepository::seeded();

        let no_family = AppearancePrefs {
            font_family: "  ".to_string(),
            ..Preferences::defaults().appearance
        };
        assert!(matches!(
            block_on(repo.save_appearance(no_family)).expect_err("rejected"),
            AppFailure::Internal(_)
        ));

        let bad_size = AppearancePrefs {
            font_size_px: 72,
            ..Preferences::defaults().appearance
        };
        assert!(matches!(
            block_on(repo.save_appearance(bad_size)).expect_err("rejected"),
            AppFailure::Internal(_)
        ));

        let bad_interval = PollPrefs {
            layout_refresh_secs: 7,
            ..Preferences::defaults().poll
        };
        assert!(matches!(
            block_on(repo.save_poll_prefs(bad_interval)).expect_err("rejected"),
            AppFailure::Internal(_)
        ));

        assert_eq!(
            repo.snapshot(),
            Preferences::defaults(),
            "a rejected write leaves the store untouched"
        );
    }
}
