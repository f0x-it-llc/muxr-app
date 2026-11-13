//! The cold-start splash orchestrator: icon, burst and the Connect screen's piece-by-piece
//! assembly over one master clock, played once per process. The assembly lays out the route's
//! own pieces through the route's own layout, so the pixels beneath match when it unmounts.

use std::sync::atomic::{AtomicBool, Ordering};

use frust::authoring::{
    Affine, AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx,
    PaintScene, Point, SemanticsCtx, Size, View, Widget, any, build_child, rebuild_child,
    teardown_child,
};
use frust::{
    Align, Alignment, FrameTime, Image, ImageFit, RwSignal, Set, SizedBox, Stack, Theme, stack,
};

use super::icon_phase::{IconArt, icon_phase};
use super::{accent_ink, delta_ms, reduce_motion, spec, with_alpha};
use crate::ui::backdrop::page_backdrop;

pub const ROOT_ROUTE: &str = "/";

/// Process-lifetime by design: a warm navigation back to `/` never replays the launch animation.
static COLD_START_RESOLVED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashPlan {
    Full,
    Reduced,
    Skip,
}

/// The splash assembles the Connect screen, so any other start route has nothing to hand off to.
pub fn splash_plan(already_played: bool, start_route: &str, reduce_motion: bool) -> SplashPlan {
    if already_played || start_route != ROOT_ROUTE {
        SplashPlan::Skip
    } else if reduce_motion {
        SplashPlan::Reduced
    } else {
        SplashPlan::Full
    }
}

/// Always claims `flag`, even for [`SplashPlan::Skip`]: a cold start on another route spends
/// the launch too, so a later navigation to `/` is warm.
fn claim(flag: &AtomicBool, start_route: &str, reduce_motion: bool) -> SplashPlan {
    let already_played = flag.swap(true, Ordering::SeqCst);
    splash_plan(already_played, start_route, reduce_motion)
}

/// Call once from the composition root's first `build`, not `Component::init`: at `init` the
/// shell has not yet applied the theme.
pub fn claim_cold_start(start_route: &str, reduce_motion: bool) -> SplashPlan {
    let plan = claim(&COLD_START_RESOLVED, start_route, reduce_motion);
    log::debug!("cold-start splash: {plan:?} (route {start_route}, reduce_motion {reduce_motion})");
    plan
}

pub fn dissolve_cue_ms() -> f64 {
    spec::ICON_DELAY_MS
}

pub fn assembly_start_ms() -> f64 {
    dissolve_cue_ms()
}

pub fn assembly_end_ms() -> f64 {
    let last = spec::piece_delay_ms(spec::PIECE_DELAYS_MS.len() - 1);
    assembly_start_ms() + last + spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS)
}

pub fn burst_end_ms() -> f64 {
    dissolve_cue_ms() + spec::BURST_MS
}

pub fn sequence_end_ms() -> f64 {
    assembly_end_ms().max(burst_end_ms())
}

/// Decode `art` once and clone it in; this runs on every rebuild. `pieces` and `assemble` must
/// be the route's own builders, or the handoff crosses two different pictures.
pub fn splash_sequence<State: 'static>(
    art: IconArt,
    seed: u64,
    pieces: Vec<AnyView<State>>,
    assemble: impl FnOnce(Vec<AnyView<State>>) -> AnyView<State>,
    done: RwSignal<bool>,
) -> impl View<State> {
    let staggered = pieces
        .into_iter()
        .enumerate()
        .map(|(index, piece)| any(piece_entry(spec::piece_delay_ms(index), piece))) // erasure: keep accumulator into Vec<AnyView>
        .collect();
    let assembly = assemble(staggered);

    // Bottom-to-top, matching the source's z-index ladder (ui-layer 5, burst 8, icon 10).
    stack()
        .child(page_backdrop::<State>().block_input(true))
        // The end cue rides the assembly's gate, so a frame gap straddling it delays the unmount
        // rather than cutting a piece short; valid while the dissolve and assembly start coincide.
        .child(delayed(
            assembly_start_ms(),
            stack()
                .child(assembly)
                .child(cue::<State>(sequence_end_ms() - assembly_start_ms(), done)),
        ))
        .child(delayed(
            dissolve_cue_ms(),
            super::burst::burst::<State>(seed),
        ))
        .child(icon_phase::<State>(art))
}

/// Mounted instead of [`splash_sequence`] under reduced motion; a vector `art` shows the
/// backdrop alone. Its cue ignores reduced motion, or the hold would end on its first frame.
pub fn reduced_splash<State: 'static>(art: IconArt, done: RwSignal<bool>) -> impl View<State> {
    let mut layers: Vec<AnyView<State>> = vec![any(page_backdrop::<State>().block_input(true))]; // erasure: keep accumulator into Vec<AnyView>
    if let IconArt::Image(source) = art {
        layers.push(any(Align(
            Alignment::CENTER,
            SizedBox::<State>(Some(spec::ICON_SIZE), Some(spec::ICON_SIZE))
                .child(Image(source).fit(ImageFit::Contain)),
        )));
    }
    layers.push(any(
        cue::<State>(spec::REDUCED_HOLD_MS, done).honor_reduce_motion(false)
    ));
    Stack(layers)
}

/// Reduced motion opens a gate immediately: no motion means no timing either.
pub fn delayed_showing(elapsed_ms: f64, delay_ms: f64, reduce_motion: bool) -> bool {
    reduce_motion || elapsed_ms >= delay_ms
}

/// Lays `child` out from the first frame but withholds paint until `delay_ms`, so geometry
/// stays stable and the child's own clock starts at the cue.
pub fn delayed<V>(delay_ms: f64, child: V) -> DelayedView<V> {
    DelayedView { delay_ms, child }
}

pub struct DelayedView<V> {
    delay_ms: f64,
    child: V,
}

pub struct DelayedWidget {
    delay_ms: f64,
    child: ChildPod,
    elapsed_ms: f64,
    last_frame: Option<FrameTime>,
}

impl<State: 'static, V: View<State>> View<State> for DelayedView<V> {
    type Element = DelayedWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> DelayedWidget {
        DelayedWidget {
            delay_ms: self.delay_ms,
            child: build_child(&self.child, ctx),
            elapsed_ms: 0.0,
            last_frame: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut DelayedWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.delay_ms = self.delay_ms;
        rebuild_child(&prev.child, &self.child, &mut element.child, ctx)
    }

    fn teardown(&self, element: &mut DelayedWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for DelayedWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let reduce = reduce_motion(Theme::from_paint_ctx(ctx));
        self.elapsed_ms += delta_ms(&mut self.last_frame, ctx.frame_time());
        if delayed_showing(self.elapsed_ms, self.delay_ms, reduce) {
            self.child.paint_child(ctx, scene);
        } else {
            // Closed: keep frames coming or the gate's clock stops.
            ctx.request_frame();
        }
    }

    // No `event` impl: the overlay is non-interactive and its backdrop swallows input.

    fn semantics(&self, _ctx: &mut SemanticsCtx) {
        // Does not forward: the route underneath already publishes the Connect screen's nodes.
    }
}

/// A zero-size leaf that sets `signal` once, `after_ms` into its own clock.
pub fn cue<State: 'static>(after_ms: f64, signal: RwSignal<bool>) -> CueView<State> {
    CueView {
        after_ms,
        signal,
        honor_reduce_motion: true,
        state: std::marker::PhantomData,
    }
}

/// A cue honoring reduced motion ends a timeline on its first frame; one that does not
/// measures a hold and always waits its window out.
pub fn cue_due(
    elapsed_ms: f64,
    after_ms: f64,
    reduce_motion: bool,
    honors_reduce_motion: bool,
) -> bool {
    (honors_reduce_motion && reduce_motion) || elapsed_ms >= after_ms
}

pub struct CueView<State: 'static> {
    after_ms: f64,
    signal: RwSignal<bool>,
    honor_reduce_motion: bool,
    state: std::marker::PhantomData<fn() -> State>,
}

impl<State: 'static> CueView<State> {
    /// Default `true`; [`reduced_splash`]'s hold passes `false`, or it ends as it starts.
    pub fn honor_reduce_motion(mut self, honor: bool) -> Self {
        self.honor_reduce_motion = honor;
        self
    }
}

pub struct CueWidget {
    after_ms: f64,
    signal: RwSignal<bool>,
    honor_reduce_motion: bool,
    elapsed_ms: f64,
    last_frame: Option<FrameTime>,
    /// Set once, so a frame painted after the write does not write again.
    fired: bool,
}

impl<State: 'static> View<State> for CueView<State> {
    type Element = CueWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> CueWidget {
        CueWidget {
            after_ms: self.after_ms,
            signal: self.signal,
            honor_reduce_motion: self.honor_reduce_motion,
            elapsed_ms: 0.0,
            last_frame: None,
            fired: false,
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        element: &mut CueWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.after_ms = self.after_ms;
        element.signal = self.signal;
        element.honor_reduce_motion = self.honor_reduce_motion;
        ChangeFlags::NONE
    }
}

impl Widget for CueWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::ZERO)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        if self.fired {
            return;
        }
        let reduce = reduce_motion(Theme::from_paint_ctx(ctx));
        self.elapsed_ms += delta_ms(&mut self.last_frame, ctx.frame_time());
        if cue_due(
            self.elapsed_ms,
            self.after_ms,
            reduce,
            self.honor_reduce_motion,
        ) {
            self.fired = true;
            self.signal.set(true);
        }
        // One more frame either way: it advances the clock, and on the firing frame acts on the
        // write outside a paint pass.
        ctx.request_frame();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PieceMotion {
    pub dy: f64,
    pub rotation: f64,
    pub scale: f64,
    pub opacity: f64,
    /// The land-flash envelope, peaking at [`spec::LAND_FLASH_PEAK`] through the flash.
    pub glow: f64,
}

/// The landed terminal state: exactly what the route's own page paints.
const LANDED: PieceMotion = PieceMotion {
    dy: 0.0,
    rotation: 0.0,
    scale: 1.0,
    opacity: 1.0,
    glow: 0.0,
};

/// One spring drives offset, rotation, scale and (clamped) opacity; the flash rides its own
/// envelope over the same window.
pub fn piece_motion(elapsed_ms: f64, delay_ms: f64, reduce_motion: bool) -> PieceMotion {
    if reduce_motion {
        return LANDED;
    }
    let t = elapsed_ms - delay_ms;
    if t < 0.0 {
        return PieceMotion {
            dy: spec::PIECE_ENTER_DY,
            rotation: spec::PIECE_ENTER_ROTATION,
            scale: spec::PIECE_ENTER_SCALE,
            opacity: 0.0,
            glow: 0.0,
        };
    }
    if piece_finished(elapsed_ms, delay_ms) {
        return LANDED;
    }

    let p = spec::SPRING.transform((t / spec::PIECE_IN_MS).clamp(0.0, 1.0));
    PieceMotion {
        dy: spec::PIECE_ENTER_DY * (1.0 - p),
        rotation: spec::PIECE_ENTER_ROTATION * (1.0 - p),
        scale: spec::PIECE_ENTER_SCALE + (1.0 - spec::PIECE_ENTER_SCALE) * p,
        opacity: p.clamp(0.0, 1.0),
        glow: flash_envelope(t),
    }
}

fn flash_envelope(t: f64) -> f64 {
    if !(0.0..spec::LAND_FLASH_MS).contains(&t) {
        return 0.0;
    }
    let eased = spec::LAND_FLASH_EASE.transform(t / spec::LAND_FLASH_MS);
    if eased < spec::LAND_FLASH_PEAK {
        eased / spec::LAND_FLASH_PEAK
    } else {
        ((1.0 - eased) / (1.0 - spec::LAND_FLASH_PEAK)).max(0.0)
    }
}

pub fn piece_finished(elapsed_ms: f64, delay_ms: f64) -> bool {
    elapsed_ms - delay_ms >= spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS)
}

/// Paint-time only: layout passes through, so a wrapped piece occupies its unwrapped twin's box.
pub fn piece_entry<V>(delay_ms: f64, child: V) -> PieceEntryView<V> {
    PieceEntryView { delay_ms, child }
}

pub struct PieceEntryView<V> {
    delay_ms: f64,
    child: V,
}

pub struct PieceEntryWidget {
    delay_ms: f64,
    child: ChildPod,
    /// Since this piece's first paint, i.e. since the assembly was cued.
    elapsed_ms: f64,
    last_frame: Option<FrameTime>,
}

impl PieceEntryWidget {
    /// `draw_shadow` approximates `box-shadow:0 0 24px 6px var(--amber-glow)`: spread grows the
    /// rect, blur is the std-dev, and the envelope scales both plus the alpha.
    fn paint_flash(&self, ctx: &PaintCtx, scene: &mut dyn PaintScene, glow: f64) {
        let spread = spec::LAND_FLASH_SPREAD * glow;
        let origin = ctx.origin();
        let size = ctx.size();
        scene.draw_shadow(
            Point::new(origin.x - spread, origin.y - spread),
            Size::new(size.width + 2.0 * spread, size.height + 2.0 * spread),
            spread,
            spec::LAND_FLASH_BLUR * glow,
            with_alpha(
                accent_ink(Theme::from_paint_ctx(ctx)),
                spec::AMBER_GLOW_ALPHA * glow,
            ),
        );
    }
}

impl<State: 'static, V: View<State>> View<State> for PieceEntryView<V> {
    type Element = PieceEntryWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> PieceEntryWidget {
        PieceEntryWidget {
            delay_ms: self.delay_ms,
            child: build_child(&self.child, ctx),
            elapsed_ms: 0.0,
            last_frame: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut PieceEntryWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.delay_ms = self.delay_ms;
        rebuild_child(&prev.child, &self.child, &mut element.child, ctx)
    }

    fn teardown(&self, element: &mut PieceEntryWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for PieceEntryWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let reduce = reduce_motion(Theme::from_paint_ctx(ctx));
        self.elapsed_ms += delta_ms(&mut self.last_frame, ctx.frame_time());
        let motion = piece_motion(self.elapsed_ms, self.delay_ms, reduce);

        if motion == LANDED {
            // Terminal: painted exactly as the route's own page, which the handoff rests on.
            self.child.paint_child(ctx, scene);
            return;
        }

        if motion.opacity <= 0.0 && motion.glow <= 0.0 {
            ctx.request_frame();
            return;
        }

        let origin = ctx.origin();
        let size = ctx.size();
        let pivot = Point::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
        scene.push_transform(
            Affine::translate(pivot.to_vec2())
                * Affine::translate((0.0, motion.dy))
                * Affine::rotate(motion.rotation)
                * Affine::scale(motion.scale)
                * Affine::translate(-pivot.to_vec2()),
        );
        if motion.glow > 0.0 {
            // Outside the opacity layer so the glow is not clipped to the piece's box.
            self.paint_flash(ctx, scene, motion.glow);
        }
        if motion.opacity > 0.0 {
            scene.push_layer(origin, size, motion.opacity as f32);
            self.child.paint_child(ctx, scene);
            scene.pop_layer();
        }
        scene.pop_transform();
        ctx.request_frame();
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // A transparent wrapper forwards; the duplicate-node problem is solved at `DelayedWidget`.
        self.child.semantics_child(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_cold_start_on_the_root_route_plays_the_full_sequence() {
        assert_eq!(splash_plan(false, "/", false), SplashPlan::Full);
    }

    #[test]
    fn reduced_motion_selects_the_static_fallback() {
        assert_eq!(splash_plan(false, "/", true), SplashPlan::Reduced);
    }

    #[test]
    fn a_second_launch_decision_in_the_same_process_never_replays() {
        assert_eq!(splash_plan(true, "/", false), SplashPlan::Skip);
        assert_eq!(splash_plan(true, "/", true), SplashPlan::Skip);
    }

    #[test]
    fn a_cold_start_on_a_deep_link_skips_the_splash_entirely() {
        for route in ["/sessions", "/terminal", "/settings", "/sessions/17", ""] {
            assert_eq!(
                splash_plan(false, route, false),
                SplashPlan::Skip,
                "route {route} has no assembled screen to hand off to"
            );
            assert_eq!(splash_plan(false, route, true), SplashPlan::Skip);
        }
    }

    #[test]
    fn the_claim_is_spent_once_per_process_whatever_the_outcome() {
        // A hermetic flag, independent of which test in the binary ran first.
        let flag = AtomicBool::new(false);
        assert_eq!(claim(&flag, "/", false), SplashPlan::Full);
        assert_eq!(claim(&flag, "/", false), SplashPlan::Skip);

        let deep_linked = AtomicBool::new(false);
        assert_eq!(claim(&deep_linked, "/sessions", false), SplashPlan::Skip);
        assert_eq!(claim(&deep_linked, "/", false), SplashPlan::Skip);
    }

    #[test]
    fn the_process_wide_claim_is_consumed_by_its_first_caller() {
        let _first = claim_cold_start(ROOT_ROUTE, false);
        assert_eq!(claim_cold_start(ROOT_ROUTE, false), SplashPlan::Skip);
    }

    #[test]
    fn the_timeline_matches_the_sources_own_arithmetic() {
        assert_eq!(dissolve_cue_ms(), 1_000.0);
        assert_eq!(assembly_start_ms(), 1_000.0);
        assert_eq!(assembly_end_ms(), 2_360.0);
        assert_eq!(burst_end_ms(), 1_600.0);
        assert_eq!(sequence_end_ms(), 2_360.0);
    }

    #[test]
    fn the_sequence_outlasts_both_the_assembly_and_the_burst() {
        let end = sequence_end_ms();
        assert!(end >= assembly_end_ms(), "the last piece must have landed");
        assert!(end >= burst_end_ms(), "every particle must have faded");
        assert_eq!(
            end,
            assembly_end_ms(),
            "the assembly is the later of the two"
        );
        let window = end - assembly_start_ms();
        assert_eq!(window, 1_360.0);
        assert_eq!(
            window,
            spec::piece_delay_ms(spec::PIECE_DELAYS_MS.len() - 1)
                + spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS),
            "the cue waits the last stagger plus the piece's own window"
        );
    }

    #[test]
    fn the_beats_are_ordered_with_the_assembly_riding_the_dissolve() {
        assert_eq!(dissolve_cue_ms(), assembly_start_ms());
        assert!(assembly_start_ms() < burst_end_ms());
        assert!(burst_end_ms() < assembly_end_ms());
        assert!(assembly_end_ms() <= sequence_end_ms());
    }

    #[test]
    fn the_piece_staggers_are_the_sources_data_delays_in_order() {
        assert_eq!(
            spec::PIECE_DELAYS_MS.to_vec(),
            vec![0.0, 130.0, 260.0, 390.0, 480.0, 580.0, 720.0, 860.0]
        );
        for pair in spec::PIECE_DELAYS_MS.windows(2) {
            assert!(pair[1] > pair[0], "staggers must increase: {pair:?}");
        }
        assert_eq!(spec::piece_delay_ms(0), 0.0);
        assert_eq!(spec::piece_delay_ms(7), 860.0);
        assert_eq!(spec::piece_delay_ms(99), 860.0);
    }

    #[test]
    fn a_gate_opens_exactly_at_its_delay() {
        assert!(!delayed_showing(0.0, 1_000.0, false));
        assert!(!delayed_showing(999.9, 1_000.0, false));
        assert!(delayed_showing(1_000.0, 1_000.0, false));
        assert!(delayed_showing(9_999.0, 1_000.0, false));
    }

    #[test]
    fn reduced_motion_opens_every_gate_immediately() {
        assert!(delayed_showing(0.0, 4_000.0, true));
    }

    #[test]
    fn a_cue_comes_due_exactly_at_its_window() {
        assert!(!cue_due(0.0, sequence_end_ms(), false, true));
        assert!(!cue_due(
            sequence_end_ms() - 0.1,
            sequence_end_ms(),
            false,
            true
        ));
        assert!(cue_due(sequence_end_ms(), sequence_end_ms(), false, true));
    }

    #[test]
    fn reduced_motion_ends_the_full_timeline_on_its_first_frame() {
        assert!(cue_due(0.0, sequence_end_ms(), true, true));
    }

    #[test]
    fn the_reduced_holds_own_cue_is_not_short_circuited_by_reduced_motion() {
        assert!(!cue_due(0.0, spec::REDUCED_HOLD_MS, true, false));
        assert!(!cue_due(
            spec::REDUCED_HOLD_MS - 0.1,
            spec::REDUCED_HOLD_MS,
            true,
            false
        ));
        assert!(cue_due(
            spec::REDUCED_HOLD_MS,
            spec::REDUCED_HOLD_MS,
            true,
            false
        ));
    }

    #[test]
    fn a_piece_waits_its_turn_invisible_and_off_position() {
        let m = piece_motion(0.0, 130.0, false);
        assert_eq!(m.opacity, 0.0);
        assert_eq!(m.glow, 0.0);
        assert_eq!(m.dy, spec::PIECE_ENTER_DY);
        assert_eq!(m.scale, spec::PIECE_ENTER_SCALE);
        assert_eq!(m.rotation, spec::PIECE_ENTER_ROTATION);
    }

    #[test]
    fn a_piece_starts_its_drop_at_its_own_stagger() {
        let start = piece_motion(130.0, 130.0, false);
        assert!((start.dy - spec::PIECE_ENTER_DY).abs() < 1e-9);
        assert!((start.scale - spec::PIECE_ENTER_SCALE).abs() < 1e-9);
        assert_eq!(start.opacity, 0.0);
        let moving = piece_motion(180.0, 130.0, false);
        assert!(moving.dy > spec::PIECE_ENTER_DY);
        assert!(moving.opacity > 0.0);
    }

    #[test]
    fn the_drop_rides_a_spring_that_overshoots_before_settling() {
        let overshot = (1..=9)
            .map(|i| piece_motion(spec::PIECE_IN_MS * f64::from(i) / 10.0, 0.0, false).scale)
            .any(|scale| scale > 1.0);
        assert!(
            overshot,
            "the entrance must overshoot, not ease flat into 1"
        );
    }

    #[test]
    fn opacity_stays_in_range_and_rotation_unwinds_to_zero() {
        let entry = spec::PIECE_ENTER_ROTATION.abs();
        let mut t = 0.0;
        while t <= spec::PIECE_IN_MS {
            let m = piece_motion(t, 0.0, false);
            assert!((0.0..=1.0).contains(&m.opacity), "at {t}ms: {}", m.opacity);
            assert!(
                m.rotation.abs() <= entry + 1e-9,
                "rotation left its range at {t}ms: {}",
                m.rotation
            );
            t += 10.0;
        }
        assert_eq!(piece_motion(spec::PIECE_IN_MS, 0.0, false), LANDED);
    }

    #[test]
    fn a_landed_piece_is_pinned_to_the_routes_own_appearance() {
        for t in [
            spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS),
            5_000.0,
            60_000.0,
        ] {
            assert_eq!(piece_motion(t, 0.0, false), LANDED, "at {t}ms");
        }
    }

    #[test]
    fn the_land_flash_swells_then_fades_to_nothing() {
        assert_eq!(flash_envelope(-1.0), 0.0);
        assert_eq!(flash_envelope(0.0), 0.0);
        let peak = flash_envelope(spec::LAND_FLASH_MS * spec::LAND_FLASH_PEAK);
        assert!(peak > 0.5, "the flash must actually swell, got {peak}");
        let late = flash_envelope(spec::LAND_FLASH_MS * 0.9);
        assert!(late > 0.0 && late < peak, "and fade back out, got {late}");
        assert_eq!(flash_envelope(spec::LAND_FLASH_MS), 0.0);
        assert_eq!(flash_envelope(spec::LAND_FLASH_MS + 100.0), 0.0);
        let mut t = 0.0;
        while t <= spec::LAND_FLASH_MS {
            assert!((0.0..=1.0).contains(&flash_envelope(t)), "at {t}ms");
            t += 5.0;
        }
    }

    #[test]
    fn a_piece_finishes_after_the_longer_of_its_spring_and_its_flash() {
        let window = spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS);
        assert!(!piece_finished(129.0, 130.0));
        assert!(!piece_finished(130.0 + window - 1.0, 130.0));
        assert!(piece_finished(130.0 + window, 130.0));
    }

    #[test]
    fn reduced_motion_lands_every_piece_immediately() {
        for t in [0.0, 100.0, 5_000.0] {
            for delay in spec::PIECE_DELAYS_MS {
                assert_eq!(piece_motion(t, delay, true), LANDED);
            }
        }
    }

    #[test]
    fn the_whole_assembly_is_landed_by_the_sequences_end() {
        let assembly_window = sequence_end_ms() - assembly_start_ms();
        for (index, _) in spec::PIECE_DELAYS_MS.iter().enumerate() {
            let delay = spec::piece_delay_ms(index);
            assert!(
                piece_finished(assembly_window, delay),
                "piece {index} must have landed before the overlay unmounts"
            );
        }
    }
}
