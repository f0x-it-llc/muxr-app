//! The app's fonts: JetBrains Mono as the app-wide (and terminal-default) face,
//! Symbols Nerd Font Mono as the always-present nerd-font fallback, plus the
//! downloadable Nerd Font catalog on top. App-level infrastructure beside
//! `crate::net`: Settings browses/installs fonts, the terminal grid shapes with them.
//!
//! | Module | Owns |
//! |---|---|
//! | this one | the bundled faces, the app-wide theme swap, and what "installed" means |
//! | [`manifest`] | the catalog's wire format (`displayName` vs `family`) |
//! | [`source`] | where bytes come from — HTTPS, or a test's own source |
//! | [`store`] | the on-disk cache, SHA-256 integrity, registration, the installed record |
//! | [`catalog`] | the picker's reactive state and the download state machine |
//!
//! **Registration is a push**, into [`frust::register_app_fonts`]'s
//! process-wide pending slot every shell drains into its `TextContext` — once
//! before the first frame for [`register_bundled_fonts`]'s bytes, once per
//! frame thereafter, so a **downloaded** face installs mid-session, no relaunch.
//!
//! **Family names are fontique's, not ours** — resolved from a registered
//! font's own name-table preference, not asserted here; see
//! [`installed_families`]'s unit test for the tripwire against silent
//! TTF-name drift, and [`store`] for why a downloaded family is read back
//! from a real registration, never taken from the manifest. **Call order:
//! register, then re-theme** — [`register_bundled_fonts`] must run before
//! [`apply_app_wide_theme`] (see that function's own doc for why).

pub mod catalog;
mod json;
pub mod manifest;
pub mod source;
pub mod store;

use frust::authoring::text::{FontFamily, GenericSlot, TextStyle};
use frust::{Theme, TypeScale, set_app_theme};

pub use source::{use_font_source_for_tests, use_offline_font_source_for_tests};
pub use store::{reset_installed_fonts_for_tests, use_font_cache_dir_for_tests};

/// The app-wide and terminal-default font family — JetBrains Mono. Single
/// source for the type-scale swap below, the terminal grid's default stack
/// ([`default_terminal_family`]/[`resolve_terminal_family`]), and the
/// persisted preference default
/// (`crate::features::settings::domain::entities::Preferences::defaults`).
pub const DEFAULT_TERMINAL_FAMILY: &str = "JetBrains Mono";

/// The always-present nerd-font/powerline glyph fallback — Symbols Nerd Font
/// Mono. Covers private-use glyphs the primary face lacks (a TUI's
/// icons/branch glyphs), so it never leads a stack itself, only trails one.
pub const SYMBOLS_FALLBACK_FAMILY: &str = "Symbols Nerd Font Mono";

const JETBRAINS_MONO_LIGHT: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Light.ttf");
const JETBRAINS_MONO_LIGHT_ITALIC: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-LightItalic.ttf");
const JETBRAINS_MONO_REGULAR: &[u8] =
    include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
const JETBRAINS_MONO_ITALIC: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Italic.ttf");
const JETBRAINS_MONO_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Medium.ttf");
const JETBRAINS_MONO_BOLD: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Bold.ttf");
const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

/// Every bundled TTF's raw bytes, in the order [`register_bundled_fonts`]
/// registers them — the six JetBrains Mono faces (all resolve into one
/// [`DEFAULT_TERMINAL_FAMILY`] family; fontique's typographic-family
/// preference is what unifies Light/Medium's own family-name-table entries
/// back onto the same name the Regular/Bold/Italic faces already carry) plus
/// Symbols Nerd Font Mono.
const BUNDLED_FONTS: [&[u8]; 7] = [
    JETBRAINS_MONO_LIGHT,
    JETBRAINS_MONO_LIGHT_ITALIC,
    JETBRAINS_MONO_REGULAR,
    JETBRAINS_MONO_ITALIC,
    JETBRAINS_MONO_MEDIUM,
    JETBRAINS_MONO_BOLD,
    SYMBOLS_NERD_FONT_MONO,
];

/// The two families that ship in the binary — always installed, never
/// downloaded, and the floor [`installed_families`] starts from.
const BUNDLED_FAMILIES: [&str; 2] = [DEFAULT_TERMINAL_FAMILY, SYMBOLS_FALLBACK_FAMILY];

/// Push every bundled TTF into `frust`'s pending font registry. Call once,
/// from the app's setup block, before [`apply_app_wide_theme`] (see the
/// [module docs](self)).
pub fn register_bundled_fonts() {
    for bytes in BUNDLED_FONTS {
        frust::register_app_fonts(bytes.to_vec());
    }
}

/// Re-register every valid cached font family, off the UI thread.
///
/// Call once, from the app's setup block. Fire-and-forget by design: the scan
/// reads and hashes every cached file ([`store::scan_cache_and_register`]), so
/// it runs on the blocking pool and folds its families into
/// [`installed_families`] whenever it lands — a few frames into the launch at
/// worst, long before a picker can be opened. Until then a persisted family
/// that is only in the cache resolves to the bundled stack
/// ([`resolve_terminal_family`]) and corrects itself on the terminal's next
/// appearance poll, which is why nothing here blocks a frame waiting for it.
pub fn register_cached_fonts_at_startup() {
    frust::spawn(async move {
        match frust::spawn_blocking(store::scan_cache_and_register).await {
            Ok(families) if !families.is_empty() => {
                log::info!("fonts: registered {} cached families", families.len());
            }
            Ok(_) => {}
            Err(err) => log::warn!("fonts: cached-font scan failed to run: {err}"),
        }
    });
}

/// Every family this process can shape with: the two bundled faces plus every
/// downloaded family registered this run (from the startup cache scan, or from
/// a download that landed since — see [`store`]).
pub fn installed_families() -> Vec<String> {
    let mut families: Vec<String> = BUNDLED_FAMILIES.iter().map(|f| f.to_string()).collect();
    for family in store::installed_downloads() {
        if !families.contains(&family) {
            families.push(family);
        }
    }
    families
}

/// What a family is *called* in the UI: the catalog's own label for a
/// downloaded face (`BlexMono Nerd Font Mono` → `IBM Plex Mono Nerd Font
/// Mono`), or the family name itself for a bundled one (whose two names are
/// the same) and for anything this device has never installed.
///
/// The label survives a relaunch without a catalog: it is recorded in the
/// cache's own sidecar (see [`store::display_name_for`]).
///
/// A bundled family answers its own name unconditionally — it is not the
/// cache's to rename, even if a catalog entry's bytes once happened to
/// register under it.
pub fn display_name(family: &str) -> String {
    if BUNDLED_FAMILIES.contains(&family) {
        return family.to_string();
    }
    store::display_name_for(family).unwrap_or_else(|| family.to_string())
}

/// Font families a user may pick as the terminal's *primary* face:
/// [`installed_families`] minus [`SYMBOLS_FALLBACK_FAMILY`] — a fallback face
/// that covers private-use glyphs the primary face lacks, never something a
/// picker should offer as the leading choice (see that constant's own doc).
pub fn pickable_terminal_families() -> Vec<String> {
    installed_families()
        .into_iter()
        .filter(|family| family != SYMBOLS_FALLBACK_FAMILY)
        .collect()
}

/// The terminal grid's default font stack — [`DEFAULT_TERMINAL_FAMILY`] then
/// [`SYMBOLS_FALLBACK_FAMILY`], ending in the `Monospace` generic slot so a
/// host missing both bundled faces still shapes *some* monospace face rather
/// than falling back to a proportional one. What [`resolve_terminal_family`]
/// falls back to for an unknown/not-installed stored family, and what
/// `TerminalController::font_family`
/// (`crate::features::terminal::presentation::controller`) is seeded with
/// before the live pane's first appearance-preference read lands.
pub fn default_terminal_family() -> FontFamily {
    FontFamily::stack_with_generic(
        [DEFAULT_TERMINAL_FAMILY, SYMBOLS_FALLBACK_FAMILY],
        GenericSlot::Monospace,
    )
}

/// Resolve a persisted `AppearancePrefs::font_family` string into the grid's
/// primary+fallback stack: `stored` itself when it names an
/// [`installed_families`] entry (trailed by [`SYMBOLS_FALLBACK_FAMILY`] and
/// the `Monospace` generic, same shape as [`default_terminal_family`]); the
/// default stack for anything else — a value persisted before this bundling
/// existed (`docs/redesign.html`'s own `Hack Nerd Font Mono` mock), or a
/// downloaded family whose cache has since been cleared.
///
/// **This is the app's one unknown-family grace, and it lives here on
/// purpose.** The data layer stores whatever family the user picked without
/// judging it (a downloaded name is not in any compile-time list, and `data`
/// is framework-free — it cannot know what is installed); this resolution, at
/// the presentation edge, is what turns a family that no longer resolves into
/// the shipped default rather than a blank or broken stack.
pub fn resolve_terminal_family(stored: &str) -> FontFamily {
    if installed_families().iter().any(|family| family == stored) {
        FontFamily::stack_with_generic([stored, SYMBOLS_FALLBACK_FAMILY], GenericSlot::Monospace)
    } else {
        default_terminal_family()
    }
}

/// [`DEFAULT_TERMINAL_FAMILY`] with a `Monospace` generic tail, so a host
/// missing the bundled face still shapes some monospace font rather than
/// falling back to a proportional one.
fn app_wide_family() -> FontFamily {
    FontFamily::stack_with_generic([DEFAULT_TERMINAL_FAMILY], GenericSlot::Monospace)
}

/// `style` with its family swapped to [`app_wide_family`] — every other
/// field (`size`/`weight`/`style`/`color`/`letter_spacing`/`line_height`/
/// `align`) carried over unchanged.
fn with_app_family(style: TextStyle) -> TextStyle {
    TextStyle {
        family: app_wide_family(),
        ..style
    }
}

/// Map every one of [`TypeScale`]'s 30 slots (15 M3 baseline roles + 15
/// M3-Expressive `_emphasized` siblings) onto [`app_wide_family`]. Sizes,
/// weights, colors, and every other token are untouched — family only (see
/// [`with_app_family`]). Glyph's Space Mono display slots are weight 700;
/// they become JetBrains Mono Bold, the same weight, new family.
fn jetbrains_type_scale(scale: TypeScale) -> TypeScale {
    TypeScale {
        display_large: with_app_family(scale.display_large),
        display_medium: with_app_family(scale.display_medium),
        display_small: with_app_family(scale.display_small),
        headline_large: with_app_family(scale.headline_large),
        headline_medium: with_app_family(scale.headline_medium),
        headline_small: with_app_family(scale.headline_small),
        title_large: with_app_family(scale.title_large),
        title_medium: with_app_family(scale.title_medium),
        title_small: with_app_family(scale.title_small),
        body_large: with_app_family(scale.body_large),
        body_medium: with_app_family(scale.body_medium),
        body_small: with_app_family(scale.body_small),
        label_large: with_app_family(scale.label_large),
        label_medium: with_app_family(scale.label_medium),
        label_small: with_app_family(scale.label_small),
        display_large_emphasized: with_app_family(scale.display_large_emphasized),
        display_medium_emphasized: with_app_family(scale.display_medium_emphasized),
        display_small_emphasized: with_app_family(scale.display_small_emphasized),
        headline_large_emphasized: with_app_family(scale.headline_large_emphasized),
        headline_medium_emphasized: with_app_family(scale.headline_medium_emphasized),
        headline_small_emphasized: with_app_family(scale.headline_small_emphasized),
        title_large_emphasized: with_app_family(scale.title_large_emphasized),
        title_medium_emphasized: with_app_family(scale.title_medium_emphasized),
        title_small_emphasized: with_app_family(scale.title_small_emphasized),
        body_large_emphasized: with_app_family(scale.body_large_emphasized),
        body_medium_emphasized: with_app_family(scale.body_medium_emphasized),
        body_small_emphasized: with_app_family(scale.body_small_emphasized),
        label_large_emphasized: with_app_family(scale.label_large_emphasized),
        label_medium_emphasized: with_app_family(scale.label_medium_emphasized),
        label_small_emphasized: with_app_family(scale.label_small_emphasized),
    }
}

/// Force the app-wide theme to [`Theme::glyph_baseline`] with every
/// type-scale slot's family swapped to JetBrains Mono (see
/// [`jetbrains_type_scale`]). Call once, from the app's setup block, after
/// [`register_bundled_fonts`] (see the [module docs](self) for the ordering
/// contract). `Theme::glyph_baseline()` — the canonical no-theme-installed
/// fallback — is deliberately untouched everywhere else; this only ever
/// forces the *active* theme via `frust::set_app_theme`.
///
/// **Only the bundled face is ever the app-wide one.** A downloaded font is a
/// *terminal* face, never the chrome's: the catalog is monospace Nerd Fonts,
/// and re-theming the whole app onto one would re-shape every screen from a
/// picker row.
pub fn apply_app_wide_theme() {
    let theme = Theme::builder(Theme::glyph_baseline())
        .map_type_scale(jetbrains_type_scale)
        .build();
    set_app_theme(theme);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use frust::authoring::text::TextContext;

    use super::*;

    #[test]
    fn registering_the_bundled_fonts_yields_exactly_the_expected_family_names() {
        let mut cx = TextContext::new();
        let mut names: BTreeSet<String> = BTreeSet::new();
        for bytes in BUNDLED_FONTS {
            let registered = cx
                .register_fonts(bytes.to_vec())
                .expect("every bundled TTF must parse");
            for family in registered {
                names.insert(family.name);
            }
        }
        let expected: BTreeSet<String> = BUNDLED_FAMILIES.iter().map(|f| f.to_string()).collect();
        assert_eq!(names, expected);
    }

    #[test]
    fn every_jetbrains_mono_face_registers_under_the_one_family() {
        // Guards the fontique typographic-family-preference fact the module
        // docs describe: Light/Medium's own family-name-table entries would
        // otherwise split them off from Regular/Bold/Italic into their own
        // families ("JetBrains Mono Light"/"JetBrains Mono Medium").
        let mut cx = TextContext::new();
        for bytes in [
            JETBRAINS_MONO_LIGHT,
            JETBRAINS_MONO_LIGHT_ITALIC,
            JETBRAINS_MONO_REGULAR,
            JETBRAINS_MONO_ITALIC,
            JETBRAINS_MONO_MEDIUM,
            JETBRAINS_MONO_BOLD,
        ] {
            let registered = cx
                .register_fonts(bytes.to_vec())
                .expect("every JetBrains Mono face must parse");
            assert_eq!(registered.len(), 1);
            assert_eq!(registered[0].name, DEFAULT_TERMINAL_FAMILY);
            assert_eq!(registered[0].face_count, 1);
        }
    }

    #[test]
    fn installed_families_starts_from_the_two_bundled_faces() {
        // Downloaded families are appended to these two, never instead of them
        // (a suite that installed one runs under its own cache root — see
        // `store`'s own tests).
        let families = installed_families();
        assert_eq!(
            &families[..2],
            &[
                DEFAULT_TERMINAL_FAMILY.to_string(),
                SYMBOLS_FALLBACK_FAMILY.to_string(),
            ]
        );
    }

    #[test]
    fn pickable_terminal_families_drops_the_symbols_fallback() {
        assert!(
            !pickable_terminal_families()
                .iter()
                .any(|family| family == SYMBOLS_FALLBACK_FAMILY),
            "the fallback face is never a primary-face choice"
        );
        assert!(
            pickable_terminal_families()
                .iter()
                .any(|family| family == DEFAULT_TERMINAL_FAMILY)
        );
    }

    #[test]
    fn a_bundled_family_displays_under_its_own_name() {
        assert_eq!(
            display_name(DEFAULT_TERMINAL_FAMILY),
            DEFAULT_TERMINAL_FAMILY
        );
        assert_eq!(display_name("Never Installed Mono"), "Never Installed Mono");
    }

    #[test]
    fn default_terminal_family_is_the_bundled_stack() {
        assert_eq!(
            default_terminal_family(),
            FontFamily::stack_with_generic(
                [DEFAULT_TERMINAL_FAMILY, SYMBOLS_FALLBACK_FAMILY],
                GenericSlot::Monospace,
            )
        );
    }

    #[test]
    fn resolve_terminal_family_uses_an_installed_stored_value_as_the_primary() {
        assert_eq!(
            resolve_terminal_family(DEFAULT_TERMINAL_FAMILY),
            default_terminal_family(),
            "the shipped default is itself an installed family"
        );
    }

    #[test]
    fn resolve_terminal_family_falls_back_for_an_unknown_stored_value() {
        // A persisted value naming a face this device does not have — a
        // pre-bundling default (`docs/redesign.html`'s own mock), or a
        // downloaded family whose cache was cleared.
        assert_eq!(
            resolve_terminal_family("Hack Nerd Font Mono"),
            default_terminal_family()
        );
    }

    #[test]
    fn the_app_wide_theme_maps_every_type_scale_slot_to_the_bundled_family() {
        let theme = Theme::builder(Theme::glyph_baseline())
            .map_type_scale(jetbrains_type_scale)
            .build();
        let expected = app_wide_family();
        let scale = &theme.type_scale;
        for family in [
            &scale.display_large.family,
            &scale.display_medium.family,
            &scale.display_small.family,
            &scale.headline_large.family,
            &scale.headline_medium.family,
            &scale.headline_small.family,
            &scale.title_large.family,
            &scale.title_medium.family,
            &scale.title_small.family,
            &scale.body_large.family,
            &scale.body_medium.family,
            &scale.body_small.family,
            &scale.label_large.family,
            &scale.label_medium.family,
            &scale.label_small.family,
            &scale.display_large_emphasized.family,
            &scale.display_medium_emphasized.family,
            &scale.display_small_emphasized.family,
            &scale.headline_large_emphasized.family,
            &scale.headline_medium_emphasized.family,
            &scale.headline_small_emphasized.family,
            &scale.title_large_emphasized.family,
            &scale.title_medium_emphasized.family,
            &scale.title_small_emphasized.family,
            &scale.body_large_emphasized.family,
            &scale.body_medium_emphasized.family,
            &scale.body_small_emphasized.family,
            &scale.label_large_emphasized.family,
            &scale.label_medium_emphasized.family,
            &scale.label_small_emphasized.family,
        ] {
            assert_eq!(*family, expected);
        }
    }

    #[test]
    fn the_app_wide_theme_keeps_every_non_family_token_from_the_glyph_baseline() {
        let baseline = Theme::glyph_baseline();
        let themed = Theme::builder(baseline.clone())
            .map_type_scale(jetbrains_type_scale)
            .build();
        // Sizes/weights/colors/every other token: unchanged — family only.
        assert_eq!(
            themed.type_scale.title_large_emphasized.size,
            baseline.type_scale.title_large_emphasized.size
        );
        assert_eq!(
            themed.type_scale.title_large_emphasized.weight,
            baseline.type_scale.title_large_emphasized.weight
        );
        assert_eq!(
            themed.type_scale.body_large.color,
            baseline.type_scale.body_large.color
        );
        assert_eq!(themed.light, baseline.light);
        assert_eq!(themed.dark, baseline.dark);
        assert_eq!(themed.shape, baseline.shape);
        assert_eq!(themed.motion, baseline.motion);
    }
}
