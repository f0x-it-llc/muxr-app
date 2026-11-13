//! Width-derived cell geometry: the user picks the font size and the grid resizes to the
//! measured cell box. All lengths are logical px.

use frust::authoring::text::{FontFamily, TextContext, TextStyle};
use frust::authoring::{Color, LayoutCtx, Size};

use super::style::{CellStyle, LINE_HEIGHT};

/// Ten ASCII digits: one cell wide in any monospace face, long enough to average quantization.
pub const CELL_PROBE: &str = "0123456789";

/// Used for an unbounded axis: the VT default, 80x24.
pub const FALLBACK_COLS: u16 = 80;
pub const FALLBACK_ROWS: u16 = 24;

/// App-chosen: below this the solver shrinks the type rather than the grid.
pub const MIN_COLS: u16 = 20;
pub const MIN_ROWS: u16 = 4;

/// At this floor the grid knowingly overflows its box and the clip trims it.
pub const MIN_FONT_SIZE: f32 = 4.0;

/// A guard against a near-zero measured advance asking for a multi-million-cell emulator.
pub const MAX_CELLS_PER_AXIS: u16 = 512;

const FIT_ITERATIONS: usize = 8;

/// Applied from the second shrink pass: a face that quantizes its advance would otherwise
/// stall on a fixed point.
const FIT_BACKOFF: f64 = 0.9;

/// Overflow per axis treated as a fit: `f32` shaper jitter, not a clipped column.
const FIT_TOLERANCE: f64 = 0.01;

/// `font_size` is below the requested size only when the box is too small for [`MIN_COLS`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellGeometry {
    pub font_size: f32,
    pub width: f64,
    pub height: f64,
    pub cols: u16,
    pub rows: u16,
}

impl CellGeometry {
    pub fn grid_width(self) -> f64 {
        self.width * f64::from(self.cols)
    }

    pub fn grid_height(self) -> f64 {
        self.height * f64::from(self.rows)
    }

    pub fn grid_size(self) -> Size {
        Size::new(self.grid_width(), self.grid_height())
    }
}

/// `measure(font_size) -> (cell_advance, line_height)` probes the engine's own monospace face.
pub fn solve_cell_geometry(
    available: Size,
    font_size: f32,
    mut measure: impl FnMut(f32) -> (f64, f64),
) -> CellGeometry {
    let mut size = font_size.max(MIN_FONT_SIZE);
    let mut metrics = measure(size);

    for pass in 0..FIT_ITERATIONS {
        let shrink = shrink_factor(available, metrics);
        if shrink >= 1.0 {
            break;
        }
        let shrink = if pass == 0 {
            shrink
        } else {
            shrink.min(FIT_BACKOFF)
        };
        let next = (f64::from(size) * shrink) as f32;
        // Already at the floor, or rounded back to the same `f32`: no progress possible.
        if next >= size || next.is_nan() {
            break;
        }
        if next < MIN_FONT_SIZE {
            size = MIN_FONT_SIZE;
            metrics = measure(size);
            break;
        }
        size = next;
        metrics = measure(size);
    }

    let (cols, rows) = grid_for(available, metrics);
    CellGeometry {
        font_size: size,
        width: metrics.0,
        height: metrics.1,
        cols,
        rows,
    }
}

/// `>= 1.0` means [`MIN_COLS`] x [`MIN_ROWS`] already fit; an unbounded axis never constrains.
fn shrink_factor(available: Size, (cell_width, cell_height): (f64, f64)) -> f64 {
    let mut factor = f64::INFINITY;
    if available.width.is_finite() && available.width > 0.0 && cell_width > 0.0 {
        factor = factor.min((available.width + FIT_TOLERANCE) / (f64::from(MIN_COLS) * cell_width));
    }
    if available.height.is_finite() && available.height > 0.0 && cell_height > 0.0 {
        factor =
            factor.min((available.height + FIT_TOLERANCE) / (f64::from(MIN_ROWS) * cell_height));
    }
    if factor.is_finite() { factor } else { 1.0 }
}

fn grid_for(available: Size, (cell_width, cell_height): (f64, f64)) -> (u16, u16) {
    (
        axis_cells(available.width, cell_width, FALLBACK_COLS),
        axis_cells(available.height, cell_height, FALLBACK_ROWS),
    )
}

fn axis_cells(available: f64, cell: f64, fallback: u16) -> u16 {
    // Positive comparisons only: a NaN advance must take the fallback.
    if !available.is_finite() || available <= 0.0 || !cell.is_finite() || cell <= 0.0 {
        return fallback;
    }
    let count = ((available + FIT_TOLERANCE) / cell).floor();
    count.clamp(1.0, f64::from(MAX_CELLS_PER_AXIS)) as u16
}

/// The grid re-solves when `family` changes: two monospace faces can measure a different
/// advance at the same size.
pub fn measure_cell(ctx: &mut LayoutCtx, font_size: f32, family: &FontFamily) -> (f64, f64) {
    let style = CellStyle::default().text_style(font_size, Color::BLACK, family);
    let text_ctx = ctx.text_context::<TextContext>();
    measure_with(text_ctx, &style, font_size)
}

pub fn measure_with(text_ctx: &mut TextContext, style: &TextStyle, font_size: f32) -> (f64, f64) {
    let probe = text_ctx.layout(CELL_PROBE, style, None);
    let size = probe.size();
    let n = CELL_PROBE.chars().count() as f64;
    let width = if size.width > 0.0 && n > 0.0 {
        size.width / n
    } else {
        f64::from(font_size)
    };
    let height = if size.height > 0.0 {
        size.height
    } else {
        f64::from(font_size * LINE_HEIGHT)
    };
    (width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear_face(ratio: f64) -> impl FnMut(f32) -> (f64, f64) {
        move |size| {
            (
                f64::from(size) * ratio,
                f64::from(size) * f64::from(LINE_HEIGHT),
            )
        }
    }

    #[test]
    fn the_grid_is_the_box_divided_by_the_measured_cell() {
        // A phone safe-area box; a 0.6em face at 12px gives 7.2px cells and 14.4px rows.
        let geometry = solve_cell_geometry(Size::new(392.7, 872.7), 12.0, linear_face(0.6));
        assert_eq!(geometry.font_size, 12.0, "the type never moved");
        assert!((geometry.width - 7.2).abs() < 0.001, "{geometry:?}");
        assert_eq!(geometry.cols, 54);
        assert_eq!(geometry.rows, 60);
        assert!(geometry.grid_width() <= 392.7 + FIT_TOLERANCE);
        assert!(geometry.grid_height() <= 872.7 + FIT_TOLERANCE);
    }

    #[test]
    fn the_ratio_is_read_off_the_probe_not_assumed() {
        let wide = solve_cell_geometry(Size::new(400.0, 400.0), 10.0, linear_face(0.7));
        let narrow = solve_cell_geometry(Size::new(400.0, 400.0), 10.0, linear_face(0.5));
        assert!(narrow.cols > wide.cols, "{narrow:?} vs {wide:?}");
        assert_eq!(narrow.cols, 80);
        assert_eq!(wide.cols, 57);
    }

    #[test]
    fn a_bigger_box_never_yields_a_smaller_grid() {
        let mut last = (0u16, 0u16);
        for width in (100..2000).step_by(37) {
            for height in [200.0, 900.0] {
                let geometry = solve_cell_geometry(
                    Size::new(f64::from(width), height),
                    12.0,
                    linear_face(0.6),
                );
                if height == 900.0 {
                    assert!(
                        geometry.cols >= last.0,
                        "cols fell from {} to {} at width {width}",
                        last.0,
                        geometry.cols
                    );
                    last = (geometry.cols, geometry.rows);
                }
            }
        }
        let short = solve_cell_geometry(Size::new(400.0, 300.0), 12.0, linear_face(0.6));
        let tall = solve_cell_geometry(Size::new(400.0, 1200.0), 12.0, linear_face(0.6));
        assert!(tall.rows > short.rows);
        assert_eq!(tall.cols, short.cols, "the other axis is untouched");
    }

    #[test]
    fn a_box_too_narrow_for_min_cols_shrinks_the_type_not_the_grid() {
        let geometry = solve_cell_geometry(Size::new(100.0, 400.0), 12.0, linear_face(0.6));
        assert!(geometry.font_size < 12.0, "{geometry:?}");
        assert!(geometry.cols >= MIN_COLS, "{geometry:?}");
        assert!(
            geometry.grid_width() <= 100.0 + FIT_TOLERANCE,
            "{geometry:?}"
        );
    }

    #[test]
    fn a_hopeless_box_stops_at_the_font_size_floor() {
        let geometry = solve_cell_geometry(Size::new(12.0, 400.0), 12.0, linear_face(0.6));
        assert_eq!(geometry.font_size, MIN_FONT_SIZE);
        assert!(geometry.cols >= 1, "never a zero-column grid: {geometry:?}");
    }

    #[test]
    fn a_face_whose_advance_quantizes_upward_still_converges() {
        let geometry = solve_cell_geometry(Size::new(90.0, 400.0), 12.0, |size| {
            (
                (f64::from(size) * 0.6).ceil(),
                f64::from(size) * f64::from(LINE_HEIGHT),
            )
        });
        assert!(geometry.cols >= MIN_COLS, "{geometry:?}");
        assert!(geometry.font_size <= 12.0);
    }

    #[test]
    fn an_unbounded_axis_falls_back_instead_of_dividing_by_infinity() {
        let geometry = solve_cell_geometry(
            Size::new(f64::INFINITY, f64::INFINITY),
            12.0,
            linear_face(0.6),
        );
        assert_eq!(
            (geometry.cols, geometry.rows),
            (FALLBACK_COLS, FALLBACK_ROWS)
        );
        assert_eq!(geometry.font_size, 12.0, "nothing constrained the type");

        let geometry = solve_cell_geometry(Size::new(400.0, f64::INFINITY), 12.0, linear_face(0.6));
        assert_eq!(geometry.cols, 55);
        assert_eq!(geometry.rows, FALLBACK_ROWS);
    }

    #[test]
    fn a_degenerate_measurement_cannot_ask_for_a_million_cells() {
        let geometry = solve_cell_geometry(Size::new(400.0, 400.0), 12.0, |_| (0.001, 0.001));
        assert_eq!(geometry.cols, MAX_CELLS_PER_AXIS);
        assert_eq!(geometry.rows, MAX_CELLS_PER_AXIS);
    }
}
