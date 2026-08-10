//! [`TerminalGridView`]/[`TerminalGridWidget`] — the retained grid: the
//! `View`/`Widget` pair that batches, shapes and paints the emulator's screen.
//!
//! # Three rules this widget is built around
//!
//! 1. **Shaping happens in `layout`, so a new generation must report
//!    `LAYOUT | PAINT`.** `layout` is the only pass carrying a `TextContext`,
//!    and a bare `PAINT` would leave the grid frozen under the mobile shells'
//!    intra-frame layout-skip gate — the exact defect the layout-skip idiom
//!    exists to catch. `paint` only replays what `layout` cached.
//! 2. **The widget requests no frames for content.** The feed pump's single
//!    signal write per wake (see the [parent module](super)) is the sole
//!    content wake source; a grid that called `request_frame` would repaint at
//!    panel rate forever and make the pump's whole coalescing mechanism moot.
//!    The one exception is the cursor blink, which is pixels-only and therefore
//!    requests the *paced* cosmetic-loop class the mobile frame gate may
//!    throttle.
//! 3. **The emulator lives behind an `Rc<RefCell<_>>`, never a signal.** Bytes
//!    arrive at up to 30 Hz; a signal write per chunk would wake a whole-app
//!    rebuild per chunk. The generation counter is the only signal, and it is
//!    written once per pump wake.
//!
//! # Cursor
//!
//! `docs/terminal.html` v2 blinks its caret, so this one does too — note the
//! Flutter incumbent's never did, which makes the blink a **new** behavior to
//! look at on a device rather than a port of one. Three shapes
//! ([`CursorStyle`], eventually fed from Settings' terminal-appearance group),
//! and two states:
//!
//! - **Focused** — filled in the palette's cursor ink, blinking on a
//!   [`CURSOR_BLINK_PERIOD_MS`] cycle, frozen lit under `reduce_motion` (a
//!   caret is a *position* cue: it must never disappear for a user who asked
//!   motion to stop). A focused block cursor re-draws the glyph under it in the
//!   background ink, so the character stays readable — that inverse glyph is
//!   shaped in `layout` with everything else, since `paint` cannot shape.
//! - **Unfocused** — a hollow outline of the cell, never blinking, never
//!   requesting a frame. The incumbent behavior, and every mainstream terminal's
//!   (Terminal.app, iTerm2, VTE): an unfocused terminal shows *where* the
//!   cursor is without pretending to be taking input.
//!
//! Focus is now two inputs ORed together: the plain `focused` builder (what a
//! screen passes when it knows), and the pod's own recorded focus path
//! (`PaintCtx::has_focus`), which is what a tap on the grid claims.
//!
//! # The grid is the IME client
//!
//! Given an [`InputSeam`] (and `read_only` clear), this widget *is* the
//! terminal's keystroke source — the one place `request_focus` and
//! `publish_ime_state` are called, because both are reachable only from inside
//! a `Widget` impl:
//!
//! - a pointer `Down` claims focus and publishes an
//!   [`ImeContentType::Terminal`] surface, which is what raises the soft
//!   keyboard and what tells both platforms to switch off autocorrect, smart
//!   quotes/dashes and autocapitalization;
//! - every focus-routed `Key`/`Ime` event is handed to the shared
//!   [`KeystrokeSource`](super::KeystrokeSource), whose bytes go straight to
//!   the feed through [`InputSeam::dispatch`] — see [`input`](super::input) for
//!   the diff engine, the latch and every rule they implement;
//! - the surface is **re-published after every handled event and on every
//!   focused paint**, exactly as `frust-widgets`' own `TextInput` does. That is
//!   deliberately idempotent: both mobile bridges compare the published state
//!   against their live mirror and do nothing when they already agree, so the
//!   only publication that actually costs a platform `restartInput` is the one
//!   following a window **re-anchor** (`Emission::reconciled`) — see
//!   `KeystrokeSource`'s own rolling-window docs.
//!
//! Without a seam — every call site before the input task, and every
//! `read_only` grid — the widget claims no focus, publishes nothing and emits
//! nothing: it is inert, exactly as it was.
//!
//! Two session-wide facts are **published onto the seam** from here rather
//! than kept as widget state, because the key bar shares that session and must
//! agree about both:
//!
//! - `read_only` (`build`/`rebuild` → [`InputSeam::set_read_only`]), so a
//!   read-only session is inert at the seam's own byte exit and not merely at
//!   this widget's `event` gate;
//! - the **scrub boundaries** for what the source has buffered
//!   ([`input`](super::input)'s "what is retained" docs): a blur
//!   ([`note_focus_edge`](TerminalGridWidget::note_focus_edge)), turning
//!   read-only, and this view's `teardown`.
//!
//! # Selection, the loupe, and who owns which half of the gesture
//!
//! Given a [`SelectionSeam`] (`.selection(..)`), the grid is also the selection
//! gesture's **hands**: it measures, it tracks the finger, and it paints. It is
//! not the gesture's *recogniser* — the enclosing
//! [`GestureDetector`](frust::GestureDetector) that
//! [`terminal_surface`](super::terminal_surface) wraps it in owns the timing and
//! fires [`SelectionSeam::begin`] at the threshold. The split matters because
//! the detector's `on_long_press` callback carries no pointer position: the grid
//! is what records where the finger is, so the callback only has to say *now*.
//!
//! - a `Down` **clears an existing selection before anything else** — a tap that
//!   dismisses a selection must not also claim focus and summon the keyboard;
//! - a `Move` during a live drag re-resolves both endpoints and repaints. No
//!   throttle, and no signal write: this is pixels, and a frust paint pass walks
//!   the whole tree, so a plain `request_redraw` also refreshes the Copy chip
//!   sitting above this widget;
//! - an `Up` ends the drag and anchors the chip.
//!
//! **Selection survives `read_only`.** The focus gate is the read-only gate for
//! *input*; reading and copying a session you cannot type into is exactly what
//! a read-only terminal is for, so the selection arms sit ahead of that gate.
//!
//! **Selection does not survive new output.** `build`/`rebuild` hand the
//! generation they carry to [`SelectionSeam::observe_generation`], which
//! drops a held selection (and its chip) the instant that generation moves —
//! see [`selection`](super::selection)'s module docs for why a rendered-screen
//! selection cannot safely retarget itself under streaming content instead.
//!

//! The **loupe** is a re-render, not a backdrop sample: a
//! [`LOUPE_W`]x[`LOUPE_H`] rounded window floating [`LOUPE_LIFT`] above the
//! finger, inside which the same runs are painted again under a
//! [`LOUPE_SCALE`] transform. Its one subtlety is [`loupe_content_offset`] —
//! when the window is clamped against an edge, the magnification is re-centred
//! on the true finger position rather than on the window, so the loupe keeps
//! showing what is under the finger even where the box cannot follow it.
//!
//! # The touch state machine: one `Down`, three outcomes
//!
//! Every pointer `Down` arms a three-way discrimination, resolved by what the
//! finger does next rather than by asking who wants the gesture. The threshold
//! is the framework's own [`TOUCH_SLOP`] — the same constant the enclosing
//! [`GestureDetector`](frust::GestureDetector) disarms its long-press at, so a
//! press can never be a scroll here and a long-press there:
//!
//! - **past the slop before the long-press threshold** → a **drag-scroll** for
//!   the rest of the gesture. Each `Move`'s `dy` accumulates, and every whole
//!   cell height of it emits one wheel step through
//!   [`on_wheel`](TerminalGridView::on_wheel) at the cell under the *current*
//!   finger, the remainder carried rather than dropped. A finger moving **down**
//!   reveals **earlier** lines, i.e. emits wheel-**up** steps — the incumbent's
//!   mapping.
//! - **held past the long-press threshold within the slop** → the selection
//!   above, verbatim. The detector fires
//!   [`SelectionSeam::begin`](super::SelectionSeam::begin) and every later phase
//!   is the selection's; this machine disarms on the first phase the selection
//!   answers `Handled`, so no step and no tap can come out of a gesture the
//!   selection is driving.
//! - **released within the slop before either** → a **content tap**: the tapped
//!   CELL, through [`on_content_tap`](TerminalGridView::on_content_tap).
//!
//! **The focus claim is invariant across all three.** The `Down` arm claims the
//! focus session exactly as it always did — before this machine can know which
//! outcome it is looking at — because frust-core reads *any* `Down` that
//! requested no focus as a blur (`RenderRoot::event`), so a claim deferred to
//! the tap branch would blur the grid on every drag and every hold. A content
//! tap is therefore reported **in addition to** the claim, never instead of it.
//!
//! Both hooks are **optional and report-only**: they name a cell and nothing
//! else (see the [parent module](super)' layout-blindness note), they survive
//! [`read_only`](TerminalGridView::read_only) exactly as the selection does —
//! the read-only gate is the *input* gate, and looking at a session you cannot
//! type into is what a read-only terminal is for — and a grid with neither
//! wired behaves precisely as it did before they existed: a tap is a focus
//! claim, and a scroll is ignored.
//!
//! A desktop `InputEvent::Scroll` folds into the **same** step machinery (one
//! step per wheel line, cell under the cursor), so there is one accumulator and
//! one hook rather than a second path that could drift from it.
//!
//! **Accepted limitation: no fling.** Steps stop when the finger stops — there
//! is no velocity phase carrying the scrollback on after the release. That
//! matches the incumbent, and a fling over a *remote* scrollback (every step is
//! an RPC round trip) is a decision to take with a device in hand.
//!
//! **Residual, recorded rather than papered over:** the long-press threshold is
//! wall time, and only the *detector* holds a clock — the event pass carries
//! none. So on the detector's rarer fire-on-release path (a hold released
//! within the same frame it crossed the threshold, before any rebuild delivered
//! the `Housekeeping` fire), this widget sees an in-slop `Up` it cannot tell
//! from a tap, and reports the cell *before* the detector's own callback
//! selects the word. The result is one extra content tap alongside a correct
//! selection, i.e. one redundant focus report on the pane the user is already
//! working in — not a lost or wrong gesture. Closing it would mean duplicating
//! the framework's own (unexported) long-press threshold as a second app-side
//! constant, which is a worse trade than the redundant report.
//!
//! # Not here
//!
//! No scrollback view, no key repeat, no selection handles, no double-tap-select
//! and no select-all — the incumbent has none of those, and neither does this.
//! An `InputEvent::Housekeeping` broadcast is still `Ignored` — a leaf has no
//! children to forward one to and must never consume one.

use std::rc::Rc;
use std::time::Duration;

use frust::authoring::text::{TextContext, TextLayout};
use frust::authoring::{
    Affine, BoxConstraints, Brush, BuildCtx, ChangeFlags, Color, EventCtx, EventResult, InputEvent,
    LayoutCtx, PaintCtx, PaintScene, Point, PointerEvent, PointerPhase, Rect, Role, RoundedRect,
    ScrollDelta, SemanticsCtx, Shape, Size, Vec2, View, Widget,
};
use frust::input::TOUCH_SLOP;
use frust::{FrameTime, Theme};

use super::batch::{RunBatch, batch_screen};
use super::geometry::{CellGeometry, measure_cell, solve_cell_geometry};
use super::selection::{self, GridPoint, Segment};
use super::style::{CellStyle, TerminalPalette};
use super::{ContentTapSink, CursorStyle, InputSeam, SelectionSeam, SharedEmulator, WheelSink};

/// The caret's blink period, in ms — one full on/off cycle.
///
/// Transcribed from `docs/terminal.html`'s own `@keyframes blink` (1.1s), the
/// same value the mock card's caret uses, so the real grid and the mock chrome
/// blink together rather than beating against each other.
pub const CURSOR_BLINK_PERIOD_MS: f64 = 1100.0;

/// The underline cursor's thickness, as a fraction of the cell height (floored
/// at one logical px so it never vanishes on a small grid).
const CURSOR_UNDERLINE_FRACTION: f64 = 0.12;
/// The bar cursor's thickness, as a fraction of the cell width (same floor).
const CURSOR_BAR_FRACTION: f64 = 0.16;
/// Stroke width of the unfocused hollow outline, in logical px.
const CURSOR_OUTLINE_WIDTH: f64 = 1.0;
/// Flattening tolerance for the outline's rect path.
const OUTLINE_TOLERANCE: f64 = 0.1;
/// Underline (SGR 4) stroke width, and its offset down the cell as a fraction
/// of the cell height — carried from the measured S9 renderer.
const TEXT_UNDERLINE_WIDTH: f64 = 1.0;
const TEXT_UNDERLINE_Y_FRACTION: f64 = 0.92;

/// Ceiling on the wheel steps one event may emit.
///
/// A guard, not a design limit: 256 lines is far past any real drag or wheel
/// notch, and the cap is what stops a synthetic or degenerate delta (a NaN-free
/// but enormous `dy`, a cell height measured near zero) from spinning the step
/// loop — the same shape of backstop [`geometry`](super::geometry)'s own
/// `MAX_CELLS_PER_AXIS` is.
const MAX_WHEEL_STEPS: f64 = 256.0;

/// Whether the caret is lit `elapsed_ms` into its blink cycle.
pub fn cursor_lit(elapsed_ms: f64) -> bool {
    (elapsed_ms % CURSOR_BLINK_PERIOD_MS) < CURSOR_BLINK_PERIOD_MS / 2.0
}

// ---------------------------------------------------------------------------
// The loupe (transcribed from the incumbent, exactly)
// ---------------------------------------------------------------------------

/// The magnifier window's width, in logical px.
pub const LOUPE_W: f64 = 116.0;
/// …and its height.
pub const LOUPE_H: f64 = 46.0;
/// How much the cells inside it are magnified.
pub const LOUPE_SCALE: f64 = 1.75;
/// How far above the finger the window's **bottom edge** floats — enough that
/// the hand holding the phone is not covering the thing it is magnifying.
pub const LOUPE_LIFT: f64 = 28.0;
/// The gutter the window is kept inside the terminal rect by.
pub const LOUPE_MARGIN: f64 = 4.0;
/// The window's corner radius.
const LOUPE_RADIUS: f64 = 4.0;
/// Its border width.
const LOUPE_BORDER: f64 = 1.0;
/// Its drop shadow's blur and ink alpha.
const LOUPE_SHADOW_BLUR: f64 = 6.0;
const LOUPE_SHADOW_ALPHA: f32 = 0.4;
/// Flattening tolerance for the window's rounded border path.
const LOUPE_TOLERANCE: f64 = 0.1;

/// The loupe window's box inside a `bounds`-sized terminal rect, for a finger at
/// `focal` (widget-local px).
///
/// Horizontally centred on the finger, floating [`LOUPE_LIFT`] above it, then
/// clamped to keep a [`LOUPE_MARGIN`] gutter on every side. Written
/// low-bound-last (`max` after `min`) so a rect too small to hold the window
/// pins it to the top-left rather than panicking on an inverted range.
pub fn loupe_box(focal: Point, bounds: Size) -> Rect {
    let x = (focal.x - LOUPE_W / 2.0)
        .min(bounds.width - LOUPE_MARGIN - LOUPE_W)
        .max(LOUPE_MARGIN);
    let y = (focal.y - LOUPE_LIFT - LOUPE_H)
        .min(bounds.height - LOUPE_MARGIN - LOUPE_H)
        .max(LOUPE_MARGIN);
    Rect::new(x, y, x + LOUPE_W, y + LOUPE_H)
}

/// **The subtle part.** How far the magnified content is shifted off the
/// window's own centre: `focal - centre_of(window)`.
///
/// While the window can follow the finger, this is `(0, LOUPE_LIFT + LOUPE_H/2)`
/// — no horizontal shift, and a constant vertical one, since the window floats
/// *above* the finger by design. Once a clamp pins the window against an edge —
/// a finger in a corner, which is precisely when a magnifier matters most — the
/// window stops moving but the finger keeps going, and re-deriving the offset
/// from the **clamped** centre is what keeps the cell under the finger in the
/// window instead of freezing on the last cell the window could centre on.
pub fn loupe_content_offset(focal: Point, window: Rect) -> Vec2 {
    focal.to_vec2() - window.center().to_vec2()
}

/// The transform that paints the grid magnified into `window`: the content point
/// `focal` lands on the window's centre, scaled [`LOUPE_SCALE`] about it.
///
/// Both points are **absolute** (paint-space), like every other coordinate a
/// `PaintScene` method takes. Equivalent to translating the content by
/// [`loupe_content_offset`] and scaling about the window's centre — expressed as
/// one composition here so there is a single place the arithmetic lives.
fn loupe_transform(focal: Point, window: Rect) -> Affine {
    Affine::translate(window.center().to_vec2())
        * Affine::scale(LOUPE_SCALE)
        * Affine::translate(-focal.to_vec2())
}

/// Where the cursor is and what sits under it, captured during `layout` (the
/// only pass that can shape) so `paint` touches neither the emulator nor the
/// text engine.
struct CursorCell {
    row: u16,
    col: u16,
    /// The character under the cursor, re-shaped in the background ink for the
    /// focused block style. `None` when the cell is blank.
    inverse_glyph: Option<TextLayout>,
}

/// The declarative grid — see [`TerminalGridWidget`].
pub struct TerminalGridView {
    generation: u64,
    emulator: SharedEmulator,
    palette: TerminalPalette,
    font_size: f32,
    cursor: CursorStyle,
    focused: bool,
    label: String,
    input: Option<InputSeam>,
    read_only: bool,
    selection: Option<SelectionSeam>,
    on_content_tap: Option<ContentTapSink>,
    on_wheel: Option<WheelSink>,
}

/// Build a grid view over `emulator` at `generation` (see
/// [`TerminalGridView`]).
///
/// `generation` must be a **tracked** read of the pump's signal in the caller's
/// `build`: it is what makes a repaint happen at all.
pub fn terminal_grid(
    emulator: SharedEmulator,
    generation: u64,
    palette: TerminalPalette,
    font_size: f32,
) -> TerminalGridView {
    TerminalGridView {
        generation,
        emulator,
        palette,
        font_size,
        cursor: CursorStyle::default(),
        focused: false,
        label: String::new(),
        input: None,
        read_only: false,
        selection: None,
        on_content_tap: None,
        on_wheel: None,
    }
}

impl TerminalGridView {
    /// The caret shape (Settings' terminal-appearance group is the eventual
    /// source).
    #[must_use]
    pub fn cursor(mut self, cursor: CursorStyle) -> Self {
        self.cursor = cursor;
        self
    }

    /// Whether the grid holds keyboard focus — filled, blinking caret when it
    /// does; hollow outline when it does not.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// The grid's accessible name (the session/pane it shows).
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Make this grid the session's **keystroke source**: a tap claims focus and
    /// raises the keyboard, and every key/IME event becomes bytes on `seam`'s
    /// feed (see the [module docs](self)).
    ///
    /// Without one the grid is a display surface only — no focus, no IME
    /// surface, no emissions.
    #[must_use]
    pub fn input(mut self, seam: InputSeam) -> Self {
        self.input = Some(seam);
        self
    }

    /// A read-only grid: never an IME client, never an emitter, whatever seam
    /// it was given.
    ///
    /// The **focus gate is the read-only gate** (`TextInput`'s own rule): a
    /// widget that refuses focus is unreachable by `Key`/`Ime`, which are
    /// focus-routed, so nothing downstream needs a second guard. A read-only
    /// grid is inert, not a shield — it consumes nothing.
    #[must_use]
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Make this grid **selectable**: long-press-and-drag over `seam`, with the
    /// highlight and the loupe painted here (see the [module docs](self)).
    ///
    /// Ordinarily installed by [`terminal_surface`](super::terminal_surface),
    /// which also wires the gesture detector that starts the gesture and mounts
    /// the Copy chip that ends it — a grid given a seam but no detector tracks
    /// and paints, but nothing ever begins.
    ///
    /// Unlike [`input`](Self::input), this is **not** gated on
    /// [`read_only`](Self::read_only): copying out of a session you cannot type
    /// into is the point of a read-only terminal.
    #[must_use]
    pub fn selection(mut self, seam: SelectionSeam) -> Self {
        self.selection = Some(seam);
        self
    }

    /// Report the **cell** a content tap landed on (see the
    /// [module docs](self)' touch state machine).
    ///
    /// Fires on the tap branch only — a release within [`TOUCH_SLOP`] of the
    /// press, before a long-press or a drag-scroll claimed the gesture — and
    /// always **in addition to** the focus claim the `Down` already made, never
    /// instead of it. Not gated on [`read_only`](Self::read_only): pointing at
    /// a pane you cannot type into is still pointing at it.
    ///
    /// Without one, a tap claims focus and reports nothing, exactly as it did
    /// before this hook existed.
    #[must_use]
    pub fn on_content_tap(mut self, sink: ContentTapSink) -> Self {
        self.on_content_tap = Some(sink);
        self
    }

    /// Report one wheel step per line of scroll, at the cell it applies to.
    ///
    /// One hook for both sources: a touch drag past the slop (its `dy`
    /// accumulated against the cell height, finger-down meaning wheel-up) and a
    /// desktop [`InputEvent::Scroll`] (one step per wheel line, cell under the
    /// cursor). See the [module docs](self) for the accumulation and the
    /// direction mapping.
    ///
    /// Without one, a drag scrolls nothing and an `InputEvent::Scroll` stays
    /// `Ignored`, exactly as it was.
    #[must_use]
    pub fn on_wheel(mut self, sink: WheelSink) -> Self {
        self.on_wheel = Some(sink);
        self
    }
}

/// The pointer gesture in flight — the touch state machine's whole state (see
/// the [module docs](self)).
///
/// `Copy` so an event arm can read it by value and reassign in the same
/// expression, the shape frust's own gesture recogniser uses.
#[derive(Clone, Copy, Debug)]
enum Grab {
    /// No press this widget armed: nothing down, or a gesture the selection —
    /// or a `Down` outside the grid's own box — disarmed.
    Idle,
    /// A finger is down and still within [`TOUCH_SLOP`] of where it landed: a
    /// tap, a drag-scroll and a long-press selection are all still open.
    Armed {
        /// Where the press landed, in widget-local px.
        origin: Point,
    },
    /// The press crossed the slop before the long-press fired, so this gesture
    /// is a scroll for the rest of its life.
    Scrolling {
        /// The position the scroll accumulator has already consumed up to.
        last: Point,
    },
}

/// The retained grid: re-batches and re-shapes the emulator's screen on every
/// new generation, then paints one background rect and one glyph run per
/// batched run, plus the caret. See the [module docs](self).
pub struct TerminalGridWidget {
    generation: u64,
    emulator: SharedEmulator,
    palette: TerminalPalette,
    font_size: f32,
    cursor: CursorStyle,
    focused: bool,
    label: String,
    batch: RunBatch,
    /// Which generation `batch` was shaped from (`None` = never / invalidated).
    shaped: Option<u64>,
    /// The `(content box, font size)` the geometry was solved against, and the
    /// solution. Re-solved only when either changes.
    geometry: Option<(Size, f32, CellGeometry)>,
    cursor_cell: Option<CursorCell>,
    /// Frame clock epoch the current blink cycle started from (`None` until the
    /// first focused paint, and reset whenever focus changes so a freshly
    /// focused grid shows a lit caret immediately).
    blink_from: Option<FrameTime>,
    /// The keystroke source + feed this grid emits through, when it is one (see
    /// the [module docs](self)).
    input: Option<InputSeam>,
    read_only: bool,
    /// The selection gesture this grid is the hands of, when it is selectable
    /// (see the [module docs](self)).
    selection: Option<SelectionSeam>,
    /// The caret cell in **absolute** coordinates, captured during `paint` (the
    /// only pass that knows the grid's absolute origin — `EventCtx::origin` is
    /// parent-relative) and published as the platform's candidate-placement
    /// hint.
    caret_rect: Option<Rect>,
    /// Effective focus as of the last paint, for the blink-reset edge — and,
    /// now, for the blur scrub (see [`note_focus_edge`](Self::note_focus_edge)).
    was_focused: bool,
    /// The seam's focus claim as of the last time this grid took focus. What
    /// tells a hand-off to the key bar from a real blur — see
    /// [`InputSeam::focus_claim`].
    focus_claim: u64,
    /// The pointer gesture in flight (see [`Grab`]).
    grab: Grab,
    /// The scroll accumulator's sub-step remainder, in **lines**: travel
    /// shorter than one cell height is carried to the next event rather than
    /// dropped, which is what makes a slow drag scroll smoothly instead of
    /// swallowing everything under a line. Reset at every press.
    scroll_carry: f64,
    /// Where a content tap is reported, when a screen asked for them.
    on_content_tap: Option<ContentTapSink>,
    /// Where wheel steps are reported, when a screen asked for them.
    on_wheel: Option<WheelSink>,
}

impl<State: 'static> View<State> for TerminalGridView {
    type Element = TerminalGridWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> TerminalGridWidget {
        // Seed the seam's last-seen generation before anything can be
        // selected — see `SelectionSeam::observe_generation`.
        if let Some(seam) = self.selection.as_ref() {
            seam.observe_generation(self.generation);
        }
        // The read-only posture belongs to the session, not to this widget:
        // published here so the key bar's caps are gated by the same flag
        // (see `InputSeam::set_read_only`, which also scrubs when it rises).
        if let Some(seam) = self.input.as_ref() {
            seam.set_read_only(self.read_only);
        }
        TerminalGridWidget {
            generation: self.generation,
            emulator: Rc::clone(&self.emulator),
            palette: self.palette,
            font_size: self.font_size,
            cursor: self.cursor,
            focused: self.focused,
            label: self.label.clone(),
            batch: RunBatch::default(),
            shaped: None,
            geometry: None,
            cursor_cell: None,
            blink_from: None,
            input: self.input.clone(),
            read_only: self.read_only,
            selection: self.selection.clone(),
            caret_rect: None,
            was_focused: self.focused,
            focus_claim: self.input.as_ref().map_or(0, InputSeam::focus_claim),
            grab: Grab::Idle,
            scroll_carry: 0.0,
            on_content_tap: self.on_content_tap.clone(),
            on_wheel: self.on_wheel.clone(),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut TerminalGridWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;

        // Anything that changes what is shaped forces LAYOUT — never a bare
        // PAINT (see the module docs' rule 1).
        let reshapes = prev.generation != self.generation
            || prev.font_size != self.font_size
            || prev.palette != self.palette
            || !Rc::ptr_eq(&prev.emulator, &self.emulator);
        if reshapes {
            element.generation = self.generation;
            element.font_size = self.font_size;
            element.palette = self.palette;
            element.emulator = Rc::clone(&self.emulator);
            element.shaped = None;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }

        // Cursor shape/focus are pixels only.
        if prev.cursor != self.cursor {
            element.cursor = self.cursor;
            flags |= ChangeFlags::PAINT;
        }
        if prev.focused != self.focused {
            element.focused = self.focused;
            // A freshly focused grid lights its caret at once rather than
            // inheriting whatever phase the last focus left behind.
            element.blink_from = None;
            flags |= ChangeFlags::PAINT;
        }
        if prev.label != self.label {
            element.label.clear();
            element.label.push_str(&self.label);
            // Semantics are re-collected after every layout/paint pass, so a
            // label change only needs the tree walked again.
            flags |= ChangeFlags::PAINT;
        }

        // The seam holds `Rc`s and a signal — cheap to clone and not usefully
        // comparable, so it is reinstalled every rebuild (the same rule
        // `ExpandButtonView` applies to its callback). Only the *presence* of a
        // seam and the read-only flag change what is painted or announced.
        let clienthood = prev.input.is_some() != self.input.is_some()
            || prev.read_only != self.read_only
            || prev.selection.is_some() != self.selection.is_some();
        element.input = self.input.clone();
        element.selection = self.selection.clone();
        // The two gesture hooks are reinstalled every rebuild for the same
        // reason, and change nothing painted or announced: an `Rc<dyn Fn>` is
        // two pointer bumps to clone and not comparable at all.
        element.on_content_tap = self.on_content_tap.clone();
        element.on_wheel = self.on_wheel.clone();
        if clienthood {
            element.read_only = self.read_only;
            flags |= ChangeFlags::PAINT;
        }
        // Republished every rebuild rather than only on the `clienthood` edge:
        // the seam outlives this widget's element (a screen may rebuild the
        // grid around it), and `set_read_only` is idempotent — only the
        // false→true edge does anything, and what it does is scrub.
        if let Some(seam) = self.input.as_ref() {
            seam.set_read_only(self.read_only);
        }

        // A new generation is new output — clear a held selection rather
        // than let it retarget under streaming content (see
        // `SelectionSeam::observe_generation`). A no-op both when the
        // generation didn't move (any other app-wide rebuild this screen
        // merely sits under) and when nothing is selected.
        if let Some(seam) = self.selection.as_ref() {
            seam.observe_generation(self.generation);
        }

        flags
    }

    /// The grid is going away: drop everything the session buffered.
    ///
    /// The **unmount scrub** (review M6). It lives here rather than on the
    /// screen's `on_cleanup` for a mechanical reason: `on_cleanup` takes a
    /// `Send + Sync` closure and every handle on this seam is `Rc`-backed, so
    /// the cleanup could never hold one. `ComponentWidget::teardown` tears the
    /// element tree down *before* it disposes the owner and drops `State`
    /// (frust's `docs/CODE_STANDARDS.md`), so this runs first on the same
    /// unmount — leaving the seam's own `Zeroizing` buffers to scrub again
    /// when the last `Rc` finally drops, whichever order that happens in.
    fn teardown(&self, element: &mut TerminalGridWidget, _ctx: &mut BuildCtx<'_>) {
        if let Some(seam) = element.input.as_ref() {
            seam.scrub();
        }
    }
}

impl Widget for TerminalGridWidget {
    /// Solve the geometry, resize the emulator to it, re-shape if anything
    /// changed, and take **exactly the grid's own box** (constrained by `bc`):
    /// at most one cell of the offered box goes unused per axis, and a parent
    /// forcing a tight size gets that instead, with the grid anchored top-left
    /// inside it and the leftover strip painted in the terminal's background.
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let available = bc.max();
        let geometry = match self.geometry {
            Some((solved_box, solved_size, geometry))
                if solved_box == available && solved_size == self.font_size =>
            {
                geometry
            }
            _ => {
                let geometry = solve_cell_geometry(available, self.font_size, |font_size| {
                    measure_cell(ctx, font_size)
                });
                self.geometry = Some((available, self.font_size, geometry));
                // Every cached run carries the old cell size.
                self.shaped = None;
                geometry
            }
        };

        // The grid the box divides into is what the emulator must hold: this is
        // where a rotation, a fullscreen toggle or a font-size change becomes a
        // real terminal resize.
        if self
            .emulator
            .borrow_mut()
            .resize(geometry.cols, geometry.rows)
        {
            self.shaped = None;
        }

        if self.shaped != Some(self.generation) {
            self.reshape(ctx, geometry);
            self.shaped = Some(self.generation);
        }

        // Publish the px -> cell mapping every selection resolution goes
        // through. This is the only pass that knows it.
        if let Some(seam) = self.selection.as_ref() {
            seam.set_metrics(geometry);
        }

        bc.constrain(geometry.grid_size())
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let Some((_, _, geometry)) = self.geometry else {
            // Painted before ever being laid out: nothing has been measured, so
            // there is nothing honest to draw.
            return;
        };
        let origin = ctx.origin();
        let size = ctx.size();

        // Effective focus: the plain builder input OR the pod's own recorded
        // focus path, which is what a tap on the grid claims. `has_focus` is
        // authoritative for a *container-routed* blur — one that clears the
        // pod's flag without ever reaching `event` — so a freshly blurred grid
        // stops blinking on the very next frame.
        let focused = self.focused || ctx.has_focus();
        if focused != self.was_focused {
            self.was_focused = focused;
            // A freshly focused grid lights its caret at once rather than
            // inheriting whatever phase the last focus left behind.
            self.blink_from = None;
            self.note_focus_edge(focused);
        }

        // The caret cell, in absolute coordinates, for the platform's
        // candidate-placement hint (see the field's own doc).
        self.caret_rect = self.cursor_cell.as_ref().and_then(|cell| {
            (cell.row < geometry.rows && cell.col < geometry.cols).then(|| {
                let x = origin.x + f64::from(cell.col) * geometry.width;
                let y = origin.y + f64::from(cell.row) * geometry.height;
                Rect::new(x, y, x + geometry.width, y + geometry.height)
            })
        });

        // Republish the IME surface every painted frame while focused, so a
        // platform mirror reseeded behind our back (a shell's steady-active
        // reconcile) converges on what this widget believes it holds. Idempotent
        // by construction — see the module docs.
        if let Some(seam) = self.input.as_ref()
            && !self.read_only
            && ctx.has_focus()
        {
            let state = seam.ime_state(self.caret_rect);
            ctx.publish_ime_state(state);
        }

        // The caret's blink bookkeeping runs exactly once per frame — before
        // the content is painted, so the loupe's own re-paint below reuses the
        // same answer rather than requesting a second frame for it (or letting
        // the caret blink out of phase with itself inside the magnifier).
        let lit = self.cursor_lit_now(ctx, focused);

        // The derived geometry already fits the content box, so nothing should
        // reach this clip — it stays as the backstop that makes "nothing
        // outside the grid can paint" true by construction (a font floored at
        // MIN_FONT_SIZE in a hopeless box is the one case that leans on it),
        // and it is now also what keeps the loupe inside the terminal rect.
        scene.push_clip(origin, size);
        scene.fill_rect(origin, size, self.palette.background);
        self.paint_content(scene, origin, geometry, focused, lit, None);
        self.paint_loupe(scene, origin, size, geometry, focused, lit);
        scene.pop_clip();
        // No `request_frame` for content: the feed pump's generation write is
        // the sole wake source (module docs, rule 2).
    }

    /// The keystroke source's event half, the selection gesture's hands **and**
    /// the touch state machine — see the [module docs](self).
    ///
    /// One dispatch per event class, each arm's own method carrying the
    /// ordering it owes the others; a grid with no seams and no hooks is inert
    /// exactly as it was.
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        match event {
            InputEvent::Pointer(pointer) => self.pointer_event(ctx, pointer),
            // The desktop wheel folds into the touch drag's own accumulator —
            // one choke point (module docs).
            InputEvent::Scroll { position, delta } => self.wheel_event(*position, *delta),
            InputEvent::Key(_) | InputEvent::Ime(_) => self.key_event(ctx, event),
            // Not user input — this widget queues no deferred callback for the
            // broadcast to run, so it simply falls through.
            InputEvent::Housekeeping => EventResult::Ignored,
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // One node for the whole grid, labelled with the session/pane it shows.
        // Deliberately NOT the screen's text as a value: a several-thousand
        // character string rebuilt 30 times a second is not an accessibility
        // feature, it is a leak with a screen reader attached. Announcing the
        // grid's *contents* is its own job (a live region), and its own task.
        ctx.push_node(Role::Terminal, |node| {
            if !self.label.is_empty() {
                node.set_label(self.label.as_str());
            }
            // A grid that cannot take input says so, rather than looking like
            // a live terminal to a screen reader.
            if self.read_only || self.input.is_none() {
                node.set_read_only();
            }
        });
    }
}

/// Whether a widget-local `position` is inside a `size`-sized box.
fn inside(position: Point, size: Size) -> bool {
    position.x >= 0.0 && position.y >= 0.0 && position.x < size.width && position.y < size.height
}

/// Whether two rects share any area — the loupe's cull test. `kurbo`'s own
/// `Rect::intersect` returns a (possibly negative-extent) rect rather than an
/// `Option`, so the comparison is spelled out here.
fn overlaps(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

impl TerminalGridWidget {
    /// Batch the emulator's current screen, shape every run, and capture the
    /// cursor cell (see [`CursorCell`]).
    ///
    /// The `Rc` is cloned out first so the `RefCell` borrow does not also
    /// borrow `self` (the batch lives in `self`).
    fn reshape(&mut self, ctx: &mut LayoutCtx, geometry: CellGeometry) {
        let emulator = Rc::clone(&self.emulator);
        let palette = self.palette;
        let (cursor_row, cursor_col, cursor_hidden, cursor_glyph) = {
            let emulator = emulator.borrow();
            let screen = emulator.screen();
            batch_screen(screen, &mut self.batch, |style| {
                style.resolve(&palette).1.is_none()
            });
            let (row, col) = screen.cursor_position();
            let under = screen
                .cell(row, col)
                .filter(|cell| cell.has_contents())
                .map(|cell| (CellStyle::from_cell(cell), cell.contents().to_string()));
            (row, col, screen.hide_cursor(), under)
        };

        // One `TextContext::layout` per run — one vello `draw_glyphs` per run
        // downstream. `None` max width: a run never wraps.
        let text_ctx = ctx.text_context::<TextContext>();
        for run in self.batch.active_mut() {
            let (fg, _) = run.style().resolve(&palette);
            let style = run.style().text_style(geometry.font_size, fg);
            let layout = text_ctx.layout(run.text(), &style, None);
            run.set_layout(layout);
        }

        self.cursor_cell = (!cursor_hidden).then(|| {
            // The glyph under a focused block cursor, re-inked in the
            // background so it reads as inverted rather than disappearing.
            let inverse_glyph = cursor_glyph.map(|(style, text)| {
                let inverse = style.text_style(geometry.font_size, palette.background);
                text_ctx.layout(&text, &inverse, None)
            });
            CursorCell {
                row: cursor_row,
                col: cursor_col,
                inverse_glyph,
            }
        });
    }

    /// Every pointer phase, in the order the two owners must see it: the
    /// selection arms first (they work `read_only` and with no [`InputSeam`] at
    /// all), then the touch state machine.
    ///
    /// The hand-over point is `Handled`, not "the selection had an opinion":
    /// its arms answer `Ignored` for the phases it has nothing to do with (a
    /// `Move` with no drag live, an `Up` that ends nothing), and those are
    /// exactly the phases a scroll or a tap is made of.
    fn pointer_event(&mut self, ctx: &mut EventCtx, pointer: &PointerEvent) -> EventResult {
        if let Some(seam) = self.selection.clone()
            && let Some(result) = self.selection_event(ctx, pointer, &seam)
            && matches!(result, EventResult::Handled)
        {
            // A phase the selection owned — a clearing tap's `Down`, or any
            // phase of a live selection drag. Disarm rather than race it: no
            // content tap and no wheel step comes out of a gesture the
            // selection is driving.
            self.grab = Grab::Idle;
            return result;
        }
        match pointer.phase {
            PointerPhase::Down => self.press(ctx, pointer.position),
            PointerPhase::Move => self.drag(pointer.position),
            PointerPhase::Up => self.release(pointer.position),
            // Flags only — a `Cancel` arm never touches app state
            // (`docs/CODE_STANDARDS.md`'s Interaction Semantics), and a stolen
            // gesture reports neither a tap nor a step.
            PointerPhase::Cancel => {
                self.grab = Grab::Idle;
                EventResult::Ignored
            }
        }
    }

    /// A `Down`: arm the touch machine, then claim the focus session exactly as
    /// this widget always has — the claim is invariant across all three
    /// outcomes (see the [module docs](self)).
    fn press(&mut self, ctx: &mut EventCtx, at: Point) -> EventResult {
        // Armed only inside the grid's own box: the surface stack is
        // full-bleed, so a press on the sub-cell remainder strip is not a cell
        // and must not become one on release.
        let inside_grid = inside(at, ctx.size());
        self.grab = if inside_grid {
            Grab::Armed { origin: at }
        } else {
            Grab::Idle
        };
        self.scroll_carry = 0.0;

        // Cloned, not borrowed: the arm below writes `self.focus_claim` while
        // the seam is in hand, and the clone is two `Rc` bumps.
        let Some(seam) = self.input.clone() else {
            return EventResult::Ignored;
        };
        if self.read_only || !inside_grid {
            return EventResult::Ignored;
        }
        // Every tap re-claims focus and re-publishes `active: true`, so a
        // keyboard the user dismissed comes back on the next tap rather than
        // needing a blur first. The claim is recorded so the next blur edge can
        // tell a real blur from the key bar taking the surface (see
        // `note_focus_edge`).
        self.focus_claim = seam.claim_focus();
        ctx.request_focus();
        let state = seam.ime_state(self.caret_rect);
        ctx.publish_ime_state(state);
        ctx.request_redraw();
        EventResult::Handled
    }

    /// A `Move` the selection did not take: the slop test once, and scroll
    /// input from then on.
    fn drag(&mut self, at: Point) -> EventResult {
        match self.grab {
            // No press of ours: a desktop hover move, or a gesture the
            // selection (or the bounds check) already disarmed.
            Grab::Idle => EventResult::Ignored,
            Grab::Armed { origin } => {
                if (at - origin).hypot() <= TOUCH_SLOP {
                    return EventResult::Ignored;
                }
                // Past the slop before the long-press threshold, so this is a
                // scroll for the rest of the gesture. The accumulator starts at
                // the press **origin**, so the steps a drag emits count its
                // whole travel rather than only what is left after the slop.
                self.grab = Grab::Scrolling { last: origin };
                self.scroll_to(at)
            }
            Grab::Scrolling { .. } => self.scroll_to(at),
        }
    }

    /// Consume the drag from wherever the accumulator got to up to `at`.
    fn scroll_to(&mut self, at: Point) -> EventResult {
        let Grab::Scrolling { last } = self.grab else {
            return EventResult::Ignored;
        };
        self.grab = Grab::Scrolling { last: at };
        let Some(height) = self.line_height() else {
            return EventResult::Ignored;
        };
        self.wheel_steps((at.y - last.y) / height, at)
    }

    /// An `Up` the selection did not take: the tap branch.
    fn release(&mut self, at: Point) -> EventResult {
        let tapped = matches!(
            self.grab,
            Grab::Armed { origin } if (at - origin).hypot() <= TOUCH_SLOP
        );
        self.grab = Grab::Idle;
        if !tapped {
            return EventResult::Ignored;
        }
        let (Some(sink), Some(cell)) = (self.on_content_tap.clone(), self.cell_at(at)) else {
            return EventResult::Ignored;
        };
        sink(u32::from(cell.col), u32::from(cell.row));
        EventResult::Handled
    }

    /// The desktop wheel, through the **same** step machinery a touch drag goes
    /// through.
    ///
    /// A line delta already *is* steps; a precision (trackpad) delta is pixels
    /// and divides by the cell height exactly like a finger's `dy`. Either way
    /// positive `y` means the content moves **down** — earlier lines revealed —
    /// which is the same thing a finger moving down means, so both reach
    /// [`wheel_steps`](Self::wheel_steps) with one sign convention.
    fn wheel_event(&mut self, position: Point, delta: ScrollDelta) -> EventResult {
        let lines = match delta {
            ScrollDelta::Lines(_, y) => y,
            ScrollDelta::Pixels(_, y) => {
                let Some(height) = self.line_height() else {
                    return EventResult::Ignored;
                };
                y / height
            }
        };
        self.wheel_steps(lines, position)
    }

    /// Fold `lines` of scroll into the accumulator and emit one step per whole
    /// line it crosses, at the cell under `at`, keeping the remainder.
    ///
    /// Positive `lines` is content moving **down** (a finger dragging down, a
    /// wheel rolling away from the hand): earlier lines are revealed, which is
    /// a wheel **up** step — the incumbent's mapping.
    fn wheel_steps(&mut self, lines: f64, at: Point) -> EventResult {
        if !lines.is_finite() || lines == 0.0 {
            return EventResult::Ignored;
        }
        let (Some(sink), Some(cell)) = (self.on_wheel.clone(), self.cell_at(at)) else {
            // Nothing to report to: the accumulator stays where it was rather
            // than banking travel a hook wired later would inherit.
            return EventResult::Ignored;
        };
        // Clamped, so a degenerate delta cannot spin the loop below.
        self.scroll_carry = (self.scroll_carry + lines).clamp(-MAX_WHEEL_STEPS, MAX_WHEEL_STEPS);
        let (col, row) = (u32::from(cell.col), u32::from(cell.row));
        let mut steps = 0u32;
        while self.scroll_carry.abs() >= 1.0 {
            let up = self.scroll_carry > 0.0;
            self.scroll_carry -= if up { 1.0 } else { -1.0 };
            sink(up, col, row);
            steps += 1;
        }
        if steps == 0 {
            EventResult::Ignored
        } else {
            EventResult::Handled
        }
    }

    /// The measured cell height, once `layout` has solved one and it is usable
    /// as a divisor.
    fn line_height(&self) -> Option<f64> {
        self.geometry
            .map(|(_, _, geometry)| geometry.height)
            .filter(|height| height.is_finite() && *height > 0.0)
    }

    /// The zero-based cell under a widget-local `at`, clamped to the grid —
    /// `None` before the first `layout` measured one.
    fn cell_at(&self, at: Point) -> Option<GridPoint> {
        self.geometry
            .map(|(_, _, geometry)| selection::cell_at(at, geometry))
    }

    /// One **focus-routed** event folded into the shared keystroke source (see
    /// the [module docs](self)).
    fn key_event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let Some(seam) = self.input.clone() else {
            return EventResult::Ignored;
        };
        if self.read_only || !ctx.has_focus() {
            return EventResult::Ignored;
        }
        // One shared fold, so the key bar's caps derive bytes exactly the same
        // way this widget does (see [`InputSeam::absorb`]).
        if seam.absorb(event).is_none() {
            return EventResult::Ignored;
        }
        // The publish is unconditional and idempotent: leaving a stale editing
        // state behind would make a shell's steady-active reconcile push the
        // *pre-keystroke* text back into the platform mirror.
        let state = seam.ime_state(self.caret_rect);
        ctx.publish_ime_state(state);
        ctx.request_redraw();
        EventResult::Handled
    }

    /// The selection half of `event` — see
    /// [`pointer_event`](Self::pointer_event) for the ordering. `Some(result)`
    /// when this phase was the selection's to answer, `None` to fall through.
    ///
    /// Deliberately **no** `capture_pointer` here: the enclosing
    /// [`GestureDetector`](frust::GestureDetector) that
    /// [`terminal_surface`](super::terminal_surface) wraps this grid in captures
    /// and forwards every `Move`/`Up` to its child unconditionally, so a second
    /// capture would only muddy which pod owns the gesture.
    fn selection_event(
        &mut self,
        ctx: &mut EventCtx,
        pointer: &PointerEvent,
        seam: &SelectionSeam,
    ) -> Option<EventResult> {
        match pointer.phase {
            PointerPhase::Down => {
                seam.track(pointer.position, true);
                // BEFORE anything else (see the module docs): a tap that
                // dismisses a selection must not also claim focus and summon
                // the keyboard.
                if seam.clear_if_active() {
                    ctx.request_redraw();
                    // This `Down` is `Handled` right here, but frust-core's
                    // root-level focus bookkeeping does not consult that: ANY
                    // pointer `Down` that requested no focus is read as a
                    // blur, regardless of `Handled` (`RenderRoot::event`).
                    // Left alone, the very next paint would see this widget's
                    // own focus edge flip true -> false and scrub the
                    // in-progress input baseline out from under a session
                    // that is plainly still being typed into.
                    //
                    // So this Down claims the seam's focus counter — the
                    // SAME hand-off mechanism a key-bar cap's own `Down` uses
                    // (`InputSeam::claim_focus`, `keybar.rs`) — without
                    // calling `ctx.request_focus()`: the counter moves, but
                    // `self.focus_claim` (this widget's own recorded
                    // snapshot) is deliberately left stale, so
                    // `note_focus_edge` reads the mismatch as a hand-off, not
                    // a departure, and skips the scrub. The keyboard is still
                    // not summoned — nothing here requests focus or
                    // publishes an IME surface, exactly as before.
                    if let Some(input) = self.input.as_ref() {
                        input.claim_focus();
                    }
                    return Some(EventResult::Handled);
                }
                None
            }
            PointerPhase::Move => {
                seam.track(pointer.position, true);
                if !seam.dragging() {
                    return Some(EventResult::Ignored);
                }
                // Unthrottled and signal-free: both endpoints re-resolve and the
                // frame repaints, which is also what refreshes the loupe.
                seam.extend();
                ctx.request_redraw();
                Some(EventResult::Handled)
            }
            PointerPhase::Up => {
                seam.track(pointer.position, false);
                if !seam.dragging() {
                    return Some(EventResult::Ignored);
                }
                seam.finish();
                ctx.request_redraw();
                Some(EventResult::Handled)
            }
            // Flags only — a `Cancel` arm never touches app state
            // (`docs/CODE_STANDARDS.md`'s Interaction Semantics). The loupe has
            // to go (it is a *gesture* affordance, and the gesture is over); the
            // selection it produced stays until the next tap clears it.
            PointerPhase::Cancel => {
                seam.track(pointer.position, false);
                seam.cancel_drag();
                ctx.request_redraw();
                Some(EventResult::Ignored)
            }
        }
    }

    /// One end of a focus edge, as `paint` observed it: the **blur scrub**
    /// (review M6), and the claim bookkeeping that keeps it honest.
    ///
    /// A blur is where buffered keystrokes stop being live, so it is a scrub
    /// boundary — but the seam's claim counter can move without the session
    /// actually stopping, in two cases, and neither is a blur:
    ///
    /// - a tap on a key bar cap takes the focus path away from this grid,
    ///   claiming the seam itself (`keybar.rs`'s own `Down` arm);
    /// - a tap on the grid *that this widget itself consumed* — the
    ///   selection Down arm's clearing tap (see `selection_event`). frust-core
    ///   reads any pointer `Down` that requested no focus as a blur
    ///   regardless of whether the widget reported it `Handled`
    ///   (`RenderRoot::event`), so a tap that only dismisses a selection
    ///   still forces this widget's own focus edge false on the very next
    ///   paint. That arm claims the seam right there, for exactly this
    ///   reason.
    ///
    /// Either way the same session is still being typed into, and scrubbing
    /// would silently cost the user their next Backspace (a delete is only
    /// observable as a shortening, and an emptied mirror has nothing to
    /// shorten). So the seam's claim counter is what distinguishes all three
    /// cases from a real blur: unchanged across the edge means nothing else
    /// took the surface.
    ///
    /// **Residual, recorded rather than papered over:** a cap holding the
    /// focus path when the keyboard is dismissed tears down with the bar and
    /// reports no blur of its own, so that path's scrub waits for the pane's
    /// teardown (or the grid's next real blur). Every path that *submits* a
    /// line already scrubs at the source, which is where a typed secret
    /// actually ends.
    fn note_focus_edge(&mut self, focused: bool) {
        let Some(seam) = self.input.clone() else {
            return;
        };
        if focused {
            self.focus_claim = seam.focus_claim();
        } else if seam.focus_claim() == self.focus_claim {
            seam.scrub();
        }
    }

    /// Whether the caret is lit this frame, doing the blink cycle's bookkeeping
    /// exactly once per paint.
    ///
    /// Split out of the drawing below so the loupe's re-paint reuses the answer:
    /// asking twice would request two paced frames and (worse) let the caret
    /// blink out of phase with itself inside the magnifier.
    fn cursor_lit_now(&mut self, ctx: &mut PaintCtx, focused: bool) -> bool {
        if !focused {
            return false;
        }
        if Theme::from_paint_ctx(ctx).is_some_and(|theme| theme.motion.reduce_motion) {
            // Frozen lit: a caret is a position cue, so it must not vanish for
            // a user who asked motion to stop.
            return true;
        }
        let from = *self.blink_from.get_or_insert(ctx.frame_time());
        let since = ctx.frame_time().saturating_sub(from).as_secs_f64() * 1000.0;
        // Paced at the blink's own half-period: a 1.1s two-state blink needs one
        // paint per visibility flip, not the cosmetic cap — the gate repaints the
        // full grid on every paced frame, so naming the interval is the
        // difference between ~2 and ~30 grid paints a second at focused idle.
        // (`cursor_lit` derives phase from the frame clock, so cadence never
        // shifts the flip points.)
        ctx.request_frame_paced_at(Duration::from_millis((CURSOR_BLINK_PERIOD_MS / 2.0) as u64));
        cursor_lit(since)
    }

    /// Paint the grid's cells, the selection wash and the caret at `origin`.
    ///
    /// `cull` (grid-local, i.e. relative to `origin`) drops everything outside a
    /// region — how the loupe re-paints a 116x46 window without submitting a
    /// whole screen of glyph runs the clip would only throw away. `None` paints
    /// everything.
    ///
    /// Takes `&self`: this is called **twice** per frame (once for the grid,
    /// once magnified inside the loupe), so it must be free of per-frame
    /// bookkeeping — see [`cursor_lit_now`](Self::cursor_lit_now).
    fn paint_content(
        &self,
        scene: &mut dyn PaintScene,
        origin: Point,
        geo: CellGeometry,
        focused: bool,
        lit: bool,
        cull: Option<Rect>,
    ) {
        for run in self.batch.active() {
            let x = f64::from(run.col()) * geo.width;
            let y = f64::from(run.row()) * geo.height;
            let width = f64::from(run.cells()) * geo.width;
            if let Some(cull) = cull
                && !overlaps(cull, Rect::new(x, y, x + width, y + geo.height))
            {
                continue;
            }
            let (x, y) = (origin.x + x, origin.y + y);
            let (fg, bg) = run.style().resolve(&self.palette);
            if let Some(bg) = bg {
                scene.fill_rect(Point::new(x, y), Size::new(width, geo.height), bg);
            }
            if let Some(layout) = run.layout() {
                for glyphs in layout.to_scene_runs(Point::new(x, y)) {
                    scene.draw_glyph_run(glyphs);
                }
            }
            if run.style().underline {
                let uy = y + geo.height * TEXT_UNDERLINE_Y_FRACTION;
                scene.stroke_line(
                    Point::new(x, uy),
                    Point::new(x + width, uy),
                    TEXT_UNDERLINE_WIDTH,
                    fg,
                );
            }
        }

        self.paint_selection(scene, origin, geo, cull);
        self.paint_cursor(scene, origin, geo, focused, lit);
    }

    /// Paint the selection wash: one rect per line-wise
    /// [`Segment`](super::selection::Segment), **over** the runs it covers (see
    /// [`style`](super::style)'s selection note for why over and not behind).
    fn paint_selection(
        &self,
        scene: &mut dyn PaintScene,
        origin: Point,
        geo: CellGeometry,
        cull: Option<Rect>,
    ) {
        let Some(range) = self.selection.as_ref().and_then(SelectionSeam::range) else {
            return;
        };
        for Segment { row, col, span } in selection::segments(range, geo.cols) {
            if row >= geo.rows {
                break;
            }
            let x = f64::from(col) * geo.width;
            let y = f64::from(row) * geo.height;
            let width = f64::from(span) * geo.width;
            if let Some(cull) = cull
                && !overlaps(cull, Rect::new(x, y, x + width, y + geo.height))
            {
                continue;
            }
            scene.fill_rect(
                Point::new(origin.x + x, origin.y + y),
                Size::new(width, geo.height),
                self.palette.selection,
            );
        }
    }

    /// Paint the caret at `origin` — filled and `lit` when focused, a hollow
    /// outline when not.
    fn paint_cursor(
        &self,
        scene: &mut dyn PaintScene,
        origin: Point,
        geo: CellGeometry,
        focused: bool,
        lit: bool,
    ) {
        let Some(cell) = &self.cursor_cell else {
            return;
        };
        if cell.row >= geo.rows || cell.col >= geo.cols {
            return;
        }
        let x = origin.x + f64::from(cell.col) * geo.width;
        let y = origin.y + f64::from(cell.row) * geo.height;

        if !focused {
            // Hollow outline, inset by half the stroke so the whole line lands
            // inside the cell. No blink, no frame request.
            let half = CURSOR_OUTLINE_WIDTH / 2.0;
            let path = Rect::new(half, half, geo.width - half, geo.height - half)
                .to_path(OUTLINE_TOLERANCE);
            scene.stroke_path(
                Point::new(x, y),
                &path,
                CURSOR_OUTLINE_WIDTH,
                &Brush::Solid(self.palette.cursor),
            );
            return;
        }
        if !lit {
            return;
        }

        let ink = self.palette.cursor;
        match self.cursor {
            CursorStyle::Block => {
                scene.fill_rect(Point::new(x, y), Size::new(geo.width, geo.height), ink);
                if let Some(glyph) = cell.inverse_glyph.as_ref() {
                    for glyphs in glyph.to_scene_runs(Point::new(x, y)) {
                        scene.draw_glyph_run(glyphs);
                    }
                }
            }
            CursorStyle::Underline => {
                let thickness = (geo.height * CURSOR_UNDERLINE_FRACTION).max(1.0);
                scene.fill_rect(
                    Point::new(x, y + geo.height - thickness),
                    Size::new(geo.width, thickness),
                    ink,
                );
            }
            CursorStyle::Bar => {
                let thickness = (geo.width * CURSOR_BAR_FRACTION).max(1.0);
                scene.fill_rect(Point::new(x, y), Size::new(thickness, geo.height), ink);
            }
        }
    }

    /// Paint the magnifier, if a selection drag is live.
    ///
    /// A **re-render**, not a backdrop sample: the same runs are painted again
    /// under a [`LOUPE_SCALE`] transform inside a rounded clip, so what the user
    /// sees is real text re-rasterized at 1.75x rather than a blown-up bitmap.
    /// Never intercepts input — this is paint only, and `event` knows nothing
    /// about it.
    fn paint_loupe(
        &self,
        scene: &mut dyn PaintScene,
        origin: Point,
        size: Size,
        geo: CellGeometry,
        focused: bool,
        lit: bool,
    ) {
        let Some(seam) = self.selection.as_ref() else {
            return;
        };
        if !seam.dragging() {
            return;
        }
        let focal = seam.pointer();
        let window = loupe_box(focal, size);
        let at = Point::new(origin.x + window.x0, origin.y + window.y0);
        let box_size = Size::new(window.width(), window.height());
        // Absolute (paint-space) counterparts: `push_transform` composes with
        // the scene's current transform, and every other coordinate here is
        // absolute too.
        let focal_abs = Point::new(origin.x + focal.x, origin.y + focal.y);
        let window_abs = window + Vec2::new(origin.x, origin.y);

        scene.draw_shadow(
            at,
            box_size,
            LOUPE_RADIUS,
            LOUPE_SHADOW_BLUR,
            Color::BLACK.with_alpha(LOUPE_SHADOW_ALPHA),
        );
        scene.push_clip_rounded(at, box_size, LOUPE_RADIUS);
        scene.fill_rect(at, box_size, self.palette.background);
        scene.push_transform(loupe_transform(focal_abs, window_abs));
        // The content window in grid-local coordinates: the box the magnified
        // view actually shows, always centred on the finger (that is what the
        // clamped-centre offset buys).
        let half = Vec2::new(
            window.width() / LOUPE_SCALE / 2.0,
            window.height() / LOUPE_SCALE / 2.0,
        );
        let cull = Rect::new(
            focal.x - half.x,
            focal.y - half.y,
            focal.x + half.x,
            focal.y + half.y,
        );
        self.paint_content(scene, origin, geo, focused, lit, Some(cull));
        scene.pop_transform();
        scene.pop_clip();

        // The border last, over the magnified content — inset by half the
        // stroke so the whole hairline lands inside the window.
        let half = LOUPE_BORDER / 2.0;
        let path = RoundedRect::new(
            half,
            half,
            window.width() - half,
            window.height() - half,
            LOUPE_RADIUS,
        )
        .to_path(LOUPE_TOLERANCE);
        scene.stroke_path(at, &path, LOUPE_BORDER, &Brush::Solid(self.palette.cursor));
    }

    /// The solved geometry, once `layout` has run — a test/diagnostic accessor.
    pub fn geometry(&self) -> Option<CellGeometry> {
        self.geometry.map(|(_, _, geometry)| geometry)
    }

    /// How many runs the last batch produced — a test/diagnostic accessor.
    pub fn run_count(&self) -> usize {
        self.batch.count()
    }

    /// Retained run slots — the allocation-reuse accessor (see `batch`).
    pub fn run_capacity(&self) -> usize {
        self.batch.capacity()
    }

    /// The generation the widget currently holds — a test accessor.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    //! The touch state machine, driven by synthesized pointer sequences.
    //!
    //! Deliberately **below** the mounted suites (`tests/term_widget.rs`,
    //! `tests/term_selection.rs`): those own the assembly — the real
    //! [`GestureDetector`](frust::GestureDetector), its wall-clock long-press
    //! and the Copy chip — while what has to be pinned here is the
    //! discrimination itself, which is a property of this widget's own `event`
    //! and is exercised one phase at a time. The one thing that costs is the
    //! long-press: the detector is what fires it, so a test stands in for the
    //! detector by calling [`SelectionSeam::begin`](super::SelectionSeam::begin)
    //! at the point the threshold would have, exactly as the detector's
    //! `on_long_press` does.

    use std::any::Any;
    use std::cell::RefCell;

    use frust::authoring::PointerButton;

    use crate::features::terminal::domain::feed::TerminalFeed;

    use super::super::{SharedFeed, TerminalEmulator};
    use super::*;

    /// The font size every test lays out at — the app's own terminal default.
    const FONT_SIZE: f32 = 13.0;

    /// A phone-shaped content box, in logical px (the OP9's safe-area box).
    const PHONE: Size = Size::new(392.7, 872.7);

    /// The screen every test points at: two lines of ordinary words, so a
    /// long-press has something to select and a tap has something to land on.
    const FIXTURE: &[u8] = b"cargo build release\r\nusr local bin";

    /// A feed that neither produces nor accepts anything — the grid's pointer
    /// half touches neither direction, and a session's bytes are not what these
    /// tests are about.
    struct NullFeed;

    impl TerminalFeed for NullFeed {
        fn drain_output(&mut self, _sink: &mut dyn FnMut(&[u8])) -> usize {
            0
        }

        fn send_input(&mut self, _bytes: &[u8]) {}

        fn resize(&mut self, _cols: u16, _rows: u16) {}
    }

    /// A built, laid-out grid plus every handle a test pokes it through.
    struct Probe {
        widget: TerminalGridWidget,
        /// The cell box `layout` solved — every coordinate below is derived
        /// from it rather than from an assumed font metric.
        geo: CellGeometry,
        /// `(col, row)` per content tap reported, in order.
        taps: Rc<RefCell<Vec<(u32, u32)>>>,
        /// `(up, col, row)` per wheel step reported, in order.
        steps: Rc<RefCell<Vec<(bool, u32, u32)>>>,
        input: InputSeam,
        selection: SelectionSeam,
    }

    impl Probe {
        /// A grid with both hooks wired.
        fn new() -> Self {
            Self::build(true, false)
        }

        /// …with neither wired: the pre-hook grid, bit for bit.
        fn unwired() -> Self {
            Self::build(false, false)
        }

        /// …with both wired over a read-only session.
        fn read_only() -> Self {
            Self::build(true, true)
        }

        fn build(hooks: bool, read_only: bool) -> Self {
            let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
            emulator.borrow_mut().feed(FIXTURE);
            let feed: SharedFeed = Rc::new(RefCell::new(NullFeed));
            let input = InputSeam::new(feed);
            let selection = SelectionSeam::new(Rc::clone(&emulator));
            let taps: Rc<RefCell<Vec<(u32, u32)>>> = Rc::new(RefCell::new(Vec::new()));
            let steps: Rc<RefCell<Vec<(bool, u32, u32)>>> = Rc::new(RefCell::new(Vec::new()));

            let mut view = terminal_grid(
                Rc::clone(&emulator),
                1,
                TerminalPalette::fallback(),
                FONT_SIZE,
            )
            .input(input.clone())
            .read_only(read_only)
            .selection(selection.clone());
            if hooks {
                let tapped = Rc::clone(&taps);
                let stepped = Rc::clone(&steps);
                view = view
                    .on_content_tap(Rc::new(move |col, row| {
                        tapped.borrow_mut().push((col, row))
                    }))
                    .on_wheel(Rc::new(move |up, col, row| {
                        stepped.borrow_mut().push((up, col, row));
                    }));
            }

            let mut counter = 0u64;
            let mut widget = View::<()>::build(&view, &mut BuildCtx::new(&mut counter));
            let mut text_ctx = TextContext::new();
            {
                let erased: &mut dyn Any = &mut text_ctx;
                let mut ctx = LayoutCtx::with_text_context(erased);
                widget.layout(&mut ctx, &BoxConstraints::loose(PHONE));
            }
            let geo = widget.geometry().expect("solved during layout");
            Probe {
                widget,
                geo,
                taps,
                steps,
                input,
                selection,
            }
        }

        /// Deliver one pointer phase at `at`, reporting what the grid did.
        fn pointer(&mut self, phase: PointerPhase, at: Point) -> EventResult {
            let event = InputEvent::Pointer(PointerEvent {
                phase,
                position: at,
                button: PointerButton::Primary,
            });
            self.dispatch(&event)
        }

        /// Deliver one desktop wheel event at `at`.
        fn scroll(&mut self, delta: ScrollDelta, at: Point) -> EventResult {
            self.dispatch(&InputEvent::Scroll {
                position: at,
                delta,
            })
        }

        fn dispatch(&mut self, event: &InputEvent) -> EventResult {
            let mut state = ();
            let mut ctx = EventCtx::new(&mut state, Point::ZERO, self.geo.grid_size());
            self.widget.event(&mut ctx, event)
        }

        /// The centre of cell `(col, row)`, in widget-local px.
        fn cell(&self, col: u16, row: u16) -> Point {
            Point::new(
                (f64::from(col) + 0.5) * self.geo.width,
                (f64::from(row) + 0.5) * self.geo.height,
            )
        }

        /// `n` cell heights, as a vertical offset.
        fn lines(&self, n: f64) -> Vec2 {
            Vec2::new(0.0, self.geo.height * n)
        }

        fn taps(&self) -> Vec<(u32, u32)> {
            self.taps.borrow().clone()
        }

        fn steps(&self) -> Vec<(bool, u32, u32)> {
            self.steps.borrow().clone()
        }
    }

    // -----------------------------------------------------------------------
    // The tap branch
    // -----------------------------------------------------------------------

    #[test]
    fn a_tap_reports_the_cell_it_landed_on_and_still_claims_the_focus_session() {
        let mut probe = Probe::new();
        let at = probe.cell(7, 3);
        let before = probe.input.focus_claim();

        assert_eq!(probe.pointer(PointerPhase::Down, at), EventResult::Handled);
        assert!(
            probe.input.focus_claim() > before,
            "the Down claims focus exactly as it always did — before anything \
             knows whether this gesture is a tap"
        );
        // No finger is perfectly still: a jitter inside the slop is still a tap.
        let lifted = at + Vec2::new(2.0, -3.0);
        probe.pointer(PointerPhase::Move, lifted);
        assert_eq!(
            probe.pointer(PointerPhase::Up, lifted),
            EventResult::Handled
        );

        assert_eq!(probe.taps(), vec![(7, 3)], "one tap, at the cell under it");
        assert!(probe.steps().is_empty(), "a tap is not a scroll");
    }

    #[test]
    fn a_read_only_session_still_reports_taps_and_steps() {
        // The read-only gate is the *input* gate: it stops keystrokes, not
        // looking at a pane (the app layer decides what a reported cell is
        // allowed to do with it).
        let mut probe = Probe::read_only();
        let at = probe.cell(2, 1);
        let before = probe.input.focus_claim();

        probe.pointer(PointerPhase::Down, at);
        assert_eq!(
            probe.input.focus_claim(),
            before,
            "a read-only grid still claims no focus"
        );
        probe.pointer(PointerPhase::Up, at);
        assert_eq!(probe.taps(), vec![(2, 1)]);

        probe.scroll(ScrollDelta::Lines(0.0, 1.0), at);
        assert_eq!(probe.steps(), vec![(true, 2, 1)]);
    }

    // -----------------------------------------------------------------------
    // The drag-scroll branch
    // -----------------------------------------------------------------------

    #[test]
    fn a_vertical_drag_emits_one_step_per_line_height_and_carries_the_remainder() {
        let mut probe = Probe::new();
        let start = probe.cell(4, 10);
        // 5.4 cell heights: five whole lines and a remainder that must survive
        // to the next Move rather than being dropped.
        let end = start + probe.lines(5.4);

        probe.pointer(PointerPhase::Down, start);
        assert_eq!(probe.pointer(PointerPhase::Move, end), EventResult::Handled);

        let steps = probe.steps();
        assert_eq!(steps.len(), 5, "floor(5.4) whole lines: {steps:?}");
        assert!(
            steps.iter().all(|(up, ..)| *up),
            "a finger moving DOWN reveals EARLIER lines, i.e. wheel-up: {steps:?}"
        );
        let cell = selection::cell_at(end, probe.geo);
        assert!(
            steps
                .iter()
                .all(|(_, col, row)| (*col, *row) == (u32::from(cell.col), u32::from(cell.row))),
            "every step names the cell under the CURRENT finger: {steps:?}"
        );

        // 0.6 more lines completes the sixth step — proof the 0.4 remainder was
        // retained across the two events.
        probe.pointer(PointerPhase::Move, end + probe.lines(0.6));
        assert_eq!(
            probe.steps().len(),
            6,
            "the sub-line remainder is carried, never dropped"
        );
        assert!(probe.taps().is_empty(), "a drag is not a tap");
    }

    #[test]
    fn a_drag_the_other_way_emits_wheel_down_steps() {
        let mut probe = Probe::new();
        let start = probe.cell(4, 20);
        probe.pointer(PointerPhase::Down, start);
        probe.pointer(PointerPhase::Move, start - probe.lines(3.0));

        let steps = probe.steps();
        assert_eq!(steps.len(), 3);
        assert!(
            steps.iter().all(|(up, ..)| !*up),
            "a finger moving UP reveals LATER lines, i.e. wheel-down: {steps:?}"
        );
    }

    #[test]
    fn a_movement_inside_the_slop_scrolls_nothing_at_all() {
        let mut probe = Probe::new();
        let start = probe.cell(6, 6);
        probe.pointer(PointerPhase::Down, start);
        // Half the slop, in the axis that scrolls: still a tap candidate.
        let jittered = start + Vec2::new(0.0, TOUCH_SLOP / 2.0);
        assert_eq!(
            probe.pointer(PointerPhase::Move, jittered),
            EventResult::Ignored
        );
        assert!(probe.steps().is_empty(), "the slop is the whole point");
        probe.pointer(PointerPhase::Up, jittered);
        assert_eq!(probe.taps().len(), 1, "and it is still a tap on release");
    }

    #[test]
    fn a_drag_past_the_slop_stays_a_scroll_even_when_the_finger_then_holds() {
        let mut probe = Probe::new();
        let start = probe.cell(2, 2);
        let dragged = start + probe.lines(2.0);

        probe.pointer(PointerPhase::Down, start);
        probe.pointer(PointerPhase::Move, dragged);
        assert_eq!(probe.steps().len(), 2);

        // A hold after the crossing is just another Move at the same place: the
        // gesture stays in scroll mode (it moved nothing, so it emits nothing),
        // and nothing arms a selection — the enclosing detector disarmed its own
        // long-press at the same slop.
        assert_eq!(
            probe.pointer(PointerPhase::Move, dragged),
            EventResult::Ignored
        );
        assert_eq!(probe.steps().len(), 2, "a stationary Move adds no step");
        assert!(
            probe.selection.range().is_none(),
            "no selection comes out of a scroll"
        );

        // …and the release is not a tap.
        assert_eq!(
            probe.pointer(PointerPhase::Up, dragged),
            EventResult::Ignored
        );
        assert!(probe.taps().is_empty());
    }

    // -----------------------------------------------------------------------
    // The long-press branch
    // -----------------------------------------------------------------------

    #[test]
    fn a_long_press_within_the_slop_selects_and_reports_neither_tap_nor_step() {
        let mut probe = Probe::new();
        // Inside "cargo" on the top row.
        let at = probe.cell(2, 0);
        probe.pointer(PointerPhase::Down, at);

        // What the enclosing GestureDetector does at the threshold — the grid
        // is the gesture's hands, never its recogniser.
        probe.selection.begin();
        assert!(probe.selection.dragging(), "the selection drag is live");

        // A drag that now crosses the slop belongs to the selection, not to the
        // scroll: it extends the range and emits no step.
        let dragged = at + Vec2::new(probe.geo.width * 6.0, 0.0);
        assert_eq!(
            probe.pointer(PointerPhase::Move, dragged),
            EventResult::Handled
        );
        assert_eq!(
            probe.pointer(PointerPhase::Up, dragged),
            EventResult::Handled
        );

        assert!(
            probe.selection.range().is_some(),
            "the long press selected, exactly as it did before"
        );
        assert!(probe.taps().is_empty(), "a long press is not a tap");
        assert!(probe.steps().is_empty(), "a long press is not a scroll");
    }

    #[test]
    fn a_tap_that_clears_a_selection_reports_no_cell() {
        // The clearing tap's `Down` is `Handled` by the selection arm, and a
        // dismissal must not also address a pane — the same reason it must not
        // summon the keyboard.
        let mut probe = Probe::new();
        let at = probe.cell(2, 0);
        probe.pointer(PointerPhase::Down, at);
        probe.selection.begin();
        probe.pointer(PointerPhase::Up, at);
        assert!(probe.selection.is_active(), "something is selected");
        assert!(
            probe.taps().is_empty(),
            "a perfectly stationary hold is not a tap either"
        );

        let elsewhere = probe.cell(9, 1);
        assert_eq!(
            probe.pointer(PointerPhase::Down, elsewhere),
            EventResult::Handled
        );
        probe.pointer(PointerPhase::Up, elsewhere);
        assert!(
            probe.taps().is_empty(),
            "the clearing tap reported no cell: {:?}",
            probe.taps()
        );
    }

    // -----------------------------------------------------------------------
    // The desktop wheel
    // -----------------------------------------------------------------------

    #[test]
    fn a_desktop_scroll_emits_one_step_per_wheel_line_at_the_cell_under_the_cursor() {
        let mut probe = Probe::new();
        let at = probe.cell(9, 6);

        assert_eq!(
            probe.scroll(ScrollDelta::Lines(0.0, 2.0), at),
            EventResult::Handled
        );
        assert_eq!(probe.steps(), vec![(true, 9, 6), (true, 9, 6)]);

        probe.steps.borrow_mut().clear();
        probe.scroll(ScrollDelta::Lines(0.0, -1.0), at);
        assert_eq!(probe.steps(), vec![(false, 9, 6)]);

        // A precision (trackpad) delta is pixels, and folds into the same
        // accumulator: half a line is not a step, two halves are.
        probe.steps.borrow_mut().clear();
        let half = probe.geo.height / 2.0;
        assert_eq!(
            probe.scroll(ScrollDelta::Pixels(0.0, half), at),
            EventResult::Ignored
        );
        assert!(probe.steps().is_empty(), "half a line is not a step yet");
        probe.scroll(ScrollDelta::Pixels(0.0, half), at);
        assert_eq!(probe.steps().len(), 1, "the two halves make one step");
    }

    // -----------------------------------------------------------------------
    // Unwired: the pre-hook grid, bit for bit
    // -----------------------------------------------------------------------

    #[test]
    fn a_grid_with_neither_hook_claims_focus_and_reports_nothing() {
        let mut probe = Probe::unwired();
        let at = probe.cell(3, 3);
        let before = probe.input.focus_claim();

        assert_eq!(
            probe.pointer(PointerPhase::Down, at),
            EventResult::Handled,
            "the focus claim is untouched by the hooks' absence"
        );
        assert!(probe.input.focus_claim() > before);
        assert_eq!(
            probe.pointer(PointerPhase::Move, at + probe.lines(4.0)),
            EventResult::Ignored,
            "no hook, no scroll"
        );
        assert_eq!(probe.pointer(PointerPhase::Up, at), EventResult::Ignored);
        assert_eq!(
            probe.scroll(ScrollDelta::Lines(0.0, 3.0), at),
            EventResult::Ignored,
            "an InputEvent::Scroll stays ignored, exactly as it was"
        );
        assert!(probe.taps().is_empty());
        assert!(probe.steps().is_empty());
    }

    #[test]
    fn a_press_outside_the_grids_own_box_is_never_a_tap() {
        // The surface stack is full-bleed, so a press can land on the sub-cell
        // remainder strip the grid does not own.
        let mut probe = Probe::new();
        let outside = Point::new(probe.geo.grid_width() + 4.0, probe.geo.grid_height() + 4.0);
        assert_eq!(
            probe.pointer(PointerPhase::Down, outside),
            EventResult::Ignored
        );
        probe.pointer(PointerPhase::Up, outside);
        assert!(probe.taps().is_empty());
    }
}
