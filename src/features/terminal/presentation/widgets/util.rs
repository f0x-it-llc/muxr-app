use frust::authoring::Color;
use frust::authoring::text::{FontStyle, LineHeight, TextStyle};
use frust::{
    AnyView, Axis, CrossAxisAlignment, FlexChild, FlexView, SizedBox, Theme, View, inflexible,
};

use frust_glyph::{BadgeVariant, badge};

use crate::ui::palette::inked;

use super::super::chrome::LineStyles;
use super::card::{BODY_LINE_HEIGHT, BODY_SIZE, CardInk};

/// Every run picks a Glyph type role so nothing falls back to `TextStyle::default()`'s 16px black.
pub(crate) fn sized(style: &TextStyle, size: f32, color: Color) -> TextStyle {
    let mut style = inked(style, color);
    style.size = size;
    style
}

/// The one place the chip's word and variant live, so the status row and rail cannot drift apart.
pub(crate) fn read_only_badge<State: 'static>() -> impl View<State> {
    badge(READ_ONLY_CHIP, BadgeVariant::Info)
}

const READ_ONLY_CHIP: &str = "read-only";

pub(crate) fn h_gap<State: 'static>(gap: f64) -> FlexChild<State> {
    inflexible(SizedBox::<State>(Some(gap), None))
}

pub(crate) fn v_gap<State: 'static>(gap: f64) -> impl View<State> {
    SizedBox::<State>(None, Some(gap))
}

pub(crate) fn centered_row<State: 'static>(children: Vec<FlexChild<State>>) -> FlexView<State> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

pub(crate) fn stretch_column<State: 'static>(children: Vec<AnyView<State>>) -> FlexView<State> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Stretch)
}

/// `.term-body`'s 12.5px/2.0 monospace, tinted per line kind, with `.cm` italic.
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
        output: base.with_color(ink.out),
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
        for style in [&styles.prompt, &styles.output, &styles.comment] {
            assert_eq!(style.size, BODY_SIZE);
            assert_eq!(
                style.line_height,
                LineHeight::FontSizeRelative(BODY_LINE_HEIGHT)
            );
        }
    }
}
