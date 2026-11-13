//! The token-revoke animation: the name locks left to right through deterministic noise, then
//! the row collapses. Timing constants are in milliseconds, read off [`now_ms`], which tests
//! pin through [`set_animation_clock_for_tests`].

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

const FRAME_MS: f64 = 40.0;
const MAX_FRAMES: f64 = 14.0;
const COLLAPSE_DELAY_MS: f64 = 120.0;
const COLLAPSE_MS: f64 = 300.0;
const COLLAPSE_START_MS: f64 = MAX_FRAMES * FRAME_MS + COLLAPSE_DELAY_MS;
/// 560ms of scramble, a 120ms hold, then a 300ms collapse.
pub const TIMELINE_MS: f64 = COLLAPSE_START_MS + COLLAPSE_MS;

const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-#$%&";

const LOCK_SPAN: f64 = 0.4;

/// Zero duration: the opacity wrapper snaps to this module's ratio instead of re-easing it.
const ZERO: Timing = Timing::Duration(Duration::ZERO, Curve::Linear);

/// Numerical Recipes' LCG constants: a given `(frame, index)` always yields the same character.
fn lcg_next(seed: u32) -> u32 {
    seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)
}

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

thread_local! {
    static PINNED_CLOCK_MS: Cell<Option<f64>> = const { Cell::new(None) };
    static CLOCK_EPOCH: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// `None` restores the wall clock. Public, not `#[cfg(test)]`, since `tests/` links this build.
pub fn set_animation_clock_for_tests(now_ms: Option<f64>) {
    PINNED_CLOCK_MS.with(|pinned| pinned.set(now_ms));
}

/// With no clock pinned, this pins one at `delta_ms`.
pub fn advance_animation_clock_for_tests(delta_ms: f64) {
    PINNED_CLOCK_MS.with(|pinned| {
        pinned.set(Some(pinned.get().unwrap_or(0.0) + delta_ms));
    });
}

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

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ScramblePose {
    pub(super) display: String,
    pub(super) glitching: bool,
    /// The row's height and opacity factor, `1.0` to `0.0`.
    pub(super) ratio: f64,
    pub(super) running: bool,
}

/// Under `reduce` the row goes straight to its end state.
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

/// A snapshot, since the RPC's `refresh_tokens` routinely removes the token mid-animation.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct RevokeScramble {
    pub(super) token: AuthToken,
    pub(super) index: usize,
    pub(super) answered: bool,
    started_ms: f64,
}

impl RevokeScramble {
    pub(super) fn pose(&self, reduce: bool) -> ScramblePose {
        pose(
            &self.token.name,
            (now_ms() - self.started_ms).max(0.0),
            reduce,
        )
    }

    /// A collapsed row waits at zero height for the RPC answer, so a refusal restores it cleanly.
    fn spent(&self, reduce: bool) -> bool {
        self.answered && !self.pose(reduce).running
    }
}

/// Writes read through `try_get_untracked`: a revoke future can outlive the screen, and reading
/// a disposed signal panics where writing one no-ops. Only a build reads outright.
#[derive(Clone, Copy)]
pub(super) struct RevokeAnimations {
    entries: RwSignal<Vec<RevokeScramble>>,
}

impl RevokeAnimations {
    pub(super) fn new() -> Self {
        Self {
            entries: RwSignal::new(Vec::new()),
        }
    }

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

    /// The entry goes at the next sweep, so a fast answer never cuts the animation short.
    pub(super) fn answered(self, name: &str) {
        let Some(mut entries) = self.entries.try_get_untracked() else {
            return;
        };
        for entry in entries.iter_mut().filter(|entry| entry.token.name == name) {
            entry.answered = true;
        }
        self.entries.set(entries);
    }

    pub(super) fn live(self) -> Vec<RevokeScramble> {
        self.entries.get()
    }

    /// Bookkeeping only: `build` already skips spent entries. Runs from a `spawn_local`, never
    /// inline in the rebuild reading the signal.
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

pub(super) fn collapsing(ratio: f64, row: impl View<SettingsState>) -> impl View<SettingsState> {
    AnimatedOpacity(
        ratio,
        CollapseView {
            child: row,
            ratio: ratio.clamp(0.0, 1.0),
        },
    )
    .timing(ZERO)
}

struct CollapseView<V> {
    child: V,
    ratio: f64,
}

struct CollapseWidget {
    child: ChildPod,
    ratio: f64,
}

impl<State: 'static, V: View<State>> View<State> for CollapseView<V> {
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
        // The natural height, which a transcribed constant could only approximate.
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
        // The surrounding `AnimatedOpacity` layer clips the overflow; no second clip here.
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

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
        ctx.request_frame();
    }
}

pub(super) fn frame_ticker() -> AnyView<SettingsState> // erasure: keep accumulator into Vec<AnyView>
{
    any(FrameTicker)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::features::settings::domain::entities::TokenScope;

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
