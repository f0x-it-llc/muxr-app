//! [`MockPreferencesRepository`] — the in-memory [`PreferencesRepository`] the
//! mock era runs on.
//!
//! Seeds [`Preferences::defaults`] — the seed is not transcribed a second
//! time here, so the screen, the mock and the shipped defaults cannot drift
//! apart (see that constructor's own doc for what it holds, including its one
//! deliberate divergence from `docs/redesign.html`'s settings-screen mock).
//!
//! # In-memory, and honestly so
//!
//! Nothing survives a process restart: no secure-storage or
//! shared-preferences plugin is wired yet (`docs/ARCHITECTURE.md`'s Planned
//! section). What *is* real is the round trip — a picker's choice is written
//! here and read back out of here, so when a plugin lands it replaces this one
//! file and nothing above it.
//!
//! # Framework-free
//!
//! No `frust::` types (the data-layer rule): the store is a plain
//! [`Mutex<Preferences>`], which is what makes it `Send + Sync` for the
//! controller's `Arc` under `use_controller`'s bounds.
//!
//! # Failure mapping
//!
//! The feature's second failure-mapping site into [`AppFailure`]. A value
//! outside its own domain-declared choice set maps to [`AppFailure::Internal`]
//! (a bug — every write comes from a picker built out of that same set); a real
//! store maps an I/O error to [`AppFailure::Storage`] in exactly this spot.

use std::sync::Mutex;
use std::time::Duration;

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle, PollPrefs, Preferences,
};
use crate::features::settings::domain::repositories::PreferencesRepository;

/// Simulated round-trip latency on every call — see
/// [`MockAuthTokensRepository`](super::MockAuthTokensRepository)'s own note.
const MOCK_LATENCY: Duration = Duration::from_millis(20);

/// An in-memory [`PreferencesRepository`]. See the [module docs](self).
pub struct MockPreferencesRepository {
    /// The live preference record. Replaced in place by each save.
    prefs: Mutex<Preferences>,
}

impl MockPreferencesRepository {
    /// A repository seeded with [`Preferences::defaults`].
    pub fn seeded() -> Self {
        Self {
            prefs: Mutex::new(Preferences::defaults()),
        }
    }

    /// The live record, cloned — a **synchronous** peek for this module's own
    /// unit tests, never part of the [`PreferencesRepository`] contract.
    pub fn snapshot(&self) -> Preferences {
        self.lock().clone()
    }

    /// The store, with a poisoned lock recovered rather than propagated.
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

/// Reject an appearance record carrying a value no picker could have produced.
///
/// The font family is deliberately **not** checked against a list: the font
/// picker offers whatever faces are installed on the device, bundled or
/// downloaded (`crate::fonts`), so the legal set is a runtime fact this
/// framework-free layer cannot know — the real store
/// ([`PrefsRepository`](super::PrefsRepository)) makes the same call, and its
/// module docs carry the full rationale. Only an empty name is refused: it
/// could never name a face.
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
    // `CursorStyle` is a closed enum whose every variant is offered, so there
    // is nothing to reject — the exhaustive match is the tripwire for a
    // variant added without adding it to `ALL`.
    match prefs.cursor_style {
        CursorStyle::Block | CursorStyle::Underline | CursorStyle::Bar => Ok(()),
    }
}

/// Reject a poll record carrying an interval no picker could have produced.
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
        // The defect this pins: the family was clamped against a four-name
        // const, so a downloaded font's name — which appears in no
        // compile-time list — was rejected here and silently reset to the
        // bundled default on the next load, losing the user's choice.
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
