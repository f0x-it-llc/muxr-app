//! `.term-statusrow` — the pane title plus the one expand button, the whole of
//! the card's chrome band.

use std::rc::Rc;

use frust::authoring::text::LineHeight;
use frust::{
    Align, Alignment, AnyView, EdgeInsets, GestureDetector, Padding, SizedBox, Stack, any,
    flexible, inflexible,
};

use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;

use super::super::chrome::{
    EXPAND_SIZE, FULLSCREEN_EXIT, FULLSCREEN_EXPAND, clipped_label, expand_button, term_box,
};
use super::card::CardInk;
use super::util::{h_gap, row, sized};

/// `.term-statusrow{padding:14px 14px 14px 18px}` with the **vertical** pair
/// tightened to 6px — the mock's 14px made a 56px chrome band. Horizontal
/// values are the mock's, untouched.
const STATUS_PAD_TOP: f64 = 6.0;
const STATUS_PAD_RIGHT: f64 = 14.0;
const STATUS_PAD_LEFT: f64 = 18.0;
/// The row's whole height, and so the expand button's press target: the
/// 28px button (the row's shrink floor) plus [`STATUS_PAD_TOP`] above and
/// below. The title's own line box never decides this number.
const STATUS_ROW_H: f64 = EXPAND_SIZE + 2.0 * STATUS_PAD_TOP;
/// `.term-pane-title{font-size:13.5px}` — a fixed-size logotype-ish run,
/// overriding only the display role's size (the `crate::ui::topbar`
/// brand-wordmark precedent).
const TITLE_SIZE: f32 = 13.5;
/// Its line box, tightened to match — the display role's 36px box would be
/// taller than [`STATUS_ROW_H`] itself.
const TITLE_LINE_HEIGHT: f32 = 18.0;

/// The expand button's accessible names — a *word* per state, never the
/// `⛶`/`⤡` it draws (`crate::ui::topbar`'s `BACK_LABEL` rule).
const EXPAND_LABEL: &str = "Fullscreen";
const COLLAPSE_LABEL: &str = "Exit fullscreen";

/// `.term-statusrow` — the pane title (ellipsized, single line) and, at the
/// trailing edge, the **one** fullscreen affordance: no menu row, no
/// gesture ("one button, always in the same place").
///
/// The padding is applied to the **title**, not the whole row — that lets
/// the button's column run the row's full [`STATUS_ROW_H`]: a 28px circle
/// centred in a 40px column, the padding band above/below part of the same
/// target (padding the row instead would leave a 28px target in a 40px row).
///
/// Two press paths, one callback, never both: the column is a [`Stack`]
/// whose **top** child is the button and whose bottom is a
/// [`GestureDetector`] over the full-height box. Hit-testing runs
/// topmost-first and stops at the first handler, so a press on the circle
/// is the button's and a press above/below it is the detector's.
pub(crate) fn status_row<State: 'static, F: Fn(&mut State) + 'static>(
    title: &str,
    fullscreen: bool,
    colors: &Palette,
    ink: &CardInk,
    on_toggle: F,
) -> AnyView<State> {
    let theme = active_theme();
    let mut title_style = sized(&theme.type_scale.display_small, TITLE_SIZE, ink.prompt);
    title_style.line_height = LineHeight::Absolute(TITLE_LINE_HEIGHT);

    let (mark, label) = if fullscreen {
        (FULLSCREEN_EXIT, COLLAPSE_LABEL)
    } else {
        (FULLSCREEN_EXPAND, EXPAND_LABEL)
    };
    // `.expand-btn.on{...}` — the same trio the `:hover` state uses, which a
    // touch screen has no notion of.
    let (fill, tint, border) = if fullscreen {
        (colors.amber_faint, colors.amber, colors.amber_border)
    } else {
        (ink.expand_fill, colors.fg_muted, ink.card_border)
    };

    // Shared so both halves of the press column can fire it (each just clones the handle).
    let on_toggle = Rc::new(on_toggle);
    let on_button = Rc::clone(&on_toggle);
    let button = SizedBox::<State>(Some(EXPAND_SIZE), Some(STATUS_ROW_H)).child(Align(
        Alignment::CENTER,
        expand_button(mark, label, move |state: &mut State| on_button(state))
            .tones(fill, tint, border)
            .on(fullscreen),
    ));
    let press_band = GestureDetector(SizedBox::<State>(Some(EXPAND_SIZE), Some(STATUS_ROW_H)))
        .on_tap(move |state: &mut State| on_toggle(state));

    any(term_box(
        row(vec![
            flexible(
                1,
                Padding(
                    EdgeInsets {
                        left: STATUS_PAD_LEFT,
                        top: STATUS_PAD_TOP,
                        right: 0.0,
                        bottom: STATUS_PAD_TOP,
                    },
                    clipped_label(title, title_style),
                ),
            ),
            h_gap(spacing::SM),
            inflexible(Stack(vec![any(press_band), any(button)])),
            // Trailing padding as a spacer — the button column owns the vertical padding.
            h_gap(STATUS_PAD_RIGHT),
        ]),
        ink.head,
        0.0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_expand_affordance_announces_a_word_and_not_a_mark() {
        // The a11y half of the vector rule.
        for label in [EXPAND_LABEL, COLLAPSE_LABEL] {
            assert!(
                label.chars().all(|c| c.is_ascii_alphabetic() || c == ' '),
                "{label:?} is a name, not a mark"
            );
        }
    }

    #[test]
    fn the_transcribed_geometry_is_the_mocks() {
        assert_eq!(
            (STATUS_PAD_TOP, STATUS_PAD_RIGHT, STATUS_PAD_LEFT),
            (6.0, 14.0, 18.0)
        );
        assert_eq!(EXPAND_SIZE, 28.0);
    }

    #[test]
    fn the_status_row_is_as_short_as_its_button_allows() {
        // The *whole* height is the press target (`status_row`'s own doc).
        assert_eq!(STATUS_ROW_H, 40.0);
        assert!(
            f64::from(TITLE_LINE_HEIGHT) + 2.0 * STATUS_PAD_TOP < STATUS_ROW_H,
            "the title's line box never decides the row's height"
        );
    }
}
