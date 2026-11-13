//! The `.conn-row` strip's connection heartbeat: a ping ring around the success dot and a rolling
//! latency readout, on the session-list poll cadence. Reduced motion drops the ring and the roll;
//! pings keep running.

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

const RING_MS: f64 = 1600.0;
const ROLL_MS: f64 = 300.0;

/// The ring's opacity at the ping, fading linearly to 0 over [`RING_MS`].
const RING_PEAK_OPACITY: f64 = 0.7;
const RING_SCALE_MIN: f64 = 0.6;
const RING_SCALE_MAX: f64 = 2.8;

/// Headroom slot the ring animates inside. `AnimatedOpacity` clips to its unscaled layout rect, so
/// the slot is sized to the ring at [`RING_SCALE_MAX`] (`DOT * 2.8`, plus rounding) while matching the
/// strip's text line box: the row height and the dot's resting `y` do not move.
const RING_SLOT: f64 = 20.0;

/// Zero-duration follower: the wrappers snap to [`ring_pose`]'s frame value; this module owns the clock.
const ZERO: Timing = Timing::Duration(Duration::ZERO, Curve::Linear);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RingPose {
    opacity: f64,
    scale: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LatencyRoll {
    from: u32,
    to: u32,
    started: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct HeartbeatVisual {
    pub(super) ring: Option<RingPose>,
    pub(super) latency_ms: u32,
    pub(super) rolling: bool,
}

impl HeartbeatVisual {
    pub(super) fn animating(self) -> bool {
        self.ring.is_some() || self.rolling
    }
}

/// Signals, written outside `build` (interval tick, ping completion); `Copy` so one value serves
/// the closure, the future and the screen's state.
#[derive(Clone, Copy)]
pub(super) struct HeartbeatState {
    ping_started: RwSignal<Option<Instant>>,
    roll: RwSignal<Option<LatencyRoll>>,
}

impl HeartbeatState {
    pub(super) fn new() -> Self {
        Self {
            ping_started: RwSignal::new(None),
            roll: RwSignal::new(None),
        }
    }

    /// Starts a ping's ring clock as the tick fires the round trip.
    pub(super) fn start_ping(self) {
        self.ping_started.set(Some(Instant::now()));
    }

    /// Records the roll to a ping's answer, on the UI thread.
    pub(super) fn settle(self, from: u32, to: u32) {
        self.roll.set(Some(LatencyRoll {
            from,
            to,
            started: Instant::now(),
        }));
    }

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

/// Milliseconds from `since` to `now`, saturating so a re-recorded start reads zero.
fn elapsed_ms(since: Instant, now: Instant) -> f64 {
    now.saturating_duration_since(since).as_secs_f64() * 1000.0
}

/// The ring's pose `elapsed_ms` into its clock; `None` once spent, or always under `reduce`.
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

/// The reading to print `elapsed_ms` into a roll from `from` to `to`; `None` past [`ROLL_MS`].
fn rolled(from: u32, to: u32, elapsed_ms: f64) -> Option<u32> {
    if elapsed_ms >= ROLL_MS {
        return None;
    }
    let progress = (elapsed_ms / ROLL_MS).max(0.0);
    let from = f64::from(from);
    let to = f64::from(to);
    Some((from + (to - from) * progress).round() as u32)
}

/// [`RADIUS_FULL`] is an unclamped "fully rounded" sentinel; `Container::glow` clamps it to the
/// fill's corner before `draw_shadow`.
fn glow_dot<State: 'static>(ink: Color) -> impl View<State> {
    colored_box::<State>()
        .size(DOT, DOT)
        .radius(RADIUS_FULL)
        .fill(ink)
        .glow(ink, GLOW_STD_DEV, GLOW_SPREAD)
}

/// Constant size whether or not a ping is in flight; both stack children always present, since
/// [`stack`] reconciles positionally.
pub(super) fn pinging_dot(ring: Option<RingPose>, ink: Color) -> impl View<SessionsState> {
    SizedBox::<SessionsState>(Some(RING_SLOT), Some(RING_SLOT)).child(
        stack()
            .child(ring_layer(ring, ink))
            .child(Align(Alignment::CENTER, glow_dot::<SessionsState>(ink))),
    )
}

/// The ring at `pose`, or an empty [`RING_SLOT`] slot. Opacity layer outermost, scale inside it.
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

/// Zero-size [`View`]/[`Widget`] whose paint requests a paced frame ([`PaintCtx::request_frame_paced`]),
/// capped at the theme's cosmetic rate. Mount only while the animation still runs.
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
        assert_eq!(rolled(36, 76, ROLL_MS), None);
        assert_eq!(rolled(36, 76, ROLL_MS * 10.0), None);
        assert_eq!(rolled(76, 36, ROLL_MS / 2.0), Some(56));
    }

    #[test]
    fn the_ring_slot_contains_the_ring_at_its_maximum_extent() {
        let widest = (0..=RING_MS as u32)
            .filter_map(|ms| ring_pose(f64::from(ms), false))
            .fold(0.0_f64, |widest, pose| widest.max(pose.scale * DOT));
        assert!(
            RING_SLOT >= widest,
            "a {RING_SLOT}px slot clips a ring drawn out to {widest}px"
        );
    }

    /// The ticker requests a paced `CosmeticLoop` frame, not an unpaced `Transition` one.
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

        /// The dot's glow radius is its own clamped half-width plus spread, never the raw sentinel.
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
