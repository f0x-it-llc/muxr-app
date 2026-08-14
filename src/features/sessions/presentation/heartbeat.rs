//! The `.conn-row` strip's connection heartbeat: a radar ping ring around its
//! success dot, and the rolling latency readout at the other end of the row.
//!
//! `pages.rs`'s `Component::init` times one real `GetVersion` round trip
//! ([`crate::connection::ConnectionController::ping`]) while `/sessions` is on
//! top, on the **same live-resolved cadence** as the session-list poll —
//! `settings`' own stored `layout_refresh_secs` preference, the "session
//! poll" group (`super::controllers`' module docs' "Session poll cadence"
//! section) — rather than a heartbeat-private constant. Each tick starts the
//! ring's own clock; the measurement that comes back is **rolled** into the
//! readout over [`ROLL_MS`] rather than snapping to it. Ported from
//! `examples/glyph-catalog`'s "connection heartbeat" interaction moment — the
//! catalog drives that moment off a canned latency cycle, this one off the
//! wire.
//!
//! # Wall clock, and the frame ticker that keeps it moving
//!
//! A signal write wakes exactly one rebuild, so the interval's tick alone
//! would render a single frame of a 1.6s animation. [`FrameTicker`] — a
//! zero-size [`View`]/[`Widget`] pair authored through [`frust::authoring`],
//! the same seam [`super::chrome`] uses, whose only job is an unconditional
//! [`PaintCtx::request_frame`] in its own `paint` — is what keeps `build`
//! re-invoked while an animation is still running, and [`ring_pose`]/
//! [`rolled`] read `Instant::now()` on each of those passes. **Mount
//! discipline lives at the call site, not in the widget**: the screen mounts a
//! ticker only while [`HeartbeatVisual::animating`], so a settled strip asks
//! for no frames at all and a covered page (which the navigator paint-culls)
//! self-quiets.
//!
//! # The clip lesson [`RING_SLOT`] encodes
//!
//! [`AnimatedOpacity`] composites its child under `PaintScene::push_layer`,
//! which records **its own (unscaled) layout rect** as that layer's clip —
//! captured before the nested [`AnimatedScale`] has pushed its transform,
//! since the opacity layer paints outermost. A slot sized to the ring's own
//! diameter therefore clips the ring away the instant it scales past `1.0x`.
//! So the outer slot is [`RING_SLOT`] — at least the ring's extent at
//! [`RING_SCALE_MAX`] — with the small unscaled ring [`Align`]-centered inside
//! it.
//!
//! # The ring is a `success` hairline circle, not `circular_progress`
//!
//! The catalog strokes its ring with `circular_progress(Determinate(1.0))`,
//! which inks itself from the theme's `primary` with no colour seam of its
//! own. On this screen that is the brand amber, and amber is never a health
//! signal here ([`super::widgets`]' colour-role rule) — a ping emanating from
//! the liveness dot is exactly a health signal. It is also the wrong shape at
//! this size: that widget's 4px stroke over a dot-sized box leaves no hole to
//! read as a ring. The ring is therefore [`super::chrome::panel`]'s hairline
//! rounded-rect border at [`super::chrome::RADIUS_FULL`], taking an
//! already-resolved ink like every other piece on this screen.
//!
//! # Reduced motion
//!
//! Collapses the whole moment: no ring, no roll (the readout snaps to whatever
//! the last ping measured), and no ticker mounted. Pings themselves keep
//! running — a round-trip measurement is not an animation.

use std::time::{Duration, Instant};

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, Color, LayoutCtx, PaintCtx, PaintScene, Size, View,
    Widget,
};
use frust::motion::{AnimatedOpacity, AnimatedScale};
use frust::{
    Align, Alignment, AnyView, Curve, FlexChild, Get, RwSignal, Set, SizedBox, Stack, Timing, any,
    inflexible,
};

use super::chrome::{RADIUS_FULL, panel, swatch};
use super::pages::SessionsState;
use super::widgets::DOT;

/// How long one ping ring takes to expand and fade out.
const RING_MS: f64 = 1600.0;
/// How long the latency readout takes to roll from the previous reading to the
/// one a ping just measured.
const ROLL_MS: f64 = 300.0;

/// The ring's opacity at the moment of the ping, fading linearly to nothing
/// over [`RING_MS`].
const RING_PEAK_OPACITY: f64 = 0.7;
/// The ring's scale arc: a small "closed" pip at the ping, growing well past
/// the dot as it fades — the radar expand.
const RING_SCALE_MIN: f64 = 0.6;
const RING_SCALE_MAX: f64 = 2.8;

/// The headroom slot the ring animates inside — see the [module docs](self)'
/// clip note for why the ring's own diameter will not do. Two things pin this
/// number: it must contain the ring at [`RING_SCALE_MAX`] (`DOT * 2.8 = 19.6`,
/// plus a rounding margin), and it is exactly the strip's own text line box,
/// so swapping a 7px dot for this cell leaves the row's height — and the dot's
/// resting `y`, which `tests/navigation_motion.rs` pins to the pixel — where
/// they were.
const RING_SLOT: f64 = 20.0;

/// An immediate "current value" follower timing: zero duration, so the
/// [`AnimatedOpacity`]/[`AnimatedScale`] wrappers snap to whatever pose
/// [`ring_pose`] computed for this frame instead of re-easing on top of it.
/// The clock is this module's, not theirs.
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
    /// The ping ring's pose, or `None` once it has faded out (and always,
    /// under reduced motion).
    pub(super) ring: Option<RingPose>,
    /// The latency to print, mid-roll or settled.
    pub(super) latency_ms: u32,
    /// Whether that number is still rolling — the readout's own ink cue.
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
/// Signals rather than retained `Component::State` because both are written
/// from outside `build` — the interval's tick and the ping's own async
/// completion — and each write is what wakes the rebuild that renders it.
/// `Copy`, so one value serves the interval closure, the future it spawns, and
/// the screen's state.
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

    /// Start a ping's ring clock — called as the tick fires the round trip,
    /// so the ring expands while the RPC is actually open.
    pub(super) fn start_ping(self) {
        self.ping_started.set(Some(Instant::now()));
    }

    /// Record the roll from the previous reading to the one a ping just
    /// measured. Called on the ping's completion, on the UI thread.
    pub(super) fn settle(self, from: u32, to: u32) {
        self.roll.set(Some(LatencyRoll {
            from,
            to,
            started: Instant::now(),
        }));
    }

    /// What the strip renders this frame, given the connection's currently
    /// published `settled_ms` (which a completed ping has already replaced —
    /// so a finished roll lands exactly on it, and a heartbeat that never ran
    /// prints the connect's own measurement).
    ///
    /// Tracked reads: the tick's clock write and the ping's roll write are
    /// what wake the rebuild that calls this.
    pub(super) fn visual(self, settled_ms: u32, reduce: bool) -> HeartbeatVisual {
        let now = Instant::now();
        let ring = self
            .ping_started
            .get()
            .and_then(|started| ring_pose(elapsed_ms(started, now), reduce));
        // A finished roll hands the readout back to the connection's own
        // published reading rather than holding its endpoint, so a reconnect
        // that republishes a different measurement is never overprinted by a
        // stale roll.
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

/// Milliseconds from `since` to `now`, saturating — a clock that appears to
/// run backwards (a re-recorded start racing this read) reads as zero elapsed
/// rather than panicking.
fn elapsed_ms(since: Instant, now: Instant) -> f64 {
    now.saturating_duration_since(since).as_secs_f64() * 1000.0
}

/// The ping ring's pose `elapsed_ms` into its own clock: fading from
/// [`RING_PEAK_OPACITY`] to nothing while expanding [`RING_SCALE_MIN`] →
/// [`RING_SCALE_MAX`]. `None` once the ring is spent — and always under
/// `reduce`, which suppresses the ping outright.
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

/// The reading to print `elapsed_ms` into a roll from `from` to `to`, or
/// `None` once the roll is over — past [`ROLL_MS`] the connection's own
/// published reading takes back over.
fn rolled(from: u32, to: u32, elapsed_ms: f64) -> Option<u32> {
    if elapsed_ms >= ROLL_MS {
        return None;
    }
    let progress = (elapsed_ms / ROLL_MS).max(0.0);
    let from = f64::from(from);
    let to = f64::from(to);
    Some((from + (to - from) * progress).round() as u32)
}

// ---------------------------------------------------------------------------
// The strip's dot cell
// ---------------------------------------------------------------------------

/// The strip's success dot with the ping ring expanding around it: one
/// [`RING_SLOT`]-square cell, the ring layer under the dot, both centered.
///
/// The cell's size is a constant whether or not a ping is in flight — the row
/// beside it must not twitch as the ring comes and goes — and its two stack
/// children are always present for the same reason ([`Stack`] reconciles
/// positionally, so a vanishing child would rebuild its sibling).
pub(super) fn pinging_dot(ring: Option<RingPose>, ink: Color) -> AnyView<SessionsState> {
    any(
        SizedBox::<SessionsState>(Some(RING_SLOT), Some(RING_SLOT)).child(Stack(vec![
            ring_layer(ring, ink),
            any(Align(
                Alignment::CENTER,
                swatch(Size::new(DOT, DOT), ink, RADIUS_FULL).glow(ink),
            )),
        ])),
    )
}

/// The ping ring at `pose`, or an empty slot of the same size when there is no
/// ping to draw. See the [module docs](self) for the nesting order — the
/// opacity layer is outermost and lays out at [`RING_SLOT`], the scale is
/// inside it around a dot-sized ring.
fn ring_layer(pose: Option<RingPose>, ink: Color) -> AnyView<SessionsState> {
    let Some(pose) = pose else {
        return any(SizedBox::<SessionsState>(Some(RING_SLOT), Some(RING_SLOT)));
    };
    any(AnimatedOpacity(
        pose.opacity,
        SizedBox::<SessionsState>(Some(RING_SLOT), Some(RING_SLOT)).child(Align(
            Alignment::CENTER,
            AnimatedScale(
                pose.scale,
                panel(
                    SizedBox::<SessionsState>(Some(DOT), Some(DOT)),
                    Color::TRANSPARENT,
                    RADIUS_FULL,
                )
                .border(ink),
            )
            .timing(ZERO),
        )),
    )
    .timing(ZERO))
}

// ---------------------------------------------------------------------------
// FrameTicker — the per-frame rebuild this module's wall clock needs
// ---------------------------------------------------------------------------

/// A zero-size sentinel [`View`]/[`Widget`] pair whose only job is an
/// unconditional [`PaintCtx::request_frame`] call in its own `paint`. Mounted
/// by the screen only while its own animation is still running, so a frame
/// request traces directly to the thing that needs it rather than riding some
/// other widget's paint behaviour as a side effect.
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

/// The retained widget for a [`FrameTicker`]. See its doc comment.
struct FrameTickerWidget;

impl Widget for FrameTickerWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::ZERO)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        ctx.request_frame();
    }
}

/// Mount a [`FrameTicker`] — call only while the heartbeat still needs another
/// frame ([`HeartbeatVisual::animating`]); the widget itself just asks.
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

        // Spent: no ring at all rather than a zero-opacity one still costing a
        // layer push (and a scale the strip would have to lay out around).
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
        // Over: the readout goes back to whatever the connection publishes,
        // which is the value this roll was heading for anyway.
        assert_eq!(rolled(36, 76, ROLL_MS), None);
        assert_eq!(rolled(36, 76, ROLL_MS * 10.0), None);
        // A roll that goes down reads the same way.
        assert_eq!(rolled(76, 36, ROLL_MS / 2.0), Some(56));
    }

    #[test]
    fn the_ring_slot_contains_the_ring_at_its_maximum_extent() {
        // The clip regression [`RING_SLOT`] exists for (see the module docs):
        // the outer slot is what `AnimatedOpacity` records as its layer clip,
        // so anything the scale pushes past it is *cut*, not merely
        // unallocated. Measured off the poses the ring actually produces,
        // sweeping its own clock, rather than off the scale constant — the
        // slot has to contain what gets drawn.
        let widest = (0..=RING_MS as u32)
            .filter_map(|ms| ring_pose(f64::from(ms), false))
            .fold(0.0_f64, |widest, pose| widest.max(pose.scale * DOT));
        assert!(
            RING_SLOT >= widest,
            "a {RING_SLOT}px slot clips a ring drawn out to {widest}px"
        );
    }
}
