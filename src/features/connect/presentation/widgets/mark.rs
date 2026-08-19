//! Piece 0 — `.connect-mark`.

use frust::{
    Align, Alignment, AnyView, EdgeInsets, Padding, Theme, any, colored_box, container, inflexible,
};

use super::{h_gap, hugging_column, row, v_gap};
use crate::ui::palette::Palette;
use crate::ui::spacing;

/// Hairline border width in logical px (`1px solid` on every bordered rule in
/// `docs/splash.html`).
const HAIRLINE: f64 = 1.0;

const MARK_SIZE: f64 = 60.0;
/// Off the app's spacing scale, so it stays local.
const MARK_GAP: f64 = 5.0;
/// One grid cell: the tile minus its padding and the single gap, halved.
const MARK_CELL: f64 = (MARK_SIZE - 2.0 * spacing::BASE - MARK_GAP) / 2.0;

/// Piece 0 — a bordered 60×60 tile holding a 2×2 grid whose diagonal is solid
/// amber and whose off-diagonal is the faint amber wash, centered in the layer
/// (`margin:0 auto`). Pure decoration — no [`Live`](super::Live).
pub fn connect_mark<S: 'static>(theme: &Theme, colors: &Palette) -> AnyView<S> {
    let cell_radius = theme.shape.extra_small;
    // Diagonal is a hairline-bordered faint wash; off-diagonal is solid accent fill.
    let faint = || {
        any(colored_box::<S>()
            .size(MARK_CELL, MARK_CELL)
            .fill(colors.amber_faint)
            .radius(cell_radius)
            .border(colors.amber_border, HAIRLINE))
    };
    let solid = || {
        any(colored_box::<S>()
            .size(MARK_CELL, MARK_CELL)
            .fill(colors.amber)
            .radius(cell_radius))
    };
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
    let tile = container(Padding(EdgeInsets::all(spacing::BASE), grid))
        .fill(colors.page)
        .radius(theme.shape.medium)
        .border(colors.outline, HAIRLINE);
    any(Align(Alignment::CENTER, tile))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_cells_tile_the_marks_content_box() {
        // 2 cells + 1 gap + 2 paddings == the tile edge.
        assert!(
            (2.0 * MARK_CELL + MARK_GAP + 2.0 * spacing::BASE - MARK_SIZE).abs() < 1e-9,
            "cells must exactly fill the mark's content box"
        );
    }
}
