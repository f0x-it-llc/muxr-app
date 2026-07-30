//! The `/` route's page: the Connect screen.
//!
//! [`connect_page`] is what the router mounts; [`connect_layout`] is the
//! arrangement half, split out because the cold-start splash
//! ([`crate::ui::splash::sequence`]) lays out the *same* eight pieces through
//! the *same* function — each wrapped in its spring-drop entrance — so the
//! frame the splash finishes on and the frame the route paints are the same
//! picture, and the overlay can simply unmount.
//!
//! Both callers hand the layout a `Vec` of already-built pieces (see
//! [`super::pieces`]); the layout itself is pure arrangement and never builds
//! one, which is what keeps the two trees honest.
//!
//! Transcribed from `docs/splash.html`'s `.ui-layer` rule: a top-anchored
//! column, `padding:30px 22px`, `gap:20px`, children stretched to the layer
//! width. The page paints its own `--bg-void` background — the shell clears
//! to `ColorScheme::surface` (Glyph's card tone), not the page tone.

use frust::{AnyView, Stack, any, safe_area};

use super::pieces::{active_theme, connect_pieces};
use crate::AppState;
use crate::ui::backdrop::page_backdrop;

/// `.ui-layer{padding:30px 22px}` — horizontal.
const LAYER_PAD_X: f64 = 22.0;
/// `.ui-layer{padding:30px 22px}` — vertical.
const LAYER_PAD_Y: f64 = 30.0;
/// `.ui-layer{gap:20px}` — between consecutive pieces.
const LAYER_GAP: f64 = 20.0;

/// Arrange `pieces` exactly as `.ui-layer` does: a safe-area-inset,
/// `LAYER_PAD_*`-padded, top-anchored column with `LAYER_GAP` between
/// consecutive pieces, each stretched to the full layer width.
///
/// Takes the pieces already built so the splash can pass its wrapped copies
/// (identical layout, animated paint) and the route can pass them plain.
pub fn connect_layout(pieces: Vec<AnyView<AppState>>) -> AnyView<AppState> {
    use frust::{Axis, CrossAxisAlignment, EdgeInsets, FlexView, Padding, SizedBox, inflexible};

    let mut children = Vec::with_capacity(pieces.len() * 2);
    for (index, piece) in pieces.into_iter().enumerate() {
        if index > 0 {
            children.push(inflexible(any(SizedBox::<AppState>(None, Some(LAYER_GAP)))));
        }
        children.push(inflexible(piece));
    }

    any(safe_area(Padding(
        EdgeInsets::symmetric(LAYER_PAD_X, LAYER_PAD_Y),
        FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch),
    )))
}

/// The Connect screen: the page background under the eight laid-out pieces.
pub fn connect_page() -> AnyView<AppState> {
    let theme = active_theme();
    any(Stack(vec![
        any(page_backdrop()),
        connect_layout(connect_pieces(&theme)),
    ]))
}
