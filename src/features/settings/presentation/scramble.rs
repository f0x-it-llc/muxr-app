//! The `auth tokens` group's revoke animation: a token's name locks
//! left-to-right through deterministic noise, then the whole two-line row
//! collapses away. Transcribed from `examples/glyph-catalog`'s "token revoke
//! — scramble" interaction moment; that crate is unreachable as a dependency
//! (standalone workspace), so the noise source
//! ([`lcg_next`]/[`scramble_char`]/[`scramble_text`]), the four timing
//! constants and the [`FrameTicker`] escape hatch are **copied**, not
//! imported.
//!
//! **Mechanism only**: the clock, the noise, the per-token timeline state
//! ([`RevokeAnimations`]) and the two custom widgets. The row itself is
//! `widgets::tokens`' (`scrambling_token_item`, sharing one row builder with
//! the live `token_item`); the decision to start one is `pages`'.
//!
//! **The clock**: elapsed time reads off a wall clock ([`now_ms`]) behind a
//! [`set_animation_clock_for_tests`] override, so a headless suite drives the
//! timeline deterministically. [`FrameTicker`] (an unconditional
//! `PaintCtx::request_frame` in its own `paint`) is what keeps `build`
//! re-invoked while a timeline runs — mounted **inside the collapsing row**,
//! so a row scrolled out of view stops requesting frames along with the
//! scroller's own paint culling.
//!
//! **The collapse** can't be a fixed-height `SizedBox(row_h * ratio)`: a real
//! `.token-item`'s height depends on the resolved type scale and platform
//! font metrics, so a transcribed number would jump the row the moment the
//! animation starts. [`CollapseView`] lays the row out at its **natural**
//! height and reports `height * ratio` as its own; the [`AnimatedOpacity`]
//! wrapped around it clips the overflow.

use std::cell::Cell;
use std::time::{Duration, Instant};

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, EventCtx, EventResult, InputEvent, LayoutCtx,
    PaintCtx, PaintScene, Point, SemanticsCtx, Size, View, Widget, any, build_child, rebuild_child,
    route_event_single, teardown_child,
};
use frust::motion::AnimatedOpacity;
use frust::{AnyView, Curve, Get, GetUntracked, RwSignal, Set, Timing};

use crate::features::settings::domain::entities::AuthToken;

use super::pages::SettingsState;

// ---------------------------------------------------------------------------
// Timing and noise (transcribed from the catalog moment, once)
// ---------------------------------------------------------------------------

/// Per-frame tick length (the reference's `setInterval(..., 40)`).
const FRAME_MS: f64 = 40.0;
/// Total scramble frames before the name freezes (the reference's `maxFrames`).
const MAX_FRAMES: f64 = 14.0;
/// Delay after the last scramble frame before the collapse starts (the
/// reference's `setTimeout(..., 120)`).
const COLLAPSE_DELAY_MS: f64 = 120.0;
/// Collapse (height + opacity) length (the reference's `.3s` transition).
const COLLAPSE_MS: f64 = 300.0;
/// When the collapse starts, on the timeline's own clock.
const COLLAPSE_START_MS: f64 = MAX_FRAMES * FRAME_MS + COLLAPSE_DELAY_MS;
/// The whole timeline, end to end — 560ms of scramble, a 120ms hold, then a
/// 300ms collapse. Public so a headless test can advance its clock past the
/// end without re-deriving the sum.
pub const TIMELINE_MS: f64 = COLLAPSE_START_MS + COLLAPSE_MS;

/// The noise charset, lifted verbatim from the reference's `CHARS`.
const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-#$%&";

/// How much of the name is locked to `·` by the end of the scramble — the
/// reference's own `lockRatio * 0.4`, i.e. the leading 40% at the last frame.
const LOCK_SPAN: f64 = 0.4;

/// An immediate "current value" follower timing: zero duration, so the
/// [`AnimatedOpacity`] wrapper snaps to whatever ratio this module computed for
/// the frame instead of re-easing on top of it. The clock is this module's, not
/// the wrapper's.
const ZERO: Timing = Timing::Duration(Duration::ZERO, Curve::Linear);

/// One step of a small linear-congruential generator (Numerical Recipes'
/// constants) — the deterministic stand-in for the reference's `Math.random`.
/// A given `(frame, index)` pair therefore always renders the same glitch
/// character, which is what makes the animation reproducible in a test.
fn lcg_next(seed: u32) -> u32 {
    seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)
}

/// One noise character for index `idx` at scramble `frame` — mirrors
/// `Math.random()<0.5 ? CHARS[...] : original`. A space stays a space.
fn scramble_char(original: char, frame: u32, idx: u32) -> char {
    if original == ' ' {
        return ' ';
    }
    let r1 = lcg_next(frame.wrapping_mul(0x9E37_79B1).wrapping_add(idx));
    if r1 & 1 == 0 {
        original
    } else {
        let r2 = lcg_next(r1);
        CHARSET[(r2 as usize) % CHARSET.len()] as char
    }
}

/// `source` as it renders at `frame` (`0..=MAX_FRAMES`): the leading
/// `lock_ratio * LOCK_SPAN` of characters are locked to `·`, the rest are
/// noise. The rebuilt string is the frame's one allocation.
fn scramble_text(source: &str, frame: u32) -> String {
    let lock_ratio = f64::from(frame) / MAX_FRAMES;
    let len = source.chars().count();
    source
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            if len > 0 && (i as f64 / len as f64) < lock_ratio * LOCK_SPAN {
                if ch == ' ' { ' ' } else { '·' }
            } else {
                scramble_char(ch, frame, i as u32)
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The clock
// ---------------------------------------------------------------------------

thread_local! {
    /// The pinned reading a test installed, in ms — see
    /// [`set_animation_clock_for_tests`].
    static PINNED_CLOCK_MS: Cell<Option<f64>> = const { Cell::new(None) };
    /// The real clock's zero: the first [`now_ms`] read on this thread. The
    /// epoch is arbitrary — only differences are ever used.
    static CLOCK_EPOCH: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Pin the animation clock at `now_ms` for a headless test, so a suite can
/// step the revoke timeline frame by frame instead of racing a real one;
/// `None` restores the wall clock. A published setter rather than
/// `#[cfg(test)]`, since a `tests/*.rs` integration binary links the normal
/// build. Never called by production code.
pub fn set_animation_clock_for_tests(now_ms: Option<f64>) {
    PINNED_CLOCK_MS.with(|pinned| pinned.set(now_ms));
}

/// Move a pinned clock forward by `delta_ms`. With no clock pinned yet this
/// pins one at `delta_ms` — the real clock's own zero is arbitrary, so that is
/// the same thing — rather than silently doing nothing to a test's timeline.
pub fn advance_animation_clock_for_tests(delta_ms: f64) {
    PINNED_CLOCK_MS.with(|pinned| {
        pinned.set(Some(pinned.get().unwrap_or(0.0) + delta_ms));
    });
}

/// The animation clock, in ms since an arbitrary fixed zero — a test's pinned
/// reading when there is one, otherwise the wall clock.
fn now_ms() -> f64 {
    if let Some(pinned) = PINNED_CLOCK_MS.with(Cell::get) {
        return pinned;
    }
    let now = Instant::now();
    let epoch = CLOCK_EPOCH.with(|cell| {
        cell.get().unwrap_or_else(|| {
            cell.set(Some(now));
            now
        })
    });
    now.saturating_duration_since(epoch).as_secs_f64() * 1000.0
}

// ---------------------------------------------------------------------------
// The timeline
// ---------------------------------------------------------------------------

/// What one revoking row renders on a single frame.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ScramblePose {
    /// The name line's text.
    pub(super) display: String,
    /// Whether the name renders in the error ink, i.e. is mid-glitch.
    pub(super) glitching: bool,
    /// The row's height and opacity factor, `1.0` → `0.0`.
    pub(super) ratio: f64,
    /// Whether this timeline still owes another frame — the row's own
    /// [`frame_ticker`] mount gate.
    pub(super) running: bool,
}

/// The pose `elapsed_ms` into a revoke of `name`.
///
/// Under `reduce` the whole moment collapses to its end state — no scramble
/// frames, no collapse, the row simply gone — which is the catalog's own
/// reduced-motion rule and, here, exactly the behaviour that shipped before
/// this animation existed.
fn pose(name: &str, elapsed_ms: f64, reduce: bool) -> ScramblePose {
    if reduce {
        return ScramblePose {
            display: String::new(),
            glitching: false,
            ratio: 0.0,
            running: false,
        };
    }
    let frame = (elapsed_ms / FRAME_MS).floor().clamp(0.0, MAX_FRAMES) as u32;
    let collapse_p = if elapsed_ms > COLLAPSE_START_MS {
        ((elapsed_ms - COLLAPSE_START_MS) / COLLAPSE_MS).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ScramblePose {
        display: scramble_text(name, frame),
        glitching: true,
        ratio: 1.0 - collapse_p,
        running: collapse_p < 1.0,
    }
}

/// One token's revoke, in flight.
///
/// Carries a **snapshot** of the row rather than a reference into `tokens`,
/// because the RPC's own `refresh_tokens` routinely lands mid-animation and
/// takes the token out of that list: the row has to keep rendering, in place,
/// from something the refresh cannot remove.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RevokeScramble {
    /// The row as it read when the revoke was confirmed — its name (which is
    /// also the key, and the scramble's source text), scope chip and created
    /// line.
    pub(super) token: AuthToken,
    /// Where that row sat in `tokens` at that moment, so a row whose token has
    /// already left the list can be put back at the position it is vanishing
    /// from.
    pub(super) index: usize,
    /// Whether the revoke RPC has answered yet, either way.
    pub(super) answered: bool,
    /// The timeline's zero, on the animation clock.
    started_ms: f64,
}

impl RevokeScramble {
    /// What this row renders now.
    pub(super) fn pose(&self, reduce: bool) -> ScramblePose {
        pose(
            &self.token.name,
            (now_ms() - self.started_ms).max(0.0),
            reduce,
        )
    }

    /// Whether this entry is finished with: its animation has played out **and**
    /// its RPC has answered. A row whose collapse ended first stays (at height
    /// zero) until the answer lands, so a refusal restores it rather than
    /// letting the list flicker back to two rows and then out again.
    fn spent(&self, reduce: bool) -> bool {
        self.answered && !self.pose(reduce).running
    }
}

/// The screen's live revoke timelines, keyed by token name.
///
/// A signal rather than plain retained state: it is written from *outside*
/// the screen's own `build` (a confirmation dialog's action; the revoke
/// future's completion), and each write wakes the rebuild that renders it.
/// Screen-scoped rather than controller-owned, since nothing here is
/// repository state.
///
/// Every write reads through `try_get_untracked`, never `get_untracked`: two
/// of the three run from a future that can outlive the screen (leaving
/// `/settings` mid-revoke disposes this signal out from under it), and
/// reading a disposed signal panics where writing one merely no-ops. Only
/// [`live`](Self::live) reads outright, since a build cannot run after the
/// component that owns it is gone.
#[derive(Clone, Copy)]
pub(super) struct RevokeAnimations {
    entries: RwSignal<Vec<RevokeScramble>>,
}

impl RevokeAnimations {
    /// A screen with nothing being revoked.
    pub(super) fn new() -> Self {
        Self {
            entries: RwSignal::new(Vec::new()),
        }
    }

    /// Snapshot `token` at list position `index` and start its timeline. A
    /// second revoke of the same name replaces the first rather than stacking
    /// two rows for one token.
    pub(super) fn begin(self, token: &AuthToken, index: usize) {
        let Some(mut entries) = self.entries.try_get_untracked() else {
            return;
        };
        entries.retain(|entry| entry.token.name != token.name);
        entries.push(RevokeScramble {
            token: token.clone(),
            index,
            answered: false,
            started_ms: now_ms(),
        });
        self.entries.set(entries);
    }

    /// Record that the revoke RPC for `name` has answered — landed or refused.
    /// The entry itself goes when [`sweep`](Self::sweep) next runs, which is
    /// what keeps a fast answer from cutting the animation short.
    pub(super) fn answered(self, name: &str) {
        let Some(mut entries) = self.entries.try_get_untracked() else {
            return;
        };
        for entry in entries.iter_mut().filter(|entry| entry.token.name == name) {
            entry.answered = true;
        }
        self.entries.set(entries);
    }

    /// Every timeline the screen may still have to render — a tracked read, so
    /// the three writes above wake the build that took it.
    pub(super) fn live(self) -> Vec<RevokeScramble> {
        self.entries.get()
    }

    /// Drop every timeline that is [`spent`](RevokeScramble::spent).
    ///
    /// Bookkeeping only — `build` already declines to render a spent entry, so
    /// the screen is correct whether or not this has run yet. It is called from
    /// a `spawn_local` off the build that first saw one, rather than inline,
    /// because a signal write in the middle of the rebuild reading it is not a
    /// trade this screen needs to make.
    pub(super) fn sweep(self, reduce: bool) {
        let Some(entries) = self.entries.try_get_untracked() else {
            return;
        };
        let kept: Vec<RevokeScramble> = entries
            .iter()
            .filter(|entry| !entry.spent(reduce))
            .cloned()
            .collect();
        if kept.len() != entries.len() {
            self.entries.set(kept);
        }
    }
}

// ---------------------------------------------------------------------------
// The collapse
// ---------------------------------------------------------------------------

/// Wrap a revoking row in its collapse: opacity and height driven together off
/// the same `ratio`, as in the catalog. See the [module docs](self) for why the
/// height is measured rather than transcribed.
pub(super) fn collapsing(ratio: f64, row: impl View<SettingsState>) -> impl View<SettingsState> {
    AnimatedOpacity(
        ratio,
        CollapseView {
            child: any(row), // erasure: keep ChildPod-fed
            ratio: ratio.clamp(0.0, 1.0),
        },
    )
    .timing(ZERO)
}

/// The declarative half of the collapse: `child` at its natural height, this
/// box at `ratio` of it.
struct CollapseView<State: 'static> {
    child: AnyView<State>,
    ratio: f64,
}

/// The retained half of a [`CollapseView`].
struct CollapseWidget {
    child: ChildPod,
    ratio: f64,
}

impl<State: 'static> View<State> for CollapseView<State> {
    type Element = CollapseWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> CollapseWidget {
        CollapseWidget {
            child: build_child(&self.child, ctx),
            ratio: self.ratio,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut CollapseWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.ratio != self.ratio {
            element.ratio = self.ratio;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut CollapseWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for CollapseWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        // The child is laid out under the incoming width but an unbounded
        // height — its natural, uncollapsed size, which is the number this
        // whole animation is scaling and the one a transcribed constant could
        // only approximate.
        let natural = self.child.layout_child(
            ctx,
            &BoxConstraints::new(
                Size::new(bc.min().width, 0.0),
                Size::new(bc.max().width, f64::INFINITY),
            ),
        );
        self.child.set_origin(Point::ZERO);
        bc.constrain(Size::new(
            natural.width,
            (natural.height * self.ratio).max(0.0),
        ))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        // Whatever hangs below this box is clipped by the `AnimatedOpacity`
        // layer wrapped around it (see the module docs), not here: a widget
        // that pushed its own clip layer would pay for a second one.
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // A single-child container still routes, or the subtree it wraps is
        // silently unreachable — the rule every hand-rolled container in this
        // app follows, even one whose child is on its way out.
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

// ---------------------------------------------------------------------------
// FrameTicker — the per-frame rebuild this module's wall clock needs
// ---------------------------------------------------------------------------

/// A zero-size sentinel [`View`]/[`Widget`] pair whose only job is an
/// unconditional [`PaintCtx::request_frame`] call in its own `paint`. Mounted
/// only while a timeline is still running, so a frame request traces directly
/// to the row that needs it rather than riding some other widget's paint
/// behaviour as a side effect.
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

/// Mount a [`FrameTicker`] — call only while the caller's own row still needs
/// another frame ([`ScramblePose::running`]); the widget itself just asks.
pub(super) fn frame_ticker() -> AnyView<SettingsState> // erasure: keep accumulator into Vec<AnyView>
{
    any(FrameTicker)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::features::settings::domain::entities::TokenScope;

    /// The redesign's own first token — the string the catalog moment was
    /// designed against.
    const NAME: &str = "zelli-mobile-100-71-31-57-mr3g6w5g";

    fn token() -> AuthToken {
        AuthToken {
            name: NAME.to_string(),
            scope: TokenScope::ReadWrite,
            created: "2026-07-02".to_string(),
        }
    }

    #[test]
    fn the_noise_is_reproducible_and_keeps_the_names_length() {
        let a = scramble_text(NAME, 3);
        let b = scramble_text(NAME, 3);
        assert_eq!(a, b, "the same frame always renders the same string");
        assert_eq!(
            a.chars().count(),
            NAME.chars().count(),
            "a scrambled name is the same length, so the row cannot reflow"
        );
        assert_ne!(a, NAME, "…and it is not simply the name back again");
        assert_ne!(
            scramble_text(NAME, 3),
            scramble_text(NAME, 7),
            "each frame is its own draw"
        );
    }

    #[test]
    fn characters_lock_left_to_right_as_the_frames_advance() {
        // The reference's own progression: nothing is locked at frame 0, and by
        // the last frame the leading `LOCK_SPAN` of the name is.
        assert_eq!(scramble_text(NAME, 0).matches('·').count(), 0);
        let locked = scramble_text(NAME, MAX_FRAMES as u32)
            .chars()
            .take_while(|ch| *ch == '·')
            .count();
        let expected = (NAME.chars().count() as f64 * LOCK_SPAN).ceil() as usize;
        assert_eq!(locked, expected, "the leading 40% is locked at the end");
    }

    #[test]
    fn the_timeline_scrambles_then_holds_then_collapses() {
        let start = pose(NAME, 0.0, false);
        assert!(start.glitching);
        assert_eq!(start.ratio, 1.0, "nothing collapses while it scrambles");
        assert!(start.running);

        // The 120ms hold between the last scramble frame and the collapse: the
        // row is still full height there.
        assert_eq!(pose(NAME, COLLAPSE_START_MS, false).ratio, 1.0);
        let half = pose(NAME, COLLAPSE_START_MS + COLLAPSE_MS / 2.0, false);
        assert!((half.ratio - 0.5).abs() < 1e-9, "{half:?}");
        assert!(half.running);

        let done = pose(NAME, TIMELINE_MS, false);
        assert_eq!(done.ratio, 0.0);
        assert!(!done.running, "the last frame of the collapse is the last");
        assert!(!pose(NAME, TIMELINE_MS * 4.0, false).running);
    }

    #[test]
    fn reduced_motion_settles_straight_to_the_end_state() {
        for elapsed in [0.0, COLLAPSE_START_MS, TIMELINE_MS] {
            let pose = pose(NAME, elapsed, true);
            assert_eq!(pose.ratio, 0.0, "at {elapsed}ms");
            assert!(pose.display.is_empty(), "no scramble frame is ever drawn");
            assert!(!pose.running, "…so no frame is ever asked for either");
        }
    }

    #[test]
    fn an_entry_is_spent_only_once_both_its_animation_and_its_rpc_are_done() {
        set_animation_clock_for_tests(Some(0.0));
        let animations = RevokeAnimations::new();
        animations.begin(&token(), 0);

        let entry = || animations.entries.get_untracked().remove(0);
        assert!(!entry().spent(false), "the RPC is still in flight");

        advance_animation_clock_for_tests(TIMELINE_MS + 1.0);
        assert!(
            !entry().spent(false),
            "a finished collapse still waits for the answer"
        );
        animations.answered(NAME);
        assert!(entry().spent(false));

        animations.sweep(false);
        assert!(animations.entries.get_untracked().is_empty());
        set_animation_clock_for_tests(None);
    }

    #[test]
    fn a_second_revoke_of_the_same_token_replaces_its_entry() {
        set_animation_clock_for_tests(Some(0.0));
        let animations = RevokeAnimations::new();
        animations.begin(&token(), 0);
        animations.answered(NAME);
        animations.begin(&token(), 1);

        let entries = animations.entries.get_untracked();
        assert_eq!(entries.len(), 1, "one row, one timeline");
        assert_eq!(entries[0].index, 1, "the newer snapshot wins");
        assert!(!entries[0].answered, "…and its RPC is in flight again");
        set_animation_clock_for_tests(None);
    }
}
