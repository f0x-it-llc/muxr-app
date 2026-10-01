//! The Tablet arrangement of the Connect screen: backdrop, the form column,
//! and, on the full band, the left panel beside it.
//!
//! [`tablet_screen`] is the whole tree the page mounts for a Tablet window.
//! The corner rule: the QR action always sits beside the CTA, and the info
//! mark ([`info_affordance`]) is mounted only on the collapsed band. The full
//! band carries the orientation paragraph inline in the panel, so the mark
//! would repeat it.
//!
//! The cold-start splash assembles the same arrangement on a Tablet window:
//! [`tablet_splash_pieces`] builds the pieces unwired, in stagger order, and
//! [`tablet_splash_layout`] lays them out through the same [`tablet_layout`]
//! the screen uses — the Tablet half of the parity contract
//! (`connect_pieces`/`connect_layout` is the Compact half).

use frust::{
    AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexView, Padding, SizedBox, Stack, Theme,
    WindowMetrics, any, colored_box, container, flexible, inflexible, safe_area, scroll_view,
    use_context,
};

use super::getting_started::info_affordance;
use super::layout::{clamp, gapped};
use super::qr_affordance::splash_qr_button;
use super::{Live, tablet_form_pieces, tablet_panel_pieces};
use crate::AppState;
use crate::features::connect::domain::entities::SavedServer;
use crate::ui::backdrop::page_backdrop;
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::Palette;
use crate::ui::spacing;

/// The full band's left panel width (`docs/tablet.html`'s `.c-left`).
const LEFT_PANEL_WIDTH: f64 = 420.0;

/// Which column a full-band piece lands in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    Panel,
    Form,
}

/// The full band's nine pieces in **stagger order**: the order the splash
/// drops them in, which [`tablet_splash_pieces`] builds and
/// [`tablet_splash_layout`] un-interleaves by the same table. The two
/// columns fill top-down together — brand row, then the form's heading, and
/// so on down both — with the action row in the last of the eight stagger
/// slots and the panel's saved list sharing it (`spec::piece_delay_ms`
/// reuses the last slot for a ninth piece). Each column's entries are in
/// that column's own top-to-bottom order, which is what makes the two
/// functions inverses.
const FULL_STAGGER: [Column; 9] = [
    Column::Panel, // brand row
    Column::Form,  // heading
    Column::Panel, // headline
    Column::Form,  // server URL
    Column::Panel, // getting-started card
    Column::Form,  // login token
    Column::Form,  // remember row
    Column::Form,  // action row
    Column::Panel, // saved list
];

/// The Tablet screen over the page backdrop. `qr` is the action beside the
/// CTA; `overlays` raises the info dialog on the collapsed band.
pub fn tablet_screen<S: 'static>(
    theme: &Theme,
    live: &Live,
    saved: &[SavedServer],
    full: bool,
    qr: AnyView<S>,
    overlays: &frust::NavigatorController<AppState>,
) -> AnyView<S> {
    let folded = (!full).then_some(saved);
    let form = tablet_form_pieces(theme, Some(live), folded, qr);
    let panel = full.then(|| tablet_panel_pieces(theme, Some(live), saved));
    let mut layers = vec![any(page_backdrop::<S>()), tablet_layout(theme, panel, form)];
    if !full {
        layers.push(info_affordance(overlays));
    }
    any(Stack(layers))
}

/// The Tablet pieces with no live wiring, in stagger order — the splash's
/// half of the seam. On the full band the panel's and the form's pieces
/// interleave per [`FULL_STAGGER`]; collapsed, the form's alone, the saved
/// list folded under its action row exactly as the screen folds it.
pub fn tablet_splash_pieces<S: 'static>(
    theme: &Theme,
    saved: &[SavedServer],
    full: bool,
) -> Vec<AnyView<S>> {
    let qr = splash_qr_button();
    if !full {
        return tablet_form_pieces(theme, None, Some(saved), qr);
    }
    let mut panel = tablet_panel_pieces(theme, None, saved).into_iter();
    let mut form = tablet_form_pieces(theme, None, None, qr).into_iter();
    let pieces: Vec<AnyView<S>> = FULL_STAGGER
        .iter()
        .map(|column| match column {
            Column::Panel => panel.next(),
            Column::Form => form.next(),
        })
        .map(|piece| piece.expect("`FULL_STAGGER` names every panel and form piece once"))
        .collect();
    debug_assert!(
        panel.next().is_none() && form.next().is_none(),
        "`FULL_STAGGER` names every panel and form piece once"
    );
    pieces
}

/// Lay out `pieces` — [`tablet_splash_pieces`]' list, each wrapped by the
/// splash — exactly as [`tablet_screen`] lays the live ones out, so the
/// overlay's last frame and the route's first are one picture.
pub fn tablet_splash_layout<S: 'static>(
    theme: &Theme,
    full: bool,
    pieces: Vec<AnyView<S>>,
) -> AnyView<S> {
    if !full {
        return tablet_layout(theme, None, pieces);
    }
    let mut panel = Vec::new();
    let mut form = Vec::new();
    for (column, piece) in FULL_STAGGER.iter().zip(pieces) {
        match column {
            Column::Panel => panel.push(piece),
            Column::Form => form.push(piece),
        }
    }
    tablet_layout(theme, Some(panel), form)
}

/// The safe-area-inset body: on the full band (`panel` is `Some`) a
/// [`LEFT_PANEL_WIDTH`] panel beside the form column; collapsed, the form
/// column alone. Each list is already in its own top-to-bottom order. The
/// form clamps like Compact's wide window and scrolls from its top when
/// taller than the frame.
fn tablet_layout<S: 'static>(
    theme: &Theme,
    panel: Option<Vec<AnyView<S>>>,
    form: Vec<AnyView<S>>,
) -> AnyView<S> {
    let form_column = any(scroll_view(Padding(
        EdgeInsets::symmetric(spacing::SHEET, spacing::HERO),
        clamp(any(gapped(form, spacing::XL))),
    )));
    let Some(panel) = panel else {
        return any(safe_area(form_column));
    };
    let colors = Palette::from_theme(theme);
    // The iPadOS top-left window control sits over the brand tile; clear it.
    // Zero corners (every other platform) leave the padding untouched.
    let corner = use_context::<WindowMetrics>()
        .map(|metrics| metrics.insets.corner_insets.top_left)
        .filter(|corner| corner.height > 0.0);
    let (corner_w, corner_h) = corner.map_or((0.0, 0.0), |c| (c.width, c.height));
    let inset = EdgeInsets {
        left: spacing::HERO + corner_w,
        top: spacing::HERO + corner_h,
        right: spacing::HERO,
        bottom: spacing::SHEET,
    };
    let panel =
        container(scroll_view(Padding(inset, gapped(panel, spacing::XL)))).fill(colors.base);
    let edge = colored_box::<S>().size(HAIRLINE, 0.0).fill(colors.rule);
    any(safe_area(
        FlexView::new(
            Axis::Horizontal,
            vec![
                inflexible(any(SizedBox::<S>(Some(LEFT_PANEL_WIDTH), None).child(panel))),
                inflexible(any(edge)),
                flexible(1, form_column),
            ],
        )
        .cross_axis(CrossAxisAlignment::Stretch),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::splash::spec;

    fn column_count(column: Column) -> usize {
        FULL_STAGGER.iter().filter(|c| **c == column).count()
    }

    #[test]
    fn the_stagger_table_names_every_panel_and_form_piece_once() {
        let theme = frust_glyph::baseline();
        assert_eq!(
            column_count(Column::Panel),
            tablet_panel_pieces::<AppState>(&theme, None, &[]).len()
        );
        assert_eq!(
            column_count(Column::Form),
            tablet_form_pieces::<AppState>(&theme, None, None, splash_qr_button()).len()
        );
        assert_eq!(
            tablet_splash_pieces::<AppState>(&theme, &[], true).len(),
            FULL_STAGGER.len()
        );
    }

    #[test]
    fn the_collapsed_band_assembles_the_form_with_the_list_folded_under_it() {
        let theme = frust_glyph::baseline();
        let form = tablet_form_pieces::<AppState>(&theme, None, None, splash_qr_button()).len();
        assert_eq!(
            tablet_splash_pieces::<AppState>(&theme, &[], false).len(),
            form + 1,
            "the form's pieces plus the folded saved list"
        );
    }

    #[test]
    fn the_action_row_takes_the_last_stagger_slot_and_the_list_shares_it() {
        let last_form = FULL_STAGGER
            .iter()
            .rposition(|c| *c == Column::Form)
            .expect("the form has pieces");
        assert_eq!(
            last_form,
            spec::PIECE_DELAYS_MS.len() - 1,
            "the CTA lands in the final slot, like the phone's"
        );
        assert_eq!(
            FULL_STAGGER.len(),
            spec::PIECE_DELAYS_MS.len() + 1,
            "one piece past the table reuses its last slot"
        );
        assert_eq!(FULL_STAGGER[FULL_STAGGER.len() - 1], Column::Panel);
    }
}
