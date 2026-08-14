//! `.pane-item` and `.new-pane-btn` — the two rows the "panes & tabs" sheet's
//! pane section is built from.

use frust::authoring::Color;
use frust::{
    Align, Alignment, AnyView, EdgeInsets, GestureDetector, Padding, SizedBox, any, flexible, icon,
    inflexible, text,
};

use crate::features::terminal::domain::entities::PaneEntry;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;

use super::super::chrome::{PANE_MARK, RADIUS_FULL, term_box};
use super::pills::{OnEdit, PILL_SIZE, edit_icon_button};
use super::util::{column, h_gap, row, sized};

/// `.pane-item{padding:11px 12px; gap:10px; margin-bottom:8px}`.
const PANE_PAD_X: f64 = 12.0;
const PANE_PAD_Y: f64 = 11.0;
const PANE_GAP: f64 = spacing::MD;
pub(crate) const PANE_ROW_GAP: f64 = spacing::SM;
/// `.pane-item .ic{width:26px;height:26px}` and the mark drawn inside it.
const PANE_ICON_BOX: f64 = 26.0;
const PANE_ICON: f64 = 13.0;
/// `.pane-item .name{font-size:11.5px}` / `.path{font-size:9.5px}`.
const PANE_NAME_SIZE: f32 = 11.5;
const PANE_PATH_SIZE: f32 = 9.5;
/// `.focused-chip{font-size:9px; padding:3px 8px}`.
const CHIP_SIZE: f32 = 9.0;
const CHIP_PAD_X: f64 = spacing::SM;
const CHIP_PAD_Y: f64 = 3.0;
/// `.focused-chip` text, and `.new-pane-btn`'s two runs.
const FOCUSED_CHIP: &str = "focused";
const NEW_PANE_PLUS: &str = "+";
const NEW_PANE_LABEL: &str = "new pane \u{b7} long-press for floating";
/// `.new-pane-btn{padding:11px; font-size:11px}`.
pub(crate) const NEW_PANE_PAD: f64 = 11.0;
/// Gap between the amber `+` and the label beside it — the source's
/// `<b>+</b> new pane` is one text node whose space is the gap; composed as
/// two runs, it needs a real one.
const NEW_PANE_GAP: f64 = spacing::XS;

/// The pane row's own edit tap box — a comfortable 44px thumb target outright:
/// a pane row's own height (53px, same measurement basis as
/// `pills`' own `PILL_EDIT_TAP_BOX` doc — `PANE_PAD_Y * 2` plus the taller of
/// the mark box and the title/cwd column) leaves headroom under it, and every
/// pane row carries an edit icon (there is no "add pane row" counterpart the
/// way a pill section has an add pill), so there is no second height class
/// this could split.
pub(crate) const PANE_EDIT_TAP_BOX: f64 = 44.0;

/// `.pane-item` — one pane's mark box, its title/cwd pair, and the `focused`
/// chip when it is the one the session is on.
///
/// The mark is [`PANE_MARK`], an app-supplied vector: the mock sets `▤` as
/// text, which is not in the bundled Glyph faces (see
/// `super::super::chrome::PANE_MARK`).
///
/// **`focused` is the caller's answer, not the pane's own flag.** A pane in a
/// tab the sheet is merely *browsing* carries whatever the last load recorded,
/// which is not where the session is now — so the sheet only passes `true` for
/// the tab it is actually on ([`super::panes_sheet::sheet_body`]), and a
/// browsed tab's rows all read unfocused rather than claiming a stale badge.
///
/// An **exited** pane's title takes the dim ink instead of the bright one — the
/// same muted tier every other "this is no longer live" run in the app uses.
///
/// `on_edit`, when `Some`, adds a trailing edit vector as its own independent
/// tap region — never wired for a read-only credential (the caller's job). See
/// [`super::pills::pill`]'s own doc for why this is a row *sibling* rather than
/// a detector nested inside the row's main-tap one: the same "transparent
/// wrapper still fires its own callback" hazard applies here.
pub(crate) fn pane_item<State: 'static, F: Fn(&mut State) + 'static>(
    pane: &PaneEntry,
    focused: bool,
    colors: &Palette,
    on_tap: F,
    on_edit: OnEdit<State>,
) -> AnyView<State> {
    let theme = active_theme();
    let (mark_fill, mark_ink) = if focused {
        (colors.amber_faint, colors.amber)
    } else {
        (colors.raised, colors.fg_muted)
    };
    let border = if focused {
        colors.amber_border
    } else {
        colors.outline
    };
    let title_ink = if pane.exited {
        colors.fg_dim
    } else {
        colors.fg
    };

    let info = column::<State>(vec![
        any(text(pane.title.clone()).style(sized(
            &theme.type_scale.body_small,
            PANE_NAME_SIZE,
            title_ink,
        ))),
        any(text(pane.cwd.clone()).style(sized(
            &theme.type_scale.label_small,
            PANE_PATH_SIZE,
            colors.fg_dim,
        ))),
    ]);

    let mut children = vec![
        inflexible(term_box(
            SizedBox::<State>(Some(PANE_ICON_BOX), Some(PANE_ICON_BOX)).child(Align(
                Alignment::CENTER,
                icon(PANE_MARK).size(PANE_ICON).color(mark_ink),
            )),
            mark_fill,
            theme.shape.extra_small,
        )),
        h_gap(PANE_GAP),
        flexible(1, info),
    ];
    if focused {
        children.push(h_gap(PANE_GAP));
        children.push(inflexible(
            term_box(
                Padding(
                    EdgeInsets::symmetric(CHIP_PAD_X, CHIP_PAD_Y),
                    text(FOCUSED_CHIP).style(sized(
                        &theme.type_scale.label_small,
                        CHIP_SIZE,
                        colors.amber,
                    )),
                ),
                colors.amber_faint,
                RADIUS_FULL,
            )
            .border(colors.amber_border),
        ));
    }

    let main = GestureDetector(Padding(
        EdgeInsets {
            left: PANE_PAD_X,
            top: PANE_PAD_Y,
            right: if on_edit.is_some() { 0.0 } else { PANE_PAD_X },
            bottom: PANE_PAD_Y,
        },
        row(children),
    ))
    .on_tap(on_tap);

    let tile: AnyView<State> = match on_edit {
        None => any(main),
        Some(on_edit) => any(row(vec![
            flexible(1, main),
            inflexible(Padding(
                EdgeInsets {
                    left: 0.0,
                    top: 0.0,
                    right: PANE_PAD_X,
                    bottom: 0.0,
                },
                edit_icon_button(colors, PANE_EDIT_TAP_BOX, move |state: &mut State| {
                    on_edit(state)
                }),
            )),
        ])),
    };

    any(term_box(tile, colors.base, theme.shape.small).border(border))
}

/// `.new-pane-btn` — the dashed, centred row that closes the sheet's pane
/// section. The `+` is amber because it is the call to action.
///
/// Two gestures, both wired straight through: a tap fires `on_tap` (a tiled
/// pane), a long-press fires `on_long_press` (a floating one) — the row's own
/// label already says which is which (`new pane · long-press for floating`).
/// [`GestureDetector`] fires exactly one of the two per press, never both.
pub(crate) fn new_pane_row<
    State: 'static,
    T: Fn(&mut State) + 'static,
    L: Fn(&mut State) + 'static,
>(
    colors: &Palette,
    on_tap: T,
    on_long_press: L,
) -> AnyView<State> {
    let theme = active_theme();
    let label = row(vec![
        inflexible(text(NEW_PANE_PLUS).style(sized(
            &theme.type_scale.body_small,
            PILL_SIZE,
            colors.amber,
        ))),
        h_gap(NEW_PANE_GAP),
        inflexible(text(NEW_PANE_LABEL).style(sized(
            &theme.type_scale.body_small,
            PILL_SIZE,
            colors.fg_dim,
        ))),
    ]);
    any(GestureDetector(
        term_box(
            Padding(
                EdgeInsets::all(NEW_PANE_PAD),
                Align(Alignment::CENTER, label),
            ),
            Color::TRANSPARENT,
            theme.shape.small,
        )
        .border(colors.outline)
        .dashed(true),
    )
    .on_tap(on_tap)
    .on_long_press(on_long_press))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transcribed_pane_geometry_is_the_mocks() {
        assert_eq!(PANE_ICON_BOX, 26.0);
        assert_eq!((PANE_PAD_X, PANE_PAD_Y), (12.0, 11.0));
    }
}
