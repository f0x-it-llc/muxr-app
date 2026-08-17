//! [`Palette`] — the app's one theme-derived colour vocabulary, adopted by
//! all four features (`connect`/`sessions`/`terminal`/`settings`) in place of
//! the per-feature `Palette` struct each used to carry in its own
//! `presentation::widgets` (`docs/ARCHITECTURE.md`'s UI Substrate section).
//!
//! **Excluded on purpose**: `terminal`'s `GlyphInk`-sourced card fields (its
//! card/head/prompt/out/comment tones) and its hardcoded-RGB hairline pair
//! (a card border and an expand-button wash) stay terminal-local — they tint
//! one screen's fixed-tone card, not a role any other screen shares.
//!
//! **One divergence resolved here**: `connect`'s `amber` used to read
//! `scheme.primary_container`, while the other three read `scheme.primary`.
//! This module unifies on `scheme.primary` — byte-identical to
//! `primary_container` under the Glyph **dark** `ColorScheme`
//! (`crates/frust-theme`'s `glyph_dark`: both resolve to the same bright-fill
//! amber). The Glyph **light** scheme diverges — `primary` is the darker
//! text-appropriate amber, `primary_container` the bright fill — so this
//! unification is currently a no-op pixel-wise only because
//! `frust_glyph::baseline()` starts (and this app never leaves)
//! `Brightness::Dark`; a future light-mode adoption would need to re-audit
//! every `connect` call site this changed.
//!
//! **Two fields, one role**: `sessions`/`settings`/`terminal` name the page
//! tone `base`, `connect` names the same `--bg-base` role `page`. Both
//! survive here, identically derived, so an adopting call site keeps reading
//! under its own existing name.

use frust::authoring::Color;
use frust::authoring::text::TextStyle;
use frust::{StatusPalette, Theme};

/// `--amber-faint: rgba(255,182,39,0.12)` — the accent at 12% over whatever
/// is behind it. No `ColorScheme` role carries a translucent accent wash, so
/// every screen derives it from `primary` by alpha instead.
const AMBER_FAINT_ALPHA: f32 = 0.12;
/// `--amber-border: rgba(255,182,39,0.35)` — same derivation, hairline
/// weight.
const AMBER_BORDER_ALPHA: f32 = 0.35;
/// `--fg-dim` has no `ColorScheme` role of its own (frust-theme records it as
/// a documented leftover), so it derives from `on_surface_variant` by alpha —
/// the constant every screen already shared.
const FG_DIM_ALPHA: f32 = 0.62;
/// `--fg-faintest`, derived the same way as [`FG_DIM_ALPHA`] and for the same
/// reason — `settings`' own constant, the only screen that used this role.
const FG_FAINTEST_ALPHA: f32 = 0.22;

/// The active theme, or the Glyph baseline when none is threaded (headless
/// tests, a shell with no design system installed). **Never**
/// `Theme::neutral`/`m3_baseline` — see `lib.rs`'s design-system note.
pub fn active_theme() -> Theme {
    frust::use_context::<Theme>().unwrap_or_else(frust_glyph::baseline)
}

/// Every scheme-/[`StatusPalette`]-derived colour role a muxr screen paints,
/// resolved once per build from the active theme. See the [module
/// docs](self) for what stays out of this union.
#[derive(Clone, Copy)]
pub struct Palette {
    /// `--bg-base`: the screen's page tone (`sessions`/`settings`/
    /// `terminal`'s name for the role — see [`Palette::page`]).
    pub base: Color,
    /// `--bg-base`, `connect`'s own name for the same role as
    /// [`Palette::base`] — see the [module docs](self).
    pub page: Color,
    /// `--bg-surface`: the card fill (`sessions`).
    pub surface: Color,
    /// `--bg-raised`: a mark tile, pill or chip fill.
    pub raised: Color,
    /// `--bg-void`: the tone behind a card that sits over the page
    /// (`terminal`).
    pub void: Color,
    /// `--border-bright`: every hairline border.
    pub outline: Color,
    /// `--border`: a fainter separator rule.
    pub rule: Color,
    /// `--amber`: brand/CTA/selection ink only, never a health signal.
    pub amber: Color,
    /// `--amber-dim`: `terminal`'s brightness-invariant `primary_fixed_dim`.
    pub amber_dim: Color,
    /// `--amber-faint`, derived (see [`AMBER_FAINT_ALPHA`]).
    pub amber_faint: Color,
    /// `--amber-border`, derived (see [`AMBER_BORDER_ALPHA`]).
    pub amber_border: Color,
    /// `--fg`: primary text.
    pub fg: Color,
    /// `--fg-muted`: secondary text.
    pub fg_muted: Color,
    /// `--fg-dim`, derived (see [`FG_DIM_ALPHA`]).
    pub fg_dim: Color,
    /// `--fg-faintest`, derived (see [`FG_FAINTEST_ALPHA`]) (`settings`).
    pub fg_faintest: Color,
    /// `--error`.
    pub error: Color,
    /// `--success`, [`StatusPalette`]-first with a `scheme.primary` fallback.
    pub success: Color,
    /// `--success-faint`, [`StatusPalette`]-first with a
    /// `scheme.surface_container_high` fallback (`sessions`).
    pub success_faint: Color,
    /// `--warning`, [`StatusPalette`]-first with a `scheme.primary` fallback
    /// (`settings`).
    pub warning: Color,
    /// `--cyan`/info, [`StatusPalette`]-first with a `scheme.tertiary`
    /// fallback (`settings`).
    pub cyan: Color,
    /// `--cyan-faint`, [`StatusPalette`]-first with a
    /// `scheme.tertiary_container` fallback (`settings`).
    pub cyan_faint: Color,
}

impl Palette {
    /// Resolve every field above from `theme`, extending [`StatusPalette`]
    /// (see the [module docs](self)) exactly once — the lookup all four
    /// screens used to duplicate.
    pub fn from_theme(theme: &Theme) -> Palette {
        let scheme = theme.scheme();
        // Per `docs/CODE_STANDARDS.md`'s Glyph precedent: a per-status colour
        // with no `ColorScheme` field resolves `StatusPalette` first. A Glyph
        // theme always carries it; the listed scheme roles are the
        // last-resort inks if some other theme is ever installed.
        let status = theme.extension::<StatusPalette>();
        Palette {
            base: scheme.surface_container_low,
            page: scheme.surface_container_low,
            surface: scheme.surface,
            raised: scheme.surface_container_high,
            void: scheme.surface_container_lowest,
            outline: scheme.outline,
            rule: scheme.outline_variant,
            amber: scheme.primary,
            amber_dim: scheme.primary_fixed_dim,
            amber_faint: scheme.primary.with_alpha(AMBER_FAINT_ALPHA),
            amber_border: scheme.primary.with_alpha(AMBER_BORDER_ALPHA),
            fg: scheme.on_surface,
            fg_muted: scheme.on_surface_variant,
            fg_dim: scheme.on_surface_variant.with_alpha(FG_DIM_ALPHA),
            fg_faintest: scheme.on_surface_variant.with_alpha(FG_FAINTEST_ALPHA),
            error: scheme.error,
            success: status.map_or(scheme.primary, |s| s.colors(theme.brightness).success),
            success_faint: status.map_or(scheme.surface_container_high, |s| {
                s.colors(theme.brightness).success_container
            }),
            warning: status.map_or(scheme.primary, |s| s.colors(theme.brightness).warning),
            // The Glyph scheme's `tertiary` family *is* the cyan info hue
            // (its only spare), so it is the honest fallback for a theme
            // with no status palette rather than a second amber.
            cyan: status.map_or(scheme.tertiary, |s| s.colors(theme.brightness).info),
            cyan_faint: status.map_or(scheme.tertiary_container, |s| {
                s.colors(theme.brightness).info_container
            }),
        }
    }
}

/// `style` re-inked with `color` — every run picks a Glyph type role for
/// size/family/weight and an explicit role-resolved ink, so nothing falls
/// back to `TextStyle::default()`'s 16px black.
pub fn inked(style: &TextStyle, color: Color) -> TextStyle {
    let mut style = style.clone();
    style.color = color;
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::ThemeExtensions;

    #[test]
    fn fg_dim_derives_from_on_surface_variant_by_the_documented_alpha() {
        let theme = frust_glyph::baseline();
        let palette = Palette::from_theme(&theme);
        assert_eq!(
            palette.fg_dim,
            theme.scheme().on_surface_variant.with_alpha(FG_DIM_ALPHA)
        );
    }

    #[test]
    fn fg_faintest_derives_from_on_surface_variant_by_the_documented_alpha() {
        let theme = frust_glyph::baseline();
        let palette = Palette::from_theme(&theme);
        assert_eq!(
            palette.fg_faintest,
            theme
                .scheme()
                .on_surface_variant
                .with_alpha(FG_FAINTEST_ALPHA)
        );
    }

    #[test]
    fn amber_unifies_on_scheme_primary_for_every_screen() {
        let theme = frust_glyph::baseline();
        let palette = Palette::from_theme(&theme);
        assert_eq!(palette.amber, theme.scheme().primary);
    }

    /// The unification the module docs describe: asserts the equality
    /// relation (`primary == primary_container` under `frust_glyph::baseline()`'s
    /// **dark** scheme) that this module's `amber` unification depends on, so
    /// a future `frust-theme` change that splits the accent/container pair
    /// apart under Dark fails loudly here rather than only showing up as a
    /// pixel diff in a screen's device gate. (The literal hex is pinned by
    /// `canonical_field_values_match_the_glyph_dark_source`, below.)
    #[test]
    fn glyph_baseline_dark_scheme_keeps_primary_and_primary_container_equal() {
        let theme = frust_glyph::baseline();
        assert_eq!(theme.brightness, frust::Brightness::Dark);
        let scheme = theme.scheme();
        assert_eq!(scheme.primary, scheme.primary_container);
    }

    /// A handful of concrete, hand-verified field values against
    /// `frust_glyph::baseline()` — the derivations above prove the *shape* of
    /// each field's source, but not that the source itself still resolves to
    /// what `docs/glyph-design-system.html` actually specifies. Pinned to the
    /// literal hex the design system's dark `:root` block carries
    /// (`crates/frust-theme`'s `glyph_dark`/`StatusPalette::glyph` sources).
    #[test]
    fn canonical_field_values_match_the_glyph_dark_source() {
        let theme = frust_glyph::baseline();
        let palette = Palette::from_theme(&theme);
        assert_eq!(palette.page, Color::from_rgb8(0x10, 0x13, 0x1a)); // --bg-base
        assert_eq!(palette.success, Color::from_rgb8(0x5f, 0xd8, 0x8f)); // --success
        assert_eq!(palette.amber, Color::from_rgb8(0xff, 0xb6, 0x27)); // --amber
    }

    #[test]
    fn base_and_page_are_the_same_bg_base_role_under_two_names() {
        let theme = frust_glyph::baseline();
        let palette = Palette::from_theme(&theme);
        assert_eq!(palette.base, palette.page);
        assert_eq!(palette.base, theme.scheme().surface_container_low);
    }

    #[test]
    fn success_warning_and_cyan_resolve_status_palette_when_attached() {
        let theme = frust_glyph::baseline();
        let status = theme
            .extension::<StatusPalette>()
            .expect("glyph baseline attaches a StatusPalette");
        let colors = status.colors(theme.brightness);
        let palette = Palette::from_theme(&theme);
        assert_eq!(palette.success, colors.success);
        assert_eq!(palette.success_faint, colors.success_container);
        assert_eq!(palette.warning, colors.warning);
        assert_eq!(palette.cyan, colors.info);
        assert_eq!(palette.cyan_faint, colors.info_container);
    }

    #[test]
    fn success_warning_and_cyan_fall_back_to_scheme_roles_with_no_status_palette() {
        let mut theme = frust_glyph::baseline();
        theme.extensions = ThemeExtensions::new();
        let scheme = *theme.scheme();
        let palette = Palette::from_theme(&theme);
        assert_eq!(palette.success, scheme.primary);
        assert_eq!(palette.success_faint, scheme.surface_container_high);
        assert_eq!(palette.warning, scheme.primary);
        assert_eq!(palette.cyan, scheme.tertiary);
        assert_eq!(palette.cyan_faint, scheme.tertiary_container);
    }

    #[test]
    fn inked_overrides_only_the_colour() {
        let style = TextStyle::default();
        let recolored = inked(&style, Color::from_rgb8(1, 2, 3));
        assert_eq!(recolored.color, Color::from_rgb8(1, 2, 3));
        assert_eq!(recolored.size, style.size);
        assert_eq!(recolored.family, style.family);
    }
}
