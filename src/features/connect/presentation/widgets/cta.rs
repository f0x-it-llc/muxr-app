//! Piece 6 — `.btn-primary`, the amber CTA, and the pinned pairing CTA above
//! it.

use std::rc::Rc;

use frust::{Alignment, Either, SizedBox, SizedBoxView, Theme, View, button, flexible, inflexible};

use super::pairing::{pairing_banner, pairing_chrome};
use super::{Live, block_column, centered_row, h_gap, v_gap};
use crate::ui::palette::Palette;
use crate::ui::spacing;

/// `.btn-primary`'s label. `pub` because `tests/connect.rs` locates the CTA by
/// its accessibility label.
pub const CONNECT_LABEL: &str = "→ connect";

/// The platform minimum tap target: `ButtonWidget`'s own vertical padding
/// lands short of it at this label's size, so a [`SizedBox`] forces the floor.
const CTA_HEIGHT: f64 = 44.0;

/// Piece 6 — the full-bleed CTA firing [`Live::on_connect`], plus the pairing
/// pair above it while a scanned pairing is stashed.
/// `.label_alignment(Alignment::CENTER)` centers the label on both axes,
/// since the widget's stretch default only auto-centers horizontally.
///
/// Known gap: `ButtonView` has no per-instance color/text-style override, so
/// the label shapes at `TextStyle::default()` rather than the mock's role,
/// and `ButtonStyle::Primary`'s fill/ink diverges from other pieces under
/// Glyph light (byte-identical under this app's canonical dark brightness).
///
/// The pairing rows live here rather than as a ninth piece because the
/// stagger table has exactly eight slots; the splash never sees them.
pub fn connect_button<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
) -> impl View<S> {
    connect_block::<S, SizedBoxView<S>>(theme, colors, live, None)
}

/// The Tablet form's action block: the same pairing rows over a button row,
/// the CTA taking the free width beside `secondary` at the CTA's own height.
pub fn connect_actions<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
    secondary: impl View<S>,
) -> impl View<S> {
    connect_block(theme, colors, live, Some(secondary))
}

fn connect_block<S: 'static, V: View<S>>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
    secondary: Option<V>,
) -> impl View<S> {
    // Shared by every CTA this piece renders; the splash reads `false`.
    let busy = live.is_some_and(|live| live.controller.busy());
    let on_connect = live.map(|live| Rc::clone(&live.on_connect));
    let cta = SizedBox::<S>(None, Some(CTA_HEIGHT)).child(
        button(CONNECT_LABEL, move |_state: &mut S| {
            if let Some(on_connect) = &on_connect {
                on_connect();
            }
        })
        .label_alignment(Alignment::CENTER)
        // `busy` spans the offer's held redirect too, since the save chain runs
        // with the form still looking live and this CTA the only guard against a re-tap.
        .loading(busy),
    );
    let cta = match secondary {
        None => Either::Left(cta),
        Some(secondary) => Either::Right(centered_row::<S>(vec![
            flexible(1, cta),
            inflexible(h_gap::<S>(spacing::MD)),
            inflexible(SizedBox::<S>(None, Some(CTA_HEIGHT)).child(secondary)),
        ])),
    };
    let Some(pairing) = pairing_chrome(live) else {
        return Either::Left(cta);
    };
    let on_connect_pinned = live.map(|live| Rc::clone(&live.on_connect_pinned));
    Either::Right(block_column::<S, _>((
        pairing_banner(theme, colors, pairing),
        v_gap::<S>(spacing::MD),
        SizedBox::<S>(None, Some(CTA_HEIGHT)).child(
            button(pairing.connect_label(), move |_state: &mut S| {
                if let Some(on_connect_pinned) = &on_connect_pinned {
                    on_connect_pinned();
                }
            })
            .label_alignment(Alignment::CENTER)
            // Same connection-controller signal the manual CTA reads.
            .loading(busy),
        ),
        v_gap::<S>(spacing::MD),
        cta,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cta_clears_the_minimum_tap_target() {
        const { assert!(CTA_HEIGHT >= 44.0) };
    }
}
