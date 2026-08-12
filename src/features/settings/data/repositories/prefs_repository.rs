//! [`PrefsRepository`] — the real, `frust-shared-preferences`-backed
//! [`PreferencesRepository`], and `settings::di::provide`'s production
//! choice once it opts in (see that module's own docs for the mock/real
//! split, including its headless-test seam).
//!
//! # Storage shape (Flutter `shared_preferences` name parity)
//!
//! Five keys, matching the Flutter app's own preference names 1:1 — pure
//! name parity, not on-disk compatibility (`frust_shared_preferences`'s own
//! module doc: every backend uses its own storage encoding):
//!
//! | Key | Type | Default | Domain field |
//! |---|---|---|---|
//! | `pref_layout_refresh_seconds` | i64 | `3` | [`PollPrefs::layout_refresh_secs`] |
//! | `pref_font_size` | i64 | `12` | [`AppearancePrefs::font_size_px`] |
//! | `pref_cursor_style` | String | `"block"` | [`AppearancePrefs::cursor_style`] |
//! | `pref_bell_notifications` | bool | `true` | [`PollPrefs::bell_notifications`] |
//! | `pref_font_family` | String | `"JetBrains Mono"` | [`AppearancePrefs::font_family`] |
//!
//! Every default above **is** [`Preferences::defaults`], read at each call
//! site rather than re-declared here — the app's shipped defaults stay the
//! one canonical source; this store is a *projection* of them, never a
//! second definition (this module's own charter, and the persisted-prefs
//! task's own note). **`pref_option_as_meta` is deliberately not modeled**:
//! it is the Flutter app's desktop-only "Option as Meta" toggle, and this
//! app's [`Preferences`] entity has no field for it — nothing to load or
//! save.
//!
//! # Never-fails-the-load, per field
//!
//! [`load_preferences`](PreferencesRepository::load_preferences) always
//! returns `Ok`: a missing key, a wrong-typed value (the plugin's own
//! `get_*` already turns that into `None`), or an out-of-range/unrecognized
//! one each fall back to *that one field's* own default rather than failing
//! the whole read — a corrupt `pref_cursor_style` must not blank out an
//! otherwise-fine font size. Every stored int is clamped too:
//! `pref_layout_refresh_seconds` to a minimum of 1s
//! ([`MIN_LAYOUT_REFRESH_SECS`]), `pref_font_size` to
//! [`AppearancePrefs::FONT_SIZES_PX`]'s own `10`–`14` range.
//!
//! # Saves: synchronous state, fire-and-forget disk
//!
//! [`save_appearance`](PreferencesRepository::save_appearance)/
//! [`save_poll_prefs`](PreferencesRepository::save_poll_prefs) clamp the
//! caller's value the same way a load does, hand the *clamped* value back
//! `Ok` synchronously — so the controller's own signal updates on the same
//! tick a picker fires, exactly like the mock this replaces — and only then
//! spawn the actual disk write. That write runs off the UI thread via
//! [`frust::spawn_blocking`]: `docs/PLUGINS_ARCHITECTURE.md`'s Data Flow
//! section and `docs/CODE_STANDARDS.md`'s Plugin Conventions both pair a
//! plugin call with `spawn_blocking`, and this is this module's one
//! sanctioned reach for `frust::` in an otherwise data-layer-framework-free
//! file — the plugin's own calls are documented cheap, but still I/O against
//! a disk-backed store. A write failure is logged (`log::warn!`) and
//! swallowed, never surfaced to the UI — the same best-effort, no-transaction
//! contract a shared-preferences-style store has always carried.
//!
//! Both the loader and the two savers apply the *same* bounds, so a value
//! already on disk from an older build (or a hand-edited store) is corrected
//! on the next load, independent of whether anything is ever saved again.

use frust::spawn_blocking;
use frust_shared_preferences::{PrefsError, SharedPreferences};

use crate::failure::AppFailure;
use crate::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle, PollPrefs, Preferences,
};
use crate::features::settings::domain::repositories::PreferencesRepository;

/// `pref_layout_refresh_seconds`.
const KEY_LAYOUT_REFRESH_SECONDS: &str = "pref_layout_refresh_seconds";
/// `pref_font_size`.
const KEY_FONT_SIZE: &str = "pref_font_size";
/// `pref_cursor_style`.
const KEY_CURSOR_STYLE: &str = "pref_cursor_style";
/// `pref_bell_notifications`.
const KEY_BELL_NOTIFICATIONS: &str = "pref_bell_notifications";
/// `pref_font_family`.
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
    /// ([`SharedPreferences::standard`]'s own backend routing —
    /// NSUserDefaults/Android `SharedPreferences`/a JSON file) as this
    /// feature's real [`PreferencesRepository`].
    ///
    /// # Errors
    /// See [`SharedPreferences::standard`]'s own contract: an unresolvable
    /// file-backend data directory, or (Android only) a host shell that
    /// never installed the platform handles this backend needs.
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

/// The inverse of [`cursor_style_to_key`]; `None` for anything that isn't
/// one of the three labels (a hand-edited store, a future removed variant)
/// — the caller falls back to the domain default rather than failing the
/// load.
fn cursor_style_from_key(key: &str) -> Option<CursorStyle> {
    CursorStyle::ALL
        .into_iter()
        .find(|style| style.label() == key)
}

/// Clamp/validate a caller-supplied [`AppearancePrefs`] against the domain's
/// own choice sets — the same contract a load applies to whatever this store
/// already holds, applied here too so a screen can never persist a value
/// neither this store nor the picker sheets that produce it would recognize
/// as valid.
fn clamp_appearance(prefs: AppearancePrefs) -> AppearancePrefs {
    let (min, max) = font_size_bounds();
    let defaults = Preferences::defaults();
    AppearancePrefs {
        font_family: if AppearancePrefs::FONT_FAMILIES.contains(&prefs.font_family.as_str()) {
            prefs.font_family
        } else {
            defaults.appearance.font_family
        },
        font_size_px: prefs.font_size_px.clamp(min, max),
        cursor_style: prefs.cursor_style,
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

        let font_family = self
            .store
            .get_string(KEY_FONT_FAMILY)
            .filter(|value| AppearancePrefs::FONT_FAMILIES.contains(&value.as_str()))
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

    #[test]
    fn font_size_bounds_are_the_domains_own_min_and_max() {
        assert_eq!(font_size_bounds(), (10, 14));
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
    fn clamp_appearance_rejects_an_unknown_family_and_clamps_the_size() {
        let clamped = clamp_appearance(AppearancePrefs {
            font_family: "Comic Sans".to_string(),
            font_size_px: 72,
            cursor_style: CursorStyle::Bar,
        });
        assert_eq!(
            clamped.font_family,
            Preferences::defaults().appearance.font_family
        );
        assert_eq!(clamped.font_size_px, 14, "clamped to the domain's own max");
        assert_eq!(clamped.cursor_style, CursorStyle::Bar, "left alone");
    }

    #[test]
    fn clamp_appearance_keeps_a_known_family_and_in_range_size() {
        let clamped = clamp_appearance(AppearancePrefs {
            font_family: "Space Mono".to_string(),
            font_size_px: 11,
            cursor_style: CursorStyle::Underline,
        });
        assert_eq!(clamped.font_family, "Space Mono");
        assert_eq!(clamped.font_size_px, 11);
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
