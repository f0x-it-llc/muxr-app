//! The row/column/gap/[`TextStyle`] helpers every widget module in this
//! directory builds from — the smallest shared vocabulary, kept in one place
//! so no two modules grow their own spelling of "a vertically-centred row".

use frust::authoring::Color;
use frust::authoring::text::{FontStyle, LineHeight, TextStyle};
use frust::{
    AnyView, Axis, CrossAxisAlignment, FlexChild, FlexView, SizedBox, Theme, any, inflexible,
};

use crate::ui::palette::inked;

use super::super::chrome::LineStyles;
use super::card::{BODY_LINE_HEIGHT, BODY_SIZE, CardInk};

/// `style` re-inked with `color` and resized — every run picks a Glyph type
/// role for family/weight and overrides only what the mock's own CSS does, so
/// nothing falls back to `TextStyle::default()`'s 16px black. The size-carrying
/// sibling of [`crate::ui::palette::inked`], which it builds on.
pub(crate) fn sized(style: &TextStyle, size: f32, color: Color) -> TextStyle {
    let mut style = inked(style, color);
    style.size = size;
    style
}

/// A `gap`-wide horizontal spacer.
pub(crate) fn h_gap<State: 'static>(gap: f64) -> FlexChild<State> {
    inflexible(SizedBox::<State>(Some(gap), None))
}

/// A `gap`-tall vertical spacer, as a view.
pub(crate) fn v_gap<State: 'static>(gap: f64) -> AnyView<State> {
    any(SizedBox::<State>(None, Some(gap)))
}

/// A vertically-centred row.
pub(crate) fn row<State: 'static>(children: Vec<FlexChild<State>>) -> FlexView<State> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

/// A column whose children each fill the available width.
pub(crate) fn column<State: 'static>(children: Vec<AnyView<State>>) -> FlexView<State> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Stretch)
}

/// The three inks the card's transcript prints in — `.term-body`'s 12.5px/2.0
/// monospace, tinted per line kind, with `.cm` italic.
///
/// Scoped to `presentation` rather than the crate, since [`LineStyles`] — the
/// `super::super::chrome` type it hands back — is.
pub(in crate::features::terminal::presentation) fn line_styles(
    theme: &Theme,
    ink: &CardInk,
) -> LineStyles {
    let mut base = sized(&theme.type_scale.body_medium, BODY_SIZE, ink.prompt);
    base.line_height = LineHeight::FontSizeRelative(BODY_LINE_HEIGHT);
    let mut comment = base.clone();
    comment.color = ink.comment;
    comment.style = FontStyle::Italic;
    LineStyles {
        prompt: base.clone(),
        output: TextStyle {
            color: ink.out,
            ..base
        },
        comment,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comment_line_is_the_only_italic_one() {
        let theme = frust_glyph::baseline();
        let styles = line_styles(&theme, &CardInk::from_theme(&theme));
        assert_eq!(styles.comment.style, FontStyle::Italic);
        assert_eq!(styles.prompt.style, FontStyle::Normal);
        assert_eq!(styles.output.style, FontStyle::Normal);
        // …and all three carry the mock's own size and doubled line box.
        for style in [&styles.prompt, &styles.output, &styles.comment] {
            assert_eq!(style.size, BODY_SIZE);
            assert_eq!(
                style.line_height,
                LineHeight::FontSizeRelative(BODY_LINE_HEIGHT)
            );
        }
    }
}
