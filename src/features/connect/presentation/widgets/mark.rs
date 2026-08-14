//! Piece 0 — `.connect-mark`.

use frust::authoring::Size;
use frust::{Align, Alignment, AnyView, EdgeInsets, Padding, Theme, any, inflexible};

use super::super::chrome::{panel, swatch};
use super::{h_gap, hugging_column, row, v_gap};
use crate::ui::palette::Palette;
use crate::ui::spacing;

/// `.connect-mark{width:60px;height:60px}`.
const MARK_SIZE: f64 = 60.0;
/// `.connect-mark{gap:5px}` — off the app's spacing scale, so it stays local.
const MARK_GAP: f64 = 5.0;
/// One grid cell: the tile minus its padding (`spacing::BASE`, the source's
/// `padding:12px`) and the single gap, halved (`grid-template-columns:1fr 1fr`).
const MARK_CELL: f64 = (MARK_SIZE - 2.0 * spacing::BASE - MARK_GAP) / 2.0;

/// Piece 0 — a bordered 60×60 tile holding a 2×2 grid whose diagonal is solid
/// amber and whose off-diagonal is the faint amber wash, centered in the layer
/// (`margin:0 auto`). Pure decoration — no [`Live`](super::Live).
pub fn connect_mark<S: 'static>(theme: &Theme, colors: &Palette) -> AnyView<S> {
    let cell = Size::new(MARK_CELL, MARK_CELL);
    let cell_radius = theme.shape.extra_small;
    // `.connect-mark span:nth-child(1),(4)` — faint wash + hairline accent
    // border; `(2),(3)` — the solid accent fill.
    let faint = || any(swatch(cell, colors.amber_faint, cell_radius).border(colors.amber_border));
    let solid = || any(swatch(cell, colors.amber, cell_radius));
    let grid = hugging_column::<S>(vec![
        any(row::<S>(vec![
            inflexible(faint()),
            inflexible(h_gap::<S>(MARK_GAP)),
            inflexible(solid()),
        ])),
        v_gap::<S>(MARK_GAP),
        any(row::<S>(vec![
            inflexible(solid()),
            inflexible(h_gap::<S>(MARK_GAP)),
            inflexible(faint()),
        ])),
    ]);
    let tile = panel(
        Padding(EdgeInsets::all(spacing::BASE), grid),
        colors.page,
        theme.shape.medium,
    )
    .border(colors.outline);
    any(Align(Alignment::CENTER, tile))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_cells_tile_the_marks_content_box() {
        // 2 cells + 1 gap + 2 paddings == the tile edge (the CSS grid's own
        // arithmetic, asserted so a later tweak to one constant can't silently
        // unbalance the tile).
        assert!(
            (2.0 * MARK_CELL + MARK_GAP + 2.0 * spacing::BASE - MARK_SIZE).abs() < 1e-9,
            "cells must exactly fill the mark's content box"
        );
    }
}
