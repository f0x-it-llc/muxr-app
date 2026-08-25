//! The loading rain: the splash's rain→assembly beats re-clocked to an
//! external `released` edge, for a band whose content is still loading.
//!
//! [`sequence`](super::sequence) plays a cold-start schedule whose length is
//! known in advance; this plays the same picture over a wait nobody can time.
//!
//! | Rain-local clock | Beat |
//! |---|---|
//! | `0..`[`spec::DENSE_END_MS`] | dense, already mid-fall |
//! | held at [`spec::DENSE_END_MS`] | still loading — the *clock* stops here, not the rain |
//! | release `r` | the clock resumes from that hold; thinning starts |
//! | `r +` [`spec::LOADING_ASSEMBLY_OFFSET_MS`] | the first piece lands ([`assembly_open_ms`]) |
//! | `r + 1800` | rain tail and last piece both done ([`sequence_end_ms`]) |
//!
//! `r` is the observed settle instant, floored at
//! [`spec::LOADING_MIN_DENSE_MS`] and ceilinged by [`auto_release_ms`].
//!
//! Mount this as the **last** child of an always-present
//! [`Stack`](frust::Stack), the live band beneath it: the erased child type
//! must not flip when the overlay goes, or the pod underneath loses its
//! identity. Pointer events stop here (a tap must not reach an unseen card);
//! semantics do not forward (the band beneath already publishes them).

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

// ---------------------------------------------------------------------------
// The timeline, as pure functions of the release instant
// ---------------------------------------------------------------------------

/// The instant a release takes effect. The floor is the point: a band that
/// resolves from cache in 20ms would otherwise flash the rain for one frame.
pub fn release_instant(observed_ms: f64, min_dense_ms: f64) -> f64 {
    observed_ms.max(min_dense_ms)
}

/// The rain's own clock at `elapsed_ms` (`release_at` is `None` while the
/// band is still loading).
///
/// Before release the clock holds at [`spec::DENSE_END_MS`], where density
/// and fade are both still 1.0 — the rain reads as indefinitely dense rather
/// than frozen, since the columns fall on the frame delta, not this clock.
pub fn rain_local_ms(elapsed_ms: f64, release_at: Option<f64>) -> f64 {
    match release_at {
        None => elapsed_ms.min(spec::DENSE_END_MS),
        Some(release) if elapsed_ms < release => elapsed_ms.min(spec::DENSE_END_MS),
        Some(release) => spec::DENSE_END_MS + (elapsed_ms - release),
    }
}

/// When the assembly starts painting, on the widget's own clock.
pub fn assembly_open_ms(release_at: f64) -> f64 {
    release_at + spec::LOADING_ASSEMBLY_OFFSET_MS
}

/// When the whole sequence is over: the later of the rain's remaining tail
/// and the last piece's landing, both measured from the release.
pub fn sequence_end_ms(release_at: f64) -> f64 {
    let rain_tail = spec::THIN_END_MS + spec::RAIN_TAIL_MS - spec::DENSE_END_MS;
    let last_landing = spec::LOADING_ASSEMBLY_OFFSET_MS
        + spec::piece_delay_ms(spec::PIECE_DELAYS_MS.len() - 1)
        + spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS);
    release_at + rain_tail.max(last_landing)
}

/// The self-release deadline. Nothing below this widget times the wait out —
/// the gRPC layer sets no per-call deadline, only a transport keepalive — so
/// a first load that never settles would otherwise rain forever.
pub fn auto_release_ms() -> f64 {
    spec::LOADING_MAX_HOLD_MS
}

/// The loading rain over `pieces`, released by `released`.
///
/// - `seed` — fixes the whole rain field, so a test replays it exactly.
/// - `tone` — the opaque fill behind the rain: the band's own page tone.
/// - `pieces`/`assemble` — the loaded content in stagger order and its
///   layout, both the *same* builders the live band uses, or the handoff
///   crosses two different pictures.
/// - `released` — `false` while loading, `true` once settled; monotonic.
/// - `done` — set `true` exactly once, when the sequence is over.
pub fn loading_sequence<State: 'static>(
    seed: u64,
    tone: Color,
    pieces: Vec<AnyView<State>>,
    assemble: impl FnOnce(Vec<AnyView<State>>) -> AnyView<State>,
    released: bool,
    done: RwSignal<bool>,
) -> AnyView<State> {
    let cell = Rc::new(Cell::new(0.0));
    let staggered = pieces
        .into_iter()
        .enumerate()
        .map(|(index, piece)| any(piece_entry(spec::piece_delay_ms(index), piece)))
        .collect();
    any(LoadingView {
        rain: any(super::rain::rain::<State>(seed).clock_cell(Rc::clone(&cell))),
        assembly: assemble(staggered),
        cell,
        tone,
        released,
        done,
    })
}

/// The declarative half of the loading rain. See [`loading_sequence`].
pub struct LoadingView<State: 'static> {
    rain: AnyView<State>,
    assembly: AnyView<State>,
    /// The rain-local clock this widget writes and the rain pod reads.
    cell: Rc<Cell<f64>>,
    tone: Color,
    released: bool,
    done: RwSignal<bool>,
}

/// The retained half of the loading rain. See [`loading_sequence`].
pub struct LoadingWidget {
    rain: ChildPod,
    assembly: ChildPod,
    cell: Rc<Cell<f64>>,
    tone: Color,
    /// The latest `released` the view carried; acted on at paint, not build.
    released: bool,
    /// The instant the release took effect, once one has.
    release_at: Option<f64>,
    elapsed_ms: f64,
    last_frame: Option<FrameTime>,
    /// Set once, so a frame painted after the write doesn't write again.
    fired: bool,
    done: RwSignal<bool>,
}

impl<State: 'static> View<State> for LoadingView<State> {
    type Element = LoadingWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> LoadingWidget {
        LoadingWidget {
            rain: build_child(&self.rain, ctx),
            assembly: build_child(&self.assembly, ctx),
            cell: Rc::clone(&self.cell),
            tone: self.tone,
            released: self.released,
            release_at: None,
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
        element.tone = self.tone;
        element.done = self.done;
        // A per-frame channel, not retained state: every rebuild mints a fresh
        // cell for the rain pod, so the writer must adopt the same one.
        element.cell = Rc::clone(&self.cell);
        let rain = rebuild_child(&prev.rain, &self.rain, &mut element.rain, ctx);
        let assembly = rebuild_child(&prev.assembly, &self.assembly, &mut element.assembly, ctx);
        rain | assembly
    }

    fn teardown(&self, element: &mut LoadingWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.rain, &mut element.rain, ctx);
        teardown_child(&self.assembly, &mut element.assembly, ctx);
    }
}

impl Widget for LoadingWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        self.rain.layout_child(ctx, bc);
        self.rain.set_origin(Point::ZERO);
        // Laid out from the first frame even while unpainted, so the pieces'
        // geometry matches the live band's by the time the assembly opens.
        let assembly = self.assembly.layout_child(ctx, bc);
        self.assembly.set_origin(Point::ZERO);
        fill_size(bc, assembly)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        if reduce_motion(Theme::from_paint_ctx(ctx)) {
            // No motion means no timeline: end on the first frame.
            if !self.fired {
                self.fired = true;
                self.done.set(true);
            }
            return;
        }

        let was_fired = self.fired;
        self.elapsed_ms += delta_ms(&mut self.last_frame, ctx.frame_time());

        if self.release_at.is_none() {
            if self.released {
                self.release_at =
                    Some(release_instant(self.elapsed_ms, spec::LOADING_MIN_DENSE_MS));
            } else if self.elapsed_ms >= auto_release_ms() {
                self.release_at = Some(auto_release_ms());
            }
        }

        scene.fill_rect(ctx.origin(), ctx.size(), self.tone);
        self.cell
            .set(rain_local_ms(self.elapsed_ms, self.release_at));
        self.rain.paint_child(ctx, scene);

        if let Some(release) = self.release_at {
            // Withheld rather than laid out late: the pieces self-clock from
            // this first paint.
            if self.elapsed_ms >= assembly_open_ms(release) {
                self.assembly.paint_child(ctx, scene);
            }
            if self.elapsed_ms >= sequence_end_ms(release) && !self.fired {
                self.fired = true;
                self.done.set(true);
            }
        }

        // One more frame while waiting, and one on the firing frame so the
        // write is acted on from a frame rather than inside a paint pass.
        if !was_fired {
            ctx.request_frame();
        }
    }

    fn event(&mut self, _ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // Pointer events only: a key/IME event is focus-routed, never
        // hit-tested, so it cannot reach an overlay that never takes focus.
        if matches!(event, InputEvent::Pointer(_)) {
            return EventResult::Handled;
        }
        EventResult::Ignored
    }

    fn semantics(&self, _ctx: &mut SemanticsCtx) {
        // Deliberately does not forward: the live band underneath already
        // contributes these nodes, and this overlay is a picture of them.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Late enough that the held clock has already reached the dense
    /// boundary — the case the continuity claim is exact for.
    const LATE_RELEASE: f64 = 2_000.0;

    #[test]
    fn the_clock_holds_at_the_dense_boundary_while_loading() {
        assert_eq!(rain_local_ms(0.0, None), 0.0);
        assert_eq!(rain_local_ms(500.0, None), 500.0);
        assert_eq!(rain_local_ms(spec::DENSE_END_MS, None), spec::DENSE_END_MS);
        // However long the load takes, the clock never leaves the boundary.
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
        // And it resumes at real time from there, never restarting.
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
        // A load slower than the floor releases when it actually settled.
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
        // The rain's own tail is the longer arm today; this pins the
        // arithmetic so a constant change shows up here.
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
}
