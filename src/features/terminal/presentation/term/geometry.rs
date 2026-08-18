//! Width-derived cell geometry: how many cells fit the box the grid was given,
//! and how big each one is.
//!
//! **Inverted from the `s9_terminal` benchmark**, which fixed the grid at
//! 80x45 and *solved for the font size* (to compare frust and the Flutter
//! incumbent rasterizing the same cell count over the same pixels). An app
//! runs the other way: the user picks a font size (Settings owns it) and the
//! **grid** resizes, so [`solve_cell_geometry`] measures the face's real cell
//! box at that size and divides the available content box by it.
//!
//! Carried over unchanged: **measure, never assume** (the cell advance comes
//! from shaping a probe string, [`CELL_PROBE`], through the engine's own
//! monospace face, injected as a closure, so the arithmetic below is
//! unit-testable with no font/`TextContext`/window); **bounded shrink
//! iterations** for a box too small to show a usable grid
//! ([`MIN_COLS`]/[`MIN_ROWS`]), up to [`FIT_ITERATIONS`] times with a
//! geometric [`FIT_BACKOFF`]; and **a float-jitter tolerance**
//! ([`FIT_TOLERANCE`]), since an `f32` shaper can measure back a
//! hundred-thousandth of a pixel over an exact division.

use frust::authoring::text::{FontFamily, TextContext, TextStyle};
use frust::authoring::{Color, LayoutCtx, Size};

use super::style::{CellStyle, LINE_HEIGHT};

/// Probe string whose shaped width, divided by its length, is the monospace cell
/// advance. Ten ASCII digits: one cell wide in every monospace face, and long
/// enough that a per-glyph quantization shows up in the average.
pub const CELL_PROBE: &str = "0123456789";

/// The grid used when an axis is unbounded (a degenerate parent, or a unit test
/// with unbounded constraints): the classic VT default, 80x24.
pub const FALLBACK_COLS: u16 = 80;
/// See [`FALLBACK_COLS`].
pub const FALLBACK_ROWS: u16 = 24;

/// The narrowest grid the solver will hand back before it starts shrinking the
/// type instead. **App-chosen, not a published minimum**: below ~20 columns a
/// terminal stops being one, so the honest response to a box that small is
/// smaller type, not a narrower grid.
pub const MIN_COLS: u16 = 20;
/// The shortest grid, for the same reason as [`MIN_COLS`]. Four rows is a
/// prompt, its command, one line of output and the next prompt.
pub const MIN_ROWS: u16 = 4;

/// Floor on the shrunk font size. Reaching it means even [`MIN_COLS`] columns no
/// longer fit at a size any text engine renders sanely; the grid then knowingly
/// overflows its box (and the clip trims it) rather than shipping a 2px font.
pub const MIN_FONT_SIZE: f32 = 4.0;

/// Ceiling on cells per axis. A guard, not a design limit: 512 columns is
/// twice the widest realistic desktop terminal, and stops a degenerate
/// measurement (a near-zero advance) from asking the emulator for a
/// multi-million-cell allocation on the layout pass.
pub const MAX_CELLS_PER_AXIS: u16 = 512;

/// Measure/shrink iterations [`solve_cell_geometry`] may run before accepting
/// whatever it has.
const FIT_ITERATIONS: usize = 8;

/// Multiplicative backoff applied from the second shrink pass onward. A face
/// that quantizes its advance reports the *same* cell width across a band of
/// sizes, so a ratio-derived step lands on a fixed point it can't escape.
const FIT_BACKOFF: f64 = 0.9;

/// Overflow, in logical px per axis, treated as a fit. One hundredth of a
/// logical pixel is sub-pixel rounding on the last column's right edge, not a
/// clipped column, and chasing it would cost a whole column.
const FIT_TOLERANCE: f64 = 0.01;

/// The cell box the grid paints on, the font size it was measured at, and the
/// grid that box divides the content box into.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellGeometry {
    /// The font size actually used, in logical px — the requested one unless
    /// the box was too small (see [`MIN_COLS`]).
    pub font_size: f32,
    /// Measured monospace advance (one cell's width) in logical px.
    pub width: f64,
    /// Measured line box (one cell's height) in logical px.
    pub height: f64,
    /// Columns that fit.
    pub cols: u16,
    /// Rows that fit.
    pub rows: u16,
}

impl CellGeometry {
    /// The painted width: [`cols`](Self::cols) cells.
    pub fn grid_width(self) -> f64 {
        self.width * f64::from(self.cols)
    }

    /// The painted height: [`rows`](Self::rows) cells.
    pub fn grid_height(self) -> f64 {
        self.height * f64::from(self.rows)
    }

    /// The painted box.
    pub fn grid_size(self) -> Size {
        Size::new(self.grid_width(), self.grid_height())
    }
}

/// Solve the cell box and grid that `font_size` yields inside `available`, from a
/// `measure(font_size) -> (cell_advance, line_height)` probe of the engine's own
/// monospace face. Pure — the measurement is injected — so the arithmetic is
/// unit-testable without a font, a `TextContext` or a window.
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
        // The first pass trusts the measured ratio (exact for a linear face);
        // later passes back off geometrically — see [`FIT_BACKOFF`].
        let shrink = if pass == 0 {
            shrink
        } else {
            shrink.min(FIT_BACKOFF)
        };
        let next = (f64::from(size) * shrink) as f32;
        // A step that can't make progress (already at the floor, or rounded
        // back to the same `f32`) must not spin.
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

/// How much the current cell box must shrink for [`MIN_COLS`] x [`MIN_ROWS`] to fit
/// `available` (`>= 1.0` means it already does). An unbounded axis never constrains.
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

/// The grid `available` divides into at this cell box.
fn grid_for(available: Size, (cell_width, cell_height): (f64, f64)) -> (u16, u16) {
    (
        axis_cells(available.width, cell_width, FALLBACK_COLS),
        axis_cells(available.height, cell_height, FALLBACK_ROWS),
    )
}

/// Cells along one axis: `floor(available / cell)`, at least one, capped at
/// [`MAX_CELLS_PER_AXIS`], falling back on an unbounded/degenerate axis.
fn axis_cells(available: f64, cell: f64, fallback: u16) -> u16 {
    // `cell` is compared positively (never `!(cell > 0.0)`): a NaN advance from a
    // degenerate measurement must take the fallback, not slip through a negated
    // partial comparison.
    if !available.is_finite() || available <= 0.0 || !cell.is_finite() || cell <= 0.0 {
        return fallback;
    }
    let count = ((available + FIT_TOLERANCE) / cell).floor();
    count.clamp(1.0, f64::from(MAX_CELLS_PER_AXIS)) as u16
}

/// Measure the monospace cell box at `font_size` in `family`: shape
/// [`CELL_PROBE`] and divide its advance by its length (height is the shaped
/// line box, [`LINE_HEIGHT`] x `font_size`). `family` matters here, not just
/// at paint time — two monospace faces can measure a different advance at the
/// same size, so the grid re-solves whenever the resolved family changes.
pub fn measure_cell(ctx: &mut LayoutCtx, font_size: f32, family: &FontFamily) -> (f64, f64) {
    // The ink is irrelevant to a measurement; the family/line-height are not,
    // which is why this goes through the same `CellStyle` every run shapes with.
    let style = CellStyle::default().text_style(font_size, Color::BLACK, family);
    let text_ctx = ctx.text_context::<TextContext>();
    measure_with(text_ctx, &style, font_size)
}

/// [`measure_cell`]'s body, against an explicit `TextContext` — the seam a test
/// that owns its own context measures through.
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

    /// A synthetic monospace face: advance `ratio` x size, line box
    /// [`LINE_HEIGHT`] x size — linear, like a real unhinted monospace face.
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
        // 392.7 logical px wide (the OP9's safe-area box) at a 0.6em face and
        // 12px type: 7.2px cells -> 54 columns, 14.4px rows -> 60 rows.
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
        // A 0.5em face fits more columns than a 0.7em one at the same size —
        // the solver never assumes 0.6em.
        let wide = solve_cell_geometry(Size::new(400.0, 400.0), 10.0, linear_face(0.7));
        let narrow = solve_cell_geometry(Size::new(400.0, 400.0), 10.0, linear_face(0.5));
        assert!(narrow.cols > wide.cols, "{narrow:?} vs {wide:?}");
        assert_eq!(narrow.cols, 80);
        assert_eq!(wide.cols, 57);
    }

    #[test]
    fn a_bigger_box_never_yields_a_smaller_grid() {
        // Monotone in both axes at a fixed font size — the property a resize
        // (rotation, a dragged desktop window, the fullscreen toggle) rests on.
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
        // …and taller is never fewer rows.
        let short = solve_cell_geometry(Size::new(400.0, 300.0), 12.0, linear_face(0.6));
        let tall = solve_cell_geometry(Size::new(400.0, 1200.0), 12.0, linear_face(0.6));
        assert!(tall.rows > short.rows);
        assert_eq!(tall.cols, short.cols, "the other axis is untouched");
    }

    #[test]
    fn a_box_too_narrow_for_min_cols_shrinks_the_type_not_the_grid() {
        // 100px at 12px/0.6em would be 13 columns — below MIN_COLS, so the
        // solver shrinks the type until 20 fit.
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
        // 20 columns cannot fit 12 logical px at any legible size: the solver
        // floors instead of shipping a 0.5px font, and the grid knowingly
        // overflows (the widget's clip trims it).
        let geometry = solve_cell_geometry(Size::new(12.0, 400.0), 12.0, linear_face(0.6));
        assert_eq!(geometry.font_size, MIN_FONT_SIZE);
        assert!(geometry.cols >= 1, "never a zero-column grid: {geometry:?}");
    }

    #[test]
    fn a_face_whose_advance_quantizes_upward_still_converges() {
        // A face rounding every advance up to a whole pixel reports the same
        // cell width across a band of sizes — the geometric backoff is what
        // stops the shrink loop spinning on that fixed point.
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

        // One bounded axis still derives its own count.
        let geometry = solve_cell_geometry(Size::new(400.0, f64::INFINITY), 12.0, linear_face(0.6));
        assert_eq!(geometry.cols, 55);
        assert_eq!(geometry.rows, FALLBACK_ROWS);
    }

    #[test]
    fn a_degenerate_measurement_cannot_ask_for_a_million_cells() {
        // A face reporting a near-zero advance would otherwise divide a
        // 400px box into 400_000 columns and hand that to the emulator.
        let geometry = solve_cell_geometry(Size::new(400.0, 400.0), 12.0, |_| (0.001, 0.001));
        assert_eq!(geometry.cols, MAX_CELLS_PER_AXIS);
        assert_eq!(geometry.rows, MAX_CELLS_PER_AXIS);
    }
}
