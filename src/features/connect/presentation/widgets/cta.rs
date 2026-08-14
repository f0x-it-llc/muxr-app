//! Piece 6 — `.btn-primary`, the amber CTA, and the pinned pairing CTA above
//! it.

use std::rc::Rc;

use frust::{Alignment, AnyView, SizedBox, Theme, any, button};

use super::pairing::{pairing_banner, pairing_chrome};
use super::{Live, column, v_gap};
use crate::ui::palette::Palette;
use crate::ui::spacing;

/// `.btn-primary`'s label. `pub` because `tests/connect.rs` locates the CTA by
/// its accessibility label.
pub const CONNECT_LABEL: &str = "→ connect";

/// The CTA's forced height, in logical px: the platform minimum tap target.
/// `ButtonWidget`'s own vertical padding (unthemed and un-overridable) lands short
/// of it at this label's size, so a [`SizedBox`] forces the floor.
const CTA_HEIGHT: f64 = 44.0;

/// Piece 6 — the full-bleed CTA firing [`Live::on_connect`], plus the pairing pair
/// above it while a scanned pairing is stashed. The layer's stretch cross-axis
/// already forces the width, so only the height is set here;
/// `.label_alignment(Alignment::CENTER)` is what centers the label on **both**
/// axes, since the widget's stretch default only auto-centers horizontally.
///
/// **Known gap**: `ButtonView` has no per-instance color or text-style override,
/// so the label shapes at `TextStyle::default()` rather than the mock's
/// `label_large_emphasized` role, and `ButtonStyle::Primary` resolves fill/ink
/// from `primary`/`on_primary` — byte-identical to the container pair every other
/// piece paints with under this app's canonical dark brightness, divergent under
/// Glyph *light*.
///
/// The pairing rows live here rather than as a ninth piece because the stagger
/// table has exactly eight slots; the splash never sees them, since it composes
/// with no [`Live`].
pub fn connect_button<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
) -> AnyView<S> {
    // Read once and shared by every CTA this piece renders; the splash reads
    // `false`, same as every other signal here.
    let busy = live.is_some_and(|live| live.controller.busy());
    let on_connect = live.map(|live| Rc::clone(&live.on_connect));
    let cta = any(SizedBox::<S>(None, Some(CTA_HEIGHT)).child(
        button(CONNECT_LABEL, move |_state: &mut S| {
            if let Some(on_connect) = &on_connect {
                on_connect();
            }
        })
        .label_alignment(Alignment::CENTER)
        // `busy` spans the offer's held redirect too: after the dialog closes on
        // accept, the mint → gate → store → revoke chain runs with the form still
        // looking live and this CTA the only thing between the user and a re-tap.
        .loading(busy),
    ));
    let Some(pairing) = pairing_chrome(live) else {
        return cta;
    };
    // `live` is `Some(..)` whenever `pairing_chrome` answered, so the CTA below
    // always has a real action behind it.
    let on_connect_pinned = live.map(|live| Rc::clone(&live.on_connect_pinned));
    any(column::<S>(vec![
        pairing_banner(theme, colors, pairing),
        v_gap::<S>(spacing::MD),
        any(SizedBox::<S>(None, Some(CTA_HEIGHT)).child(
            button(pairing.connect_label(), move |_state: &mut S| {
                if let Some(on_connect_pinned) = &on_connect_pinned {
                    on_connect_pinned();
                }
            })
            .label_alignment(Alignment::CENTER)
            // One in-flight connect, one spinner: the same connection-controller
            // signal the manual CTA reads, over the same single-flight guard.
            .loading(busy),
        )),
        v_gap::<S>(spacing::MD),
        cta,
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cta_clears_the_minimum_tap_target() {
        // The forced height is the platform minimum tap target regardless of the
        // button's own shorter padding; a future drop below the floor is the
        // regression this catches.
        const { assert!(CTA_HEIGHT >= 44.0) };
    }
}
