//! The splash assembly played behind a rain whose clock holds at the dense boundary until an
//! external `released` edge. Mount it as the last child of an always-present `Stack` over the
//! live band, so the erased child type never flips and the pod underneath keeps its identity.

use std::cell::Cell;
use std::rc::Rc;

use frust::authoring::{
    AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult,
    InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, SemanticsCtx, Size, View, Widget, any,
    build_child, rebuild_child, teardown_child,
};
use frust::{FrameTime, RwSignal, Set, Theme};

use super::sequence::piece_entry;
use super::{delta_ms, fill_size, reduce_motion, spec};

/// The floor stops a band that resolves from cache in 20ms flashing the rain for one frame.
pub fn release_instant(observed_ms: f64, min_dense_ms: f64) -> f64 {
    observed_ms.max(min_dense_ms)
}

/// Holds at [`spec::DENSE_END_MS`] until release; the columns fall on the frame delta, not on
/// this clock, so the hold reads as dense rather than frozen.
pub fn rain_local_ms(elapsed_ms: f64, release_at: Option<f64>) -> f64 {
    match release_at {
        None => elapsed_ms.min(spec::DENSE_END_MS),
        Some(release) if elapsed_ms < release => elapsed_ms.min(spec::DENSE_END_MS),
        Some(release) => spec::DENSE_END_MS + (elapsed_ms - release),
    }
}

pub fn assembly_open_ms(release_at: f64) -> f64 {
    release_at + spec::LOADING_ASSEMBLY_OFFSET_MS
}

pub fn assembly_landed_ms(open_ms: f64, piece_count: usize) -> f64 {
    open_ms
        + spec::piece_delay_ms(piece_count.saturating_sub(1))
        + spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS)
}

pub fn sequence_end_ms(release_at: f64) -> f64 {
    let rain_tail = spec::THIN_END_MS + spec::RAIN_TAIL_MS - spec::DENSE_END_MS;
    let last_landing = spec::LOADING_ASSEMBLY_OFFSET_MS
        + spec::piece_delay_ms(spec::PIECE_DELAYS_MS.len() - 1)
        + spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS);
    release_at + rain_tail.max(last_landing)
}

/// Nothing below this widget times the wait out (the gRPC layer sets no per-call deadline), so
/// a first load that never settles would otherwise rain forever.
pub fn auto_release_ms() -> f64 {
    spec::LOADING_MAX_HOLD_MS
}

/// `pieces` and `assemble` must be the builders the live band uses, or the handoff crosses two
/// pictures. `released` is monotonic; `stale` keeps input here past the landing until the
/// unmount; `done` is set exactly once.
pub fn loading_sequence<State: 'static, A: View<State>>(
    seed: u64,
    tone: Color,
    pieces: Vec<AnyView<State>>,
    assemble: impl FnOnce(Vec<AnyView<State>>) -> A,
    released: bool,
    stale: bool,
    done: RwSignal<bool>,
) -> AnyView<State> // erasure: keep accumulator into Vec<AnyView>
{
    let cell = Rc::new(Cell::new(0.0));
    let piece_count = pieces.len();
    let staggered = pieces
        .into_iter()
        .enumerate()
        .map(|(index, piece)| any(piece_entry(spec::piece_delay_ms(index), piece)))
        .collect();
    any(LoadingView {
        rain: super::rain::rain::<State>(seed).clock_cell(Rc::clone(&cell)),
        assembly: assemble(staggered),
        cell,
        piece_count,
        tone,
        released,
        stale,
        done,
    })
}

pub struct LoadingView<R, A> {
    rain: R,
    assembly: A,
    cell: Rc<Cell<f64>>,
    piece_count: usize,
    tone: Color,
    released: bool,
    stale: bool,
    done: RwSignal<bool>,
}

pub struct LoadingWidget {
    rain: ChildPod,
    assembly: ChildPod,
    cell: Rc<Cell<f64>>,
    piece_count: usize,
    tone: Color,
    /// Acted on at paint, not build.
    released: bool,
    stale: bool,
    release_at: Option<f64>,
    /// Whether a `released` edge, not the hold ceiling, has been acted on.
    settled: bool,
    /// Re-based by a late settle.
    landed_at: Option<f64>,
    end_at: Option<f64>,
    elapsed_ms: f64,
    last_frame: Option<FrameTime>,
    fired: bool,
    done: RwSignal<bool>,
}

impl<State: 'static, R: View<State>, A: View<State>> View<State> for LoadingView<R, A> {
    type Element = LoadingWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> LoadingWidget {
        LoadingWidget {
            rain: build_child(&self.rain, ctx),
            assembly: build_child(&self.assembly, ctx),
            cell: Rc::clone(&self.cell),
            piece_count: self.piece_count,
            tone: self.tone,
            released: self.released,
            stale: self.stale,
            release_at: None,
            settled: false,
            landed_at: None,
            end_at: None,
            elapsed_ms: 0.0,
            last_frame: None,
            fired: false,
            done: self.done,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut LoadingWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.released = self.released;
        element.stale = self.stale;
        element.piece_count = self.piece_count;
        element.tone = self.tone;
        element.done = self.done;
        // Every rebuild mints a fresh cell for the rain pod, so the writer adopts the same one.
        element.cell = Rc::clone(&self.cell);
        let rain = rebuild_child(&prev.rain, &self.rain, &mut element.rain, ctx);
        let assembly = rebuild_child(&prev.assembly, &self.assembly, &mut element.assembly, ctx);
        // Neither moves a child, yet both are read at paint.
        let repaint = if self.released == prev.released && self.tone == prev.tone {
            ChangeFlags::NONE
        } else {
            ChangeFlags::PAINT
        };
        rain | assembly | repaint
    }

    fn teardown(&self, element: &mut LoadingWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.rain, &mut element.rain, ctx);
        teardown_child(&self.assembly, &mut element.assembly, ctx);
    }
}

impl LoadingWidget {
    /// Pointers stop here only while this picture is not the band's own.
    fn blocking(&self) -> bool {
        !self.fired && (self.stale || self.landed_at.is_none_or(|landed| self.elapsed_ms < landed))
    }

    fn stops(&self, event: &InputEvent) -> bool {
        match event {
            // This picture has no scroll offset to match, so scroll stops here until done.
            InputEvent::Scroll { .. } => !self.fired,
            InputEvent::Pointer(_) => self.blocking(),
            _ => false,
        }
    }

    fn schedule(&mut self, from_ms: f64, open_ms: f64) {
        self.end_at = Some(sequence_end_ms(from_ms));
        self.landed_at = Some(assembly_landed_ms(open_ms, self.piece_count));
    }
}

impl Widget for LoadingWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        self.rain.layout_child(ctx, bc);
        self.rain.set_origin(Point::ZERO);
        // Laid out from the first frame so the pieces match the live band when the assembly opens.
        let assembly = self.assembly.layout_child(ctx, bc);
        self.assembly.set_origin(Point::ZERO);
        fill_size(bc, assembly)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        if reduce_motion(Theme::from_paint_ctx(ctx)) {
            // Reduced motion paints nothing and only writes the end.
            if !self.fired {
                self.fired = true;
                self.done.set(true);
                ctx.request_frame();
            }
            return;
        }

        let was_fired = self.fired;
        self.elapsed_ms += delta_ms(&mut self.last_frame, ctx.frame_time());

        match self.release_at {
            None if self.released => {
                let release = release_instant(self.elapsed_ms, spec::LOADING_MIN_DENSE_MS);
                self.release_at = Some(release);
                self.settled = true;
                self.schedule(release, assembly_open_ms(release));
            }
            None if self.elapsed_ms >= auto_release_ms() => {
                self.release_at = Some(auto_release_ms());
                self.schedule(auto_release_ms(), assembly_open_ms(auto_release_ms()));
            }
            // Settled inside the tail: the tail re-bases onto this frame, the pieces onto the frame
            // they first paint on, never earlier than the open gate.
            Some(release) if self.released && !self.settled => {
                self.settled = true;
                let open = self.elapsed_ms.max(assembly_open_ms(release));
                self.schedule(self.elapsed_ms, open);
            }
            _ => {}
        }

        scene.fill_rect(ctx.origin(), ctx.size(), self.tone);
        self.cell
            .set(rain_local_ms(self.elapsed_ms, self.release_at));
        self.rain.paint_child(ctx, scene);

        if self
            .release_at
            .is_some_and(|at| self.elapsed_ms >= assembly_open_ms(at))
        {
            // Clipped like the live scroller beneath: no painting past the band.
            scene.push_clip(ctx.origin(), ctx.size());
            self.assembly.paint_child(ctx, scene);
            scene.pop_clip();
        }
        if !self.fired && self.end_at.is_some_and(|end| self.elapsed_ms >= end) {
            self.fired = true;
            self.done.set(true);
        }

        // One frame on the firing frame too, so the write is acted on outside a paint pass.
        if !was_fired {
            ctx.request_frame();
        }
    }

    fn event(&mut self, _ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if self.stops(event) {
            return EventResult::Handled;
        }
        EventResult::Ignored
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx) {
        // Does not forward: the live band underneath already contributes these nodes.
    }
}

#[cfg(test)]
mod tests {
    use frust::authoring::{PointerButton, PointerEvent, PointerPhase, ScrollDelta};
    use frust::{SizedBox, SizedBoxView};

    use super::*;

    /// Late enough that the held clock has already reached the dense boundary.
    const LATE_RELEASE: f64 = 2_000.0;

    fn inert_view(
        released: bool,
        tone: Color,
        stale: bool,
    ) -> LoadingView<SizedBoxView<()>, SizedBoxView<()>> {
        LoadingView {
            rain: SizedBox::<()>(None, None),
            assembly: SizedBox::<()>(None, None),
            cell: Rc::new(Cell::new(0.0)),
            piece_count: 1,
            tone,
            released,
            stale,
            done: RwSignal::new(false),
        }
    }

    #[test]
    fn the_clock_holds_at_the_dense_boundary_while_loading() {
        assert_eq!(rain_local_ms(0.0, None), 0.0);
        assert_eq!(rain_local_ms(500.0, None), 500.0);
        assert_eq!(rain_local_ms(spec::DENSE_END_MS, None), spec::DENSE_END_MS);
        assert_eq!(rain_local_ms(5_000.0, None), spec::DENSE_END_MS);
        assert_eq!(rain_local_ms(60_000.0, None), spec::DENSE_END_MS);
    }

    #[test]
    fn the_clock_is_continuous_across_a_release() {
        let just_before = rain_local_ms(LATE_RELEASE - 1e-9, Some(LATE_RELEASE));
        let at_release = rain_local_ms(LATE_RELEASE, Some(LATE_RELEASE));
        assert_eq!(at_release, spec::DENSE_END_MS);
        assert!(
            (at_release - just_before).abs() < 1e-6,
            "the held clock and the resumed clock must meet: {just_before} vs {at_release}"
        );
        assert_eq!(
            rain_local_ms(LATE_RELEASE + 400.0, Some(LATE_RELEASE)),
            spec::DENSE_END_MS + 400.0
        );
    }

    #[test]
    fn the_clock_never_runs_backwards() {
        let release = release_instant(120.0, spec::LOADING_MIN_DENSE_MS);
        let mut previous = f64::NEG_INFINITY;
        for step in 0..400 {
            let now = rain_local_ms(f64::from(step) * 25.0, Some(release));
            assert!(
                now >= previous,
                "went backwards at {step}: {previous} → {now}"
            );
            previous = now;
        }
    }

    #[test]
    fn an_instant_load_still_gets_its_minimum_dense_beat() {
        assert_eq!(release_instant(0.0, 600.0), 600.0);
        assert_eq!(release_instant(599.0, 600.0), 600.0);
        assert_eq!(release_instant(900.0, 600.0), 900.0);
        assert_eq!(release_instant(30_000.0, 600.0), 30_000.0);
    }

    #[test]
    fn the_assembly_opens_one_offset_after_the_release() {
        assert_eq!(assembly_open_ms(0.0), spec::LOADING_ASSEMBLY_OFFSET_MS);
        assert_eq!(assembly_open_ms(LATE_RELEASE), LATE_RELEASE + 100.0);
    }

    #[test]
    fn the_sequence_ends_eighteen_hundred_after_the_release() {
        assert_eq!(sequence_end_ms(0.0), 1_800.0);
        assert_eq!(sequence_end_ms(LATE_RELEASE), LATE_RELEASE + 1_800.0);
    }

    #[test]
    fn the_sequence_outlasts_the_last_piece_and_the_rain() {
        let last_landing = assembly_open_ms(LATE_RELEASE)
            + spec::piece_delay_ms(spec::PIECE_DELAYS_MS.len() - 1)
            + spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS);
        assert!(
            sequence_end_ms(LATE_RELEASE) >= last_landing,
            "unmounting must not cut the last piece's landing short"
        );
        let rain_end = LATE_RELEASE + spec::THIN_END_MS + spec::RAIN_TAIL_MS - spec::DENSE_END_MS;
        assert!(
            sequence_end_ms(LATE_RELEASE) >= rain_end,
            "unmounting must not pop the still-draining columns out of existence"
        );
    }

    #[test]
    fn the_hold_has_a_deadline_of_its_own() {
        assert_eq!(auto_release_ms(), spec::LOADING_MAX_HOLD_MS);
        assert!(
            auto_release_ms() > spec::LOADING_MIN_DENSE_MS,
            "the ceiling must sit above the floor"
        );
    }

    #[test]
    fn the_assembly_lands_one_entrance_after_its_last_piece_starts() {
        let open = assembly_open_ms(LATE_RELEASE);
        let entrance = spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS);
        assert_eq!(assembly_landed_ms(open, 0), open + entrance);
        assert_eq!(assembly_landed_ms(open, 1), open + entrance);
        assert_eq!(
            assembly_landed_ms(open, 3),
            open + spec::PIECE_DELAYS_MS[2] + entrance
        );
        let last = spec::PIECE_DELAYS_MS[spec::PIECE_DELAYS_MS.len() - 1];
        assert_eq!(assembly_landed_ms(open, 20), open + last + entrance);
        assert!(
            assembly_landed_ms(open, 20) <= sequence_end_ms(LATE_RELEASE),
            "the block must lift no later than the unmount that would lift it anyway"
        );
    }

    fn wheel() -> InputEvent {
        InputEvent::Scroll {
            position: Point::ZERO,
            delta: ScrollDelta::Lines(0.0, -3.0),
        }
    }

    fn tap() -> InputEvent {
        InputEvent::Pointer(PointerEvent {
            phase: PointerPhase::Down,
            position: Point::ZERO,
            button: PointerButton::Primary,
        })
    }

    #[test]
    fn the_pointer_stops_here_only_until_the_assembly_has_landed() {
        let mut next_id = 0;
        let mut ctx = BuildCtx::new(&mut next_id);
        let mut widget = inert_view(false, spec::AMBER_FALLBACK, false).build(&mut ctx);
        assert!(
            widget.blocking(),
            "a band still loading is behind a picture of itself"
        );

        widget.landed_at = Some(1_000.0);
        widget.elapsed_ms = 999.0;
        assert!(widget.blocking(), "a piece is still dropping");
        widget.elapsed_ms = 1_000.0;
        assert!(
            !widget.blocking(),
            "from the landing the two pictures are the same one"
        );

        widget.fired = true;
        widget.landed_at = None;
        widget.elapsed_ms = 0.0;
        assert!(
            !widget.blocking(),
            "a fired overlay blocks nothing, reduced motion included"
        );
    }

    #[test]
    fn scroll_stops_here_for_the_overlays_whole_life() {
        let mut next_id = 0;
        let mut ctx = BuildCtx::new(&mut next_id);
        let mut widget = inert_view(false, spec::AMBER_FALLBACK, false).build(&mut ctx);
        widget.landed_at = Some(1_000.0);
        widget.elapsed_ms = 5_000.0;
        assert!(
            !widget.stops(&tap()),
            "a landed assembly is the band's own picture: a tap falls through"
        );
        assert!(
            widget.stops(&wheel()),
            "the band beneath scrolls; a picture of it at offset zero does not"
        );

        widget.fired = true;
        assert!(
            !widget.stops(&wheel()),
            "a fired overlay stops nothing, the wheel included"
        );
    }

    #[test]
    fn a_stale_picture_keeps_blocking_past_the_landing() {
        let mut next_id = 0;
        let mut ctx = BuildCtx::new(&mut next_id);
        let mut widget = inert_view(false, spec::AMBER_FALLBACK, true).build(&mut ctx);
        widget.landed_at = Some(1_000.0);
        widget.elapsed_ms = 1_000.0;
        assert!(
            widget.blocking(),
            "a stale picture blocks even once its own pieces have landed"
        );

        widget.elapsed_ms = 5_000.0;
        assert!(
            widget.blocking(),
            "staleness alone keeps blocking, however late"
        );

        widget.fired = true;
        assert!(
            !widget.blocking(),
            "a fired overlay blocks nothing, stale or not"
        );
    }

    #[test]
    fn a_release_or_a_tone_change_repaints_though_no_child_moved() {
        let mut next_id = 0;
        let mut ctx = BuildCtx::new(&mut next_id);
        let held = inert_view(false, spec::AMBER_FALLBACK, false);
        let mut element = held.build(&mut ctx);
        assert!(
            !held.rebuild(&held, &mut element, &mut ctx).needs_paint(),
            "an unchanged rebuild owes no paint"
        );

        let released = inert_view(true, spec::AMBER_FALLBACK, false);
        assert!(
            released
                .rebuild(&held, &mut element, &mut ctx)
                .contains(ChangeFlags::PAINT),
            "the release is read at paint, so it must ask for one"
        );

        let retoned = inert_view(true, spec::ON_AMBER_FALLBACK, false);
        assert!(
            retoned
                .rebuild(&released, &mut element, &mut ctx)
                .contains(ChangeFlags::PAINT),
            "so is the tone this widget fills with"
        );
    }
}
