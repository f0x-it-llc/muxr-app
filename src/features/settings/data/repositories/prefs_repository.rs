//! [`PrefsRepository`] — the real, `frust-shared-preferences`-backed
//! [`PreferencesRepository`].
//!
//! **Storage shape**: five keys matching the Flutter app's own preference
//! names 1:1 (`pref_layout_refresh_seconds`/`pref_font_size`/
//! `pref_cursor_style`/`pref_bell_notifications`/`pref_font_family`) — name
//! parity only, not on-disk compatibility. Every default **is**
//! [`Preferences::defaults`]. `pref_option_as_meta` (Flutter's desktop-only
//! "Option as Meta" toggle) is deliberately not modeled.
//!
//! **Never fails the load**: a missing/wrong-typed/out-of-range value falls
//! back to *that field's own* default rather than failing the whole read —
//! a corrupt `pref_cursor_style` must not blank out an otherwise-fine font
//! size. Ints are clamped too: `pref_layout_refresh_seconds` to
//! [`MIN_LAYOUT_REFRESH_SECS`], `pref_font_size` to
//! [`AppearancePrefs::FONT_SIZES_PX`]'s own range.
//!
//! **`pref_font_family` is stored as given**, the one field not validated
//! against a list: the picker offers whatever faces are installed — bundled
//! *or downloaded* — a runtime fact this framework-free layer can't know.
//! Only non-empty is required; grace for a since-uninstalled family lives at
//! the presentation edge (`crate::fonts::resolve_terminal_family`).
//!
//! **Saves are synchronous state, fire-and-forget disk**: both savers clamp
//! like a load and hand the clamped value back `Ok` synchronously — so the
//! controller's signal updates the same tick a picker fires — then spawn the
//! write via [`frust::spawn_blocking`] (this module's one sanctioned
//! `frust::` reach). A write failure is logged and swallowed, never surfaced
//! to the UI.

use frust::spawn_blocking;
use frust_shared_preferences::{PrefsError, SharedPreferences};

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle, PollPrefs, Preferences,
};
use crate::features::settings::domain::repositories::PreferencesRepository;

const KEY_LAYOUT_REFRESH_SECONDS: &str = "pref_layout_refresh_seconds";
const KEY_FONT_SIZE: &str = "pref_font_size";
const KEY_CURSOR_STYLE: &str = "pref_cursor_style";
const KEY_BELL_NOTIFICATIONS: &str = "pref_bell_notifications";
const KEY_FONT_FAMILY: &str = "pref_font_family";

/// The floor a stored/saved `pref_layout_refresh_seconds` is clamped to — a
/// `0`s (or negative, on a hand-edited store) refresh period would spin the
/// sessions poll as fast as the executor lets it.
const MIN_LAYOUT_REFRESH_SECS: u32 = 1;

/// The real [`PreferencesRepository`], over the platform's standard
/// shared-preferences store. See the [module docs](self).
pub struct PrefsRepository {
    store: SharedPreferences,
}

impl PrefsRepository {
    /// Open the platform's standard preferences store
    /// (NSUserDefaults/Android `SharedPreferences`/a JSON file) as this
    /// feature's real [`PreferencesRepository`].
    ///
    /// # Errors
    /// See [`SharedPreferences::standard`]'s own contract.
    pub fn standard() -> Result<Self, PrefsError> {
        Ok(Self {
            store: SharedPreferences::standard()?,
        })
    }
}

/// `(min, max)` of [`AppearancePrefs::FONT_SIZES_PX`] — read from the
/// domain's own choice set rather than re-declared here, so the picker's
/// range and this store's clamp can never drift apart.
fn font_size_bounds() -> (u32, u32) {
    let sizes = AppearancePrefs::FONT_SIZES_PX;
    (
        *sizes.iter().min().expect("FONT_SIZES_PX is non-empty"),
        *sizes.iter().max().expect("FONT_SIZES_PX is non-empty"),
    )
}

/// The string a [`CursorStyle`] persists as — its own `.label()` text
/// (`"block"`/`"underline"`/`"bar"`), the same casing `docs/redesign.html`'s
/// row prints.
fn cursor_style_to_key(style: CursorStyle) -> &'static str {
    style.label()
}

/// The inverse of [`cursor_style_to_key`]; `None` for anything that isn't one
/// of the three labels (a hand-edited store, a removed variant) — the caller
/// falls back to the domain default rather than failing the load.
fn cursor_style_from_key(key: &str) -> Option<CursorStyle> {
    CursorStyle::ALL
        .into_iter()
        .find(|style| style.label() == key)
}

/// Clamps a caller-supplied [`AppearancePrefs`] against the domain's own
/// choice sets — the same contract a load applies, so a screen can never
/// persist a value the store/pickers wouldn't recognize as valid. The font
/// family is the exception (see the [module docs](self)).
fn clamp_appearance(prefs: AppearancePrefs) -> AppearancePrefs {
    let (min, max) = font_size_bounds();
    let defaults = Preferences::defaults();
    AppearancePrefs {
        font_family: sane_family(prefs.font_family, defaults.appearance.font_family),
        font_size_px: prefs.font_size_px.clamp(min, max),
        cursor_style: prefs.cursor_style,
    }
}

/// `family` if it names anything at all, else `fallback` — the whole of this
/// store's font-family validation (see the [module docs](self)); an empty
/// string could never name a face.
fn sane_family(family: String, fallback: String) -> String {
    if family.trim().is_empty() {
        fallback
    } else {
        family
    }
}

/// Clamp a caller-supplied [`PollPrefs`] — today just
/// [`MIN_LAYOUT_REFRESH_SECS`]'s floor.
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

        // Whatever was stored, as long as it names something — a downloaded
        // family is legal here (see module docs).
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
        // The defect this pins: a downloaded family appears in no
        // compile-time list, and clamping against one silently reset the
        // user's choice to the bundled default on the next launch.
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
        // A persisted value outside the offered range is clamped to the nearest bound.
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
        // The one family value that could never name a face.
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
