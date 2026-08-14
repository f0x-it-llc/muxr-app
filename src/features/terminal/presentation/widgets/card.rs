//! The terminal card's own colour vocabulary ([`CardInk`]) and its body —
//! `.term-body`, the scrollable, staggered, caret-terminated transcript.
//!
//! **The canonical token wins over the mock's hex.** The card's ink is
//! **`GlyphInk`**, not `terminal.html`'s `--term-*` variables: `terminal_bg` for the
//! body, `terminal_head` for the status band, `terminal_prompt` for the prompt runs,
//! the caret and the pane title, `terminal_out`/`terminal_comment` for the two
//! dimmer tiers. That is the framework's own brightness-invariant terminal palette,
//! so this screen matches `frust::glyph::term_block` wherever the two share a page.
//! (`GlyphInk::terminal_fg`, the *untyped* body tone, is the one field [`CardInk`]
//! does not resolve: v2 types every line it prints.)
//!
//! Two card tones have **no** token and so are local constants:
//! [`TERMINAL_BORDER_RGB`] and the expand button's resting wash. `GlyphInk` carries
//! no border/hairline field, and the nearest `ColorScheme` outline is
//! brightness-swapped while this card is not.
//!
//! This struct stays terminal-local for the same reason `term::style`'s
//! `TerminalPalette` does: it tints one screen's fixed-tone card, not a role any
//! other screen shares. Everything *outside* the card resolves
//! [`crate::ui::palette::Palette`] instead — see the [directory docs](super).

use frust::authoring::Color;
use frust::{AnyView, EdgeInsets, GlyphInk, Padding, Theme, any, scroll_view};

use crate::features::terminal::domain::entities::TermLine;
use crate::ui::palette::active_theme;

use super::super::chrome::term_body;
use super::util::line_styles;

/// `.term-body{padding:18px 20px}`.
const BODY_PAD_X: f64 = 20.0;
const BODY_PAD_Y: f64 = 18.0;
/// `.term-body{font-size:12.5px; line-height:2}` — read by
/// [`line_styles`](super::util::line_styles), which inks the three line kinds
/// from this same body role.
pub(crate) const BODY_SIZE: f32 = 12.5;
pub(crate) const BODY_LINE_HEIGHT: f32 = 2.0;

/// `--term-border: rgba(255,238,210,0.07)` — the card's hairline. A **local**
/// constant: `GlyphInk` carries no border/hairline field.
const TERMINAL_BORDER_RGB: (u8, u8, u8) = (0xff, 0xee, 0xd2);
const TERMINAL_BORDER_ALPHA: f32 = 0.07;
/// `.expand-btn{background:rgba(255,238,210,0.06)}` — same family, one step
/// fainter, and the same gap.
const EXPAND_FILL_ALPHA: f32 = 0.06;

/// The card's own ink: the five `GlyphInk` tones plus the two hairlines no
/// token carries. Resolved once per build from the active theme, beside — never
/// inside — the app's shared [`Palette`](crate::ui::palette::Palette).
#[derive(Clone, Copy)]
pub(crate) struct CardInk {
    /// `GlyphInk::terminal_bg` — the card body.
    pub(crate) card: Color,
    /// `GlyphInk::terminal_head` — the status row's band.
    pub(crate) head: Color,
    /// `GlyphInk::terminal_prompt` — the pane title, the prompt runs, the
    /// caret, and the expand button's on-state ink.
    pub(crate) prompt: Color,
    /// `GlyphInk::terminal_out` / `terminal_comment` — the two dimmer tiers.
    pub(crate) out: Color,
    pub(crate) comment: Color,
    /// `--term-border`, local (see [`TERMINAL_BORDER_RGB`]).
    pub(crate) card_border: Color,
    /// `.expand-btn`'s resting wash.
    pub(crate) expand_fill: Color,
}

impl CardInk {
    /// Resolve [`CardInk`] from `theme`.
    pub(crate) fn from_theme(theme: &Theme) -> CardInk {
        // The terminal's own ink is brightness-invariant and lives in a theme
        // *extension*, exactly as `frust::glyph::term_block` resolves it.
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

/// `.term-body` — the transcript, scrollable, staggered and caret-terminated.
/// `epoch` is the controller's replay token: a change to it (a tab switch) replays
/// the reveal from the top, the mock's `renderTab`→`playReveal` behaviour.
pub(crate) fn card_body<State: 'static>(
    lines: Vec<TermLine>,
    epoch: u64,
    ink: &CardInk,
) -> AnyView<State> {
    let theme = active_theme();
    any(scroll_view(Padding(
        EdgeInsets::symmetric(BODY_PAD_X, BODY_PAD_Y),
        term_body(lines, epoch, line_styles(&theme, ink), ink.prompt),
    )))
}

#[cfg(test)]
mod tests {
    use super::super::super::chrome::CARD_RADIUS;
    use super::*;

    #[test]
    fn the_card_takes_its_ink_from_glyph_and_not_from_the_mocks_hexes() {
        // The canonical-token rule, as a tripwire: every card tone is the
        // theme extension's, never `terminal.html`'s `--term-body`/`--term-out`
        // literals (which differ from Glyph's by a shade).
        let theme = Theme::glyph_baseline();
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
        // …and specifically NOT the mock's own body/out hexes.
        assert_ne!(colors.card, Color::from_rgb8(0x1b, 0x18, 0x11));
        assert_ne!(colors.out, Color::from_rgb8(0x9b, 0x91, 0x7a));
    }

    #[test]
    fn the_cards_hairline_is_the_mocks_own_since_no_token_carries_it() {
        // The documented gap: `GlyphInk` has no border field, so this one tone
        // stays a local constant — and stays *exactly* the mock's rgba.
        let colors = CardInk::from_theme(&Theme::glyph_baseline());
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
