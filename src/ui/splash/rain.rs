//! The amber matrix rain: columns start mid-fall at full density, then drain column by column.
//! `frust` rebuilds its display list every frame, so there is no framebuffer to fade: each frame
//! repaints the head plus [`spec::RAIN_TRAIL`] glyphs with the source's alpha falloff.

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RainPhase {
    Dense,
    Thinning,
    Settled,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct RainColumn {
    /// In rows; negative above the surface right after a respawn.
    head_row: f64,
    active: bool,
}

pub fn column_count(width: f64) -> usize {
    if width <= 0.0 {
        return 0;
    }
    (width / spec::RAIN_COL_W).floor() as usize
}

pub fn rain_phase(t_ms: f64) -> RainPhase {
    if t_ms < spec::DENSE_END_MS {
        RainPhase::Dense
    } else if t_ms < spec::THIN_END_MS {
        RainPhase::Thinning
    } else {
        RainPhase::Settled
    }
}

/// The source keeps drawing [`spec::RAIN_TAIL_MS`] past thinning so the last columns fall off.
pub fn rain_finished(t_ms: f64) -> bool {
    t_ms >= spec::THIN_END_MS + spec::RAIN_TAIL_MS
}

pub fn global_density(t_ms: f64) -> f64 {
    if t_ms <= spec::DENSE_END_MS {
        return 1.0;
    }
    let span = spec::THIN_END_MS - spec::DENSE_END_MS;
    (1.0 - (t_ms - spec::DENSE_END_MS) / span).max(0.0)
}

/// Floored so the last columns stay faintly visible rather than vanishing mid-fall.
pub fn overall_fade(t_ms: f64) -> f64 {
    if t_ms < spec::DENSE_END_MS {
        1.0
    } else {
        global_density(t_ms).max(spec::RAIN_MIN_FADE)
    }
}

/// `index` counts rows above the head; `0` is the head.
pub fn trail_alpha(index: usize, fade: f64) -> f64 {
    if index == 0 {
        return spec::RAIN_HEAD_ALPHA * fade;
    }
    let falloff = (1.0 - index as f64 / spec::RAIN_TRAIL as f64).max(0.0);
    falloff * spec::RAIN_TRAIL_ALPHA * fade
}

pub fn head_exited(head_row: f64, height: f64) -> bool {
    head_row * spec::RAIN_ROW_H > height + spec::RAIN_ROW_H * spec::RAIN_EXIT_MARGIN_ROWS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnFate {
    Respawn,
    Retire,
}

/// `roll` is in `[0, 1)`; the per-column roll is what staggers the drain.
pub fn column_fate(roll: f64, density: f64) -> ColumnFate {
    if roll < density {
        ColumnFate::Respawn
    } else {
        ColumnFate::Retire
    }
}

pub fn rain<State: 'static>(seed: u64) -> RainView<State> {
    RainView {
        seed,
        clock_ms: None,
        clock_cell: None,
        state: std::marker::PhantomData,
    }
}

pub struct RainView<State: 'static> {
    seed: u64,
    clock_ms: Option<f64>,
    clock_cell: Option<Rc<Cell<f64>>>,
    state: std::marker::PhantomData<fn() -> State>,
}

impl<State: 'static> RainView<State> {
    /// A fixed rain-local clock in ms; without one the rain self-clocks from its first paint.
    pub fn clock(mut self, elapsed_ms: f64) -> Self {
        self.clock_ms = Some(elapsed_ms);
        self
    }

    /// A clock the orchestrator writes each frame, read at paint; outranks [`clock`](Self::clock).
    pub fn clock_cell(mut self, cell: Rc<Cell<f64>>) -> Self {
        self.clock_cell = Some(cell);
        self
    }
}

/// Identity, not value: a different cell holding an equal reading is still a new writer.
fn same_clock_cell(left: Option<&Rc<Cell<f64>>>, right: Option<&Rc<Cell<f64>>>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => Rc::ptr_eq(left, right),
        _ => false,
    }
}

pub struct RainWidget {
    rng: SplashRng,
    columns: Vec<RainColumn>,
    size: Size,
    /// One per distinct pool entry; `style` is kept so a theme swap re-shapes them.
    glyphs: Vec<ShapedGlyph>,
    style: Option<TextStyle>,
    elapsed_ms: f64,
    clock_ms: Option<f64>,
    clock_cell: Option<Rc<Cell<f64>>>,
    last_frame: Option<FrameTime>,
}

impl RainWidget {
    /// Seeds every column mid-fall, so the rain is already running when it first appears.
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
            // The column count is width-derived, so a resize re-seeds the field.
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
                // A fresh glyph per cell per frame: the source's flicker.
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

        // Stop requesting frames once every column has drained, even inside the tail window.
        if self.columns.iter().any(|column| column.active) {
            ctx.request_frame();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn the_head_is_brightest_and_the_trail_falls_off_linearly() {
        assert!((trail_alpha(0, 1.0) - spec::RAIN_HEAD_ALPHA).abs() < 1e-9);
        assert!((trail_alpha(1, 1.0) - (6.0 / 7.0) * 0.75).abs() < 1e-9);
        assert!((trail_alpha(6, 1.0) - (1.0 / 7.0) * 0.75).abs() < 1e-9);
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
        let mut rng = SplashRng::new(2026);
        let survivors = (0..1_000)
            .filter(|_| column_fate(rng.unit(), 0.5) == ColumnFate::Respawn)
            .count();
        assert!(
            (400..600).contains(&survivors),
            "expected ~500 survivors, got {survivors}"
        );
    }

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

        let same = rain::<()>(9).clock_cell(Rc::clone(&cell));
        assert_eq!(
            View::<()>::rebuild(&same, &first, &mut widget, &mut ctx),
            ChangeFlags::NONE
        );

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
