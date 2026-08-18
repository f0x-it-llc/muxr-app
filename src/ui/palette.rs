use frust::authoring::Color;
use frust::authoring::text::TextStyle;
use frust::{StatusPalette, Theme};

const AMBER_FAINT_ALPHA: f32 = 0.12;
const AMBER_BORDER_ALPHA: f32 = 0.35;
const FG_DIM_ALPHA: f32 = 0.62;
const FG_FAINTEST_ALPHA: f32 = 0.22;

pub fn active_theme() -> Theme {
    frust::use_context::<Theme>().unwrap_or_else(frust_glyph::baseline)
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub base: Color,
    pub page: Color,
    pub surface: Color,
    pub raised: Color,
    pub void: Color,
    pub outline: Color,
    pub rule: Color,
    pub amber: Color,
    pub amber_dim: Color,
    pub amber_faint: Color,
    pub amber_border: Color,
    pub fg: Color,
    pub fg_muted: Color,
    pub fg_dim: Color,
    pub fg_faintest: Color,
    pub error: Color,
    pub success: Color,
    pub success_faint: Color,
    pub warning: Color,
    pub cyan: Color,
    pub cyan_faint: Color,
}

impl Palette {
    /// Resolve every field above from the theme's scheme, falling back to
    /// scheme roles when no [`StatusPalette`] extension is attached.
    pub fn from_theme(theme: &Theme) -> Palette {
        let scheme = theme.scheme();
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

            cyan: status.map_or(scheme.tertiary, |s| s.colors(theme.brightness).info),
            cyan_faint: status.map_or(scheme.tertiary_container, |s| {
                s.colors(theme.brightness).info_container
            }),
        }
    }
}

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

    /// Guards this module's `amber` unification: `primary ==
    /// primary_container` under `frust_glyph::baseline()`'s dark scheme. A
    /// future `frust-theme` change splitting that pair apart under Dark
    /// fails loudly here, not only as a pixel diff in a device gate. (Hex
    /// pinned by `canonical_field_values_match_the_glyph_dark_source`, below.)
    #[test]
    fn glyph_baseline_dark_scheme_keeps_primary_and_primary_container_equal() {
        let theme = frust_glyph::baseline();
        assert_eq!(theme.brightness, frust::Brightness::Dark);
        let scheme = theme.scheme();
        assert_eq!(scheme.primary, scheme.primary_container);
    }

    /// Hand-verified against `frust_glyph::baseline()`: the derivations above
    /// prove the *shape* of each field's source, not that the source still
    /// matches what `docs/glyph-design-system.html` specifies. Pinned to the
    /// literal hex the design system's dark `:root` block carries
    /// (`crates/frust-theme`'s `glyph_dark`/`StatusPalette::glyph`).
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
