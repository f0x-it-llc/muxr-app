//! Bundled fonts (JetBrains Mono, Symbols Nerd Font Mono fallback) and the downloadable Nerd Font catalog.
//! Registration pushes into [`frust::register_app_fonts`]; downloaded faces install mid-session.
//! Call order: [`register_bundled_fonts`] before [`apply_app_wide_theme`].

pub mod catalog;
mod json;
pub mod manifest;
pub mod source;
pub mod store;

use frust::authoring::text::{FontFamily, GenericSlot, TextStyle};
use frust::{Theme, TypeScale, set_app_theme};

pub use source::{use_font_source_for_tests, use_offline_font_source_for_tests};
pub use store::{reset_installed_fonts_for_tests, use_font_cache_dir_for_tests};

/// The app-wide and terminal-default family; also the persisted preference default.
pub const DEFAULT_TERMINAL_FAMILY: &str = "JetBrains Mono";

/// Glyph fallback for private-use icons; trails a stack, never leads one.
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

/// Raw bytes of every bundled TTF, in registration order.
const BUNDLED_FONTS: [&[u8]; 7] = [
    JETBRAINS_MONO_LIGHT,
    JETBRAINS_MONO_LIGHT_ITALIC,
    JETBRAINS_MONO_REGULAR,
    JETBRAINS_MONO_ITALIC,
    JETBRAINS_MONO_MEDIUM,
    JETBRAINS_MONO_BOLD,
    SYMBOLS_NERD_FONT_MONO,
];

const BUNDLED_FAMILIES: [&str; 2] = [DEFAULT_TERMINAL_FAMILY, SYMBOLS_FALLBACK_FAMILY];

pub fn register_bundled_fonts() {
    for bytes in BUNDLED_FONTS {
        frust::register_app_fonts(bytes.to_vec());
    }
}

/// Re-registers cached families on the blocking pool; until it lands, a cache-only persisted family
/// resolves to the bundled stack.
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

pub fn installed_families() -> Vec<String> {
    let mut families: Vec<String> = BUNDLED_FAMILIES.iter().map(|f| f.to_string()).collect();
    for family in store::installed_downloads() {
        if !families.contains(&family) {
            families.push(family);
        }
    }
    families
}

/// The catalog's label for a downloaded family (from the cache sidecar), else the family name itself.
pub fn display_name(family: &str) -> String {
    if BUNDLED_FAMILIES.contains(&family) {
        return family.to_string();
    }
    store::display_name_for(family).unwrap_or_else(|| family.to_string())
}

/// [`installed_families`] minus the symbols fallback, which must never lead a stack.
pub fn pickable_terminal_families() -> Vec<String> {
    installed_families()
        .into_iter()
        .filter(|family| family != SYMBOLS_FALLBACK_FAMILY)
        .collect()
}

/// Primary + fallback stack ending in the `Monospace` generic slot.
pub fn default_terminal_family() -> FontFamily {
    FontFamily::stack_with_generic(
        [DEFAULT_TERMINAL_FAMILY, SYMBOLS_FALLBACK_FAMILY],
        GenericSlot::Monospace,
    )
}

/// Resolves a persisted family to a stack; an unknown family falls back to the default stack.
/// The data layer stores the choice unjudged, so this edge corrects an unresolvable family.
pub fn resolve_terminal_family(stored: &str) -> FontFamily {
    if installed_families().iter().any(|family| family == stored) {
        FontFamily::stack_with_generic([stored, SYMBOLS_FALLBACK_FAMILY], GenericSlot::Monospace)
    } else {
        default_terminal_family()
    }
}

fn app_wide_family() -> FontFamily {
    FontFamily::stack_with_generic([DEFAULT_TERMINAL_FAMILY], GenericSlot::Monospace)
}

fn with_app_family(style: TextStyle) -> TextStyle {
    style.with_family(app_wide_family())
}

/// Swaps the family of all 30 [`TypeScale`] slots; other tokens are untouched.
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

/// Forces the baseline theme with JetBrains Mono on every slot; call after [`register_bundled_fonts`].
/// Only the bundled face is app-wide; a downloaded font is terminal-only.
pub fn apply_app_wide_theme() {
    let theme = Theme::builder(frust_glyph::baseline())
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
        assert_eq!(
            resolve_terminal_family("Hack Nerd Font Mono"),
            default_terminal_family()
        );
    }

    #[test]
    fn the_app_wide_theme_maps_every_type_scale_slot_to_the_bundled_family() {
        let theme = Theme::builder(frust_glyph::baseline())
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
        let baseline = frust_glyph::baseline();
        let themed = Theme::builder(baseline.clone())
            .map_type_scale(jetbrains_type_scale)
            .build();
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
