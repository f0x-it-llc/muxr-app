//! A modal bottom sheet: [`show_sheet`] pushes one, [`sheet_scaffold`] is the
//! chrome it wraps a caller's body in.
//!
//! # This is a workaround, and it is meant to be deleted
//!
//! **Frust has no bottom-sheet primitive in the Glyph catalog** (FINDINGS #22).
//! `frust::glyph` ships a dialog, a command palette, a menu and a toast host —
//! no sheet — and the M3 `frust::bottom_sheet` is not compiled into this app
//! (`Cargo.toml` enables the `glyph` catalog only). So this module
//! hand-composes the sheet the same way `examples/glyph-catalog`'s
//! `pages/overlays.rs` does: a **transparent navigator page** carrying a
//! bottom-anchored panel clamped to [`MAX_WIDTH`] on wide layouts. **When
//! frust grows a Glyph sheet widget, delete this file and re-point
//! [`show_sheet`]'s four call sites at it** — the API is deliberately one
//! function taking a body builder so that swap is cheap. ([`ModalStage`] and
//! [`SCRIM_ALPHA`] would go with it: nothing outside this file stages on them
//! any more — see their own section.)
//!
//! # What the scaffold adds over the catalog's hand-rolled version
//!
//! `overlays.rs` stops at "a card pinned to the bottom" — enough for a demo
//! page, not for a modal. [`SheetScaffoldWidget`] is one retained widget (the
//! `frust::authoring` seam, like `crate::ui::backdrop`) so it can own the four
//! things a demo doesn't need:
//!
//! - a **scrim** (`colors.scrim` at [`SCRIM_ALPHA`]) it paints itself, and the
//!   modal barrier that goes with it — every pointer event that misses the
//!   panel is swallowed, and a press that both starts and ends outside the
//!   panel dismisses (the fire-on-up-inside convention, inverted; exactly
//!   `frust::glyph::dialog`'s scrim contract);
//! - the **panel rect**, which is what makes that hit test possible at all: a
//!   plain `Align`-inside-`Stack` composition has no widget that knows where
//!   the panel ended up, so a tap on the panel itself would fall through to
//!   the scrim and dismiss the sheet;
//! - the **bottom safe area**, read from `LayoutCtx::window_insets` and added
//!   to the panel's own bottom padding so a gesture-nav bar never overlaps the
//!   body;
//! - its own **enter/exit staging** — the reason this file no longer pushes a
//!   `PageTransition::SlideUp` page at all. See below.
//!
//! # The scrim fades; only the panel slides
//!
//! The sheet used to arrive as a whole page under
//! [`PageTransition::SlideUp`](frust::PageTransition::SlideUp), which
//! translates **everything** the page paints — scrim included. On device that
//! reads as a black rectangle wiping up the screen rather than the app dimming
//! behind a rising panel, and it is the one thing every platform's sheet gets
//! right (`frust::glyph::dialog` already does: "a single navigator `Layer`
//! cannot express *panel moves but scrim only fades*, so the staging lives in
//! the widget").
//!
//! So this widget takes the same road the Glyph dialog took. The page is
//! pushed with [`TransitionSpec::NONE`] — the navigator still supplies the
//! *modal contract* (input routed only to the top page, capture/focus/IME
//! threaded through its pod), the widget supplies the *motion*:
//!
//! - the **scrim** cross-fades `0 → SCRIM_ALPHA`, painted full-bleed at
//!   [`PaintCtx::origin`](frust::authoring::PaintCtx)/`size` on every frame, so
//!   its rect never translates;
//! - the **panel** slides its own height up from below the bottom edge, by
//!   offsetting its paint origin and its body pod together (the
//!   `crate::ui::topbar` reveal idiom — paint and hit-testing move as one);
//! - **`reduce_motion`** collapses the slide entirely: a linear cross-fade with
//!   no geometric motion, which is the framework's own hard rule (see
//!   [`ModalStage`]).
//!
//! Dismissal reverses both and only *then* pops: a scrim tap / `Escape` flips
//! the widget into its exit staging, and the pop is fired from `paint` when
//! that staging completes — sound because
//! [`NavigatorController::pop`] merely *enqueues* an op applied at the next
//! rebuild (`glyph::dialog`'s own documented contract, followed verbatim).
//!
//! **A body that pops the sheet itself skips that staging**, exactly as
//! `glyph::dialog`'s action buttons do and for the same reason: the Sessions
//! management sheet's rows pop the sheet *and* push a dialog (or a route) in
//! one rebuild, and a sheet sliding out under a dialog fading in reads as two
//! competing animations rather than one replacement. Only the **cancel**
//! routes — the scrim, `Escape` — play the exit.
//!
//! ## What an Android back press costs, and why
//!
//! A back press pops through the navigator's default `BackPolicy::Pop`, which
//! bypasses this widget entirely — so the sheet vanishes on that path instead
//! of playing its exit. `frust::glyph::dialog` avoids that with
//! `BackPolicy::DismissAnimated` plus a shared dismiss-signal cell, pushed
//! through `NavigatorController::push_with_options` — and **none of
//! `PushOptions`/`BackPolicy`/`push_with_options` is re-exported through the
//! `frust::` facade** (`frust::` lifts `push_transparent_for_result` and the
//! `TransitionSpec` vocabulary only), so an app-authored overlay cannot reach
//! that seam. Recorded as a framework gap rather than worked around: the
//! alternative (keeping a page-level fade purely so back has *something* to
//! play) would double-composite the scrim's own fade against the page's and
//! make the one number this file owns unreadable.
//!
//! # One sheet, one dismissal — [`SheetHandle`]
//!
//! A sheet stays interactive until its pop actually *lands*, and a pop lands a
//! rebuild after it is enqueued — so every dismissal path races every other
//! one. A second tap on a row, a close button pressed while a delayed close is
//! already pending, a scrim tap during that same window, an Android back press
//! mid-beat: each of them used to call [`NavigatorController::pop`] on its own,
//! and the second one through popped **the page under the sheet** — with the
//! sheet on the route navigator that was the route itself, which read to the
//! user as the app silently bouncing back a screen; on the overlay host it is
//! whatever overlay happens to be under it, or the app's own root page.
//!
//! So a sheet is not closed by whoever happens to be holding a navigator; it is
//! closed through [`SheetHandle`] — the identity [`show_sheet`] mints per sheet
//! *instance* and hands to its body builder. Every path goes through
//! [`SheetHandle::close`], which pops **at most once per instance** and is an
//! ordinary no-op after that: a duplicate close is a routine user race, not a
//! bug to panic on.
//! [`show_sheet`]'s own completion callback latches the same handle, so a close
//! still in flight when the page goes away some other way cannot pop anything
//! either, and [`SheetHandle::close_after`] is the delayed close ("let the
//! selection visibly move first") with latest-wins re-timing on top of the same
//! gate.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult, InputEvent, Key,
    LayoutCtx, NamedKey, PaintCtx, PaintScene, Point, PointerPhase, Rect, Role, SemanticsCtx, Size,
    View, Widget, any, build_child, rebuild_child, route_event_single, teardown_child,
};
use frust::motion::switcher::REDUCE_MOTION_DURATION;
use frust::{
    AnimationController, AnyView, Curve, FrameTime, NavigatorController, PopResult, Theme,
    TransitionSpec,
};

/// M3/Flutter bottom-sheet width convention: edge-to-edge on narrow screens,
/// capped and horizontally centered on wide ones (Material 3 gives modal
/// bottom sheets a 640dp max width; Flutter's `BottomSheet` constrains
/// likewise) — transcribed from `examples/glyph-catalog`'s
/// `pages/overlays.rs::SHEET_MAX_WIDTH`, the precedent this module follows.
pub const MAX_WIDTH: f64 = 640.0;

/// Scrim opacity behind a modal — the same value `frust::glyph::dialog`
/// picked for its modal barrier (heavier than M3's 32% so a Glyph panel reads
/// against the void surface); matched deliberately, so a sheet and a dialog
/// dim the app identically.
///
/// `pub` because it is the number the modal suites assert the painted scrim
/// against (`tests/ui_substrate.rs`): "the sheet is fully dimmed" has to be
/// spelled once, here, rather than transcribed into a test that would then pin
/// its own copy.
pub const SCRIM_ALPHA: f32 = 0.6;
/// Panel padding on the left/right/bottom edges, in logical px — Glyph's roomy
/// `--space-5` gutter (`glyph::dialog`'s `DIALOG_PADDING`). The top gutter is
/// the drag-handle zone instead (see [`handle_zone`]).
const PANEL_PAD: f64 = 20.0;
/// Drag-handle pill width/height, in logical px. An original, hand-picked
/// pair: the platform conventions agree closely (M3's drag handle is 32×4,
/// UIKit's sheet grabber ~36×5) and this sits between them.
const HANDLE_WIDTH: f64 = 36.0;
const HANDLE_HEIGHT: f64 = 4.0;
/// Gap above the handle (from the panel's top edge) and below it (to the
/// body), in logical px — Glyph's `--space-3`/`--space-4` steps.
const HANDLE_TOP: f64 = 12.0;
const HANDLE_GAP: f64 = 16.0;
/// The handle pill's ink: `on_surface_variant` faded, since no `ColorScheme`
/// role carries a "drag affordance" tone (the same alpha-derivation
/// `features::connect`'s `fg_dim` documents).
const HANDLE_ALPHA: f32 = 0.5;

/// Unthemed-fallback panel surface (Glyph dark `bg-raised` `#1e2430`) — a
/// theme resolves `colors.surface_container_high`, matching the dialog panel.
const FALLBACK_SURFACE: Color = Color::from_rgb8(0x1e, 0x24, 0x30);
/// Unthemed-fallback panel corner radius — Glyph's `--radius-lg` (14px); a
/// theme resolves `shape.large`.
const FALLBACK_RADIUS: f64 = 14.0;
/// Unthemed-fallback scrim base color; a theme resolves `colors.scrim`.
const FALLBACK_SCRIM: Color = Color::from_rgb8(0x00, 0x00, 0x00);
/// Unthemed-fallback handle ink (Glyph dark `fg-muted`); a theme resolves
/// `colors.on_surface_variant`.
const FALLBACK_HANDLE: Color = Color::from_rgb8(0xa3, 0x9c, 0x88);

/// The vertical band above the body: the panel's top gutter, the drag handle,
/// and the gap under it.
fn handle_zone() -> f64 {
    HANDLE_TOP + HANDLE_HEIGHT + HANDLE_GAP
}

// ---------------------------------------------------------------------------
// Shared modal staging (see the module docs' "scrim fades" section)
// ---------------------------------------------------------------------------
//
// [`ModalStage`] and [`SCRIM_ALPHA`] are this file's own now: the shell's top
// bar used to stage its modality tint on them (a second clock, mirroring a
// scrim it could not be covered by), and that whole mirror went with the root
// overlay host — the sheet's scrim is painted at the root and covers the bar
// itself. If frust grows a Glyph sheet and everything below is deleted, these
// two go with it rather than moving up to `crate::ui`.

/// Unthemed-fallback enter duration — Glyph's `durations.base`. Transcribed
/// from `frust::glyph::dialog`'s own fallback, which is the same token; only a
/// theme-less headless test ever reaches it (every real surface installs the
/// Glyph theme in `MuxrApp`'s `setup`).
const FALLBACK_ENTER: Duration = Duration::from_millis(220);
/// Unthemed-fallback exit duration — Glyph's `durations.fast` ("exits always
/// faster than entrances").
const FALLBACK_EXIT: Duration = Duration::from_millis(150);
/// Unthemed-fallback `spatial` easing — the Glyph curve, an authored
/// overshoot, and what the panel's slide is shaped by.
const FALLBACK_SPATIAL: Curve = Curve::Cubic(0.34, 1.35, 0.64, 1.0);
/// Unthemed-fallback `effects` easing — the Glyph curve for opacity, which
/// never overshoots, and what the scrim's fade is shaped by.
const FALLBACK_EFFECTS: Curve = Curve::Cubic(0.16, 1.0, 0.3, 1.0);
/// Unthemed-fallback `exit` easing — the Glyph accelerate-out curve, driving
/// *both* channels on the way out.
const FALLBACK_EXIT_CURVE: Curve = Curve::Cubic(0.4, 0.0, 1.0, 1.0);

/// One in-flight modal staging: a linear `0→1` driver plus the visual range it
/// maps onto and the easings each channel is shaped by.
///
/// The same shape (and for the same reason) as `crate::ui::topbar`'s own
/// `Reveal`: the range is what makes an *interrupted* staging value-continuous
/// — a dismissal mid-entrance starts a fresh driver from wherever the sheet was
/// actually painting, the "value continuity, not velocity continuity" contract
/// `frust::motion`'s implicit animations document. It differs in exactly one
/// way, deliberately: the bar's reveal runs **GlyphSlide's** timings (it is
/// matched to the page transition under it), while this runs the **modal**
/// timings `frust::glyph::dialog` resolves, because a sheet is a dialog's
/// sibling and not a page's.
///
/// Every timing is resolved **once, at the start**, and stays fixed for the
/// whole staging — `PatternSwitcher`'s rule, so a `reduce_motion` flip
/// mid-flight cannot produce a half-eased frame.
pub(crate) struct ModalStage {
    /// The `0→1` progress driver. Linear: the easing lives in
    /// [`Self::shape`], which needs raw progress to split its two channels.
    driver: AnimationController,
    /// The painted staging this transition started from.
    from: f64,
    /// The staging it is heading to (`1.0` showing, `0.0` dismissing).
    to: f64,
    /// The geometric channel's easing on the way in.
    spatial: Curve,
    /// The alpha channel's easing on the way in.
    effects: Curve,
    /// Both channels' easing on the way out.
    exit: Curve,
    /// Whether this collapsed to the reduced-motion fade.
    reduced: bool,
}

impl ModalStage {
    /// Stage a modal from `from` to `to` under `theme`'s motion policy.
    ///
    /// `reduce_motion` collapses the whole thing to the framework's own
    /// [`REDUCE_MOTION_DURATION`] linear fade — re-derived here rather than
    /// called, because neither `nav::transition::resolve_spec` nor
    /// `glyph::dialog`'s private `resolve_timings` is reachable through the
    /// `frust::`/`frust::authoring` facade an app widget is limited to
    /// (FINDINGS #41).
    pub(crate) fn start(from: f64, to: f64, theme: Option<&Theme>) -> Self {
        let showing = to > from;
        let reduced = theme.is_some_and(|theme| theme.motion.reduce_motion);
        let duration = if reduced {
            REDUCE_MOTION_DURATION
        } else if showing {
            theme.map_or(FALLBACK_ENTER, |theme| {
                Duration::from_secs_f64(theme.motion.durations.base / 1000.0)
            })
        } else {
            theme.map_or(FALLBACK_EXIT, |theme| {
                Duration::from_secs_f64(theme.motion.durations.fast / 1000.0)
            })
        };
        let mut driver = AnimationController::new(duration);
        driver.forward();
        ModalStage {
            driver,
            from,
            to,
            spatial: theme.map_or(FALLBACK_SPATIAL, |theme| theme.motion.easing.spatial),
            effects: theme.map_or(FALLBACK_EFFECTS, |theme| theme.motion.easing.effects),
            exit: theme.map_or(FALLBACK_EXIT_CURVE, |theme| theme.motion.easing.exit),
            reduced,
        }
    }

    /// Advance the driver to `now`; `true` while it is still moving.
    pub(crate) fn advance(&mut self, now: FrameTime) -> bool {
        self.driver.advance(now)
    }

    /// The staging value this frame — `from`→`to`, unshaped.
    pub(crate) fn value(&self) -> f64 {
        self.from + (self.to - self.from) * self.driver.value_clamped()
    }

    /// Where this staging settles.
    pub(crate) fn target(&self) -> f64 {
        self.to
    }

    /// Whether it is bringing the modal on screen.
    pub(crate) fn showing(&self) -> bool {
        self.to > self.from
    }

    /// The `(travelled, alpha)` pair to paint at staging value `value`:
    /// `travelled` is the fraction of the geometric journey completed (`1.0`
    /// at rest — and legitimately a little *past* `1.0` mid-entrance, since
    /// Glyph's `spatial` curve is an authored overshoot), `alpha` the opacity.
    ///
    /// An entrance eases the two channels apart (`spatial` for geometry,
    /// `effects` for opacity — Glyph's own vocabulary); an exit runs the
    /// single accelerate-out `exit` curve on both, measured from the
    /// still-shown state so that at `value == 1.0` nothing has moved yet.
    ///
    /// `reduce_motion` returns `travelled == 1.0` always — the modal is
    /// **already** where it belongs and only fades, linearly. That is the
    /// framework's hard accessibility rule, not a preference.
    pub(crate) fn shape(&self, value: f64) -> (f64, f32) {
        let p = value.clamp(0.0, 1.0);
        if self.reduced {
            return (1.0, p as f32);
        }
        if self.showing() {
            (
                self.spatial.transform(p),
                self.effects.transform(p).clamp(0.0, 1.0) as f32,
            )
        } else {
            let gone = self.exit.transform(1.0 - p).clamp(0.0, 1.0);
            (1.0 - gone, (1.0 - gone) as f32)
        }
    }
}

/// The at-rest `(travelled, alpha)` for a modal with no staging in flight:
/// fully arrived and opaque at `1.0`, fully gone and invisible at `0.0`.
fn settled_shape(shown: f64) -> (f64, f32) {
    if shown >= 1.0 { (1.0, 1.0) } else { (0.0, 0.0) }
}

/// Coerce a possibly-infinite constraint dimension to a finite value (mirrors
/// `glyph::dialog`'s helper of the same name).
fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

/// Push `build_body`'s content as a modal bottom sheet over the current page.
///
/// One call is the whole interaction: the panel slides up behind a fading
/// scrim, and dismisses on a scrim tap, `Escape`, or a back press.
/// `build_body` is re-invoked on every rebuild while the sheet is up (the
/// navigator's page-builder contract), so it may read signals freely.
///
/// Pushed with [`TransitionSpec::NONE`] on purpose — the navigator supplies
/// the modal contract, [`SheetScaffoldWidget`] supplies the motion. See the
/// [module docs](self)'s "The scrim fades; only the panel slides".
///
/// # The navigator it is pushed through
///
/// `nav` is what the page is pushed through (a sheet is a page *builder*, not a
/// path) — every caller in this app hands it the **root overlay host**'s
/// controller (`crate::AppState::overlays`), so the sheet covers the shell's
/// floating bar rather than sliding up under it, and that bar goes inert for
/// free (FINDINGS #44). It stays a plain parameter rather than a thread-local
/// read because `tests/ui_substrate.rs` drives this helper over a bespoke
/// navigator of its own.
///
/// # The body builder's [`SheetHandle`]
///
/// `build_body` is handed the sheet's own handle, and closing the sheet from
/// inside it is [`SheetHandle::close`] — never a bare `nav.pop()`, which would
/// be both unaccounted *and* ungated (see the [module docs](self)' "One sheet,
/// one dismissal"). The handle is minted here rather than returned because
/// every closer this app has lives in the body: a row that acts and dismisses,
/// a close button, the delayed close after a tab selection lands.
///
/// ```ignore
/// show_sheet(&state.overlays, move |sheet| {
///     let sheet = sheet.clone();
///     any(button("Done", move |_state: &mut AppState| {
///         sheet.close();
///     }))
/// });
/// ```
pub fn show_sheet<State: 'static>(
    nav: &NavigatorController<State>,
    build_body: impl Fn(&SheetHandle) -> AnyView<State> + 'static,
) {
    let pop_nav = nav.clone();
    // The one pop, behind the one gate.
    let sheet = SheetHandle::new(move || pop_nav.pop());
    let body_sheet = sheet.clone();
    let dismiss_sheet = sheet.clone();
    nav.push_transparent_for_result(
        move || {
            // The staged exit's own dismissal (a scrim tap, `Escape`, an
            // outside-press release) goes through the very same gate the
            // body's own closers do.
            let scrim_sheet = dismiss_sheet.clone();
            any(sheet_scaffold(build_body(&body_sheet)).on_dismiss(move || {
                scrim_sheet.close();
            }))
        },
        TransitionSpec::NONE,
        // The sheet answers through its body's own callbacks (which own
        // `&mut State` already); the pop itself carries nothing. What this
        // callback is for is the **latch**: the navigator delivers it exactly
        // once per pushed page, however that page went away (an action, a
        // scrim tap, `Escape`, an Android back press straight through
        // `BackPolicy::Pop`), so whichever way the page went, a close still in
        // flight must not land on whatever is under it now.
        move |_state: &mut State, _result: PopResult| sheet.mark_closed(),
    );
}

/// One live sheet's identity, and the one gate every dismissal path goes
/// through — see the [module docs](self)' "One sheet, one dismissal".
///
/// Cloneable and shared (`Rc`): the body builder is re-invoked on every
/// rebuild while the sheet is up, so each row, button and delayed close
/// captures its own clone of the *same* instance. Two sheets opened in
/// sequence are two handles, which is exactly the granularity the gate needs.
#[derive(Clone)]
pub struct SheetHandle {
    inner: Rc<SheetGuard>,
}

/// [`SheetHandle`]'s shared interior: the latch, the pending-close ticket, and
/// the accounted pop they gate.
struct SheetGuard {
    /// Whether this sheet instance has already been closed — set by the first
    /// [`SheetHandle::close`] and by [`SheetHandle::mark_closed`], never
    /// cleared (a sheet is closed exactly once, then the handle is spent).
    closed: Cell<bool>,
    /// The newest delayed close's ticket. [`SheetHandle::close_after`] takes
    /// one on the way in and fires only if it is still the newest when its
    /// delay elapses — "latest wins", so a second selection extends the wait
    /// rather than stacking a second close behind the first.
    pending: Cell<u64>,
    /// The one pop (`NavigatorController::pop`). Boxed (rather than the
    /// controller itself) so the handle stays free of the navigator's `State`
    /// parameter — a sheet body passes it around, and every one of those call
    /// sites would otherwise carry the app's state type for no reason.
    close: Box<dyn Fn()>,
}

impl SheetHandle {
    /// Mint a handle whose one dismissal runs `close`.
    fn new(close: impl Fn() + 'static) -> Self {
        SheetHandle {
            inner: Rc::new(SheetGuard {
                closed: Cell::new(false),
                pending: Cell::new(0),
                close: Box::new(close),
            }),
        }
    }

    /// Close the sheet: enqueue the pop — **at most once per sheet
    /// instance**.
    ///
    /// Returns whether *this* call is the one that closed it. A `false` is the
    /// guarded no-op every duplicate dismissal lands in (a second row tap
    /// inside the one-rebuild window a queued pop leaves open, a close button
    /// pressed while a delayed close is pending, a timer that woke after a
    /// scrim tap already took the sheet away), and it is ordinary rather than
    /// exceptional — hence a returned `bool` and not a `debug_assert!`, which
    /// would turn a double tap into a crash in every debug build.
    ///
    /// Skips the panel's exit staging, exactly as `glyph::dialog`'s action
    /// buttons do (see the [module docs](self)) — only the cancel routes play
    /// the slide back down, and those reach here through the scaffold's own
    /// `on_dismiss` once it has played.
    pub fn close(&self) -> bool {
        if self.inner.closed.replace(true) {
            return false;
        }
        (self.inner.close)();
        true
    }

    /// Close the sheet `delay` from now — the "let the selection visibly move
    /// first" close (`docs/terminal.html`'s `setTimeout(closeSheet, 260)`).
    ///
    /// **Latest wins.** Each call takes a fresh ticket, and only the newest
    /// ticket's wait actually closes: a second selection inside the window
    /// re-times the one pending close rather than queueing a second one behind
    /// it. The close itself still goes through [`close`](Self::close), so even
    /// a superseded or stale wait cannot pop twice.
    ///
    /// `async` rather than self-spawning on purpose: its one caller already
    /// runs inside a spawned task (it awaits the selection landing first), and
    /// awaiting keeps that ordering — "the selection has moved, *now* start
    /// the beat" — visible at the call site.
    pub async fn close_after(&self, delay: Duration) {
        let ticket = self.inner.pending.get() + 1;
        self.inner.pending.set(ticket);
        clean_signals::time::sleep(delay).await;
        if self.inner.pending.get() == ticket {
            self.close();
        }
    }

    /// Latch the handle without popping anything — the sheet's page has gone
    /// away through a route this app didn't drive (an Android back press
    /// straight through `BackPolicy::Pop`), or through one it did.
    ///
    /// [`show_sheet`]'s completion callback is where this runs, because that
    /// is the one seam the navigator delivers exactly once per pushed page,
    /// however it went away.
    fn mark_closed(&self) {
        self.inner.closed.set(true);
    }
}

/// The sheet chrome around `body`: scrim, bottom-anchored panel, drag handle,
/// safe-area padding. Wire [`on_dismiss`](SheetScaffoldView::on_dismiss) to
/// close it — [`show_sheet`] does that for you, and is what app code should
/// call.
pub fn sheet_scaffold<State: 'static>(body: AnyView<State>) -> SheetScaffoldView<State> {
    SheetScaffoldView {
        body,
        on_dismiss: None,
    }
}

/// A state-free "close this sheet" callback — the same shape (and for the same
/// reason) as `frust::glyph::dialog`'s: closing is a `NavigatorController::pop`,
/// which needs no app state.
type OnDismiss = Rc<dyn Fn()>;

/// The declarative half of the sheet chrome. See [`sheet_scaffold`].
pub struct SheetScaffoldView<State: 'static> {
    body: AnyView<State>,
    on_dismiss: Option<OnDismiss>,
}

impl<State: 'static> SheetScaffoldView<State> {
    /// Set the dismissal callback fired by a scrim tap, `Escape`, or an
    /// outside-press release. Without one the sheet is a barrier that only its
    /// own body can close.
    pub fn on_dismiss<F: Fn() + 'static>(mut self, on_dismiss: F) -> Self {
        self.on_dismiss = Some(Rc::new(on_dismiss));
        self
    }
}

/// The retained half of the sheet chrome. See [`sheet_scaffold`].
pub struct SheetScaffoldWidget {
    body: ChildPod,
    on_dismiss: Option<OnDismiss>,
    /// The panel's **resting** rect in this widget's own local coordinates —
    /// resolved in `layout`; `paint` and the barrier hit test both read it
    /// through [`Self::offset`].
    panel: Rect,
    /// The body pod's resting origin, recorded by `layout` so `paint` can
    /// slide the panel and its content together without a relayout per frame
    /// (the `crate::ui::topbar` rest-origin idiom).
    body_rest: Point,
    /// How far the sheet is staged in: `0.0` the panel entirely below the
    /// bottom edge behind a transparent scrim, `1.0` at rest.
    shown: f64,
    /// The panel's current paint offset along y, in logical px — positive
    /// while it is still below its resting place. Recorded by `paint` so the
    /// barrier hit test moves with what is on screen.
    offset: f64,
    /// The in-flight staging, `None` at rest. Created on the first **paint**
    /// after a staging is requested, not on the rebuild/event that requested
    /// it: neither a `BuildCtx` nor an `EventCtx` carries a theme to resolve
    /// the timings from (the deferred-driver idiom `frust::motion`'s switcher,
    /// the navigator and `crate::ui::topbar` all use).
    stage: Option<ModalStage>,
    /// A staging target awaiting that first paint.
    pending: Option<f64>,
    /// Whether the exit staging has completed and [`Self::on_dismiss`] has
    /// already fired — the pop is enqueued exactly once.
    dismissed: bool,
    /// A press is in flight (the barrier captured the pointer).
    captured: bool,
    /// Whether that press started outside the panel — only an outside press
    /// released outside dismisses.
    down_outside: bool,
}

impl<State: 'static> View<State> for SheetScaffoldView<State> {
    type Element = SheetScaffoldWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> SheetScaffoldWidget {
        SheetScaffoldWidget {
            body: build_child(&self.body, ctx),
            on_dismiss: self.on_dismiss.clone(),
            panel: Rect::ZERO,
            body_rest: Point::ZERO,
            // A sheet always arrives: mounting stages the entrance, which the
            // first paint starts once a theme is in scope.
            shown: 0.0,
            offset: 0.0,
            stage: None,
            pending: Some(1.0),
            dismissed: false,
            captured: false,
            down_outside: false,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut SheetScaffoldWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // Closures aren't comparable; reinstall the dismiss adapter every time.
        element.on_dismiss = self.on_dismiss.clone();
        rebuild_child(&prev.body, &self.body, &mut element.body, ctx)
    }

    fn teardown(&self, element: &mut SheetScaffoldWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.body, &mut element.body, ctx);
    }
}

impl SheetScaffoldWidget {
    /// Where the sheet is heading: a staged-but-unstarted target, the
    /// in-flight staging's, or — at rest — where it already is.
    fn heading_to(&self) -> f64 {
        self.pending
            .or_else(|| self.stage.as_ref().map(ModalStage::target))
            .unwrap_or(self.shown)
    }

    /// Begin the exit staging (a scrim tap, `Escape`, or an outside-press
    /// release). Idempotent: a sheet already on its way out — or already gone
    /// — stays on the one timeline it started, so a second tap cannot restart
    /// the exit or double-pop.
    ///
    /// The pop itself is **not** enqueued here; it fires from `paint` when the
    /// staging completes (see [`Self::close`] and the [module docs](self)).
    fn dismiss(&mut self) {
        if self.dismissed || self.heading_to() <= 0.0 {
            return;
        }
        self.pending = Some(0.0);
    }

    /// The panel rect where it is painting this frame — the resting rect
    /// displaced by the current slide offset.
    fn painted_panel(&self) -> Rect {
        Rect::new(
            self.panel.x0,
            self.panel.y0 + self.offset,
            self.panel.x1,
            self.panel.y1 + self.offset,
        )
    }

    /// Fire the dismissal callback (the page pop), exactly once.
    fn close(&mut self) {
        if self.dismissed {
            return;
        }
        self.dismissed = true;
        if let Some(on_dismiss) = &self.on_dismiss {
            on_dismiss();
        }
    }
}

impl Widget for SheetScaffoldWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let area_w = finite_or_zero(bc.max().width);
        let area_h = finite_or_zero(bc.max().height);

        // Edge-to-edge on a phone, clamped and centered past MAX_WIDTH.
        let panel_w = area_w.min(MAX_WIDTH);
        let content_w = (panel_w - 2.0 * PANEL_PAD).max(0.0);
        // Tight width (the body spans the panel), loose height (it decides how
        // tall the sheet is).
        let body_bc = BoxConstraints::new(
            Size::new(content_w, 0.0),
            Size::new(content_w, f64::INFINITY),
        );
        let body_size = self.body.layout_child(ctx, &body_bc);

        // The gesture-nav/home-indicator inset widens the bottom gutter rather
        // than replacing it, so the body never sits flush against the bar.
        let safe_bottom = ctx.window_insets().padding().bottom;
        // Never taller than the viewport, however tall the body asked to be.
        let panel_h = (handle_zone() + body_size.height + PANEL_PAD + safe_bottom).min(area_h);

        let panel_x = ((area_w - panel_w) / 2.0).max(0.0);
        let panel_y = (area_h - panel_h).max(0.0);
        self.panel = Rect::new(panel_x, panel_y, panel_x + panel_w, panel_y + panel_h);
        self.body_rest = Point::new(panel_x + PANEL_PAD, panel_y + handle_zone());
        self.body.set_origin(self.body_rest);

        bc.constrain(Size::new(area_w, area_h))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let radius = theme.map(|t| t.shape.large).unwrap_or(FALLBACK_RADIUS);
        let scrim_ink = theme.map(|t| t.scheme().scrim).unwrap_or(FALLBACK_SCRIM);
        let surface = theme
            .map(|t| t.scheme().surface_container_high)
            .unwrap_or(FALLBACK_SURFACE);
        let handle_ink = theme
            .map(|t| t.scheme().on_surface_variant)
            .unwrap_or(FALLBACK_HANDLE)
            .with_alpha(HANDLE_ALPHA);

        // Start a staged transition now a theme is in scope: the durations and
        // `reduce_motion` are paint-time reads (see the field docs).
        if let Some(target) = self.pending.take() {
            self.stage = Some(ModalStage::start(self.shown, target, theme));
        }
        // Advance it, and let the frame clock — not the rebuild — decide when
        // the sheet has arrived (or finished leaving).
        let mut closed = false;
        let (travelled, alpha) = match self.stage.as_mut() {
            None => settled_shape(self.shown),
            Some(stage) => {
                let animating = stage.advance(ctx.frame_time());
                self.shown = stage.value();
                let shaped = stage.shape(self.shown);
                if animating {
                    ctx.request_frame();
                } else {
                    self.shown = stage.target();
                    closed = !stage.showing();
                    self.stage = None;
                    // One more frame so the settled pose is the one on screen
                    // (and, on the way out, so the fully-gone frame paints
                    // before the pop's rebuild removes the page).
                    ctx.request_frame();
                }
                shaped
            }
        };

        // The panel travels its own height, so a fully-staged-out sheet sits
        // entirely below the bottom edge rather than merely displaced.
        let dy = (1.0 - travelled) * self.panel.height();
        self.offset = dy;

        // The modal barrier, full-bleed and under everything else. Painted at
        // the widget's own origin/size on **every** frame: the scrim fades, it
        // never translates (the whole point of this staging — see the module
        // docs).
        scene.fill_rect(
            ctx.origin(),
            ctx.size(),
            scrim_ink.with_alpha(SCRIM_ALPHA * alpha),
        );

        let panel_origin = Point::new(
            ctx.origin().x + self.panel.x0,
            ctx.origin().y + self.panel.y0 + dy,
        );
        // Only the TOP corners should look rounded: the panel is flush with
        // the bottom edge, so it is drawn `radius` taller than it is and the
        // bottom rounding falls off-screen. (`PaintScene` has no per-corner
        // radius; this is the cheap, standard way to get a top-rounded sheet.)
        // Mid-entrance Glyph's `spatial` easing overshoots *past* the resting
        // place, so the same overshoot is added to the drawn height — without
        // it a rising panel would tear a gap open under itself at the peak.
        let overshoot = (-dy).max(0.0);
        let faded = alpha < 1.0;
        if faded {
            scene.push_layer(ctx.origin(), ctx.size(), alpha);
        }
        scene.fill_rounded_rect(
            panel_origin,
            Size::new(self.panel.width(), self.panel.height() + radius + overshoot),
            radius,
            surface,
        );

        // The drag-handle affordance: centered pill near the panel's top edge.
        scene.fill_rounded_rect(
            Point::new(
                panel_origin.x + (self.panel.width() - HANDLE_WIDTH) / 2.0,
                panel_origin.y + HANDLE_TOP,
            ),
            Size::new(HANDLE_WIDTH, HANDLE_HEIGHT),
            HANDLE_HEIGHT / 2.0,
            handle_ink,
        );

        // Slide the body with the panel. Pod origins (rather than a
        // `push_transform`) for the same reason `crate::ui::topbar` stages its
        // slots that way: paint and hit-testing move together.
        self.body
            .set_origin(Point::new(self.body_rest.x, self.body_rest.y + dy));
        self.body.paint_child(ctx, scene);
        if faded {
            scene.pop_layer();
        }

        // Enqueue the pop only once the exit has actually played out.
        if closed {
            self.close();
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // Claim focus on EVERY Down: the root treats a Down that bubbles no
        // claim as a blur (`release_focus_session` drops focus + IME);
        // re-claiming while already focused is a change-guarded no-op. Do not
        // add a claim-once guard here — it kills the session on the second
        // tap (see frust claim-once-hygiene review, 2026-08-06).
        if let InputEvent::Pointer(p) = event
            && p.phase == PointerPhase::Down
        {
            ctx.request_focus();
        }
        // The body (or a captured/focused descendant of it) sees the event
        // first; only what it ignores reaches the barrier below.
        if route_event_single(&mut self.body, ctx, event) == EventResult::Handled {
            return EventResult::Handled;
        }
        if let InputEvent::Key(key_event) = event {
            if key_event.key == Key::Named(NamedKey::Escape) {
                self.dismiss();
                return EventResult::Handled;
            }
            return EventResult::Ignored;
        }
        let InputEvent::Pointer(p) = event else {
            return EventResult::Ignored;
        };
        // The panel where it is actually *painting* this frame, not where it
        // rests: mid-staging the two differ by the slide offset, and a hit
        // test against the resting rect would answer for a panel that isn't
        // there yet.
        let panel = self.painted_panel();
        match p.phase {
            PointerPhase::Down => {
                self.captured = true;
                self.down_outside = !panel.contains(p.position);
                ctx.capture_pointer();
                EventResult::Handled
            }
            PointerPhase::Move => {
                if self.captured {
                    EventResult::Handled
                } else {
                    EventResult::Ignored
                }
            }
            PointerPhase::Up => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                if self.down_outside && !panel.contains(p.position) {
                    self.dismiss();
                }
                self.captured = false;
                EventResult::Handled
            }
            // Cancel never touches state — just clear the barrier flag
            // (`docs/CODE_STANDARDS.md`'s Cancel rule).
            PointerPhase::Cancel => {
                self.captured = false;
                EventResult::Handled
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_container(
            Role::Dialog,
            |node| node.set_modal(),
            |ctx| {
                self.body.semantics_child(ctx);
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::SizedBox;
    use frust::authoring::{PointerButton, PointerEvent};
    use std::any::Any;

    #[test]
    fn the_handle_zone_is_the_sum_of_its_three_bands() {
        assert!((handle_zone() - (HANDLE_TOP + HANDLE_HEIGHT + HANDLE_GAP)).abs() < 1e-9);
    }

    #[test]
    fn derived_alphas_stay_legal() {
        for alpha in [SCRIM_ALPHA, HANDLE_ALPHA] {
            assert!((0.0..=1.0).contains(&alpha));
        }
    }

    // -----------------------------------------------------------------------
    // The dismissal gate
    // -----------------------------------------------------------------------

    /// A handle whose "pop" only counts itself — a bare
    /// `NavigatorController::pop` is what [`show_sheet`] installs here, so
    /// counting invocations *is* counting pops.
    fn counting_handle() -> (SheetHandle, Rc<Cell<usize>>) {
        let pops = Rc::new(Cell::new(0usize));
        let counted = Rc::clone(&pops);
        (
            SheetHandle::new(move || counted.set(counted.get() + 1)),
            pops,
        )
    }

    #[test]
    fn a_sheet_pops_once_however_many_times_it_is_told_to_close() {
        // The defect this gate exists for: the second close used to pop the
        // page *under* the sheet — the route itself.
        let (sheet, pops) = counting_handle();
        assert!(sheet.close(), "the first close is the one that closes it");
        assert!(!sheet.close(), "…and every later one is a no-op");
        assert!(!sheet.clone().close(), "…through any clone of the handle");
        assert_eq!(pops.get(), 1, "exactly one pop");
    }

    #[test]
    fn a_latched_handle_pops_nothing_at_all() {
        // The page went away some other way (an Android back press through
        // `BackPolicy::Pop`); a close still in flight must not land on
        // whatever is under it now.
        let (sheet, pops) = counting_handle();
        sheet.mark_closed();
        assert!(!sheet.close(), "the sheet is already gone");
        assert_eq!(pops.get(), 0);
    }

    // -----------------------------------------------------------------------
    // Every Down re-claims focus (no claim-once guard)
    // -----------------------------------------------------------------------

    fn ev(phase: PointerPhase, x: f64, y: f64) -> InputEvent {
        InputEvent::Pointer(PointerEvent {
            phase,
            position: Point::new(x, y),
            button: PointerButton::Primary,
        })
    }

    #[test]
    fn every_down_claims_focus_even_when_already_focused() {
        let body: AnyView<()> = any(SizedBox::<()>(Some(300.0), Some(200.0)));
        let view = SheetScaffoldView {
            body,
            on_dismiss: None,
        };
        let mut next_id = 0u64;
        let widget = view.build(&mut BuildCtx::new(&mut next_id));
        let mut pod = ChildPod::new(Box::new(widget));

        let area = Size::new(400.0, 800.0);
        let mut lctx = LayoutCtx::new();
        pod.layout_child(&mut lctx, &BoxConstraints::tight(area));

        let mut dummy = ();
        {
            let s: &mut dyn Any = &mut dummy;
            let mut ctx = EventCtx::new(s, Point::ZERO, area);
            pod.event_child(&mut ctx, &ev(PointerPhase::Down, 5.0, 5.0));
        }
        assert!(pod.is_focused(), "the first Down claims focus");
        {
            // The Down at (5, 5) lands well above the bottom-anchored panel,
            // so the matching Up dismisses (mirrors frust's own
            // `material::sheet` test) — that only clears `captured`.
            let s: &mut dyn Any = &mut dummy;
            let mut ctx = EventCtx::new(s, Point::ZERO, area);
            pod.event_child(&mut ctx, &ev(PointerPhase::Up, 5.0, 5.0));
        }

        // Force the pod out of focus the way an unrelated blur elsewhere in
        // the tree could — then prove the widget's *next* Down heals it. A
        // claim-once guard would leave this false forever after the first
        // Down; that silent, permanent loss is exactly the regression the
        // reverted guard reintroduced (frust claim-once-hygiene review,
        // 2026-08-06): the root treats an unclaimed Down as a blur signal and
        // tears down the whole focus/IME session on it.
        pod.set_focused(false);
        {
            let s: &mut dyn Any = &mut dummy;
            let mut ctx = EventCtx::new(s, Point::ZERO, area);
            pod.event_child(&mut ctx, &ev(PointerPhase::Down, 5.0, 5.0));
        }
        assert!(
            pod.is_focused(),
            "a second Down must re-claim focus even though it was externally cleared"
        );
    }

    #[test]
    fn the_panel_never_narrows_past_the_material_cap() {
        // A tripwire on the clamp's direction: the panel is `min(screen, cap)`,
        // so a phone-width screen is edge-to-edge and a tablet stops at the cap.
        assert_eq!(390.0f64.min(MAX_WIDTH), 390.0, "phone: edge to edge");
        assert_eq!(1024.0f64.min(MAX_WIDTH), MAX_WIDTH, "tablet: clamped");
    }

    // -----------------------------------------------------------------------
    // Modal staging
    // -----------------------------------------------------------------------

    /// The Glyph theme with `reduce_motion` forced either way.
    fn theme(reduce_motion: bool) -> Theme {
        let mut theme = Theme::glyph_baseline();
        theme.motion.reduce_motion = reduce_motion;
        theme
    }

    #[test]
    fn an_entrance_ends_flush_and_opaque_and_starts_gone_and_invisible() {
        let theme = theme(false);
        let stage = ModalStage::start(0.0, 1.0, Some(&theme));
        assert_eq!(stage.shape(1.0), (1.0, 1.0), "at rest: arrived, opaque");
        assert_eq!(stage.shape(0.0), (0.0, 0.0), "…and gone, invisible, at 0");
    }

    #[test]
    fn an_exit_runs_the_same_range_backwards() {
        let theme = theme(false);
        let stage = ModalStage::start(1.0, 0.0, Some(&theme));
        // Still flush at 1.0 (nothing has left yet), fully gone at 0.0 — the
        // exit is measured from the shown state, so the panel does not jump
        // the moment a dismissal is staged.
        assert_eq!(stage.shape(1.0), (1.0, 1.0));
        assert_eq!(stage.shape(0.0), (0.0, 0.0));
        assert!(!stage.showing());
    }

    #[test]
    fn reduced_motion_fades_the_sheet_without_moving_the_panel() {
        // Glyph's hard accessibility rule, at this seam: no geometric motion at
        // all, and a *linear* fade — in both directions.
        let theme = theme(true);
        for (from, to) in [(0.0, 1.0), (1.0, 0.0)] {
            let stage = ModalStage::start(from, to, Some(&theme));
            for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let (travelled, alpha) = stage.shape(p);
                assert_eq!(travelled, 1.0, "the panel never travels");
                assert_eq!(alpha, p as f32, "…it only fades, linearly");
            }
        }
    }

    #[test]
    fn the_scrims_alpha_is_the_only_thing_that_moves_it() {
        // The defect this staging exists for: the scrim's *coverage* is the
        // whole widget at every staging value (the paint uses `ctx.size()`
        // unconditionally), so only its alpha can differ frame to frame.
        let theme = theme(false);
        let stage = ModalStage::start(0.0, 1.0, Some(&theme));
        let mut last = -1.0f32;
        for p in [0.0, 0.2, 0.4, 0.6, 0.8, 1.0] {
            let (_, alpha) = stage.shape(p);
            assert!((0.0..=1.0).contains(&alpha), "alpha {alpha} out of range");
            assert!(alpha >= last, "the entrance fade never runs backwards");
            last = alpha;
        }
    }

    #[test]
    fn the_panels_entrance_overshoots_its_resting_place() {
        // Glyph's `spatial` curve is an authored overshoot, which is exactly
        // the bottom-sheet feel — and exactly why `paint` extends the drawn
        // panel by the overshoot rather than clamping it away.
        let theme = theme(false);
        let stage = ModalStage::start(0.0, 1.0, Some(&theme));
        let peak = [0.5, 0.6, 0.7, 0.8, 0.9]
            .into_iter()
            .map(|p| stage.shape(p).0)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(peak > 1.0, "the slide settles from above, peak {peak}");
    }

    #[test]
    fn an_exit_is_faster_than_the_entrance_it_reverses() {
        // "Exits always faster than entrances" — the Glyph rule the modal
        // timings inherit from `durations.fast` < `durations.base`.
        let motion = Theme::glyph_baseline().motion;
        assert!(motion.durations.fast < motion.durations.base);
    }

    #[test]
    fn a_settled_sheet_is_either_all_there_or_all_gone() {
        assert_eq!(settled_shape(1.0), (1.0, 1.0));
        assert_eq!(settled_shape(0.0), (0.0, 0.0));
    }

    #[test]
    fn the_unthemed_fallbacks_are_the_glyph_tokens_they_stand_in_for() {
        // A tripwire on the transcription: a headless frame with no theme
        // threaded must stage on the same numbers a real device does, or the
        // suites below would be measuring a different animation from the one
        // that ships.
        let motion = Theme::glyph_baseline().motion;
        assert_eq!(FALLBACK_ENTER.as_millis() as f64, motion.durations.base);
        assert_eq!(FALLBACK_EXIT.as_millis() as f64, motion.durations.fast);
        assert_eq!(FALLBACK_SPATIAL, motion.easing.spatial);
        assert_eq!(FALLBACK_EFFECTS, motion.easing.effects);
        assert_eq!(FALLBACK_EXIT_CURVE, motion.easing.exit);
    }
}
