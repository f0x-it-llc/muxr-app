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

/// `.term-statusrow{padding:14px 14px 14px 18px}` — with the **vertical** pair
/// tightened to 6px: the mock's 14px made a 56px band of chrome above the
/// terminal, and the title bar is meant to be as small as it can be while still
/// carrying the one expand button. The two horizontal values are the mock's,
/// untouched.
const STATUS_PAD_TOP: f64 = 6.0;
const STATUS_PAD_RIGHT: f64 = 14.0;
const STATUS_PAD_LEFT: f64 = 18.0;
/// The status row's whole height, and so the expand button's press target (see
/// [`status_row`]): the 28px button — the floor the row cannot shrink past —
/// plus [`STATUS_PAD_TOP`] above and below it. The title's own line box (18px)
/// is well under it and never decides this number.
const STATUS_ROW_H: f64 = EXPAND_SIZE + 2.0 * STATUS_PAD_TOP;
/// `.term-pane-title{font-size:13.5px}` — a fixed-size logotype-ish run, so it
/// overrides the display role's size (and only that; family/weight stay Space
/// Mono 700, which *is* what the role carries) — the `crate::ui::topbar`
/// brand-wordmark precedent.
const TITLE_SIZE: f32 = 13.5;
/// …and its line box, tightened to match (the display role's 36px box would
/// make "centred in the status row" mean something else entirely — and would
/// now be taller than [`STATUS_ROW_H`] itself).
const TITLE_LINE_HEIGHT: f32 = 18.0;

/// The expand button's accessible names — a *word* per state, never the
/// `⛶`/`⤡` it draws (`crate::ui::topbar`'s `BACK_LABEL` rule).
const EXPAND_LABEL: &str = "Fullscreen";
const COLLAPSE_LABEL: &str = "Exit fullscreen";

/// `.term-statusrow` — the pane title (ellipsized, single line) and, at the
/// trailing edge, the **one** fullscreen affordance.
///
/// The row this replaced in v1 was three cosmetic dots; here the trailing
/// circle is an actual button, and it is the only place fullscreen is reachable
/// from — no menu row, no gesture (the mock: "one button, always in the same
/// place").
///
/// # The row is [`STATUS_ROW_H`] tall, and so is the button's press target
///
/// The padding is applied to the **title** rather than to the whole row, which
/// is what lets the button's column run the row's full height: a 28px circle
/// centred in a 40px column, with the padding band above and below it part of
/// the same target. Padding the row instead would put those two 6px bands
/// outside every child and leave a 28px target in a 40px row — a thumb-sized
/// regression against the 56px row this replaced, where the same circle at
/// least had 14px of visual slack around it.
///
/// Two press paths, one callback, never both: the column is a [`Stack`] whose
/// **top** child is the button itself and whose bottom child is a
/// [`GestureDetector`] over the full-height box. A hit-test runs topmost-first
/// and stops at the first child that handles, so a press on the circle is the
/// button's (press-scale visual included) and a press on the band above or
/// below it is the detector's — the same toggle either way.
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
    // `.expand-btn.on{background:var(--amber-faint); color:var(--amber);
    // border-color:var(--amber-border)}` — the same trio the `:hover` state
    // uses, which a touch screen has no notion of.
    let (fill, tint, border) = if fullscreen {
        (colors.amber_faint, colors.amber, colors.amber_border)
    } else {
        (ink.expand_fill, colors.fg_muted, ink.card_border)
    };

    // Shared so both halves of the press column below can fire it; the
    // callback itself is `Fn`, so a clone of the handle is all either needs.
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
            // The row's own trailing padding, as a spacer: the button column
            // owns the vertical padding, so this axis is all that is left of
            // the `Padding` that used to wrap everything.
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
        // The a11y half of the vector rule: a label is a *name*, never the
        // shape of the glyph the mock happened to draw.
        for label in [EXPAND_LABEL, COLLAPSE_LABEL] {
            assert!(
                label.chars().all(|c| c.is_ascii_alphabetic() || c == ' '),
                "{label:?} is a name, not a mark"
            );
        }
    }

    #[test]
    fn the_transcribed_geometry_is_the_mocks() {
        // A 14px padding and an 18px one live a line apart in the source; a
        // swap is invisible in review and obvious on screen. The vertical pair
        // is the one deliberate departure from the mock (see
        // `STATUS_PAD_TOP`).
        assert_eq!(
            (STATUS_PAD_TOP, STATUS_PAD_RIGHT, STATUS_PAD_LEFT),
            (6.0, 14.0, 18.0)
        );
        assert_eq!(EXPAND_SIZE, 28.0);
    }

    #[test]
    fn the_status_row_is_as_short_as_its_button_allows() {
        // 40px: the expand button plus one tightened padding either side —
        // and the *whole* of that height is its press target (`status_row`'s
        // own doc), so the shorter row costs the thumb nothing.
        assert_eq!(STATUS_ROW_H, 40.0);
        assert!(
            f64::from(TITLE_LINE_HEIGHT) + 2.0 * STATUS_PAD_TOP < STATUS_ROW_H,
            "the title's line box never decides the row's height"
        );
    }
}
