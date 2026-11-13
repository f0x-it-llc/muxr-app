//! Cell style and colour resolution. Every slot resolves against the theme's dark side: the
//! grid paints on `GlyphInk::terminal_bg`, which never follows the platform brightness.

use frust::authoring::Color;
use frust::authoring::text::{FontFamily, FontStyle, FontWeight, LineHeight, TextStyle};
use frust::{Brightness, StatusPalette, Theme, Tween};
use frust_glyph::GlyphInk;

/// Cell height as a multiple of the font size (CSS unitless `line-height`).
pub const LINE_HEIGHT: f32 = 1.2;

/// Foreground alpha for SGR 2 (dim).
pub const DIM_ALPHA: f32 = 0.66;

/// Hand-tuned mix toward white for the bright slots 9-14.
pub const BRIGHT_MIX: f64 = 0.35;

/// Hand-tuned above the mock's 0.12: this wash paints over its text, CSS `::selection` behind.
pub const SELECTION_ALPHA: f32 = 0.28;

/// Slots 4 and 5 are hand-tuned: Glyph has no blue or magenta token.
pub const ANSI_BLUE: Color = Color::from_rgb8(0x6c, 0x8c, 0xff);

pub const ANSI_MAGENTA: Color = Color::from_rgb8(0xd9, 0x8a, 0xdf);

/// `ansi` holds `Color::Idx(0..=15)`: 0-7 normal, 8-15 bright.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalPalette {
    pub background: Color,
    pub foreground: Color,
    pub cursor: Color,
    /// Painted over the runs, so a cell with a non-default background still reads as selected.
    pub selection: Color,
    pub ansi: [Color; 16],
}

impl TerminalPalette {
    pub fn fallback() -> Self {
        terminal_palette(&frust_glyph::baseline())
    }
}

pub fn terminal_palette(theme: &Theme) -> TerminalPalette {
    let ink = theme
        .extension::<GlyphInk>()
        .copied()
        .unwrap_or_else(GlyphInk::default_ink);
    let status = theme
        .extension::<StatusPalette>()
        .copied()
        .unwrap_or_else(frust_glyph::tokens::color::status);
    let status = *status.colors(Brightness::Dark);

    let normal = [
        ink.terminal_bg,  // 0 black
        theme.dark.error, // 1 red
        status.success,   // 2 green
        status.warning,   // 3 yellow
        ANSI_BLUE,        // 4 blue
        ANSI_MAGENTA,     // 5 magenta
        status.info,      // 6 cyan
        ink.terminal_out, // 7 white
    ];
    let mut ansi = [Color::BLACK; 16];
    ansi[..8].copy_from_slice(&normal);
    ansi[8] = ink.terminal_comment;
    ansi[15] = ink.terminal_fg;
    for slot in 9..15 {
        ansi[slot] = brighten(normal[slot - 8]);
    }

    TerminalPalette {
        background: ink.terminal_bg,
        foreground: ink.terminal_fg,
        cursor: ink.terminal_prompt,
        selection: theme.dark.primary.with_alpha(SELECTION_ALPHA),
        ansi,
    }
}

fn brighten(color: Color) -> Color {
    Tween::new(color, Color::WHITE).lerp(BRIGHT_MIX)
}

/// Two consecutive cells batch into one run iff their whole `CellStyle`s are equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CellStyle {
    pub fg: vt100::Color,
    pub bg: vt100::Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub dim: bool,
}

impl CellStyle {
    pub fn from_cell(cell: &vt100::Cell) -> Self {
        Self {
            fg: cell.fgcolor(),
            bg: cell.bgcolor(),
            bold: cell.bold(),
            italic: cell.italic(),
            underline: cell.underline(),
            inverse: cell.inverse(),
            dim: cell.dim(),
        }
    }

    /// The background is `None` when it equals the palette's backdrop. Default sentinels resolve
    /// before `inverse` swaps, or `ESC[7m` on an unstyled cell would not invert.
    pub fn resolve(self, palette: &TerminalPalette) -> (Color, Option<Color>) {
        let fg = resolve_color(self.fg, palette.foreground, palette);
        let bg = resolve_color(self.bg, palette.background, palette);
        let (mut fg, bg) = if self.inverse { (bg, fg) } else { (fg, bg) };
        if self.dim {
            fg = fg.with_alpha(DIM_ALPHA);
        }
        (fg, (bg != palette.background).then_some(bg))
    }

    pub fn text_style(self, font_size: f32, color: Color, family: &FontFamily) -> TextStyle {
        TextStyle::new(font_size, color)
            .with_family(family.clone())
            .with_weight(if self.bold {
                FontWeight::SEMI_BOLD
            } else {
                FontWeight::REGULAR
            })
            .with_style(if self.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            })
            .with_line_height(LineHeight::FontSizeRelative(LINE_HEIGHT))
    }
}

pub fn resolve_color(color: vt100::Color, fallback: Color, palette: &TerminalPalette) -> Color {
    match color {
        vt100::Color::Default => fallback,
        vt100::Color::Idx(i) => indexed_color(i, palette),
        vt100::Color::Rgb(r, g, b) => Color::from_rgb8(r, g, b),
    }
}

/// Only 0-15 are themed: the 6x6x6 cube and the greyscale ramp stay computed so a program's
/// gradients are not re-tinted.
pub fn indexed_color(i: u8, palette: &TerminalPalette) -> Color {
    match i {
        0..=15 => palette.ansi[i as usize],
        16..=231 => {
            const LEVELS: [u8; 6] = [0x00, 0x5F, 0x87, 0xAF, 0xD7, 0xFF];
            let v = i - 16;
            Color::from_rgb8(
                LEVELS[(v / 36) as usize],
                LEVELS[((v % 36) / 6) as usize],
                LEVELS[(v % 6) as usize],
            )
        }
        232..=255 => {
            let g = 8 + (i - 232) * 10;
            Color::from_rgb8(g, g, g)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn palette() -> TerminalPalette {
        TerminalPalette::fallback()
    }

    #[test]
    fn the_palette_resolves_from_glyph_ink_not_from_literals() {
        let theme = frust_glyph::baseline();
        let ink = theme
            .extension::<GlyphInk>()
            .copied()
            .expect("the Glyph baseline attaches GlyphInk");
        let palette = terminal_palette(&theme);

        assert_eq!(palette.background, ink.terminal_bg);
        assert_eq!(palette.foreground, ink.terminal_fg);
        assert_eq!(palette.cursor, ink.terminal_prompt);
        assert_eq!(
            palette.selection,
            theme.dark.primary.with_alpha(SELECTION_ALPHA),
            "the selection wash is the mock's own accent, pinned dark"
        );
        assert_eq!(palette.ansi[0], ink.terminal_bg, "ANSI black = the surface");
        assert_eq!(palette.ansi[7], ink.terminal_out);
        assert_eq!(palette.ansi[8], ink.terminal_comment);
        assert_eq!(palette.ansi[15], ink.terminal_fg);
    }

    #[test]
    fn the_palette_is_brightness_invariant_like_the_ink_it_sits_on() {
        let mut dark = frust_glyph::baseline();
        dark.brightness = Brightness::Dark;
        let mut light = frust_glyph::baseline();
        light.brightness = Brightness::Light;
        assert_eq!(terminal_palette(&dark), terminal_palette(&light));
    }

    #[test]
    fn every_ansi_slot_is_distinct_so_a_colourized_log_stays_readable() {
        let palette = palette();
        for (i, a) in palette.ansi.iter().enumerate() {
            for (j, b) in palette.ansi.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "ANSI slots {i} and {j} collide");
                }
            }
        }
    }

    #[test]
    fn bright_slots_are_lighter_than_their_normal_pairs() {
        let palette = palette();
        for slot in 1..7usize {
            let normal = palette.ansi[slot].components;
            let bright = palette.ansi[slot + 8].components;
            let sum = |c: [f32; 4]| c[0] + c[1] + c[2];
            assert!(
                sum(bright) > sum(normal),
                "slot {} must be brighter than slot {slot}",
                slot + 8
            );
        }
    }

    #[test]
    fn default_colors_fall_back_and_inverse_swaps_them_after_resolution() {
        let palette = palette();
        let (fg, bg) = CellStyle::default().resolve(&palette);
        assert_eq!(fg, palette.foreground);
        assert_eq!(bg, None, "the default background is the whole-grid fill");

        let inverted = CellStyle {
            inverse: true,
            ..CellStyle::default()
        };
        let (fg, bg) = inverted.resolve(&palette);
        assert_eq!(fg, palette.background);
        assert_eq!(
            bg,
            Some(palette.foreground),
            "inverted runs paint a background"
        );
    }

    #[test]
    fn dim_dulls_the_foreground_without_touching_the_background() {
        let palette = palette();
        let dim = CellStyle {
            dim: true,
            fg: vt100::Color::Idx(2),
            ..CellStyle::default()
        };
        let (fg, bg) = dim.resolve(&palette);
        assert_eq!(fg, palette.ansi[2].with_alpha(DIM_ALPHA));
        assert_eq!(bg, None);
    }

    #[test]
    fn indexed_colors_cover_the_themed_16_the_cube_and_the_greyscale_ramp() {
        let palette = palette();
        assert_eq!(indexed_color(1, &palette), palette.ansi[1]);
        assert_eq!(indexed_color(15, &palette), palette.ansi[15]);
        assert_eq!(indexed_color(16, &palette), Color::from_rgb8(0, 0, 0));
        assert_eq!(
            indexed_color(231, &palette),
            Color::from_rgb8(0xFF, 0xFF, 0xFF)
        );
        assert_eq!(indexed_color(232, &palette), Color::from_rgb8(8, 8, 8));
        assert_eq!(
            indexed_color(255, &palette),
            Color::from_rgb8(238, 238, 238)
        );
    }

    #[test]
    fn a_true_colour_escape_paints_exactly_what_it_asked_for() {
        let palette = palette();
        let style = CellStyle {
            fg: vt100::Color::Rgb(0x12, 0x34, 0x56),
            ..CellStyle::default()
        };
        assert_eq!(
            style.resolve(&palette).0,
            Color::from_rgb8(0x12, 0x34, 0x56)
        );
    }

    #[test]
    fn bold_and_italic_pick_real_faces_not_synthesized_ones() {
        let family = crate::fonts::default_terminal_family();
        let plain = CellStyle::default().text_style(12.0, Color::WHITE, &family);
        assert_eq!(plain.weight, FontWeight::REGULAR);
        assert_eq!(plain.style, FontStyle::Normal);

        let loud = CellStyle {
            bold: true,
            italic: true,
            ..CellStyle::default()
        }
        .text_style(12.0, Color::WHITE, &family);
        assert_eq!(loud.weight, FontWeight::SEMI_BOLD);
        assert_eq!(loud.style, FontStyle::Italic);
        assert_eq!(loud.size, 12.0);
    }

    #[test]
    fn text_style_carries_the_family_it_was_given() {
        let family = FontFamily::named("Comic Sans");
        let style = CellStyle::default().text_style(12.0, Color::WHITE, &family);
        assert_eq!(style.family, family);
    }
}
