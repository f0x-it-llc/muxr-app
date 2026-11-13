//! The [`PreferencesRepository`] over `frust-shared-preferences`.
//!
//! A missing, wrong-typed or out-of-range stored value falls back to that field's default instead of failing the load.

use frust::spawn_blocking;
use frust_shared_preferences::{PrefsError, SharedPreferences};

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle, PollPrefs, Preferences,
};
use crate::features::settings::domain::repositories::PreferencesRepository;

// Key names match the Flutter app's preference names.
const KEY_LAYOUT_REFRESH_SECONDS: &str = "pref_layout_refresh_seconds";
const KEY_FONT_SIZE: &str = "pref_font_size";
const KEY_CURSOR_STYLE: &str = "pref_cursor_style";
const KEY_BELL_NOTIFICATIONS: &str = "pref_bell_notifications";
const KEY_FONT_FAMILY: &str = "pref_font_family";

/// A zero period would spin the sessions poll.
const MIN_LAYOUT_REFRESH_SECS: u32 = 1;

pub struct PrefsRepository {
    store: SharedPreferences,
}

impl PrefsRepository {
    pub fn standard() -> Result<Self, PrefsError> {
        Ok(Self {
            store: SharedPreferences::standard()?,
        })
    }
}

fn font_size_bounds() -> (u32, u32) {
    let sizes = AppearancePrefs::FONT_SIZES_PX;
    (
        *sizes.iter().min().expect("FONT_SIZES_PX is non-empty"),
        *sizes.iter().max().expect("FONT_SIZES_PX is non-empty"),
    )
}

fn cursor_style_to_key(style: CursorStyle) -> &'static str {
    style.label()
}

fn cursor_style_from_key(key: &str) -> Option<CursorStyle> {
    CursorStyle::ALL
        .into_iter()
        .find(|style| style.label() == key)
}

fn clamp_appearance(prefs: AppearancePrefs) -> AppearancePrefs {
    let (min, max) = font_size_bounds();
    let defaults = Preferences::defaults();
    AppearancePrefs {
        font_family: sane_family(prefs.font_family, defaults.appearance.font_family),
        font_size_px: prefs.font_size_px.clamp(min, max),
        cursor_style: prefs.cursor_style,
    }
}

/// The only font-family validation: installed faces are a runtime fact this layer cannot know.
fn sane_family(family: String, fallback: String) -> String {
    if family.trim().is_empty() {
        fallback
    } else {
        family
    }
}

fn clamp_poll(prefs: PollPrefs) -> PollPrefs {
    PollPrefs {
        layout_refresh_secs: prefs.layout_refresh_secs.max(MIN_LAYOUT_REFRESH_SECS),
        bell_notifications: prefs.bell_notifications,
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl PreferencesRepository for PrefsRepository {
    async fn load_preferences(&self) -> Result<Preferences, AppFailure> {
        let defaults = Preferences::defaults();
        let (min_size, max_size) = font_size_bounds();

        let font_family = self
            .store
            .get_string(KEY_FONT_FAMILY)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(defaults.appearance.font_family);

        let font_size_px = self
            .store
            .get_i64(KEY_FONT_SIZE)
            .and_then(|value| u32::try_from(value).ok())
            .map(|value| value.clamp(min_size, max_size))
            .unwrap_or(defaults.appearance.font_size_px);

        let cursor_style = self
            .store
            .get_string(KEY_CURSOR_STYLE)
            .and_then(|value| cursor_style_from_key(&value))
            .unwrap_or(defaults.appearance.cursor_style);

        let layout_refresh_secs = self
            .store
            .get_i64(KEY_LAYOUT_REFRESH_SECONDS)
            .and_then(|value| u32::try_from(value).ok())
            .map(|value| value.max(MIN_LAYOUT_REFRESH_SECS))
            .unwrap_or(defaults.poll.layout_refresh_secs);

        let bell_notifications = self
            .store
            .get_bool(KEY_BELL_NOTIFICATIONS)
            .unwrap_or(defaults.poll.bell_notifications);

        Ok(Preferences {
            appearance: AppearancePrefs {
                font_family,
                font_size_px,
                cursor_style,
            },
            poll: PollPrefs {
                layout_refresh_secs,
                bell_notifications,
            },
        })
    }

    // Savers return the clamped value synchronously and persist via `spawn_blocking`; write failures are logged, not surfaced.
    async fn save_appearance(&self, prefs: AppearancePrefs) -> Result<AppearancePrefs, AppFailure> {
        let clamped = clamp_appearance(prefs);
        let store = self.store.clone();
        let persisted = clamped.clone();
        spawn_blocking(move || {
            if let Err(err) = store.set_string(KEY_FONT_FAMILY, persisted.font_family) {
                log::warn!("preferences: failed to persist font family: {err}");
            }
            if let Err(err) = store.set_i64(KEY_FONT_SIZE, i64::from(persisted.font_size_px)) {
                log::warn!("preferences: failed to persist font size: {err}");
            }
            if let Err(err) = store.set_string(
                KEY_CURSOR_STYLE,
                cursor_style_to_key(persisted.cursor_style),
            ) {
                log::warn!("preferences: failed to persist cursor style: {err}");
            }
        });
        Ok(clamped)
    }

    async fn save_poll_prefs(&self, prefs: PollPrefs) -> Result<PollPrefs, AppFailure> {
        let clamped = clamp_poll(prefs);
        let store = self.store.clone();
        spawn_blocking(move || {
            if let Err(err) = store.set_i64(
                KEY_LAYOUT_REFRESH_SECONDS,
                i64::from(clamped.layout_refresh_secs),
            ) {
                log::warn!("preferences: failed to persist layout refresh: {err}");
            }
            if let Err(err) = store.set_bool(KEY_BELL_NOTIFICATIONS, clamped.bell_notifications) {
                log::warn!("preferences: failed to persist bell notifications: {err}");
            }
        });
        Ok(clamped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::settings::domain::entities::{FONT_SIZE_MAX_PX, FONT_SIZE_MIN_PX};

    #[test]
    fn font_size_bounds_are_the_domains_own_min_and_max() {
        assert_eq!(font_size_bounds(), (FONT_SIZE_MIN_PX, FONT_SIZE_MAX_PX));
    }

    #[test]
    fn cursor_style_keys_round_trip_for_every_variant() {
        for style in CursorStyle::ALL {
            let key = cursor_style_to_key(style);
            assert_eq!(cursor_style_from_key(key), Some(style));
        }
    }

    #[test]
    fn an_unrecognized_cursor_style_key_is_none() {
        assert_eq!(cursor_style_from_key("diamond"), None);
    }

    #[test]
    fn clamp_appearance_keeps_a_downloaded_family_and_clamps_the_size() {
        // A downloaded family is in no compile-time list and must survive the clamp.
        let clamped = clamp_appearance(AppearancePrefs {
            font_family: "FiraCode Nerd Font Mono".to_string(),
            font_size_px: 72,
            cursor_style: CursorStyle::Bar,
        });
        assert_eq!(
            clamped.font_family, "FiraCode Nerd Font Mono",
            "the family is stored as given"
        );
        assert_eq!(
            clamped.font_size_px, FONT_SIZE_MAX_PX,
            "clamped to the domain's own max"
        );
        assert_eq!(clamped.cursor_style, CursorStyle::Bar, "left alone");
    }

    #[test]
    fn clamp_appearance_clamps_oversized_and_undersized_font_sizes() {
        let over_clamped = clamp_appearance(AppearancePrefs {
            font_family: "JetBrains Mono".to_string(),
            font_size_px: 30,
            cursor_style: CursorStyle::Block,
        });
        assert_eq!(
            over_clamped.font_size_px, FONT_SIZE_MAX_PX,
            "clamped to the domain's max"
        );

        let under_clamped = clamp_appearance(AppearancePrefs {
            font_family: "JetBrains Mono".to_string(),
            font_size_px: 6,
            cursor_style: CursorStyle::Block,
        });
        assert_eq!(
            under_clamped.font_size_px, FONT_SIZE_MIN_PX,
            "clamped to the domain's min"
        );
    }

    #[test]
    fn clamp_appearance_replaces_an_empty_family_with_the_shipped_default() {
        for blank in ["", "   "] {
            let clamped = clamp_appearance(AppearancePrefs {
                font_family: blank.to_string(),
                font_size_px: 11,
                cursor_style: CursorStyle::Underline,
            });
            assert_eq!(
                clamped.font_family,
                Preferences::defaults().appearance.font_family
            );
            assert_eq!(clamped.font_size_px, 11);
        }
    }

    #[test]
    fn clamp_poll_floors_layout_refresh_at_one_second() {
        let clamped = clamp_poll(PollPrefs {
            layout_refresh_secs: 0,
            bell_notifications: false,
        });
        assert_eq!(clamped.layout_refresh_secs, MIN_LAYOUT_REFRESH_SECS);
        assert!(!clamped.bell_notifications, "left alone");
    }

    #[test]
    fn clamp_poll_leaves_an_already_valid_interval_alone() {
        let clamped = clamp_poll(PollPrefs {
            layout_refresh_secs: 10,
            bell_notifications: true,
        });
        assert_eq!(clamped.layout_refresh_secs, 10);
    }
}
