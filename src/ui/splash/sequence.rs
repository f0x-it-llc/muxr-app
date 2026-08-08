//! The splash orchestrator: the icon, the burst, the rain and the Connect
//! screen's piece-by-piece assembly, sequenced over one clock and shown
//! exactly once per process launch.
//!
//! # The timeline
//!
//! Two clocks, as the source has: the **master** clock starts when the
//! overlay's first frame paints, and the **rain-local** clock
//! (`t = tOverall - ICON_DELAY`, the source's own line) starts when the icon
//! dissolves. Every value below is derived from [`spec`], never re-typed.
//!
//! | Master | Rain-local | Beat |
//! |---|---|---|
//! | 0 | — | The icon springs in ([`icon_phase`](super::icon_phase)) |
//! | 1000 ([`rain_cue_ms`]) | 0 | Icon dissolves; [`burst`](super::burst) fires and [`rain`](super::rain) starts, already mid-fall |
//! | 2400 | 1400 | The rain begins thinning column by column |
//! | 2500 ([`assembly_start_ms`]) | 1500 | The first piece drops in; the rest stagger 130…860ms behind it |
//! | 3860 ([`assembly_end_ms`]) | 2860 | The last piece has landed and its flash has faded |
//! | 4200 ([`sequence_end_ms`]) | 3200 | The rain's own tail ends — the overlay unmounts |
//!
//! # Why the end is the rain's, not the assembly's
//!
//! [`sequence_end_ms`] is the **later** of the two ends. Unmounting at the
//! assembly's own end (rain-local 2860) would cut the last ~340ms of the
//! rain's drain, popping the few columns still falling out of existence. Past
//! [`rain::rain_finished`](super::rain::rain_finished) the overlay paints
//! nothing but its backdrop and the landed pieces — a frame already identical
//! to the route underneath, which is what makes the handoff invisible.
//!
//! # The handoff
//!
//! The overlay sits in a [`Stack`] **above** the live navigator, which is
//! already rendering `/`. Its backdrop is opaque (and input-blocking) so
//! nothing leaks through or gets tapped mid-sequence; its assembly lays out
//! the very same [`connect_pieces`](crate::features::connect::presentation::pieces::connect_pieces)
//! — same saved-server rows, same undimmed fields — through the very same
//! [`connect_layout`](crate::features::connect::presentation::pages::connect_layout)
//! the route uses, width clamp included. The only difference is that each piece
//! is wrapped in [`piece_entry`], which is a **paint-time** transform and
//! changes no geometry. So when [`Cue`] flips the caller's `done` signal and the
//! shell drops the overlay, the pixels beneath are the ones that were already
//! there. That the two pictures actually coincide is the *pieces* module's
//! contract, not this one's — see its "parity contract" section for the three
//! divergences device gate G2 closed.
//!
//! Nothing here forwards input to the assembly either: [`DelayedWidget`] has no
//! `Widget::event` of its own, so no pointer ever reaches a piece, and the
//! backdrop under it swallows what is left. The assembled fields are
//! *additionally* read-only in their own right (`pieces`' item 2), so blocking
//! focus does not depend on this file's structure staying as it is.
//!
//! # Clocking, without a per-frame rebuild
//!
//! Nothing here re-times anything from a signal: [`Delayed`] gates its child
//! by its own accumulated clock and simply doesn't paint it until its window
//! opens, and every gated child (`rain`, `burst`, each [`piece_entry`])
//! self-clocks from *its own first paint* — which is the instant the gate
//! opened. That is why the rain needs no external
//! [`clock`](super::rain::RainView::clock) plumbing: mounting it is the cue,
//! and painting it is the mount. The only signal write in the whole sequence
//! is the one that ends it.
//!
//! The icon's own [`IconPhase`](super::icon_phase::IconPhase) publication is
//! deliberately **not** used as the cue here. Both it and this module's
//! schedule derive from the same [`spec::ICON_DELAY_MS`], so cueing off the
//! signal would add a rebuild per phase without changing a single frame's
//! timing; chain [`on_phase`](super::icon_phase::IconPhaseView::on_phase)
//! from outside if something ever needs to observe the beat.
//!
//! # Reduced motion
//!
//! [`splash_plan`] resolves to [`SplashPlan::Reduced`] and the shell mounts
//! [`reduced_splash`] instead: the static icon for
//! [`spec::REDUCED_HOLD_MS`], then straight to the finished screen — no rain,
//! no burst, no assembly (the source's own `if(reduced)` branch). Each widget
//! here re-checks the theme at paint time anyway, so a theme that turns
//! `reduce_motion` on *after* the mount decision still collapses to a
//! terminal state rather than animating.
//!
//! The one thing that must **not** collapse is that hold itself: it is only
//! ever mounted under `reduce_motion`, so a cue short-circuiting on the theme
//! would end it on its own first frame. [`Cue`] therefore takes the
//! short-circuit as an opt-out ([`CueView::honor_reduce_motion`]) rather than
//! applying it unconditionally.

use std::sync::atomic::{AtomicBool, Ordering};

use frust::authoring::{
    Affine, AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, LayoutCtx, PaintCtx,
    PaintScene, Point, SemanticsCtx, Size, View, Widget, any, build_child, rebuild_child,
    teardown_child,
};
use frust::{Align, Alignment, FrameTime, Image, ImageFit, RwSignal, Set, SizedBox, Stack, Theme};

use super::icon_phase::{IconArt, icon_phase};
use super::{accent_ink, delta_ms, reduce_motion, spec, with_alpha};
use crate::ui::backdrop::page_backdrop;

// ---------------------------------------------------------------------------
// Cold-start gating
// ---------------------------------------------------------------------------

/// The one route the splash plays over: muxr's start location.
pub const ROOT_ROUTE: &str = "/";

/// Whether this process has already resolved its cold-start splash.
///
/// Process-lifetime by design: the splash is a *launch* animation, so a warm
/// in-process navigation back to `/` must never replay it. Claimed once, from
/// [`claim_cold_start`], at the composition root's `init`.
static COLD_START_RESOLVED: AtomicBool = AtomicBool::new(false);

/// What the composition root should mount for this launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashPlan {
    /// The whole sequence: icon → burst + rain → assembly.
    Full,
    /// The reduced-motion fallback: a brief static icon, then the finished
    /// screen ([`reduced_splash`]).
    Reduced,
    /// Nothing at all — already played this process, or the app cold-started
    /// somewhere other than [`ROOT_ROUTE`].
    Skip,
}

/// The cold-start decision, as a pure function of its three inputs.
///
/// - `already_played` — has this process resolved its splash yet?
/// - `start_route` — the location the app is *starting* on (a future deep
///   link may not be `/`; the splash assembles the Connect screen, so it has
///   nothing to hand off to anywhere else).
/// - `reduce_motion` — the active theme's `MotionScheme::reduce_motion`.
pub fn splash_plan(already_played: bool, start_route: &str, reduce_motion: bool) -> SplashPlan {
    if already_played || start_route != ROOT_ROUTE {
        SplashPlan::Skip
    } else if reduce_motion {
        SplashPlan::Reduced
    } else {
        SplashPlan::Full
    }
}

/// [`splash_plan`] against `flag`, which this call **always** claims.
///
/// Claiming even when the plan is [`SplashPlan::Skip`] is the point: the
/// decision is taken exactly once per process, at launch, so a cold start on
/// a non-`/` route also spends the launch — a later navigation to `/` is a
/// warm one and must not play a launch animation.
fn claim(flag: &AtomicBool, start_route: &str, reduce_motion: bool) -> SplashPlan {
    let already_played = flag.swap(true, Ordering::SeqCst);
    splash_plan(already_played, start_route, reduce_motion)
}

/// Resolve (and consume) this process's one cold-start splash.
///
/// Call once, from the composition root's first `build` (via
/// `AppState::splash_plan()`) — not `Component::init`, since at `init` time
/// the shell has not yet applied the theme (see that function's doc for
/// why). Every later call returns [`SplashPlan::Skip`]. Logs the outcome at
/// debug level: "did the splash play, and why" is otherwise only answerable
/// by watching the screen.
pub fn claim_cold_start(start_route: &str, reduce_motion: bool) -> SplashPlan {
    let plan = claim(&COLD_START_RESOLVED, start_route, reduce_motion);
    log::debug!("cold-start splash: {plan:?} (route {start_route}, reduce_motion {reduce_motion})");
    plan
}

// ---------------------------------------------------------------------------
// The timeline, derived from `spec`
// ---------------------------------------------------------------------------

/// Master-clock instant the icon dissolves and the burst + rain are cued —
/// also the rain-local clock's zero.
pub fn rain_cue_ms() -> f64 {
    spec::ICON_DELAY_MS
}

/// Master-clock instant the assembly begins (its first piece has no stagger).
pub fn assembly_start_ms() -> f64 {
    rain_cue_ms() + spec::ASSEMBLY_START_MS
}

/// Master-clock instant the last piece has finished landing (its stagger,
/// plus the longer of the spring and the land flash).
pub fn assembly_end_ms() -> f64 {
    let last = spec::piece_delay_ms(spec::PIECE_DELAYS_MS.len() - 1);
    assembly_start_ms() + last + spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS)
}

/// Master-clock instant the rain stops painting entirely (see
/// [`rain_finished`](super::rain::rain_finished)).
pub fn rain_end_ms() -> f64 {
    rain_cue_ms() + spec::THIN_END_MS + spec::RAIN_TAIL_MS
}

/// Master-clock instant the whole sequence is over and the overlay unmounts:
/// the later of [`assembly_end_ms`] and [`rain_end_ms`] (see the module docs).
pub fn sequence_end_ms() -> f64 {
    assembly_end_ms().max(rain_end_ms())
}

// ---------------------------------------------------------------------------
// The two mountable overlays
// ---------------------------------------------------------------------------

/// The full cold-start sequence, as one mountable overlay.
///
/// - `art` — the decoded app icon. Decode it **once**
///   ([`splash_icon_art`](super::icon_phase::splash_icon_art), from the
///   composition root's `init`) and clone the handle in; this function runs
///   on every rebuild.
/// - `seed` — fixes the burst's particles and the rain's whole field.
/// - `pieces` — the screen to assemble, in stagger order. Build them with the
///   *same* builder the route uses, from the same saved-server list
///   (`connect_pieces` + `splash_saved_servers`), or the handoff below has two
///   different pictures to cross.
/// - `assemble` — lays those pieces out; hand it the *same* function the
///   route's page uses (see the module docs' handoff note). It owns the
///   responsive width clamp, so this sequence needs no window arithmetic of
///   its own.
/// - `done` — set `true` once, when the sequence is over.
pub fn splash_sequence<State: 'static>(
    art: IconArt,
    seed: u64,
    pieces: Vec<AnyView<State>>,
    assemble: impl FnOnce(Vec<AnyView<State>>) -> AnyView<State>,
    done: RwSignal<bool>,
) -> AnyView<State> {
    let staggered = pieces
        .into_iter()
        .enumerate()
        .map(|(index, piece)| any(piece_entry(spec::piece_delay_ms(index), piece)))
        .collect();
    let assembly = assemble(staggered);

    // Bottom-to-top, matching the source's z-index ladder: the canvas (1)
    // under the ui-layer (5), under the burst particles (8), under the icon
    // (10). The backdrop is below all of it, and the cue is a zero-size leaf.
    any(Stack(vec![
        any(page_backdrop::<State>().block_input(true)),
        any(delayed(rain_cue_ms(), super::rain::rain::<State>(seed))),
        any(delayed(assembly_start_ms(), assembly)),
        any(delayed(rain_cue_ms(), super::burst::burst::<State>(seed))),
        any(icon_phase::<State>(art)),
        any(cue::<State>(sequence_end_ms(), done)),
    ]))
}

/// The reduced-motion fallback: the static app icon over the page backdrop
/// for [`spec::REDUCED_HOLD_MS`], then the finished screen directly.
///
/// Mounted instead of [`splash_sequence`] when [`splash_plan`] resolves to
/// [`SplashPlan::Reduced`]. There is no rain, no burst and no assembly to
/// collapse — the source's reduced branch skips them outright, and mounting
/// them only to have each self-collapse would flash a frame of nothing.
///
/// [`IconArt::Vector`] (the decode-failure fallback) shows the backdrop
/// alone: the vector stand-in is a *motion* affordance of
/// [`icon_phase`](super::icon_phase), which is inert under reduced motion by
/// contract, and a 200ms glimpse does not justify a second copy of it here.
///
/// Its cue **opts out** of the reduced-motion short-circuit: this overlay is
/// only ever mounted when `reduce_motion` is on, so a cue honoring it would
/// fire on the first painted frame and unmount the hold ~16ms after it
/// appeared — a flicker where the design reference has a 200ms beat.
pub fn reduced_splash<State: 'static>(art: IconArt, done: RwSignal<bool>) -> AnyView<State> {
    let mut layers: Vec<AnyView<State>> = vec![any(page_backdrop::<State>().block_input(true))];
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
    any(Stack(layers))
}

// ---------------------------------------------------------------------------
// Delayed — the gate every cued beat hangs off
// ---------------------------------------------------------------------------

/// Whether a gate holding for `delay_ms` has opened at `elapsed_ms`.
///
/// `reduce_motion` opens it immediately: a gate is a timing device, and a
/// theme asking for no motion asks for no timing either.
pub fn delayed_showing(elapsed_ms: f64, delay_ms: f64, reduce_motion: bool) -> bool {
    reduce_motion || elapsed_ms >= delay_ms
}

/// Lay `child` out from the first frame but don't **paint** it until
/// `delay_ms` has elapsed on this widget's own clock.
///
/// Laying out throughout is what keeps the assembly's geometry stable (and
/// identical to the route's) across the whole sequence; withholding paint is
/// what starts the child's own clock at the cue, since every splash widget
/// self-clocks from its first paint.
pub fn delayed<State: 'static, V: View<State>>(delay_ms: f64, child: V) -> DelayedView<State> {
    DelayedView {
        delay_ms,
        child: any(child),
    }
}

/// The declarative half of a paint gate. See [`delayed`].
pub struct DelayedView<State: 'static> {
    delay_ms: f64,
    child: AnyView<State>,
}

/// The retained half of a paint gate. See [`delayed`].
pub struct DelayedWidget {
    delay_ms: f64,
    child: ChildPod,
    /// Milliseconds since this gate's first paint.
    elapsed_ms: f64,
    /// The previous frame's clock, for differencing.
    last_frame: Option<FrameTime>,
}

impl<State: 'static> View<State> for DelayedView<State> {
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
        // The delay is a constant of the timeline, re-passed identically every
        // rebuild; the clock is retained state and is never reset here.
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
            // Open: the child owns its own frame requests from here.
            self.child.paint_child(ctx, scene);
        } else {
            // Closed: nothing is painted, but the gate has to keep the frames
            // coming or its own clock stops advancing. A transition with a
            // visible endpoint, so the unpaced class is the right one.
            ctx.request_frame();
        }
    }

    // No `event` impl: the splash overlay is decorative and non-interactive
    // (`.ui-layer{pointer-events:none}`), and its backdrop swallows anything
    // that reaches it — see `crate::ui::backdrop`.

    fn semantics(&self, _ctx: &mut SemanticsCtx) {
        // Deliberately does NOT forward — the one place in this app that
        // breaks the forward-to-every-child rule, and the reason is the
        // handoff: the live route is mounted *underneath* this overlay and
        // already contributes the Connect screen's nodes, so forwarding would
        // publish every one of them twice for the sequence's four seconds.
        // The gated children are otherwise decorative (rain, burst).
    }
}

// ---------------------------------------------------------------------------
// Cue — the one signal write in the sequence
// ---------------------------------------------------------------------------

/// A zero-size leaf that sets `signal` to `true` once, `after_ms` into its own
/// clock — how the overlay tells the shell to unmount it.
///
/// Honors `reduce_motion` by default (see [`cue_due`]); a cue whose window
/// *is* the reduced-motion presentation opts out with
/// [`CueView::honor_reduce_motion`].
pub fn cue<State: 'static>(after_ms: f64, signal: RwSignal<bool>) -> CueView<State> {
    CueView {
        after_ms,
        signal,
        honor_reduce_motion: true,
        state: std::marker::PhantomData,
    }
}

/// Whether a cue waiting `after_ms` is due at `elapsed_ms`.
///
/// A cue that honors `reduce_motion` is due on its first painted frame: it
/// ends a *timeline*, and a theme asking for no motion asks for no timeline
/// either (the same rule [`delayed_showing`] applies to a gate). A cue that
/// does not honor it measures a **hold** rather than a timeline and always
/// waits its window out — see [`CueView::honor_reduce_motion`].
pub fn cue_due(
    elapsed_ms: f64,
    after_ms: f64,
    reduce_motion: bool,
    honors_reduce_motion: bool,
) -> bool {
    (honors_reduce_motion && reduce_motion) || elapsed_ms >= after_ms
}

/// The declarative half of the completion cue. See [`cue`].
pub struct CueView<State: 'static> {
    after_ms: f64,
    signal: RwSignal<bool>,
    honor_reduce_motion: bool,
    /// `View<State>` is generic over the app state even though this leaf never
    /// touches it.
    state: std::marker::PhantomData<fn() -> State>,
}

impl<State: 'static> CueView<State> {
    /// Whether `reduce_motion` short-circuits this cue to its first painted
    /// frame (default `true`).
    ///
    /// `true` is right for [`splash_sequence`]'s end cue: it is defense in
    /// depth behind a four-second timeline that a reduced-motion theme should
    /// never be held behind. It is *wrong* for [`reduced_splash`]'s hold,
    /// which is only ever mounted under `reduce_motion` in the first place —
    /// so pass `false` there, or the hold ends on the frame it starts.
    pub fn honor_reduce_motion(mut self, honor: bool) -> Self {
        self.honor_reduce_motion = honor;
        self
    }
}

/// The retained half of the completion cue. See [`cue`].
pub struct CueWidget {
    after_ms: f64,
    signal: RwSignal<bool>,
    /// See [`CueView::honor_reduce_motion`].
    honor_reduce_motion: bool,
    elapsed_ms: f64,
    last_frame: Option<FrameTime>,
    /// Set once, so a frame painted after the write (the shell may take one to
    /// react) doesn't write again.
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
        // Zero-size: it paints nothing and must never be hit-tested.
        bc.constrain(Size::ZERO)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        if self.fired {
            return;
        }
        // Reduced motion ends a *timeline* on its first frame — but not
        // `reduced_splash`'s hold, which opts out (see `cue_due`).
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
        // Either way, ask for one more frame: while waiting it is what keeps
        // this widget's own clock advancing, and on the firing frame it is the
        // frame that *acts* on the write — the sequence has just stopped every
        // other animation that could have driven one, so this does not lean on
        // the reactive waker scheduling a repaint from inside a paint pass. The
        // early return above makes it the last frame either way.
        ctx.request_frame();
    }
}

// ---------------------------------------------------------------------------
// PieceEntry — one piece's spring drop and land flash
// ---------------------------------------------------------------------------

/// A piece's resolved appearance at an instant — the whole `.piece` →
/// `.piece.land` transition as plain data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PieceMotion {
    /// Vertical offset from the piece's laid-out position, in logical px.
    pub dy: f64,
    /// Rotation about the piece's own center, in radians.
    pub rotation: f64,
    /// Uniform scale about the piece's own center.
    pub scale: f64,
    /// Layer opacity, `0..=1`.
    pub opacity: f64,
    /// The land-flash envelope, `0..=1` — `0` before the landing and once the
    /// flash has faded, peaking at [`spec::LAND_FLASH_PEAK`] through it.
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

/// A piece's appearance `elapsed_ms` into the assembly, given its own
/// `delay_ms` stagger.
///
/// One spring drives offset, rotation and scale together (the source's single
/// `transform` transition on `--ease-spring`) and, clamped, the opacity —
/// exactly as `.piece{transition:opacity .5s var(--ease-spring), transform
/// .5s var(--ease-spring)}` does. The flash rides its own `ease-out` envelope
/// over the same window.
///
/// `reduce_motion` returns the landed state at every instant: the piece
/// appears, finished, with no drop and no flash.
pub fn piece_motion(elapsed_ms: f64, delay_ms: f64, reduce_motion: bool) -> PieceMotion {
    if reduce_motion {
        return LANDED;
    }
    let t = elapsed_ms - delay_ms;
    if t < 0.0 {
        // Still waiting its turn: parked at the shard state, invisible.
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
        // The spring overshoots past 1, which is the point for the three
        // transform terms (they swing a hair past their target and settle)
        // and must be clamped for opacity.
        dy: spec::PIECE_ENTER_DY * (1.0 - p),
        rotation: spec::PIECE_ENTER_ROTATION * (1.0 - p),
        scale: spec::PIECE_ENTER_SCALE + (1.0 - spec::PIECE_ENTER_SCALE) * p,
        opacity: p.clamp(0.0, 1.0),
        glow: flash_envelope(t),
    }
}

/// The `landFlash` keyframe envelope at `t` ms after the landing began: up to
/// full by [`spec::LAND_FLASH_PEAK`] of the window, back to nothing by its
/// end, both legs along the animation's own `ease-out`.
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

/// Whether a piece staggered by `delay_ms` has fully landed (spring settled,
/// flash faded) at `elapsed_ms` — after which it paints exactly like the
/// route's own copy and stops asking for frames.
pub fn piece_finished(elapsed_ms: f64, delay_ms: f64) -> bool {
    elapsed_ms - delay_ms >= spec::PIECE_IN_MS.max(spec::LAND_FLASH_MS)
}

/// Wrap `child` in the assembly's entrance: it stays invisible for
/// `delay_ms`, then drops in on the spring with an amber land flash.
///
/// Paint-time only — layout is a straight pass-through, so a wrapped piece
/// occupies exactly the box its unwrapped twin does.
pub fn piece_entry<State: 'static, V: View<State>>(
    delay_ms: f64,
    child: V,
) -> PieceEntryView<State> {
    PieceEntryView {
        delay_ms,
        child: any(child),
    }
}

/// The declarative half of a piece's entrance. See [`piece_entry`].
pub struct PieceEntryView<State: 'static> {
    delay_ms: f64,
    child: AnyView<State>,
}

/// The retained half of a piece's entrance. See [`piece_entry`].
pub struct PieceEntryWidget {
    delay_ms: f64,
    child: ChildPod,
    /// Milliseconds since this piece's first paint — i.e. since the assembly
    /// was cued, since every piece is mounted and gated together.
    elapsed_ms: f64,
    /// The previous frame's clock, for differencing.
    last_frame: Option<FrameTime>,
}

impl PieceEntryWidget {
    /// Paint the amber land-flash glow around the piece's box.
    ///
    /// `PaintScene::draw_shadow` is a gaussian-blurred rounded rect — the
    /// framework's own `box-shadow` approximation, and the only blur
    /// affordance reachable from app code — so the source's
    /// `box-shadow:0 0 24px 6px var(--amber-glow)` maps onto it directly:
    /// the spread grows the rect, the blur radius is the standard deviation
    /// (`frust_theme`'s elevation table stores CSS blur px the same way), and
    /// the envelope scales all three plus the alpha.
    ///
    /// The glow's own corner radius is the spread (a piece's radius is its
    /// business, not the wrapper's) — at peak that is 6px on a shape already
    /// blurred by 24, which is well under the blur's own softening.
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

impl<State: 'static> View<State> for PieceEntryView<State> {
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
            // Terminal: no transform, no layer, no flash — byte-for-byte what
            // the route's own page paints, which is what the handoff rests on.
            self.child.paint_child(ctx, scene);
            return;
        }

        // Still waiting its turn: nothing painted, but keep the clock running.
        if motion.opacity <= 0.0 && motion.glow <= 0.0 {
            ctx.request_frame();
            return;
        }

        let origin = ctx.origin();
        let size = ctx.size();
        let pivot = Point::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
        // `transform: translateY(…) rotate(…) scale(…)` about the box's own
        // center — spelled out in that order so the composition is visible:
        // move the pivot to the origin, apply the three terms, move it back.
        scene.push_transform(
            Affine::translate(pivot.to_vec2())
                * Affine::translate((0.0, motion.dy))
                * Affine::rotate(motion.rotation)
                * Affine::scale(motion.scale)
                * Affine::translate(-pivot.to_vec2()),
        );
        if motion.glow > 0.0 {
            // Outside the opacity layer (below), so the glow isn't clipped to
            // the piece's own box — a box-shadow lives outside its element.
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

    // No `event` impl: `.ui-layer{pointer-events:none}` — the assembly is a
    // picture of the screen, not the screen.

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // A transparent wrapper forwards (the container rule). The overlay's
        // duplicate-node problem is solved one level up, at `DelayedWidget`.
        self.child.semantics_child(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- gating ----------------------------------------------------------

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
        // A hermetic flag, so this asserts the claim's *logic* without
        // depending on which test in the binary ran first.
        let flag = AtomicBool::new(false);
        assert_eq!(claim(&flag, "/", false), SplashPlan::Full);
        assert_eq!(claim(&flag, "/", false), SplashPlan::Skip);

        // A launch that skipped still spends the claim: navigating to "/"
        // afterwards is a warm navigation, not a cold start.
        let deep_linked = AtomicBool::new(false);
        assert_eq!(claim(&deep_linked, "/sessions", false), SplashPlan::Skip);
        assert_eq!(claim(&deep_linked, "/", false), SplashPlan::Skip);
    }

    #[test]
    fn the_process_wide_claim_is_consumed_by_its_first_caller() {
        // Whatever ran first, the splash cannot be claimed twice.
        let _first = claim_cold_start(ROOT_ROUTE, false);
        assert_eq!(claim_cold_start(ROOT_ROUTE, false), SplashPlan::Skip);
    }

    // ---- timeline --------------------------------------------------------

    #[test]
    fn the_timeline_matches_the_sources_own_arithmetic() {
        // ICON_IN + ICON_HOLD.
        assert_eq!(rain_cue_ms(), 1_000.0);
        // `landPieces`: data-delay + ICON_DELAY + 1500.
        assert_eq!(assembly_start_ms(), 2_500.0);
        // Last stagger (860) + the 500ms spring/flash window.
        assert_eq!(assembly_end_ms(), 3_860.0);
        // ICON_DELAY + THIN_END + 400.
        assert_eq!(rain_end_ms(), 4_200.0);
    }

    #[test]
    fn the_sequence_outlasts_both_the_assembly_and_the_rain() {
        let end = sequence_end_ms();
        assert!(end >= assembly_end_ms(), "the last piece must have landed");
        assert!(end >= rain_end_ms(), "the rain must have stopped painting");
        assert_eq!(
            end,
            rain_end_ms(),
            "the rain's tail is the later of the two"
        );
    }

    #[test]
    fn the_beats_are_strictly_ordered() {
        assert!(rain_cue_ms() < assembly_start_ms());
        assert!(assembly_start_ms() < assembly_end_ms());
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
        // An index past the table reuses the last stagger rather than panicking.
        assert_eq!(spec::piece_delay_ms(0), 0.0);
        assert_eq!(spec::piece_delay_ms(7), 860.0);
        assert_eq!(spec::piece_delay_ms(99), 860.0);
    }

    // ---- the gate --------------------------------------------------------

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

    // ---- the cue ---------------------------------------------------------

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
        // Defense in depth: the shell mounts `reduced_splash` instead, but a
        // theme flipping `reduce_motion` on after the mount decision must not
        // be held behind the whole four-second sequence.
        assert!(cue_due(0.0, sequence_end_ms(), true, true));
    }

    #[test]
    fn the_reduced_holds_own_cue_is_not_short_circuited_by_reduced_motion() {
        // `reduced_splash` is ONLY mounted when `reduce_motion` is on, so a
        // cue honoring it there would fire on the overlay's first painted
        // frame and unmount the hold before it was ever seen.
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

    // ---- piece motion ----------------------------------------------------

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
        // The instant its delay elapses it is at the shard state but live.
        let start = piece_motion(130.0, 130.0, false);
        assert!((start.dy - spec::PIECE_ENTER_DY).abs() < 1e-9);
        assert!((start.scale - spec::PIECE_ENTER_SCALE).abs() < 1e-9);
        assert_eq!(start.opacity, 0.0);
        // ...and a hair later it is on its way in.
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
            // Unwinds from -5° toward 0, swinging a little past it on the
            // spring's overshoot — but never further than it started.
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
        // Never out of range anywhere in the window.
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
        // On the assembly-local clock the gate opens at 0, so the last piece
        // must be finished within the window the sequence leaves it.
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
