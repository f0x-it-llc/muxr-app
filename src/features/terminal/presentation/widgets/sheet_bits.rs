//! The "panes & tabs" sheet's own furniture: its fixed top row and close
//! affordance, its section labels, and the scrollable region everything below
//! the top row lives in.

use frust::authoring::Size;
use frust::{AnyView, SizedBox, any, flexible, icons, inflexible, scroll_view, text};

use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;
use crate::ui::topbar::icon_button;

use super::util::{column, h_gap, row, sized};

/// `.sheet-toprow .lbl{font-size:12.5px}` and the sheet's own label text.
const SHEET_LABEL_SIZE: f32 = 12.5;
const SHEET_LABEL: &str = "panes & tabs";
/// `.sheet-close{width:26px;height:26px}` and the vector inside it.
const CLOSE_SIZE: f64 = 26.0;
const CLOSE_ICON: f64 = 13.0;
/// The close button's accessible name — a *word*, never the `×` it draws.
const CLOSE_LABEL: &str = "Close";
/// `label_medium`'s own canonical size — an established Glyph type-scale role
/// rather than a hand-picked float, since anything below `label_small`'s 11px floor
/// made the sheet's three section labels indistinguishable on device. The
/// `margin`/`gap` figures below are the mock's own, on the shared scale.
const SECTION_SIZE: f32 = 12.0;
pub(crate) const SECTION_TOP_GAP: f64 = spacing::XL;
pub(crate) const SECTION_BOTTOM_GAP: f64 = spacing::MD;
const SECTION_GAP: f64 = spacing::XS;
/// `.sheet-section-lbl::before{content:'//'}`.
const SECTION_PREFIX: &str = "//";

/// `.sheet-toprow` — the sheet's own label and its round close button. The handle
/// above it is the Glyph sheet's own, so this deliberately draws none.
pub(crate) fn sheet_top_row<State: 'static, F: Fn(&mut State) + 'static>(
    colors: &Palette,
    on_close: F,
) -> AnyView<State> {
    let theme = active_theme();
    any(row(vec![
        flexible(
            1,
            text(SHEET_LABEL).style(sized(
                &theme.type_scale.label_large,
                SHEET_LABEL_SIZE,
                colors.fg,
            )),
        ),
        // `crate::ui::topbar::icon_button`'s third caller: a *labelled*
        // `Role::Button` over one vector, which a bare `GestureDetector` —
        // contributing no node of its own — could not give a screen reader.
        inflexible(
            icon_button(icons::CLOSE, CLOSE_LABEL, on_close)
                .ink(colors.fg_dim)
                .fill(colors.raised)
                .icon_size(CLOSE_ICON)
                .box_size(Size::new(CLOSE_SIZE, CLOSE_SIZE))
                .radius(CLOSE_SIZE / 2.0),
        ),
    ]))
}

/// `.sheet-section-lbl` — a dim label behind an amber `//` prefix, sized and
/// weighted for hierarchy rather than selection: `label_medium`'s canonical size,
/// `_emphasized` (bold) rather than a further size bump, so "spaces"/"tabs"/"panes"
/// reads as a clear step up without growing past the sheet's own top-row label or
/// reaching for the amber selection tone a pill owns. Its line box (16px, unchanged
/// by the `_emphasized` variant) is identical to the plain role, so no row grows.
pub(crate) fn section_label<State: 'static>(
    label: impl Into<String>,
    colors: &Palette,
) -> AnyView<State> {
    let theme = active_theme();
    let prefix = sized(
        &theme.type_scale.label_medium_emphasized,
        SECTION_SIZE,
        colors.amber_dim,
    );
    let body = sized(
        &theme.type_scale.label_medium_emphasized,
        SECTION_SIZE,
        colors.fg_dim,
    );
    any(row(vec![
        inflexible(text(SECTION_PREFIX).style(prefix)),
        h_gap(SECTION_GAP),
        inflexible(text(label.into()).style(body)),
    ]))
}

/// The sheet's own scrollable region: `children` (everything below the sheet's
/// fixed top row) wrapped in [`scroll_view`], so a session with many tabs/panes
/// stays fully reachable rather than the lower rows running off the sheet's bottom
/// edge (no mock counterpart).
///
/// `max_height`, when `Some`, tightens the region to that many logical px —
/// [`super::panes_sheet::sheet_body`]'s window-metrics-derived cap. `None` (no shell
/// has published metrics, i.e. every headless test) leaves the region unbounded,
/// which [`scroll_view`] resolves the same way a plain column would: it sizes itself
/// to its content rather than clipping. So a headless suite sees the geometry it
/// always has, and a device is the only place the cap ever engages.
///
/// Laid out as an ordinary (not `flexible`) child of the outer column: a sheet's
/// body is laid out at its own intrinsic height, so there is no free main-axis space
/// to take a share of — and a `flexible` child under an unbounded main axis divides
/// *zero* free space, the tight-constraint trap `examples/glyph-catalog`'s device
/// gate documents. A caller-supplied fixed height sidesteps it: [`SizedBox`]
/// tightens regardless of the ambient's unbounded axis.
pub(crate) fn sheet_scroll_body<State: 'static>(
    children: Vec<AnyView<State>>,
    max_height: Option<f64>,
) -> AnyView<State> {
    let scroller = scroll_view(column(children));
    match max_height {
        Some(height) => any(SizedBox::<State>(None, Some(height)).child(scroller)),
        None => any(scroller),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_close_affordance_announces_a_word_and_not_a_mark() {
        // The a11y half of the vector rule: a label is a *name*, never the
        // shape of the glyph the mock happened to draw.
        assert!(
            CLOSE_LABEL.chars().all(|c| c.is_ascii_alphabetic()),
            "{CLOSE_LABEL:?} is a name, not a mark"
        );
    }

    #[test]
    fn the_transcribed_close_button_is_the_mocks_own_size() {
        assert_eq!(CLOSE_SIZE, 26.0);
    }
}
