//! The Connect screen's layout helpers: the responsive clamp and the gapped
//! column, shared by the page, the Tablet arrangement and the splash.
//!
//! They live under `widgets` so [`super::tablet`] and the page both import
//! them from below, never from each other; the page re-exports
//! [`connect_layout`] for the splash.

use frust::{
    Align, Alignment, AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexView, Padding, SizedBox,
    WindowMetrics, any, inflexible, safe_area, use_context,
};

use crate::responsive::Responsive;
use crate::ui::spacing;

/// The content column's max width past
/// [`WIDE_BREAKPOINT_PX`](crate::responsive::WIDE_BREAKPOINT_PX): a wide window
/// centers the form rather than stretching it edge to edge. A plain clamp, not
/// a layout switch (see `crate::responsive`).
pub const CONNECT_CLAMP_WIDTH: f64 = 520.0;
/// Where the clamped column sits in a wide window: centered horizontally,
/// top-anchored vertically. Not [`Alignment::CENTER`] — the splash assembles
/// into a bounded `Stack` layer, and `CENTER` there would trade this clamp's
/// horizontal jump for a vertical one.
const CLAMP_ALIGNMENT: Alignment = Alignment { x: 0.0, y: -1.0 };
/// Arranges `pieces` into a safe-area-inset, padded, top-anchored column with
/// [`spacing::SHEET`] between them, each stretched to the layer width — and,
/// past the wide breakpoint, clamped to [`CONNECT_CLAMP_WIDTH`] and
/// centered.
///
/// Takes the pieces already built, so the splash can pass its wrapped copies
/// and the screen its live ones. The clamp lives here rather than in the
/// screen's own `build`, so the splash and the route agree on layout without
/// a jump between them.
pub fn connect_layout<S: 'static>(pieces: Vec<AnyView<S>>) -> AnyView<S> {
    // `None` (no shell published metrics yet, or a bare test harness) takes the
    // same branch as a genuinely narrow window.
    let wide_clamp = Responsive::of(use_context::<WindowMetrics>()).wide_clamp;
    clamped_layout(pieces, wide_clamp)
}

/// [`connect_layout`] with the clamp decided by the caller — the screen's
/// Compact branch, which has already read its [`Responsive`] once.
pub(crate) fn clamped_layout<S: 'static>(pieces: Vec<AnyView<S>>, wide_clamp: bool) -> AnyView<S> {
    let layer = any(safe_area(Padding(
        EdgeInsets::symmetric(spacing::SHEET, spacing::HERO),
        gapped(pieces, spacing::SHEET),
    )));
    if !wide_clamp {
        return layer;
    }
    clamp(layer)
}

/// Centers `child` at no more than [`CONNECT_CLAMP_WIDTH`], top-anchored.
pub(super) fn clamp<S: 'static>(child: AnyView<S>) -> AnyView<S> {
    any(Align(
        CLAMP_ALIGNMENT,
        SizedBox::<S>(Some(CONNECT_CLAMP_WIDTH), None).child(child),
    ))
}

/// `pieces` stacked full-width with `gap` between them.
pub(super) fn gapped<S: 'static>(pieces: Vec<AnyView<S>>, gap: f64) -> FlexView<S> {
    let mut children = Vec::with_capacity(pieces.len() * 2);
    for (index, piece) in pieces.into_iter().enumerate() {
        if index > 0 {
            children.push(inflexible(any(SizedBox::<S>(None, Some(gap)))));
        }
        children.push(inflexible(piece));
    }
    FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch)
}
