//! The Tablet arrangement. The QR action always sits beside the CTA; the info mark mounts
//! only on the collapsed band, since the full band carries its paragraph inline.

use frust::{
    AnyView, CrossAxisAlignment, EdgeInsets, Padding, SizedBox, Stack, Theme, View, WindowMetrics,
    any, colored_box, container, row, safe_area, scroll_view, use_context,
};

use super::cta::Prominence;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Column {
    Panel,
    Form,
}

/// Stagger order: the splash drops them in this order and [`tablet_splash_layout`]
/// un-interleaves by the same table. Each column's entries keep its own top-to-bottom order,
/// which makes the two functions inverses; the ninth piece reuses the last slot.
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

pub fn tablet_screen<S: 'static, Q: View<S>>(
    theme: &Theme,
    live: &Live,
    saved: &[SavedServer],
    full: bool,
    qr: Q,
    overlays: &frust::NavigatorController<AppState>,
) -> impl View<S> + use<S, Q> {
    let folded = (!full).then_some(saved);
    let form = tablet_form_pieces(theme, Some(live), Prominence::for_saved(saved), folded, qr);
    let panel = full.then(|| tablet_panel_pieces(theme, Some(live), saved));
    let mut layers = vec![
        any(page_backdrop::<S>()),
        any(tablet_layout(theme, panel, form)),
    ];
    if !full {
        layers.push(any(info_affordance(overlays)));
    }
    Stack(layers)
}

pub fn tablet_splash_pieces<S: 'static>(
    theme: &Theme,
    saved: &[SavedServer],
    full: bool,
) -> Vec<AnyView<S>> {
    let qr = splash_qr_button();
    if !full {
        return tablet_form_pieces(theme, None, Prominence::for_saved(saved), Some(saved), qr);
    }
    let mut panel = tablet_panel_pieces(theme, None, saved).into_iter();
    let mut form =
        tablet_form_pieces(theme, None, Prominence::for_saved(saved), None, qr).into_iter();
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

/// Lays the splash's pieces out exactly as [`tablet_screen`] lays the live ones.
pub fn tablet_splash_layout<S: 'static>(
    theme: &Theme,
    full: bool,
    pieces: Vec<AnyView<S>>,
) -> AnyView<S> // erasure: keep stored view table/registry
{
    if !full {
        return any(tablet_layout(theme, None, pieces));
    }
    let mut panel = Vec::new();
    let mut form = Vec::new();
    for (column, piece) in FULL_STAGGER.iter().zip(pieces) {
        match column {
            Column::Panel => panel.push(piece),
            Column::Form => form.push(piece),
        }
    }
    any(tablet_layout(theme, Some(panel), form))
}

fn tablet_layout<S: 'static>(
    theme: &Theme,
    panel: Option<Vec<AnyView<S>>>,
    form: Vec<AnyView<S>>,
) -> impl View<S> {
    let form_column = scroll_view(Padding(
        EdgeInsets::symmetric(spacing::SHEET, spacing::HERO),
        clamp(gapped(form, spacing::XL)),
    ));
    let Some(panel) = panel else {
        return safe_area(form_column);
    };
    let colors = Palette::from_theme(theme);
    // The iPadOS top-left window control sits over the brand tile; zero corners elsewhere.
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
    safe_area(
        row()
            .child(SizedBox::<S>(Some(LEFT_PANEL_WIDTH), None).child(panel))
            .child(edge)
            .flex(1, form_column)
            .cross_axis(CrossAxisAlignment::Stretch),
    )
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
            tablet_form_pieces::<AppState>(
                &theme,
                None,
                Prominence::Primary,
                None,
                splash_qr_button()
            )
            .len()
        );
        assert_eq!(
            tablet_splash_pieces::<AppState>(&theme, &[], true).len(),
            FULL_STAGGER.len()
        );
    }

    #[test]
    fn the_collapsed_band_assembles_the_form_with_the_list_folded_under_it() {
        let theme = frust_glyph::baseline();
        let form = tablet_form_pieces::<AppState>(
            &theme,
            None,
            Prominence::Primary,
            None,
            splash_qr_button(),
        )
        .len();
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
