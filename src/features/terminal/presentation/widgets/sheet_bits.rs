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
/// rather than a hand-picked float, since anything below `label_small`'s 11px
/// floor made the sheet's three section labels indistinguishable on device.
/// The `margin`/`gap` figures below are the mock's own
/// (`.sheet-section-lbl{margin:16px 0 10px; gap:6px}`), on the shared scale.
const SECTION_SIZE: f32 = 12.0;
pub(crate) const SECTION_TOP_GAP: f64 = spacing::XL;
pub(crate) const SECTION_BOTTOM_GAP: f64 = spacing::MD;
const SECTION_GAP: f64 = spacing::XS;
/// `.sheet-section-lbl::before{content:'//'}`.
const SECTION_PREFIX: &str = "//";

/// `.sheet-toprow` — the sheet's own label and its round close button.
///
/// The handle above it is the Glyph sheet's own (it draws one at the panel's
/// top), so this deliberately draws none: two would read as a mistake.
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
        // `crate::ui::topbar::icon_button`'s third caller (after the bar's own
        // slots and the Connect FAB): a *labelled* `Role::Button` over one
        // vector, which a bare `GestureDetector` — contributing no node of its
        // own — could not give a screen reader.
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
/// weighted for hierarchy rather than selection: `label_medium`'s own
/// canonical size, `_emphasized` (bold) rather than a further size bump, so
/// "spaces"/"tabs"/"panes" reads as a clear step up without growing past the
/// sheet's own top-row label ([`SHEET_LABEL_SIZE`]'s 12.5px) or reaching for
/// the amber selection tone a pill/chip owns (see
/// [`super::pills::pill`]) — amber stays the *selected* colour, this label is
/// only ever `amber_dim`/`fg_dim`. `label_medium`'s own line box (16px,
/// unchanged by either the size or the `_emphasized` variant —
/// `frust-theme`'s type-scale table) is identical to the plain role, so no row
/// this label sits in grows.
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

/// The sheet's own scrollable region: `children` (the spaces/tabs/panes
/// sections and their gaps — everything below the sheet's fixed top row)
/// wrapped in [`scroll_view`], so a session with many tabs/panes stays fully
/// reachable rather than the lower rows running off the sheet's own bottom
/// edge (no mock counterpart — `docs/terminal.html` never had enough rows to
/// need one).
///
/// `max_height`, when `Some`, tightens the region to that many logical px —
/// [`super::panes_sheet::sheet_body`]'s own window-metrics-derived cap, read
/// once per build the same way every other responsive value in this app is (see
/// `crate::responsive`). `None` — no shell has published metrics yet, or a
/// bare headless test, which is every test in this suite — leaves the region
/// unbounded, which [`scroll_view`] resolves the same way a plain column
/// would: it sizes itself to its own content rather than clipping anything
/// (`frust_widgets::scroll::ScrollWidget::layout`'s own documented fallback
/// for an infinite incoming height). So a headless suite sees the identical
/// geometry it always has — every existing hit-test locator in
/// `tests/terminal.rs`/`tests/terminal_live.rs` keeps working unchanged —
/// and a device, which always publishes metrics, is the only place the cap
/// (and so the actual scrolling) ever engages.
///
/// Laid out as an ordinary (not `flexible`) child of the outer column
/// [`super::panes_sheet::sheet_body`] composes: a sheet's body is laid out at
/// its own intrinsic height (the panel grows to fit it), so there is no free
/// main-axis space to take a share of — and a `flexible` child
/// under an unbounded main axis divides *zero* free space
/// (`frust_widgets::FlexWidget::layout`'s own free-space computation) —
/// the tight-constraint trap `examples/glyph-catalog`'s own device-gate
/// fixes document. A caller-supplied fixed height sidesteps that trap
/// entirely: [`SizedBox`] tightens to it regardless of the ambient's own
/// unbounded axis, which is exactly why this takes a resolved `f64`
/// rather than a flex share.
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
