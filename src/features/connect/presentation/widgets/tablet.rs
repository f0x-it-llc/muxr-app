//! The Tablet arrangement of the Connect screen: backdrop, the form column,
//! and, on the full band, the left panel beside it.
//!
//! [`tablet_screen`] is the whole tree the page mounts for a Tablet window.
//! The corner rule: the QR action always sits beside the CTA, and the info
//! mark ([`info_affordance`]) is mounted only on the collapsed band. The full
//! band carries the orientation paragraph inline in the panel, so the mark
//! would repeat it.

use frust::{
    AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexView, Padding, SizedBox, Stack, Theme,
    WindowMetrics, any, colored_box, container, flexible, inflexible, safe_area, scroll_view,
    use_context,
};

use super::getting_started::info_affordance;
use super::layout::{clamp, gapped};
use super::{Live, tablet_form_pieces, tablet_panel_pieces};
use crate::AppState;
use crate::features::connect::domain::entities::SavedServer;
use crate::ui::backdrop::page_backdrop;
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::Palette;
use crate::ui::spacing;

/// The full band's left panel width (`docs/tablet.html`'s `.c-left`).
const LEFT_PANEL_WIDTH: f64 = 420.0;

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
    let mut layers = vec![
        any(page_backdrop::<S>()),
        any(safe_area(tablet_body(theme, live, saved, full, qr))),
    ];
    if !full {
        layers.push(info_affordance(overlays));
    }
    any(Stack(layers))
}

/// On the full band a [`LEFT_PANEL_WIDTH`] panel beside the form column;
/// collapsed, the form column alone, the saved list folded under its button
/// row. The form clamps like Compact's wide window and scrolls from its top
/// when taller than the frame.
fn tablet_body<S: 'static>(
    theme: &Theme,
    live: &Live,
    saved: &[SavedServer],
    full: bool,
    qr: AnyView<S>,
) -> AnyView<S> {
    let folded = (!full).then_some(saved);
    let form = gapped(tablet_form_pieces(theme, live, folded, qr), spacing::XL);
    let form_column = any(scroll_view(Padding(
        EdgeInsets::symmetric(spacing::SHEET, spacing::HERO),
        clamp(any(form)),
    )));
    if !full {
        return form_column;
    }
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
    let panel = container(scroll_view(Padding(
        inset,
        gapped(tablet_panel_pieces(theme, live, saved), spacing::XL),
    )))
    .fill(colors.base);
    let edge = colored_box::<S>().size(HAIRLINE, 0.0).fill(colors.rule);
    any(FlexView::new(
        Axis::Horizontal,
        vec![
            inflexible(any(SizedBox::<S>(Some(LEFT_PANEL_WIDTH), None).child(panel))),
            inflexible(any(edge)),
            flexible(1, form_column),
        ],
    )
    .cross_axis(CrossAxisAlignment::Stretch))
}
