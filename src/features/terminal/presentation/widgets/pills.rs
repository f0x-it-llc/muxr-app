//! `.pill`, `.pill.sel`, `.pill.add` — the sheet's space/tab chips — and the
//! trailing edit affordance every renamable chip in this screen carries
//! ([`edit_icon_button`], shared with [`super::panes`]'s own rows).

use std::rc::Rc;

use frust::authoring::{Color, DashPattern, Size};
use frust::{
    AnyView, BorderStyle, EdgeInsets, GestureDetector, Padding, any, container, flexible, icons,
    inflexible, text,
};

use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;
use crate::ui::topbar::icon_button;

use super::super::chrome::{DASH_OFF, DASH_ON, HAIRLINE, RADIUS_FULL};
use super::util::{row, sized};

/// `.pill{font-size:11px; padding:8px 13px}` and `.pill-row{gap:8px}`.
pub(crate) const PILL_SIZE: f32 = 11.0;
const PILL_PAD_X: f64 = 13.0;
const PILL_PAD_Y: f64 = spacing::SM;
pub(crate) const PILL_GAP: f64 = spacing::SM;

/// The trailing edit affordance's drawn vector size, in logical px — smaller
/// than every other mark this screen draws, so it reads as *subordinate* to
/// the label beside it.
const EDIT_ICON_SIZE: f64 = 16.0;
/// The pill chip's own edit tap box — [`icon_button`]'s fixed-box seam, as
/// an actual layout box (the edit icon is a row *sibling* of the main tap
/// surface, see [`pill`], so its hit region is exactly its own laid-out
/// rect). 30px, under the pill's own 33px fill height, so every pill —
/// including an "add" pill with no edit icon — keeps sharing one height
/// (`tests/terminal.rs`'s `pills()` locator groups all nine by it); short of
/// the 40–44px comfortable-thumb-target norm elsewhere. See
/// [`super::panes::PANE_EDIT_TAP_BOX`] for the row this ceiling doesn't bind.
const PILL_EDIT_TAP_BOX: f64 = 30.0;
/// Gap between a chip's main content and its trailing edit icon.
const EDIT_GAP: f64 = spacing::XS;
/// The edit affordance's accessible name — a *word*, never the pencil mark it
/// draws (the `crate::ui::topbar` `BACK_LABEL` rule).
const EDIT_LABEL: &str = "Rename";

/// A chip's optional trailing edit callback — `Some` shows the icon
/// ([`pill`]/[`super::panes::pane_item`]'s own doc on why it is a shared `Rc`
/// rather than a third generic type parameter apiece).
pub(crate) type OnEdit<State> = Option<Rc<dyn Fn(&mut State)>>;

/// Which of `.pill`'s states a pill is in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PillState {
    /// `.pill` — raised fill, bright hairline, muted ink.
    Idle,
    /// `.pill.sel` — the amber trio.
    Selected,
    /// The tab the **session** is on while the sheet browses another one —
    /// no `terminal.html` counterpart (its mock had one notion of "the
    /// tab"). Deliberately minimal: the selected pill's amber edge/ink over
    /// an ordinary raised fill, so the browsed pill stays the only full
    /// amber wash.
    Active,
    /// `.pill.add` — a dashed hairline and the dimmest ink.
    Add,
}

/// A pill's `(fill, border, ink)` for `state` — split out of [`pill`] so the
/// four states can be asserted to actually differ.
fn pill_tones(state: PillState, colors: &Palette) -> (Color, Color, Color) {
    match state {
        PillState::Idle => (colors.raised, colors.outline, colors.fg_muted),
        PillState::Selected => (colors.amber_faint, colors.amber_border, colors.amber),
        PillState::Active => (colors.raised, colors.amber_border, colors.amber),
        PillState::Add => (colors.raised, colors.outline, colors.fg_dim),
    }
}

/// The chip affordance's trailing edit icon — [`icons::EDIT`], a vector
/// (chrome-icon rule), its own tap region via [`icon_button`]'s
/// fire-on-up-inside contract and a fixed `tap_box`. No fill, no border —
/// deliberately unadorned, reading as subordinate to the label beside it.
pub(crate) fn edit_icon_button<State: 'static, F: Fn(&mut State) + 'static>(
    colors: &Palette,
    tap_box: f64,
    on_edit: F,
) -> AnyView<State> {
    any(icon_button(icons::EDIT, EDIT_LABEL, on_edit)
        .ink(colors.fg_dim)
        .icon_size(EDIT_ICON_SIZE)
        .box_size(Size::new(tap_box, tap_box))
        .radius(tap_box / 2.0))
}

/// One `.pill`. The label is content text — the add pills' `+` is a **run**,
/// not a vector, since it sits inside the sentence, not chrome.
///
/// `on_edit`, when `Some`, adds a trailing [`icons::EDIT`] vector as its
/// **own** tap region — never wired for [`PillState::Add`] or a read-only
/// credential (the caller's job).
///
/// **Two row siblings, not one detector nested in the other.**
/// [`GestureDetector`] is *transparent* — every event still reaches its
/// child and fires its own callback regardless — so nesting the icon's
/// detector inside the label's would fire both on an icon tap. Independent
/// siblings keep them disjoint.
pub(crate) fn pill<State: 'static, F: Fn(&mut State) + 'static>(
    label: impl Into<String>,
    state: PillState,
    colors: &Palette,
    on_tap: F,
    on_edit: OnEdit<State>,
) -> AnyView<State> {
    let theme = active_theme();
    let (fill, border, ink) = pill_tones(state, colors);
    let label_view = text(label.into()).style(sized(&theme.type_scale.body_small, PILL_SIZE, ink));

    let content: AnyView<State> = match on_edit {
        None => any(GestureDetector(Padding(
            EdgeInsets::symmetric(PILL_PAD_X, PILL_PAD_Y),
            label_view,
        ))
        .on_tap(on_tap)),
        Some(on_edit) => any(row(vec![
            flexible(
                1,
                GestureDetector(Padding(
                    EdgeInsets {
                        left: PILL_PAD_X,
                        top: PILL_PAD_Y,
                        right: 0.0,
                        bottom: PILL_PAD_Y,
                    },
                    label_view,
                ))
                .on_tap(on_tap),
            ),
            inflexible(Padding(
                EdgeInsets {
                    left: EDIT_GAP,
                    top: 0.0,
                    right: PILL_PAD_X,
                    bottom: 0.0,
                },
                edit_icon_button(colors, PILL_EDIT_TAP_BOX, move |state: &mut State| {
                    on_edit(state)
                }),
            )),
        ])),
    };

    let pill = container(content)
        .fill(fill)
        .radius(RADIUS_FULL)
        .border(border, HAIRLINE);
    let pill = if state == PillState::Add {
        pill.border_style(BorderStyle::Dashed(DashPattern::new(DASH_ON, DASH_OFF)))
    } else {
        pill
    };
    any(pill)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pill_state_is_visibly_a_different_pill() {
        // The sheet can show a browsed tab and the session's tab at once, so
        // no two states may resolve to the same trio.
        let colors = Palette::from_theme(&frust_glyph::baseline());
        let tones: Vec<(Color, Color, Color)> = [
            PillState::Idle,
            PillState::Selected,
            PillState::Active,
            PillState::Add,
        ]
        .into_iter()
        .map(|state| pill_tones(state, &colors))
        .collect();
        for (i, a) in tones.iter().enumerate() {
            for b in tones.iter().skip(i + 1) {
                assert_ne!(a, b, "two pill states paint the same");
            }
        }
        // The browsed pill is the only one wearing the full amber wash.
        assert_eq!(tones[1].0, colors.amber_faint);
        assert_eq!(tones[2].0, colors.raised);
        assert_eq!(
            tones[2].1, colors.amber_border,
            "…while still reading amber"
        );
    }

    #[test]
    fn a_selected_pill_wears_the_themes_own_accent() {
        // The theme's accent, not a mock hex.
        let theme = frust_glyph::baseline();
        let colors = Palette::from_theme(&theme);
        let (fill, border, ink) = pill_tones(PillState::Selected, &colors);
        assert_eq!(ink, theme.scheme().primary);
        assert_eq!(fill, colors.amber_faint);
        assert_eq!(border, colors.amber_border);
    }
}
