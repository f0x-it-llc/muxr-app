//! The terminal card's ink ([`CardInk`]) and its `.term-body` transcript. Canonical `GlyphInk`
//! terminal tokens win over the mock's `--term-*` hex; the border and expand wash have no token
//! and stay local constants.

use frust::authoring::Color;
use frust::{EdgeInsets, Padding, Theme, View, scroll_view};
use frust_glyph::GlyphInk;

use crate::features::terminal::domain::entities::TermLine;
use crate::ui::palette::active_theme;

use super::super::chrome::term_body;
use super::util::line_styles;

/// `.term-body{padding:18px 20px}`.
const BODY_PAD_X: f64 = 20.0;
const BODY_PAD_Y: f64 = 18.0;
/// `.term-body{font-size:12.5px; line-height:2}`.
pub(crate) const BODY_SIZE: f32 = 12.5;
pub(crate) const BODY_LINE_HEIGHT: f32 = 2.0;

/// `--term-border: rgba(255,238,210,0.07)`; no `GlyphInk` token carries a hairline.
const TERMINAL_BORDER_RGB: (u8, u8, u8) = (0xff, 0xee, 0xd2);
const TERMINAL_BORDER_ALPHA: f32 = 0.07;
/// `.expand-btn{background:rgba(255,238,210,0.06)}`.
const EXPAND_FILL_ALPHA: f32 = 0.06;

#[derive(Clone, Copy)]
pub(crate) struct CardInk {
    pub(crate) card: Color,
    pub(crate) head: Color,
    pub(crate) prompt: Color,
    pub(crate) out: Color,
    pub(crate) comment: Color,
    pub(crate) card_border: Color,
    pub(crate) expand_fill: Color,
}

impl CardInk {
    pub(crate) fn from_theme(theme: &Theme) -> CardInk {
        // Brightness-invariant, via the theme extension.
        let ink = theme
            .extension::<GlyphInk>()
            .copied()
            .unwrap_or_else(GlyphInk::default_ink);
        let (r, g, b) = TERMINAL_BORDER_RGB;
        let hairline = Color::from_rgb8(r, g, b);
        CardInk {
            card: ink.terminal_bg,
            head: ink.terminal_head,
            prompt: ink.terminal_prompt,
            out: ink.terminal_out,
            comment: ink.terminal_comment,
            card_border: hairline.with_alpha(TERMINAL_BORDER_ALPHA),
            expand_fill: hairline.with_alpha(EXPAND_FILL_ALPHA),
        }
    }
}

/// A change to `epoch` (a tab switch) replays the reveal from the top.
pub(crate) fn card_body<State: 'static>(
    lines: Vec<TermLine>,
    epoch: u64,
    ink: &CardInk,
) -> impl View<State> {
    let theme = active_theme();
    scroll_view(Padding(
        EdgeInsets::symmetric(BODY_PAD_X, BODY_PAD_Y),
        term_body(lines, epoch, line_styles(&theme, ink), ink.prompt),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::super::chrome::CARD_RADIUS;
    use super::*;

    #[test]
    fn the_card_takes_its_ink_from_glyph_and_not_from_the_mocks_hexes() {
        let theme = frust_glyph::baseline();
        let ink = theme
            .extension::<GlyphInk>()
            .copied()
            .expect("the Glyph baseline carries GlyphInk");
        let colors = CardInk::from_theme(&theme);

        assert_eq!(colors.card, ink.terminal_bg);
        assert_eq!(colors.head, ink.terminal_head);
        assert_eq!(colors.prompt, ink.terminal_prompt);
        assert_eq!(colors.out, ink.terminal_out);
        assert_eq!(colors.comment, ink.terminal_comment);
        assert_ne!(colors.card, Color::from_rgb8(0x1b, 0x18, 0x11));
        assert_ne!(colors.out, Color::from_rgb8(0x9b, 0x91, 0x7a));
    }

    #[test]
    fn the_cards_hairline_is_the_mocks_own_since_no_token_carries_it() {
        let colors = CardInk::from_theme(&frust_glyph::baseline());
        let (r, g, b) = TERMINAL_BORDER_RGB;
        assert_eq!((r, g, b), (0xff, 0xee, 0xd2));
        assert_eq!(colors.card_border.components[3], TERMINAL_BORDER_ALPHA);
        assert_eq!(colors.expand_fill.components[3], EXPAND_FILL_ALPHA);
    }

    #[test]
    fn the_transcribed_body_geometry_is_the_mocks() {
        assert_eq!((BODY_PAD_X, BODY_PAD_Y), (20.0, 18.0));
        assert_eq!(CARD_RADIUS, 22.0);
    }
}
