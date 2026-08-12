//! Bundled fonts: JetBrains Mono as the app-wide (and terminal-default) face,
//! Symbols Nerd Font Mono as the always-present nerd-font/powerline glyph
//! fallback.
//!
//! # Registration is a push, embedded at compile time
//!
//! [`register_bundled_fonts`] pushes every bundled TTF's raw bytes (each
//! `include_bytes!`d from `assets/fonts/`, see [`BUNDLED_FONTS`]) into
//! [`frust::register_app_fonts`]'s process-wide pending slot — identical on
//! Android/iOS/desktop, no Gradle/Xcode asset wiring. Bundled bytes rather
//! than a loose asset file: the running shell drains the slot into its
//! `TextContext` at construction (before the first frame), so the face must
//! already be linked into the binary by then.
//!
//! # Family names are fontique's, not ours
//!
//! fontique resolves a registered font's family by its own name-table
//! preference (typographic family, name id 16, over family, name id 1) —
//! [`installed_families`]'s unit test constructs its own `TextContext` and
//! registers the same bytes [`register_bundled_fonts`] pushes, asserting the
//! resulting names match [`DEFAULT_TERMINAL_FAMILY`]/
//! [`SYMBOLS_FALLBACK_FAMILY`] exactly — a tripwire against silent TTF-name
//! drift (a font update that renames its own family would otherwise leave
//! every `FontFamily::stack`/`stack_with_generic` call below resolving to
//! nothing, with no compile error).
//!
//! # Call order: register, then re-theme
//!
//! [`register_bundled_fonts`] must run before [`apply_app_wide_theme`] —
//! both from the app's `setup` block, alongside `frust::glyph_theme::install`
//! (the pre-first-frame slot every shell drains at construction; see that
//! function's own doc for why a later call is too late). [`apply_app_wide_theme`]
//! itself doesn't touch a font byte, only [`frust_theme::TypeScale`]'s
//! `family` tokens — the bytes those tokens now name must already be pending
//! by the time this runs, or the shell falls back to whatever monospace face
//! the platform has (the `Monospace` generic tail every stack below ends in).

use frust::authoring::text::{FontFamily, GenericSlot, TextStyle};
use frust::{Theme, TypeScale, set_app_theme};

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

const JETBRAINS_MONO_LIGHT: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Light.ttf");
const JETBRAINS_MONO_LIGHT_ITALIC: &[u8] =
    include_bytes!("../assets/fonts/JetBrainsMono-LightItalic.ttf");
const JETBRAINS_MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
const JETBRAINS_MONO_ITALIC: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf");
const JETBRAINS_MONO_MEDIUM: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf");
const JETBRAINS_MONO_BOLD: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf");
const SYMBOLS_NERD_FONT_MONO: &[u8] =
    include_bytes!("../assets/fonts/SymbolsNerdFontMono-Regular.ttf");

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

/// Push every bundled TTF into `frust`'s pending font registry. Call once,
/// from the app's setup block, before [`apply_app_wide_theme`] (see the
/// [module docs](self)).
pub fn register_bundled_fonts() {
    for bytes in BUNDLED_FONTS {
        frust::register_app_fonts(bytes.to_vec());
    }
}

/// The family names [`register_bundled_fonts`] installs — bundled-only for
/// now (a future task extends this with downloaded faces). See the
/// [module docs](self) for why this is asserted against fontique's actual
/// resolution rather than just trusted.
pub fn installed_families() -> Vec<String> {
    vec![
        DEFAULT_TERMINAL_FAMILY.to_string(),
        SYMBOLS_FALLBACK_FAMILY.to_string(),
    ]
}

/// Font families a user may pick as the terminal's *primary* face:
/// [`installed_families`] minus [`SYMBOLS_FALLBACK_FAMILY`] — a fallback face
/// that covers private-use glyphs the primary face lacks, never something a
/// picker should offer as the leading choice (see that constant's own doc).
/// Dynamic rather than a const, so a later task's downloaded faces show up
/// here for free once they land in [`installed_families`].
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
/// family a later task's catalog hasn't downloaded yet. Never errors: an
/// unresolvable preference degrades to the shipped default rather than a
/// blank or broken stack.
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
        let expected: BTreeSet<String> = installed_families().into_iter().collect();
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
    fn installed_families_names_the_two_bundled_faces() {
        assert_eq!(
            installed_families(),
            vec![
                DEFAULT_TERMINAL_FAMILY.to_string(),
                SYMBOLS_FALLBACK_FAMILY.to_string(),
            ]
        );
    }

    #[test]
    fn pickable_terminal_families_drops_the_symbols_fallback() {
        assert_eq!(
            pickable_terminal_families(),
            vec![DEFAULT_TERMINAL_FAMILY.to_string()],
            "the fallback face is never a primary-face choice"
        );
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
        // A pre-task-27 persisted value (`docs/redesign.html`'s own mock
        // default) that no longer names a bundled face.
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
