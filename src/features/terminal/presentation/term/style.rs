//! Cell style and colour resolution: the [`TerminalPalette`] the grid paints
//! from (its own doc carries the theme-token → ANSI-16 mapping table), and
//! [`CellStyle`], the whole of what makes two neighbouring cells batch into
//! one run. [`terminal_palette`] resolves the palette once per
//! `Component::build` and hands it to the widget as a plain value — no
//! widget-side theme lookup per frame.
//!
//! **Every slot resolves against the theme's DARK side, whatever the platform
//! brightness is.** The grid's surface is `GlyphInk::terminal_bg`, which is
//! brightness-invariant by Glyph's own mandate — pulling, say,
//! `StatusPalette`'s *light* warning (tuned for AA on paper) would paint
//! dark-on-dark the moment the phone left dark mode (the trap
//! `docs/CODE_STANDARDS.md` records as "`GlyphInk` is never brightness-swapped").
//!
//! **Two slots have no token at all.** Glyph is a single-accent system whose
//! only hue-bearing tokens are amber, red, green and cyan — no blue, no
//! magenta, and the nearest scheme role (`tertiary`) reuses the cyan hue,
//! colliding slot 4 with slot 6. So [`ANSI_BLUE`]/[`ANSI_MAGENTA`] are
//! hand-tuned constants in the same luminance band as the tokened hues rather
//! than xterm's classic table, whose `#0000EE` blue is illegible on a
//! `#1c1912` surface. The honest fix is a `GlyphInk` widening upstream.

use frust::authoring::Color;
use frust::authoring::text::{FontFamily, FontStyle, FontWeight, LineHeight, TextStyle};
use frust::{Brightness, StatusPalette, Theme, Tween};
use frust_glyph::GlyphInk;

/// Cell height as a multiple of the font size (CSS unitless `line-height`).
/// 1.2 — kterm's `TerminalStyle.height` default and what the `s9_terminal`
/// benchmark ran at, so row pitch derives from font size the same way. The
/// *measured* line box still comes back from the shaper; this only sets what
/// the shaper is asked for. See `geometry`.
pub const LINE_HEIGHT: f32 = 1.2;

/// Alpha applied to the foreground for SGR 2 (dim).
pub const DIM_ALPHA: f32 = 0.66;

/// How far a normal slot is mixed toward white to make its bright pair (slots
/// 9-14). **Hand-tuned**: xterm's literals aren't derivable from Glyph's
/// tokens, but "visibly lighter, same hue" is. 0.35 is where the pairs
/// separate clearly on `terminal_bg` without washing out.
pub const BRIGHT_MIX: f64 = 0.35;

/// Alpha of the selection wash over the runs it covers. **Hand-tuned, and
/// deliberately not the mock's 0.12** (see the [module docs](self)): a CSS
/// `::selection` background paints *behind* its text, and this one paints
/// over it. High enough to be unmistakable over a coloured run, low enough
/// that the glyphs under it stay readable.
pub const SELECTION_ALPHA: f32 = 0.28;

/// ANSI slot 4 (blue). **Hand-tuned** — Glyph carries no blue token (see the
/// [module docs](self)); this sits in the same luminance band as the tokened hues.
pub const ANSI_BLUE: Color = Color::from_rgb8(0x6c, 0x8c, 0xff);

/// ANSI slot 5 (magenta). **Hand-tuned**, for the same reason as
/// [`ANSI_BLUE`].
pub const ANSI_MAGENTA: Color = Color::from_rgb8(0xd9, 0x8a, 0xdf);

/// Every colour the grid paints, resolved once per build from the active theme by
/// [`terminal_palette`] — the mapping, and the two gaps the [module docs](self)
/// explain:
///
/// | Slot | ANSI | Source |
/// |---|---|---|
/// | default bg | SGR 49 | `GlyphInk::terminal_bg` |
/// | default fg | SGR 39 | `GlyphInk::terminal_fg` — the *untyped* body ink, which is what "no SGR colour set" means |
/// | cursor | — | `GlyphInk::terminal_prompt` (the amber caret the mock blinks) |
/// | selection | — | `Theme::dark.primary` at [`SELECTION_ALPHA`] |
/// | 0 | black | `GlyphInk::terminal_bg` — `ESC[30m` vanishes into the surface, as in a real terminal |
/// | 1 | red | `Theme::dark.error` |
/// | 2 | green | `StatusPalette` dark `success` |
/// | 3 | yellow | `StatusPalette` dark `warning` (**not** the accent amber, reserved for the cursor/prompt) |
/// | 4 | blue | [`ANSI_BLUE`] |
/// | 5 | magenta | [`ANSI_MAGENTA`] |
/// | 6 | cyan | `StatusPalette` dark `info` (Glyph's one spare hue) |
/// | 7 | white | `GlyphInk::terminal_out` — the dimmer output tier |
/// | 8 | bright black | `GlyphInk::terminal_comment` — the dimmest ink, i.e. "grey" |
/// | 9-14 | bright r/g/y/b/m/c | their normal slot mixed [`BRIGHT_MIX`] toward white |
/// | 15 | bright white | `GlyphInk::terminal_fg` |
/// | 16-231 / 232-255 | xterm cube / greyscale | computed, not tokened ([`indexed_color`]) |
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalPalette {
    /// SGR 49 / never-set: the whole-grid backdrop fill.
    pub background: Color,
    /// SGR 39 / never-set.
    pub foreground: Color,
    /// The cursor's ink (block fill, underline, bar, and the unfocused
    /// outline).
    pub cursor: Color,
    /// The wash a selected cell is painted over with: the same swatch as the
    /// mock's `--amber`, painted **over** the runs rather than behind them, so
    /// a cell with a non-default background still reads as selected. CSS
    /// `::selection` sits *behind* its text (why the mock's 0.12 is enough
    /// there, [`SELECTION_ALPHA`] higher here); text isn't recoloured, which
    /// would re-shape every partly selected run on every pointer move.
    pub selection: Color,
    /// `Color::Idx(0..=15)`: 0-7 normal, 8-15 bright.
    pub ansi: [Color; 16],
}

impl TerminalPalette {
    /// The palette for an app with no theme installed at all — Glyph's own
    /// baseline tokens, so the grid still paints its intended colours.
    pub fn fallback() -> Self {
        terminal_palette(&frust_glyph::baseline())
    }
}

/// Resolve [`TerminalPalette`] from `theme` (see the [module docs](self)).
pub fn terminal_palette(theme: &Theme) -> TerminalPalette {
    let ink = theme
        .extension::<GlyphInk>()
        .copied()
        .unwrap_or_else(GlyphInk::default_ink);
    // Explicitly the DARK status colours, whatever the platform brightness —
    // see the module docs' brightness note.
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
    // The two tokened bright slots, then the derived six between them.
    ansi[8] = ink.terminal_comment;
    ansi[15] = ink.terminal_fg;
    for slot in 9..15 {
        ansi[slot] = brighten(normal[slot - 8]);
    }

    TerminalPalette {
        background: ink.terminal_bg,
        foreground: ink.terminal_fg,
        cursor: ink.terminal_prompt,
        // Explicitly the DARK accent, for the same reason the status colours
        // above are: this palette never follows the platform brightness.
        selection: theme.dark.primary.with_alpha(SELECTION_ALPHA),
        ansi,
    }
}

/// A normal slot's bright pair: [`BRIGHT_MIX`] of the way to white.
fn brighten(color: Color) -> Color {
    Tween::new(color, Color::WHITE).lerp(BRIGHT_MIX)
}

/// Everything about a cell that affects how it paints. Two consecutive cells
/// batch into one run iff their whole `CellStyle`s are equal — the same
/// contract kterm and the `s9_terminal` benchmark batch on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CellStyle {
    /// SGR foreground, unresolved (a `Color::Default` sentinel until
    /// [`CellStyle::resolve`] runs).
    pub fg: vt100::Color,
    /// SGR background, unresolved.
    pub bg: vt100::Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub dim: bool,
}

impl CellStyle {
    /// The style of one emulator cell.
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

    /// Resolved `(foreground, background)` against `palette`, applying
    /// `inverse` (swap) and `dim` (foreground alpha). The background is
    /// `None` when it resolves to the palette's own background, already
    /// covered by the whole-grid backdrop — one fewer rect per run.
    ///
    /// **The two default sentinels resolve against their own fallbacks BEFORE
    /// `inverse` swaps them** — swapping first would leave `ESC[7m` on an
    /// otherwise-unstyled cell painting light-on-dark, i.e. not inverted at all.
    pub fn resolve(self, palette: &TerminalPalette) -> (Color, Option<Color>) {
        let fg = resolve_color(self.fg, palette.foreground, palette);
        let bg = resolve_color(self.bg, palette.background, palette);
        let (mut fg, bg) = if self.inverse { (bg, fg) } else { (fg, bg) };
        if self.dim {
            fg = fg.with_alpha(DIM_ALPHA);
        }
        (fg, (bg != palette.background).then_some(bg))
    }

    /// The shaping style for this run's text at `font_size`, inked `color`, in
    /// `family` — the resolved primary+fallback stack `TerminalGridWidget` holds
    /// (a caller with no controller behind it passes
    /// `crate::fonts::default_terminal_family()`).
    pub fn text_style(self, font_size: f32, color: Color, family: &FontFamily) -> TextStyle {
        TextStyle {
            family: family.clone(),
            weight: if self.bold {
                FontWeight::SEMI_BOLD
            } else {
                FontWeight::REGULAR
            },
            style: if self.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
            line_height: LineHeight::FontSizeRelative(LINE_HEIGHT),
            ..TextStyle::new(font_size, color)
        }
    }
}

/// Map a `vt100::Color` onto a paint colour: the default sentinel onto
/// `fallback`, `Idx` through the palette / xterm-256 tables, `Rgb` straight
/// through (a program that asked for a literal colour gets it).
pub fn resolve_color(color: vt100::Color, fallback: Color, palette: &TerminalPalette) -> Color {
    match color {
        vt100::Color::Default => fallback,
        vt100::Color::Idx(i) => indexed_color(i, palette),
        vt100::Color::Rgb(r, g, b) => Color::from_rgb8(r, g, b),
    }
}

/// The xterm 256-colour table: 0-15 the themed palette, 16-231 the 6x6x6 cube,
/// 232-255 the 24-step greyscale ramp. Only the first 16 are themed — the cube
/// and ramp are *computed* coordinates in a colour space a program picked
/// deliberately, so re-tinting them would corrupt gradients and heat maps.
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
        // The same theme in both brightnesses must yield the identical grid
        // palette: the grid's surface never follows the platform.
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

        // `ESC[7m` on otherwise-unstyled text must actually invert: the two
        // `Color::Default` sentinels resolve to their fallbacks first, then swap.
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
        // 16 is the cube origin (black), 231 its far corner (white).
        assert_eq!(indexed_color(16, &palette), Color::from_rgb8(0, 0, 0));
        assert_eq!(
            indexed_color(231, &palette),
            Color::from_rgb8(0xFF, 0xFF, 0xFF)
        );
        // 232 is the darkest grey step, 255 the lightest.
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
