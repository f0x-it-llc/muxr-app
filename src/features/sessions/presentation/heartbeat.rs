//! The `.conn-row` strip's connection heartbeat: a radar ping ring around its
//! success dot, and a rolling latency readout.
//!
//! `pages.rs`'s `Component::init` times each real `GetVersion` round trip on
//! the same live-resolved cadence as the session-list poll (`settings`'
//! `layout_refresh_secs`), not a heartbeat-private constant. Each tick starts
//! the ring's clock; the answer rolls into the readout over [`ROLL_MS`]
//! rather than snapping.
//!
//! [`FrameTicker`] keeps `build` re-invoked mid-animation (a signal write
//! otherwise wakes exactly one rebuild); the screen mounts it only while
//! [`HeartbeatVisual::animating`], so a settled strip requests no frames.
//! It requests a *paced* frame ([`PaintCtx::request_frame_paced`]), so the
//! ring's decorative loop runs at the theme's cosmetic rate rather than every
//! vsync. The ring's pose is computed from the wall clock, so a lower frame
//! rate only smooths the motion; it never changes the ring's timing.
//!
//! **Clip note:** [`AnimatedOpacity`] records its own *unscaled* layout rect
//! as its child layer's clip, captured before the nested [`AnimatedScale`]
//! pushes its transform — so a slot sized to the ring's resting diameter
//! would clip it away past `1.0x` scale. [`RING_SLOT`] is sized to the
//! ring's extent at [`RING_SCALE_MAX`] instead.
//!
//! The ring is a `success`-hued hairline border (a plain `frust::container`),
//! not `circular_progress` — that widget inks from the theme's `primary`
//! (brand amber here, never a health signal) and has the wrong stroke shape.
//!
//! Reduced motion drops the ring and the roll (readout snaps, no ticker
//! mounted); pings keep running — a round trip isn't an animation.

use std::time::{Duration, Instant};

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, Color, LayoutCtx, PaintCtx, PaintScene, Size, View,
    Widget,
};
use frust::motion::{AnimatedOpacity, AnimatedScale};
use frust::{
    Align, Alignment, Curve, Either, FlexChild, Get, RwSignal, Set, SizedBox, Timing, colored_box,
    container, inflexible, stack,
};

use super::pages::SessionsState;
use super::widgets::DOT;
use crate::ui::paint::{GLOW_SPREAD, GLOW_STD_DEV, HAIRLINE, RADIUS_FULL};

/// How long one ping ring takes to expand and fade out.
const RING_MS: f64 = 1600.0;
/// How long the latency readout takes to roll to a new reading.
const ROLL_MS: f64 = 300.0;

/// The ring's opacity at the ping, fading linearly to 0 over [`RING_MS`].
const RING_PEAK_OPACITY: f64 = 0.7;
/// The ring's scale arc: a closed pip at the ping, growing past the dot as
/// it fades — the radar expand.
const RING_SCALE_MIN: f64 = 0.6;
const RING_SCALE_MAX: f64 = 2.8;

/// The headroom slot the ring animates inside — see the [module docs](self)
/// clip note. Sized to contain the ring at [`RING_SCALE_MAX`] (`DOT * 2.8 =
/// 19.6`, plus rounding) while matching the strip's text line box, so
/// swapping in this cell doesn't move the row's height or the dot's resting
/// `y` (pinned to the pixel by `tests/navigation_motion.rs`).
const RING_SLOT: f64 = 20.0;

/// Immediate "current value" follower: zero duration, so `AnimatedOpacity`/
/// `AnimatedScale` snap to [`ring_pose`]'s frame value instead of re-easing —
/// this module owns the clock, not those wrappers.
const ZERO: Timing = Timing::Duration(Duration::ZERO, Curve::Linear);

/// The ping ring's pose on one frame — an alpha and a scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RingPose {
    opacity: f64,
    scale: f64,
}

/// A latency readout in transit: where the number started, what the ping that
/// ended it measured, and when the roll between them began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LatencyRoll {
    from: u32,
    to: u32,
    started: Instant,
}

/// Everything the strip renders of the heartbeat on one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct HeartbeatVisual {
    /// `None` once faded out, and always under reduced motion.
    pub(super) ring: Option<RingPose>,
    pub(super) latency_ms: u32,
    /// Whether the number is mid-roll — the readout's own ink cue.
    pub(super) rolling: bool,
}

impl HeartbeatVisual {
    /// Whether this frame is part of an animation, i.e. whether the screen
    /// still owes it another frame. The screen's [`frame_ticker`] mount gate.
    pub(super) fn animating(self) -> bool {
        self.ring.is_some() || self.rolling
    }
}

/// The heartbeat's screen-scoped observed state: the clock a ping starts, and
/// the roll its answer starts.
///
/// Signals, not retained `Component::State`, since both are written from
/// outside `build` (the interval tick, the ping's async completion) and each
/// write wakes the rebuild that renders it. `Copy` so one value serves the
/// interval closure, the spawned future, and the screen's state.
#[derive(Clone, Copy)]
pub(super) struct HeartbeatState {
    ping_started: RwSignal<Option<Instant>>,
    roll: RwSignal<Option<LatencyRoll>>,
}

impl HeartbeatState {
    /// A heartbeat that has not beaten yet: no ring, no roll, and a readout
    /// showing whatever the connect measured.
    pub(super) fn new() -> Self {
        Self {
            ping_started: RwSignal::new(None),
            roll: RwSignal::new(None),
        }
    }

    /// Starts a ping's ring clock; call as the tick fires the round trip so
    /// the ring expands while the RPC is open.
    pub(super) fn start_ping(self) {
        self.ping_started.set(Some(Instant::now()));
    }

    /// Records the roll to a ping's answer; call on completion, on the UI
    /// thread.
    pub(super) fn settle(self, from: u32, to: u32) {
        self.roll.set(Some(LatencyRoll {
            from,
            to,
            started: Instant::now(),
        }));
    }

    /// What the strip renders this frame, given the connection's currently
    /// published `settled_ms` — a completed ping already replaced it, so a
    /// finished roll lands exactly on it and a heartbeat that never ran
    /// prints the connect's own measurement.
    ///
    /// Reads `ping_started`/`roll` tracked, so their writes wake the rebuild
    /// that calls this.
    pub(super) fn visual(self, settled_ms: u32, reduce: bool) -> HeartbeatVisual {
        let now = Instant::now();
        let ring = self
            .ping_started
            .get()
            .and_then(|started| ring_pose(elapsed_ms(started, now), reduce));
        let rolling = self
            .roll
            .get()
            .filter(|_| !reduce)
            .and_then(|roll| rolled(roll.from, roll.to, elapsed_ms(roll.started, now)));
        HeartbeatVisual {
            ring,
            latency_ms: rolling.unwrap_or(settled_ms),
            rolling: rolling.is_some(),
        }
    }
}

/// Milliseconds from `since` to `now`, saturating so a start re-recorded
/// mid-read reads as zero rather than panicking.
fn elapsed_ms(since: Instant, now: Instant) -> f64 {
    now.saturating_duration_since(since).as_secs_f64() * 1000.0
}

/// The ring's pose `elapsed_ms` into its own clock: opacity fades
/// [`RING_PEAK_OPACITY`] → 0 while it scales [`RING_SCALE_MIN`] →
/// [`RING_SCALE_MAX`]. `None` once spent, or always under `reduce`.
fn ring_pose(elapsed_ms: f64, reduce: bool) -> Option<RingPose> {
    if reduce || !(0.0..RING_MS).contains(&elapsed_ms) {
        return None;
    }
    let progress = elapsed_ms / RING_MS;
    Some(RingPose {
        opacity: RING_PEAK_OPACITY * (1.0 - progress),
        scale: RING_SCALE_MIN + (RING_SCALE_MAX - RING_SCALE_MIN) * progress,
    })
}

/// The reading to print `elapsed_ms` into a roll from `from` to `to`;
/// `None` past [`ROLL_MS`], when the connection's own published reading
/// takes back over.
fn rolled(from: u32, to: u32, elapsed_ms: f64) -> Option<u32> {
    if elapsed_ms >= ROLL_MS {
        return None;
    }
    let progress = (elapsed_ms / ROLL_MS).max(0.0);
    let from = f64::from(from);
    let to = f64::from(to);
    Some((from + (to - from) * progress).round() as u32)
}

/// The success dot's own shape and glow, generic over `State` so the
/// glow-radius regression test below can paint it standalone rather than
/// stand up a full `SessionsState`. [`pinging_dot`] fixes `State` to
/// `SessionsState`.
///
/// [`RADIUS_FULL`] is a deliberately unclamped "fully rounded" sentinel —
/// `frust_widgets::Container::glow` is what clamps it to the fill's own
/// rendered corner before it reaches `draw_shadow`
/// (`the_dots_glow_radius_clamps_to_the_dot_not_the_radius_full_sentinel`
/// pins that clamp from this call site).
fn glow_dot<State: 'static>(ink: Color) -> impl View<State> {
    colored_box::<State>()
        .size(DOT, DOT)
        .radius(RADIUS_FULL)
        .fill(ink)
        .glow(ink, GLOW_STD_DEV, GLOW_SPREAD)
}

/// The strip's success dot with the ping ring expanding around it: a
/// [`RING_SLOT`]-square cell, ring layer under the dot, both centered.
///
/// Constant size whether or not a ping is in flight (the row must not
/// twitch), and both stack children always present — [`stack`] reconciles
/// positionally, so a vanishing child would rebuild its sibling.
pub(super) fn pinging_dot(ring: Option<RingPose>, ink: Color) -> impl View<SessionsState> {
    SizedBox::<SessionsState>(Some(RING_SLOT), Some(RING_SLOT)).child(
        stack()
            .child(ring_layer(ring, ink))
            .child(Align(Alignment::CENTER, glow_dot::<SessionsState>(ink))),
    )
}

/// The ring at `pose`, or an empty [`RING_SLOT`] slot. Opacity layer
/// outermost (lays out at `RING_SLOT`), scale inside it around a dot-sized
/// ring — see the [module docs](self) clip note.
fn ring_layer(pose: Option<RingPose>, ink: Color) -> impl View<SessionsState> {
    let Some(pose) = pose else {
        return Either::Left(SizedBox::<SessionsState>(Some(RING_SLOT), Some(RING_SLOT)));
    };
    Either::Right(
        AnimatedOpacity(
            pose.opacity,
            SizedBox::<SessionsState>(Some(RING_SLOT), Some(RING_SLOT)).child(Align(
                Alignment::CENTER,
                AnimatedScale(
                    pose.scale,
                    container(SizedBox::<SessionsState>(Some(DOT), Some(DOT)))
                        .border(ink, HAIRLINE)
                        .radius(RADIUS_FULL),
                )
                .timing(ZERO),
            )),
        )
        .timing(ZERO),
    )
}

/// A zero-size sentinel [`View`]/[`Widget`] whose only job is a paced
/// [`PaintCtx::request_frame_paced`] in `paint` (a `CosmeticLoop` frame,
/// capped at the theme's cosmetic rate rather than produced every vsync).
/// Mount only while the animation needing it is still running.
struct FrameTicker;

impl<State: 'static> View<State> for FrameTicker {
    type Element = FrameTickerWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> FrameTickerWidget {
        FrameTickerWidget
    }

    fn rebuild(
        &self,
        _prev: &Self,
        _element: &mut FrameTickerWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        ChangeFlags::PAINT
    }
}

struct FrameTickerWidget;

impl Widget for FrameTickerWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::ZERO)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        ctx.request_frame_paced();
    }
}

/// Mounts a [`FrameTicker`] — call only while the heartbeat still needs
/// another frame ([`HeartbeatVisual::animating`]).
pub(super) fn frame_ticker() -> FlexChild<SessionsState> {
    inflexible(FrameTicker)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_expands_and_fades_across_its_own_clock() {
        let start = ring_pose(0.0, false).expect("the ping's own frame draws a ring");
        assert!(
            (start.opacity - RING_PEAK_OPACITY).abs() < 1e-9,
            "{start:?}"
        );
        assert!((start.scale - RING_SCALE_MIN).abs() < 1e-9, "{start:?}");

        let mid = ring_pose(RING_MS / 2.0, false).expect("half way through");
        assert!(
            (mid.opacity - RING_PEAK_OPACITY / 2.0).abs() < 1e-9,
            "{mid:?}"
        );
        assert!(
            (mid.scale - (RING_SCALE_MIN + RING_SCALE_MAX) / 2.0).abs() < 1e-9,
            "{mid:?}"
        );

        // Spent: no ring, not a zero-opacity one still costing a layer push.
        assert_eq!(ring_pose(RING_MS, false), None);
        assert_eq!(ring_pose(RING_MS * 4.0, false), None);
    }

    #[test]
    fn reduced_motion_draws_no_ring_at_any_point_in_the_cycle() {
        for elapsed in [0.0, RING_MS / 2.0, RING_MS, RING_MS * 4.0] {
            assert_eq!(ring_pose(elapsed, true), None, "at {elapsed}ms");
        }
    }

    #[test]
    fn the_latency_rolls_rather_than_snapping() {
        assert_eq!(
            rolled(36, 76, 0.0),
            Some(36),
            "the roll starts where it was"
        );
        assert_eq!(rolled(36, 76, ROLL_MS / 2.0), Some(56), "…passes through");
        // Over: reverts to the connection's own published value.
        assert_eq!(rolled(36, 76, ROLL_MS), None);
        assert_eq!(rolled(36, 76, ROLL_MS * 10.0), None);
        // A roll that goes down reads the same way.
        assert_eq!(rolled(76, 36, ROLL_MS / 2.0), Some(56));
    }

    #[test]
    fn the_ring_slot_contains_the_ring_at_its_maximum_extent() {
        // Regression for the module docs' clip note: measured off the poses
        // the ring actually produces, not the scale constant.
        let widest = (0..=RING_MS as u32)
            .filter_map(|ms| ring_pose(f64::from(ms), false))
            .fold(0.0_f64, |widest, pose| widest.max(pose.scale * DOT));
        assert!(
            RING_SLOT >= widest,
            "a {RING_SLOT}px slot clips a ring drawn out to {widest}px"
        );
    }

    /// Regression for the heartbeat burning a full-scene frame every vsync
    /// while idle: the ticker must request a paced `CosmeticLoop` frame, not
    /// an unpaced `Transition` one. Asserted on the frame class the paint
    /// itself produces, through [`PaintCtx::for_test`].
    #[test]
    fn the_frame_ticker_requests_a_paced_cosmetic_frame_not_an_unpaced_transition() {
        use frust::authoring::Point;
        use frust_core::{FrameTime, TickClass};

        struct NullScene;

        impl PaintScene for NullScene {
            fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
            fn draw_text(&mut self, _origin: Point, _text: &str) {}
            fn draw_shadow(
                &mut self,
                _origin: Point,
                _size: Size,
                _radius: f64,
                _std_dev: f64,
                _color: Color,
            ) {
            }
        }

        let mut ticker = FrameTickerWidget;
        let mut ctx = PaintCtx::for_test(Point::ZERO, Size::ZERO, FrameTime::ZERO);
        ticker.paint(&mut ctx, &mut NullScene);

        assert!(ctx.needs_frame(), "the ticker must keep a frame coming");
        assert_eq!(
            ctx.frame_class(),
            Some(TickClass::CosmeticLoop),
            "the ticker must request a paced frame, not an unpaced Transition"
        );
        assert!(ctx.needs_frame_paced_only());
    }

    mod dot_glow_regression {
        use frust::authoring::Point;
        use frust_core::{FrameTime, RenderRoot};

        use super::*;

        /// A paint target recording only the shadow radii it sees — this dot
        /// paints exactly one `draw_shadow` (the glow) and nothing else this
        /// suite cares about.
        #[derive(Default)]
        struct ShadowRecorder {
            radii: Vec<f64>,
        }

        impl PaintScene for ShadowRecorder {
            fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
            fn draw_text(&mut self, _origin: Point, _text: &str) {}
            fn draw_shadow(
                &mut self,
                _origin: Point,
                _size: Size,
                radius: f64,
                _std_dev: f64,
                _color: Color,
            ) {
                self.radii.push(radius);
            }
        }

        /// Regression for the frust-side `Container::glow` radius clamp: a
        /// dot's `.radius(RADIUS_FULL)` (a deliberately unclamped "fully
        /// rounded" sentinel) used to feed straight into `draw_shadow`,
        /// ringing the dot in a dark blob far larger than the shape it was
        /// supposed to halo — device-confirmed on the sessions strip. Paints
        /// the exact same shape [`pinging_dot`]'s dot does and pins the
        /// emitted glow radius to the dot's own clamped half-width plus
        /// spread, never the raw sentinel.
        #[test]
        fn the_dots_glow_radius_clamps_to_the_dot_not_the_radius_full_sentinel() {
            let ink = Color::from_rgb8(0x30, 0xd1, 0x5c);
            let mut root = RenderRoot::new();
            let mut state = ();
            root.rebuild(&mut |_state: &mut ()| glow_dot::<()>(ink), &mut state);
            root.layout(Size::new(DOT, DOT));
            let mut scene = ShadowRecorder::default();
            root.paint(&mut scene, FrameTime::ZERO);

            assert_eq!(
                scene.radii.len(),
                1,
                "exactly one glow — the dot's own: {:?}",
                scene.radii
            );
            let expected = DOT / 2.0 + GLOW_SPREAD;
            assert!(
                (scene.radii[0] - expected).abs() < 1e-9,
                "the dot's glow radius must clamp to DOT/2 + GLOW_SPREAD \
                 ({expected}), not the RADIUS_FULL sentinel: got {:?}",
                scene.radii
            );
            assert!(
                scene.radii[0] < RADIUS_FULL,
                "…and must never reach the unclamped RADIUS_FULL sentinel \
                 ({RADIUS_FULL}): got {:?}",
                scene.radii
            );
        }
    }
}
