//! The amber matrix rain: full-surface columns of falling glyphs that start
//! mid-fall at full density, then drain away column by column.
//!
//! Three beats on the **rain-local** clock (which starts when the icon
//! dissolves, not when the app launched):
//!
//! | Beat | Window | Behavior |
//! |---|---|---|
//! | [`RainPhase::Dense`] | `0..`[`spec::DENSE_END_MS`] | every column respawns forever |
//! | [`RainPhase::Thinning`] | `..`[`spec::THIN_END_MS`] | respawn probability ramps 1 → 0 |
//! | [`RainPhase::Settled`] | after that | nothing respawns; the last columns fall off and the widget goes quiet ([`rain_finished`]) |
//!
//! Thinning is per column: a column whose head leaves the surface rolls once
//! against the current density and either respawns or retires for good, so
//! the rain visibly drains instead of switching off.
//!
//! No previous-frame fade: the HTML source fakes long trails by never
//! clearing its canvas, but `frust` paints an immediate display list rebuilt
//! from scratch every frame, so there's no retained framebuffer to fade
//! (and a translucent rect here would dim the app content beneath). This
//! widget instead repaints the visible trail window every frame — the head
//! plus [`spec::RAIN_TRAIL`] glyphs above it, with the same alpha falloff —
//! with the only visible difference being a slightly crisper tail.
//!
//! Everything random comes from the widget's seeded [`SplashRng`], so a
//! given seed replays exactly. The rain self-clocks from its first paint by
//! default (mounting it is the cue); [`RainView::clock`] and the live
//! [`RainView::clock_cell`] override that. Fall speed is scaled by the
//! real frame delta ([`super::frame_factor`]) so the rain falls at the same
//! speed on a 120Hz device as on a 60Hz one.
//!
//! Under `MotionScheme::reduce_motion` the rain never starts: nothing
//! painted, no frames requested, no clock advanced.

use std::cell::Cell;
use std::rc::Rc;

use frust::authoring::text::TextStyle;
use frust::authoring::{
    Affine, BoxConstraints, BuildCtx, ChangeFlags, LayoutCtx, PaintCtx, PaintScene, Size, View,
    Widget,
};
use frust::{FrameTime, Theme};

use super::spec;
use super::{
    ShapedGlyph, SplashRng, accent_ink, delta_ms, fill_size, frame_factor, mono_style,
    reduce_motion, with_alpha,
};

/// Which beat the rain-local clock is in. See the module docs' table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RainPhase {
    /// Full density: every column that leaves the surface comes back.
    Dense,
    /// Draining: respawn probability ramps 1 → 0.
    Thinning,
    /// Nothing respawns; the survivors fall off and the rain goes quiet.
    Settled,
}

/// One column's live state.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RainColumn {
    /// The head's vertical position, in rows (may be negative — above the
    /// surface — right after a respawn).
    head_row: f64,
    /// Cleared for good once a head exits and the density roll fails.
    active: bool,
}

/// How many columns fit across `width` (`Math.floor(W/colW)`).
pub fn column_count(width: f64) -> usize {
    if width <= 0.0 {
        return 0;
    }
    (width / spec::RAIN_COL_W).floor() as usize
}

/// The beat at `t_ms` on the rain-local clock.
pub fn rain_phase(t_ms: f64) -> RainPhase {
    if t_ms < spec::DENSE_END_MS {
        RainPhase::Dense
    } else if t_ms < spec::THIN_END_MS {
        RainPhase::Thinning
    } else {
        RainPhase::Settled
    }
}

/// Whether the rain has stopped painting entirely.
///
/// The source keeps drawing for [`spec::RAIN_TAIL_MS`] past
/// [`spec::THIN_END_MS`] (`if(t < THIN_END + 400)`) so the last few columns
/// can fall off screen at zero density, then stops for good.
pub fn rain_finished(t_ms: f64) -> bool {
    t_ms >= spec::THIN_END_MS + spec::RAIN_TAIL_MS
}

/// The global respawn probability at `t_ms`: 1 through the dense beat, then a
/// linear ramp to 0 across the thinning window
/// (`Math.max(0, 1 - (t-DENSE_END)/(THIN_END-DENSE_END))`).
pub fn global_density(t_ms: f64) -> f64 {
    if t_ms <= spec::DENSE_END_MS {
        return 1.0;
    }
    let span = spec::THIN_END_MS - spec::DENSE_END_MS;
    (1.0 - (t_ms - spec::DENSE_END_MS) / span).max(0.0)
}

/// The overall ink fade at `t_ms`: full through the dense beat, then the
/// density with a floor so the last columns stay faintly visible rather than
/// vanishing mid-fall (`Math.max(0.15, globalDensity)`).
pub fn overall_fade(t_ms: f64) -> f64 {
    if t_ms < spec::DENSE_END_MS {
        1.0
    } else {
        global_density(t_ms).max(spec::RAIN_MIN_FADE)
    }
}

/// Alpha for the glyph `index` rows above a column's head (`0` *is* the
/// head), at overall `fade`.
///
/// The head keeps its own near-opaque alpha; each trail glyph falls off
/// linearly as `(1 - index/trail) * 0.75`, both scaled by `fade`.
pub fn trail_alpha(index: usize, fade: f64) -> f64 {
    if index == 0 {
        return spec::RAIN_HEAD_ALPHA * fade;
    }
    let falloff = (1.0 - index as f64 / spec::RAIN_TRAIL as f64).max(0.0);
    falloff * spec::RAIN_TRAIL_ALPHA * fade
}

/// Whether a head at `head_row` has fallen far enough below a `height`-tall
/// surface to be recycled (`py > H + fontSize*6`).
pub fn head_exited(head_row: f64, height: f64) -> bool {
    head_row * spec::RAIN_ROW_H > height + spec::RAIN_ROW_H * spec::RAIN_EXIT_MARGIN_ROWS
}

/// What happens to a column whose head just exited.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnFate {
    /// Comes back at the top (the source's `drops[c] = rand(-6,-1)`).
    Respawn,
    /// Goes dark for the rest of the sequence.
    Retire,
}

/// Decide an exited column's fate from a `[0,1)` `roll` against `density`
/// (`if(Math.random() < globalDensity)`).
///
/// At density 1 every roll respawns; at density 0 every roll retires; in
/// between the per-column roll is what staggers the drain.
pub fn column_fate(roll: f64, density: f64) -> ColumnFate {
    if roll < density {
        ColumnFate::Respawn
    } else {
        ColumnFate::Retire
    }
}

/// The full-surface matrix rain. `seed` fixes every random choice.
pub fn rain<State: 'static>(seed: u64) -> RainView<State> {
    RainView {
        seed,
        clock_ms: None,
        clock_cell: None,
        state: std::marker::PhantomData,
    }
}

/// The declarative half of the rain. See [`rain`].
pub struct RainView<State: 'static> {
    seed: u64,
    clock_ms: Option<f64>,
    clock_cell: Option<Rc<Cell<f64>>>,
    /// `View<State>` is generic over the app state even though a decorative
    /// leaf never touches it.
    state: std::marker::PhantomData<fn() -> State>,
}

impl<State: 'static> RainView<State> {
    /// Drive the rain-local clock externally (milliseconds since the rain was
    /// cued) instead of letting the widget self-clock from its first paint.
    ///
    /// The orchestrator uses this when it wants one clock across the whole
    /// sequence; a standalone mount can ignore it.
    pub fn clock(mut self, elapsed_ms: f64) -> Self {
        self.clock_ms = Some(elapsed_ms);
        self
    }

    /// Drive the rain-local clock from a cell the orchestrator writes each
    /// frame, read at paint. Outranks [`clock`](Self::clock) and the
    /// self-clock: `clock` only carries a value known at *build* time, so a
    /// clock derived from the orchestrator's own paint needs this instead.
    pub fn clock_cell(mut self, cell: Rc<Cell<f64>>) -> Self {
        self.clock_cell = Some(cell);
        self
    }
}

/// Whether two optional clock cells are the *same* cell — identity, not
/// value: a rebuild that hands over a different cell holding an equal
/// reading has still moved the rain onto a new writer.
fn same_clock_cell(left: Option<&Rc<Cell<f64>>>, right: Option<&Rc<Cell<f64>>>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => Rc::ptr_eq(left, right),
        _ => false,
    }
}

/// The retained half of the rain. See [`rain`].
pub struct RainWidget {
    /// The seeded generator behind every column start, fall speed, glyph pick
    /// and respawn roll.
    rng: SplashRng,
    /// Live column state, rebuilt whenever the surface size changes.
    columns: Vec<RainColumn>,
    /// The surface the columns were laid out for.
    size: Size,
    /// One shaped glyph per distinct pool entry, plus the style they were
    /// shaped at so a theme swap re-shapes them.
    glyphs: Vec<ShapedGlyph>,
    style: Option<TextStyle>,
    /// The rain-local clock.
    elapsed_ms: f64,
    /// An orchestrator-supplied clock, when there is one.
    clock_ms: Option<f64>,
    /// A live orchestrator-supplied clock, read at paint. Outranks both.
    clock_cell: Option<Rc<Cell<f64>>>,
    /// The previous frame's clock, for differencing.
    last_frame: Option<FrameTime>,
}

impl RainWidget {
    /// (Re)seed every column mid-fall — the source's `initCols`, whose whole
    /// point is that the rain is already running when it first appears rather
    /// than booting up from an empty screen.
    fn reset_columns(&mut self) {
        let count = column_count(self.size.width);
        let rows = if spec::RAIN_ROW_H > 0.0 {
            self.size.height / spec::RAIN_ROW_H
        } else {
            0.0
        };
        self.columns = (0..count)
            .map(|_| RainColumn {
                head_row: self.rng.unit() * rows,
                active: true,
            })
            .collect();
    }
}

impl<State: 'static> View<State> for RainView<State> {
    type Element = RainWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> RainWidget {
        RainWidget {
            rng: SplashRng::new(self.seed),
            columns: Vec::new(),
            size: Size::ZERO,
            glyphs: Vec::new(),
            style: None,
            elapsed_ms: 0.0,
            clock_ms: self.clock_ms,
            clock_cell: self.clock_cell.clone(),
            last_frame: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut RainWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.clock_ms = self.clock_ms;
        let same_cell = same_clock_cell(prev.clock_cell.as_ref(), self.clock_cell.as_ref());
        element.clock_cell = self.clock_cell.clone();
        // A genuinely new seed restarts the rain; the ordinary rebuild
        // re-passes the same one and changes nothing.
        if prev.seed != self.seed {
            element.rng = SplashRng::new(self.seed);
            element.reset_columns();
            return ChangeFlags::PAINT;
        }
        if prev.clock_ms != self.clock_ms || !same_cell {
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

impl Widget for RainWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = fill_size(bc, Size::new(spec::RAIN_COL_W, spec::RAIN_ROW_H));
        if size != self.size {
            // A resize (rotation, split-screen) re-seeds the field: the column
            // count is width-derived, so keeping the old vector would leave a
            // stale strip either uncovered or off-surface.
            self.size = size;
            self.reset_columns();
        }

        let theme = Theme::from_layout_ctx(ctx).cloned();
        let style = mono_style(theme.as_ref(), spec::RAIN_FONT_SIZE);
        if self.style.as_ref() != Some(&style) {
            self.glyphs = (0..spec::DISTINCT_LEN)
                .map(|i| ShapedGlyph::shape(ctx, spec::distinct_glyph(i), &style))
                .collect();
            self.style = Some(style);
        }
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        if reduce_motion(theme) {
            return;
        }

        let delta = delta_ms(&mut self.last_frame, ctx.frame_time());
        self.elapsed_ms = match (self.clock_cell.as_ref(), self.clock_ms) {
            (Some(cell), _) => cell.get(),
            (None, Some(external)) => external,
            (None, None) => self.elapsed_ms + delta,
        };
        if rain_finished(self.elapsed_ms) {
            return;
        }

        let density = global_density(self.elapsed_ms);
        let fade = overall_fade(self.elapsed_ms);
        let factor = frame_factor(delta);
        let ink = accent_ink(theme);
        let origin = ctx.origin();
        let height = self.size.height;

        for index in 0..self.columns.len() {
            if !self.columns[index].active {
                continue;
            }
            let head_row = self.columns[index].head_row;
            if head_exited(head_row, height) {
                match column_fate(self.rng.unit(), density) {
                    ColumnFate::Respawn => {
                        self.columns[index].head_row = self
                            .rng
                            .range(spec::RAIN_RESPAWN_MIN_ROW, spec::RAIN_RESPAWN_MAX_ROW);
                    }
                    ColumnFate::Retire => self.columns[index].active = false,
                }
                continue;
            }

            let x = origin.x + index as f64 * spec::RAIN_COL_W;
            let head_y = head_row * spec::RAIN_ROW_H;
            for trail in 0..spec::RAIN_TRAIL {
                let y = head_y - trail as f64 * spec::RAIN_ROW_H;
                if y < -spec::RAIN_ROW_H || y > height {
                    continue;
                }
                // A fresh glyph per cell per frame — the source's flicker.
                let glyph = spec::pool_to_distinct(self.rng.below(spec::POOL_LEN));
                let color = if trail == 0 {
                    with_alpha(spec::RAIN_HEAD_COLOR, trail_alpha(trail, fade))
                } else {
                    with_alpha(ink, trail_alpha(trail, fade))
                };
                if let Some(shaped) = self.glyphs.get(glyph) {
                    shaped.draw(scene, Affine::translate((x, origin.y + y)), color);
                }
            }

            let step = self
                .rng
                .range(spec::RAIN_ADVANCE_MIN, spec::RAIN_ADVANCE_MAX);
            self.columns[index].head_row += step * factor;
        }

        // Once every column has drained there is nothing left to animate, even
        // if the tail window hasn't elapsed — stop asking for frames rather
        // than spinning the shell on an empty surface.
        if self.columns.iter().any(|column| column.active) {
            ctx.request_frame();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- geometry --------------------------------------------------------

    #[test]
    fn columns_tile_the_width_and_degrade_on_a_degenerate_surface() {
        assert_eq!(column_count(340.0), 24); // the design reference's phone
        assert_eq!(column_count(spec::RAIN_COL_W), 1);
        assert_eq!(column_count(spec::RAIN_COL_W - 0.01), 0);
        assert_eq!(column_count(0.0), 0);
        assert_eq!(column_count(-100.0), 0);
    }

    #[test]
    fn a_head_only_exits_a_full_margin_below_the_surface() {
        let height = 650.0;
        let last_visible = height / spec::RAIN_ROW_H;
        assert!(!head_exited(last_visible, height), "still on screen");
        assert!(
            !head_exited(last_visible + spec::RAIN_EXIT_MARGIN_ROWS, height),
            "the trail is still draining through the margin"
        );
        assert!(head_exited(
            last_visible + spec::RAIN_EXIT_MARGIN_ROWS + 0.01,
            height
        ));
    }

    // ---- phases / density ------------------------------------------------

    #[test]
    fn the_phases_follow_the_rain_local_clock() {
        assert_eq!(rain_phase(0.0), RainPhase::Dense);
        assert_eq!(rain_phase(spec::DENSE_END_MS - 1.0), RainPhase::Dense);
        assert_eq!(rain_phase(spec::DENSE_END_MS), RainPhase::Thinning);
        assert_eq!(rain_phase(spec::THIN_END_MS - 1.0), RainPhase::Thinning);
        assert_eq!(rain_phase(spec::THIN_END_MS), RainPhase::Settled);
    }

    #[test]
    fn painting_stops_only_after_the_tail_window() {
        assert!(!rain_finished(spec::THIN_END_MS));
        assert!(!rain_finished(spec::THIN_END_MS + spec::RAIN_TAIL_MS - 1.0));
        assert!(rain_finished(spec::THIN_END_MS + spec::RAIN_TAIL_MS));
    }

    #[test]
    fn density_holds_at_one_then_ramps_linearly_to_zero() {
        assert_eq!(global_density(0.0), 1.0);
        assert_eq!(global_density(spec::DENSE_END_MS), 1.0);
        let midpoint = (spec::DENSE_END_MS + spec::THIN_END_MS) / 2.0;
        assert!((global_density(midpoint) - 0.5).abs() < 1e-9);
        assert_eq!(global_density(spec::THIN_END_MS), 0.0);
        assert_eq!(global_density(spec::THIN_END_MS + 5_000.0), 0.0);
    }

    #[test]
    fn the_overall_fade_never_drops_below_its_floor() {
        assert_eq!(overall_fade(0.0), 1.0);
        let midpoint = (spec::DENSE_END_MS + spec::THIN_END_MS) / 2.0;
        assert!((overall_fade(midpoint) - 0.5).abs() < 1e-9);
        assert_eq!(overall_fade(spec::THIN_END_MS), spec::RAIN_MIN_FADE);
        assert_eq!(
            overall_fade(spec::THIN_END_MS + 1_000.0),
            spec::RAIN_MIN_FADE
        );
    }

    // ---- trail alpha -----------------------------------------------------

    #[test]
    fn the_head_is_brightest_and_the_trail_falls_off_linearly() {
        assert!((trail_alpha(0, 1.0) - spec::RAIN_HEAD_ALPHA).abs() < 1e-9);
        // (1 - 1/7) * 0.75
        assert!((trail_alpha(1, 1.0) - (6.0 / 7.0) * 0.75).abs() < 1e-9);
        // (1 - 6/7) * 0.75 — the dimmest drawn glyph.
        assert!((trail_alpha(6, 1.0) - (1.0 / 7.0) * 0.75).abs() < 1e-9);
        // Past the trail length the falloff clamps at zero, never negative.
        assert_eq!(trail_alpha(spec::RAIN_TRAIL, 1.0), 0.0);
        assert_eq!(trail_alpha(spec::RAIN_TRAIL + 3, 1.0), 0.0);
    }

    #[test]
    fn the_trail_is_strictly_dimmer_the_further_it_is_from_the_head() {
        let mut previous = f64::INFINITY;
        for index in 0..spec::RAIN_TRAIL {
            let alpha = trail_alpha(index, 1.0);
            assert!(alpha < previous, "alpha must decrease at index {index}");
            assert!((0.0..=1.0).contains(&alpha));
            previous = alpha;
        }
    }

    #[test]
    fn the_overall_fade_scales_every_alpha_uniformly() {
        for index in 0..spec::RAIN_TRAIL {
            let full = trail_alpha(index, 1.0);
            let faded = trail_alpha(index, 0.4);
            assert!((faded - full * 0.4).abs() < 1e-9, "at index {index}");
        }
    }

    // ---- respawn ---------------------------------------------------------

    #[test]
    fn at_full_density_every_column_respawns() {
        for roll in [0.0, 0.25, 0.5, 0.75, 0.999] {
            assert_eq!(column_fate(roll, 1.0), ColumnFate::Respawn, "roll {roll}");
        }
    }

    #[test]
    fn at_half_density_the_roll_decides_column_by_column() {
        assert_eq!(column_fate(0.0, 0.5), ColumnFate::Respawn);
        assert_eq!(column_fate(0.49, 0.5), ColumnFate::Respawn);
        assert_eq!(column_fate(0.5, 0.5), ColumnFate::Retire);
        assert_eq!(column_fate(0.99, 0.5), ColumnFate::Retire);
    }

    #[test]
    fn at_zero_density_every_column_retires() {
        for roll in [0.0, 0.25, 0.5, 0.999] {
            assert_eq!(column_fate(roll, 0.0), ColumnFate::Retire, "roll {roll}");
        }
    }

    #[test]
    fn a_half_density_field_drains_to_roughly_half_its_columns() {
        // The statistical shape of the drain (not an exact count): with a
        // seeded RNG, ~half of a wide field survives one round at 0.5.
        let mut rng = SplashRng::new(2026);
        let survivors = (0..1_000)
            .filter(|_| column_fate(rng.unit(), 0.5) == ColumnFate::Respawn)
            .count();
        assert!(
            (400..600).contains(&survivors),
            "expected ~500 survivors, got {survivors}"
        );
    }

    // ---- column seeding --------------------------------------------------

    #[test]
    fn columns_start_mid_fall_and_span_the_surface() {
        let mut widget = RainWidget {
            rng: SplashRng::new(5),
            columns: Vec::new(),
            size: Size::new(340.0, 650.0),
            glyphs: Vec::new(),
            style: None,
            elapsed_ms: 0.0,
            clock_ms: None,
            clock_cell: None,
            last_frame: None,
        };
        widget.reset_columns();

        assert_eq!(widget.columns.len(), column_count(340.0));
        let rows = 650.0 / spec::RAIN_ROW_H;
        for column in &widget.columns {
            assert!(column.active);
            assert!(
                (0.0..rows).contains(&column.head_row),
                "a column must start somewhere on the surface, got {}",
                column.head_row
            );
        }
        // Staggered, not all at the same row (the "already running" look).
        let first = widget.columns[0].head_row;
        assert!(
            widget
                .columns
                .iter()
                .any(|c| (c.head_row - first).abs() > 1.0),
            "column starts must be staggered"
        );
    }

    #[test]
    fn the_same_seed_replays_the_same_field() {
        let field = |seed| {
            let mut widget = RainWidget {
                rng: SplashRng::new(seed),
                columns: Vec::new(),
                size: Size::new(340.0, 650.0),
                glyphs: Vec::new(),
                style: None,
                elapsed_ms: 0.0,
                clock_ms: None,
                clock_cell: None,
                last_frame: None,
            };
            widget.reset_columns();
            widget.columns
        };
        assert_eq!(field(77), field(77));
        assert_ne!(field(77), field(78));
    }

    #[test]
    fn a_clock_cell_repaints_only_when_the_cell_itself_changes() {
        let mut ids = 0u64;
        let mut ctx = BuildCtx::new(&mut ids);
        let cell = Rc::new(Cell::new(0.0));

        let first = rain::<()>(9).clock_cell(Rc::clone(&cell));
        let mut widget = View::<()>::build(&first, &mut ctx);
        assert!(widget.clock_cell.is_some(), "the cell reaches the widget");

        // The ordinary rebuild re-passes the same cell: nothing to repaint.
        let same = rain::<()>(9).clock_cell(Rc::clone(&cell));
        assert_eq!(
            View::<()>::rebuild(&same, &first, &mut widget, &mut ctx),
            ChangeFlags::NONE
        );

        // A different cell — even holding an equal reading — is a new writer.
        let moved = rain::<()>(9).clock_cell(Rc::new(Cell::new(0.0)));
        assert_eq!(
            View::<()>::rebuild(&moved, &same, &mut widget, &mut ctx),
            ChangeFlags::PAINT
        );
        assert!(
            same_clock_cell(widget.clock_cell.as_ref(), moved.clock_cell.as_ref()),
            "the widget must adopt the cell it was just repainted for"
        );
    }
}
