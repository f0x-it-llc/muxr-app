//! Piece 0: `.connect-mark`.

use frust::{
    Align, Alignment, EdgeInsets, Padding, Theme, View, colored_box, container, inflexible,
};

use super::{centered_row, h_gap, hugging_column, v_gap};
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::Palette;
use crate::ui::spacing;

const MARK_SIZE: f64 = 60.0;
/// Off the app's spacing scale, so it stays local.
const MARK_GAP: f64 = 5.0;
const MARK_CELL: f64 = (MARK_SIZE - 2.0 * spacing::BASE - MARK_GAP) / 2.0;

pub fn connect_mark<S: 'static>(theme: &Theme, colors: &Palette) -> impl View<S> {
    let cell_radius = theme.shape.extra_small;
    // Diagonal is a hairline-bordered faint wash; off-diagonal is solid accent fill.
    let faint = || {
        colored_box::<S>()
            .size(MARK_CELL, MARK_CELL)
            .fill(colors.amber_faint)
            .radius(cell_radius)
            .border(colors.amber_border, HAIRLINE)
    };
    let solid = || {
        colored_box::<S>()
            .size(MARK_CELL, MARK_CELL)
            .fill(colors.amber)
            .radius(cell_radius)
    };
    let grid = hugging_column::<S, _>((
        centered_row::<S>(vec![
            inflexible(faint()),
            inflexible(h_gap::<S>(MARK_GAP)),
            inflexible(solid()),
        ]),
        v_gap::<S>(MARK_GAP),
        centered_row::<S>(vec![
            inflexible(solid()),
            inflexible(h_gap::<S>(MARK_GAP)),
            inflexible(faint()),
        ]),
    ));
    let tile = container(Padding(EdgeInsets::all(spacing::BASE), grid))
        .fill(colors.page)
        .radius(theme.shape.medium)
        .border(colors.outline, HAIRLINE);
    Align(Alignment::CENTER, tile)
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
