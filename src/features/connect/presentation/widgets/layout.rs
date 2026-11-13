//! Lives under `widgets` so [`super::tablet`] and the page both import it from below.

use frust::{
    Align, Alignment, AnyView, Axis, CrossAxisAlignment, EdgeInsets, Either, FlexView, Padding,
    SizedBox, View, WindowMetrics, any, inflexible, safe_area, use_context,
};

use crate::responsive::Responsive;
use crate::ui::spacing;

pub const CONNECT_CLAMP_WIDTH: f64 = 520.0;
/// Not [`Alignment::CENTER`]: the splash assembles into a bounded `Stack` layer, where
/// `CENTER` would trade this clamp's horizontal jump for a vertical one.
const CLAMP_ALIGNMENT: Alignment = Alignment { x: 0.0, y: -1.0 };
/// The clamp lives here so the splash and the route agree on layout without a jump.
pub fn connect_layout<S: 'static>(pieces: Vec<AnyView<S>>) -> AnyView<S> // erasure: keep stored view table/registry
{
    // `None` (no shell metrics yet, or a bare test harness) takes the narrow branch.
    let wide_clamp = Responsive::of(use_context::<WindowMetrics>()).wide_clamp;
    any(clamped_layout(pieces, wide_clamp))
}

pub(crate) fn clamped_layout<S: 'static>(
    pieces: Vec<AnyView<S>>,
    wide_clamp: bool,
) -> impl View<S> {
    let layer = safe_area(Padding(
        EdgeInsets::symmetric(spacing::SHEET, spacing::HERO),
        gapped(pieces, spacing::SHEET),
    ));
    if !wide_clamp {
        return Either::Left(layer);
    }
    Either::Right(clamp(layer))
}

pub(super) fn clamp<S: 'static>(child: impl View<S>) -> impl View<S> {
    Align(
        CLAMP_ALIGNMENT,
        SizedBox::<S>(Some(CONNECT_CLAMP_WIDTH), None).child(child),
    )
}

pub(super) fn gapped<S: 'static>(pieces: Vec<AnyView<S>>, gap: f64) -> FlexView<S> {
    let mut children = Vec::with_capacity(pieces.len() * 2);
    for (index, piece) in pieces.into_iter().enumerate() {
        if index > 0 {
            children.push(inflexible(SizedBox::<S>(None, Some(gap))));
        }
        children.push(inflexible(piece));
    }
    FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch)
}
