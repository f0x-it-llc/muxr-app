//! [`TerminalGridView`]/[`TerminalGridWidget`] — the retained grid: the
//! `View`/`Widget` pair that batches, shapes and paints the emulator's screen.
//! Three rules it is built around:
//!
//! 1. **A new generation reports `LAYOUT | PAINT`, never bare `PAINT`.** Shaping
//!    happens in `layout` (the only pass carrying a `TextContext`); `paint` only
//!    replays what `layout` cached, so a bare `PAINT` would freeze the grid.
//! 2. **The widget requests no frames for content.** The feed pump's one signal
//!    write per wake is the sole content wake source; a `request_frame` here
//!    would repaint at panel rate forever and moot the coalescing. The one
//!    exception is the cursor blink (pixels-only), which requests the *paced*
//!    frame class the mobile frame gate may throttle.
//! 3. **The emulator lives behind an `Rc<RefCell<_>>`, never a signal** — the
//!    generation counter is the only signal, written once per pump wake.
//!
//! **The grid is the IME client** given an [`InputSeam`] with `read_only` clear —
//! the one place `request_focus`/`publish_ime_state` are called, deliberately not
//! at the same moment (press claims focus, the tap branch publishes). Given a
//! selection seam it's the gesture's *hands* (measure/track/paint) while the
//! enclosing [`GestureDetector`](frust::GestureDetector) owns timing. **One
//! `Down`, three outcomes** (tap / drag-scroll / long-press selection),
//! discriminated at [`TOUCH_SLOP`] so a press can't be a scroll here and a
//! long-press there. Accepted limitations: no fling (remote-scrollback fling is
//! an RPC-per-step decision deferred to device testing), and the detector's
//! fire-on-release path can report one redundant content tap for an in-slop
//! `Up`. Not implemented: scrollback view, key repeat, selection handles,
//! double-tap-select, select-all.

use std::rc::Rc;
use std::time::Duration;

use frust::authoring::text::{FontFamily, TextContext, TextLayout};
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

/// The caret's blink period, in ms — one full on/off cycle. Matches
/// `docs/terminal.html`'s `@keyframes blink` (1.1s) so the real grid and the
/// mock card blink in sync.
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
/// of the cell height.
const TEXT_UNDERLINE_WIDTH: f64 = 1.0;
const TEXT_UNDERLINE_Y_FRACTION: f64 = 0.92;

/// Ceiling on the wheel steps one event may emit. A guard, not a design limit:
/// 256 lines is far past any real drag, and the cap stops a degenerate delta (an
/// enormous `dy`, a cell height near zero) from spinning the step loop.
const MAX_WHEEL_STEPS: f64 = 256.0;

/// Whether the caret is lit `elapsed_ms` into its blink cycle.
pub fn cursor_lit(elapsed_ms: f64) -> bool {
    (elapsed_ms % CURSOR_BLINK_PERIOD_MS) < CURSOR_BLINK_PERIOD_MS / 2.0
}

// The loupe (transcribed from the incumbent, exactly).

/// The magnifier window's width, in logical px.
pub const LOUPE_W: f64 = 116.0;
/// …and its height.
pub const LOUPE_H: f64 = 46.0;
/// How much the cells inside it are magnified.
pub const LOUPE_SCALE: f64 = 1.75;
/// How far above the finger the window's **bottom edge** floats, clear of the
/// hand.
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
/// `focal` (widget-local px): centred on the finger, floating [`LOUPE_LIFT`]
/// above it, clamped to a [`LOUPE_MARGIN`] gutter. `max` after `min` so a rect
/// too small to hold the window pins it top-left rather than inverting the range.
pub fn loupe_box(focal: Point, bounds: Size) -> Rect {
    let x = (focal.x - LOUPE_W / 2.0)
        .min(bounds.width - LOUPE_MARGIN - LOUPE_W)
        .max(LOUPE_MARGIN);
    let y = (focal.y - LOUPE_LIFT - LOUPE_H)
        .min(bounds.height - LOUPE_MARGIN - LOUPE_H)
        .max(LOUPE_MARGIN);
    Rect::new(x, y, x + LOUPE_W, y + LOUPE_H)
}

/// **The subtle part.** How far the magnified content is shifted off the window's
/// own centre: `focal - centre_of(window)`.
///
/// A constant vertical shift while the window follows the finger (it floats
/// *above* it by design). Once a clamp pins the window against an edge — a
/// finger in a corner, exactly when a magnifier matters most — the window stops
/// but the finger keeps going; deriving the offset from the **clamped** centre
/// keeps the cell under the finger inside the window.
pub fn loupe_content_offset(focal: Point, window: Rect) -> Vec2 {
    focal.to_vec2() - window.center().to_vec2()
}

/// The transform that paints the grid magnified into `window`: the content point
/// `focal` lands on the window's centre, scaled [`LOUPE_SCALE`] about it. Both
/// points are **absolute** (paint-space), like every `PaintScene` coordinate.
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
    /// focused block style; `None` when blank.
    inverse_glyph: Option<TextLayout>,
}

/// The declarative grid — see [`TerminalGridWidget`].
pub struct TerminalGridView {
    generation: u64,
    emulator: SharedEmulator,
    palette: TerminalPalette,
    font_size: f32,
    font_family: FontFamily,
    cursor: CursorStyle,
    focused: bool,
    label: String,
    input: Option<InputSeam>,
    read_only: bool,
    selection: Option<SelectionSeam>,
    on_content_tap: Option<ContentTapSink>,
    on_wheel: Option<WheelSink>,
}

/// Build a grid view over `emulator` at `generation`, which must be a
/// **tracked** read of the pump's signal in the caller's `build` — that's what
/// makes a repaint happen at all. `font_family` starts at
/// `crate::fonts::default_terminal_family()`.
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
        font_family: crate::fonts::default_terminal_family(),
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
    /// The caret shape (Settings' terminal-appearance group is the source).
    #[must_use]
    pub fn cursor(mut self, cursor: CursorStyle) -> Self {
        self.cursor = cursor;
        self
    }

    /// The primary+fallback font stack to shape with, resolved by
    /// `crate::fonts::resolve_terminal_family`. Defaults to
    /// [`crate::fonts::default_terminal_family`], a real monospace stack.
    #[must_use]
    pub fn font_family(mut self, family: FontFamily) -> Self {
        self.font_family = family;
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
    /// feed. Without one, the grid is a display surface only.
    #[must_use]
    pub fn input(mut self, seam: InputSeam) -> Self {
        self.input = Some(seam);
        self
    }

    /// A read-only grid: never an IME client, never an emitter, whatever seam it
    /// was given. The **focus gate is the read-only gate** — a widget that
    /// refuses focus is unreachable by focus-routed `Key`/`Ime`, so nothing
    /// downstream needs a second guard. Inert, not a shield: it consumes nothing.
    #[must_use]
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Make this grid **selectable**: long-press-and-drag over `seam`, with the
    /// highlight and the loupe painted here. Ordinarily installed by
    /// [`terminal_surface`](super::terminal_surface), which also wires the
    /// detector that starts the gesture and the chip that ends it. Unlike
    /// [`input`](Self::input), **not** gated on [`read_only`](Self::read_only).
    #[must_use]
    pub fn selection(mut self, seam: SelectionSeam) -> Self {
        self.selection = Some(seam);
        self
    }

    /// Report the **cell** a content tap landed on, and ask whether that tap
    /// should raise the keyboard.
    ///
    /// Fires on the tap branch only (a release within [`TOUCH_SLOP`] of the
    /// press), always **in addition to** the focus claim the `Down` already
    /// made. Not gated on [`read_only`](Self::read_only) — pointing at a pane
    /// you can't type into is still pointing at it, though the publish it can
    /// trigger *is* gated. Without a sink, a tap claims focus and raises the
    /// keyboard.
    #[must_use]
    pub fn on_content_tap(mut self, sink: ContentTapSink) -> Self {
        self.on_content_tap = Some(sink);
        self
    }

    /// Report one wheel step per line of scroll, at the cell it applies to — one
    /// hook for both a touch drag past the slop and a desktop
    /// [`InputEvent::Scroll`]. Without one, a drag scrolls nothing.
    #[must_use]
    pub fn on_wheel(mut self, sink: WheelSink) -> Self {
        self.on_wheel = Some(sink);
        self
    }
}

/// The pointer gesture in flight — the touch state machine's whole state. `Copy`
/// so an event arm can read it by value and reassign in the same expression.
#[derive(Clone, Copy, Debug)]
enum Grab {
    /// No press this widget armed: nothing down, or disarmed by the selection or
    /// a `Down` outside the grid's box.
    Idle,
    /// A finger is down, still within [`TOUCH_SLOP`] of where it landed: tap,
    /// drag-scroll, and long-press selection are all still open.
    Armed {
        /// Where the press landed, in widget-local px.
        origin: Point,
    },
    /// The press crossed the slop before the long-press fired — a scroll for the
    /// rest of its life.
    Scrolling {
        /// The position the scroll accumulator has already consumed up to.
        last: Point,
    },
}

/// The retained grid: re-batches and re-shapes the emulator's screen on every new
/// generation, then paints one background rect and one glyph run per batched run,
/// plus the caret. See the [module docs](self).
pub struct TerminalGridWidget {
    generation: u64,
    emulator: SharedEmulator,
    palette: TerminalPalette,
    font_size: f32,
    /// The font stack every run and every measurement shapes with.
    font_family: FontFamily,
    cursor: CursorStyle,
    focused: bool,
    label: String,
    batch: RunBatch,
    /// Which generation `batch` was shaped from (`None` = never / invalidated).
    shaped: Option<u64>,
    /// The `(content box, font size)` the geometry was solved against, and the
    /// solution. Re-solved only when either changes, or
    /// [`geometry_family`](Self::geometry_family) no longer matches.
    geometry: Option<(Size, f32, CellGeometry)>,
    /// The font family [`geometry`](Self::geometry) was solved against — a
    /// sibling field, not part of that tuple, since `FontFamily` isn't `Copy`.
    /// `None` exactly when `geometry` is.
    geometry_family: Option<FontFamily>,
    cursor_cell: Option<CursorCell>,
    /// Frame clock epoch the current blink cycle started from. `None` until the
    /// first focused paint, and reset on a focus change so a freshly focused
    /// grid shows a lit caret immediately.
    blink_from: Option<FrameTime>,
    /// The keystroke source + feed this grid emits through, when it is one.
    input: Option<InputSeam>,
    read_only: bool,
    /// The selection gesture this grid is the hands of, when selectable.
    selection: Option<SelectionSeam>,
    /// The caret cell in **absolute** coordinates, captured during `paint` (the
    /// only pass that knows the grid's absolute origin) and published as the
    /// platform's candidate-placement hint.
    caret_rect: Option<Rect>,
    /// Effective focus as of the last paint, for the blink-reset edge and the
    /// blur scrub (see [`note_focus_edge`](Self::note_focus_edge)).
    was_focused: bool,
    /// The seam's focus claim as of the last time this grid took focus — tells a
    /// hand-off to the key bar apart from a real blur.
    focus_claim: u64,
    /// Whether this grid has an IME surface **up**: raised by a qualifying tap
    /// (or a keystroke, which implies one is already up), dropped only on a
    /// real blur. Gates the focused-paint republish — load-bearing, since
    /// `press`'s `request_focus` makes `has_focus()` true by the next frame, so
    /// gating on focus alone would raise the keyboard on every `Down`
    /// regardless of what the tap branch decided.
    ime_engaged: bool,
    /// The pointer gesture in flight (see [`Grab`]).
    grab: Grab,
    /// The scroll accumulator's sub-step remainder, in **lines**: travel
    /// shorter than one cell height carries to the next event instead of
    /// dropping, so a slow drag scrolls smoothly. Reset at every press.
    scroll_carry: f64,
    /// Where a content tap is reported, when a screen asked for them.
    on_content_tap: Option<ContentTapSink>,
    /// Where wheel steps are reported, when a screen asked for them.
    on_wheel: Option<WheelSink>,
}

impl<State: 'static> View<State> for TerminalGridView {
    type Element = TerminalGridWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> TerminalGridWidget {
        // Seed the seam's last-seen generation before anything can be selected.
        if let Some(seam) = self.selection.as_ref() {
            seam.observe_generation(self.generation);
        }
        // Read-only belongs to the session, published here so the key bar's
        // caps are gated by the same flag.
        if let Some(seam) = self.input.as_ref() {
            seam.set_read_only(self.read_only);
        }
        TerminalGridWidget {
            generation: self.generation,
            emulator: Rc::clone(&self.emulator),
            palette: self.palette,
            font_size: self.font_size,
            font_family: self.font_family.clone(),
            cursor: self.cursor,
            focused: self.focused,
            label: self.label.clone(),
            batch: RunBatch::default(),
            shaped: None,
            geometry: None,
            geometry_family: None,
            cursor_cell: None,
            blink_from: None,
            input: self.input.clone(),
            read_only: self.read_only,
            selection: self.selection.clone(),
            caret_rect: None,
            was_focused: self.focused,
            focus_claim: self.input.as_ref().map_or(0, InputSeam::focus_claim),
            ime_engaged: false,
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

        // Anything that changes what is shaped forces LAYOUT, never a bare
        // PAINT (rule 1) — a font-family change rides the same contract as a
        // font-size change; this only clears the shaped run cache.
        let reshapes = prev.generation != self.generation
            || prev.font_size != self.font_size
            || prev.font_family != self.font_family
            || prev.palette != self.palette
            || !Rc::ptr_eq(&prev.emulator, &self.emulator);
        if reshapes {
            element.generation = self.generation;
            element.font_size = self.font_size;
            element.font_family = self.font_family.clone();
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
            // A freshly focused grid lights its caret at once.
            element.blink_from = None;
            flags |= ChangeFlags::PAINT;
        }
        if prev.label != self.label {
            element.label.clear();
            element.label.push_str(&self.label);
            // Semantics are re-collected after every layout/paint pass.
            flags |= ChangeFlags::PAINT;
        }

        // Seams/hooks are `Rc`-backed: cheap to clone, not comparable, so they're
        // reinstalled every rebuild. Only *presence* changes what is painted.
        let clienthood = prev.input.is_some() != self.input.is_some()
            || prev.read_only != self.read_only
            || prev.selection.is_some() != self.selection.is_some();
        element.input = self.input.clone();
        element.selection = self.selection.clone();
        element.on_content_tap = self.on_content_tap.clone();
        element.on_wheel = self.on_wheel.clone();
        if clienthood {
            element.read_only = self.read_only;
            flags |= ChangeFlags::PAINT;
        }
        // Republished every rebuild, not just on the `clienthood` edge: the seam
        // outlives this element, and `set_read_only` is idempotent — only the
        // false→true edge does anything (scrub).
        if let Some(seam) = self.input.as_ref() {
            seam.set_read_only(self.read_only);
        }

        // A new generation is new output — clear a held selection rather than
        // let it retarget under streaming content. No-op when neither moved.
        if let Some(seam) = self.selection.as_ref() {
            seam.observe_generation(self.generation);
        }

        flags
    }

    /// The grid is going away: drop everything the session buffered — the
    /// **unmount scrub**. Lives here rather than on the screen's `on_cleanup`
    /// because `on_cleanup` needs `Send + Sync` and this seam is `Rc`-backed.
    /// `ComponentWidget::teardown` tears the element tree down *before*
    /// disposing the owner, so this runs first.
    fn teardown(&self, element: &mut TerminalGridWidget, _ctx: &mut BuildCtx<'_>) {
        if let Some(seam) = element.input.as_ref() {
            seam.scrub();
        }
    }
}

impl Widget for TerminalGridWidget {
    /// Solve the geometry, resize the emulator to it, re-shape if anything
    /// changed, and take **exactly the grid's own box** (constrained by `bc`):
    /// at most one cell of the offered box goes unused per axis; a parent
    /// forcing a tight size gets that instead, the grid anchored top-left with
    /// the leftover strip painted in the terminal's background.
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let available = bc.max();
        // `geometry_family` joins the box/size in the cache key: a different
        // family can measure a different cell advance at the identical size.
        let cache_hit = matches!(
            (&self.geometry, self.geometry_family.as_ref()),
            (Some((solved_box, solved_size, _)), Some(solved_family))
                if *solved_box == available
                    && *solved_size == self.font_size
                    && *solved_family == self.font_family
        );
        let geometry = if cache_hit {
            self.geometry
                .expect("cache_hit only true when self.geometry is Some")
                .2
        } else {
            let geometry = solve_cell_geometry(available, self.font_size, |font_size| {
                measure_cell(ctx, font_size, &self.font_family)
            });
            self.geometry = Some((available, self.font_size, geometry));
            self.geometry_family = Some(self.font_family.clone());
            // Every cached run carries the old cell size (or family).
            self.shaped = None;
            geometry
        };

        // The grid the box divides into is what the emulator must hold — where
        // a rotation, fullscreen toggle, or font-size change becomes a real
        // terminal resize.
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

        // Publish the px -> cell mapping; this is the only pass that knows it.
        if let Some(seam) = self.selection.as_ref() {
            seam.set_metrics(geometry);
        }

        bc.constrain(geometry.grid_size())
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let Some((_, _, geometry)) = self.geometry else {
            // Painted before ever being laid out: nothing measured, nothing
            // honest.
            return;
        };
        let origin = ctx.origin();
        let size = ctx.size();

        // Effective focus: the builder input OR the pod's own recorded focus
        // path. `has_focus` is authoritative for a *container-routed* blur (one
        // that clears the pod's flag without reaching `event`), so a freshly
        // blurred grid stops blinking on the very next frame.
        let focused = self.focused || ctx.has_focus();
        if focused != self.was_focused {
            self.was_focused = focused;
            // A freshly focused grid lights its caret at once.
            self.blink_from = None;
            self.note_focus_edge(focused);
        }

        // The caret cell, absolute, for the candidate-placement hint.
        self.caret_rect = self.cursor_cell.as_ref().and_then(|cell| {
            (cell.row < geometry.rows && cell.col < geometry.cols).then(|| {
                let x = origin.x + f64::from(cell.col) * geometry.width;
                let y = origin.y + f64::from(cell.row) * geometry.height;
                Rect::new(x, y, x + geometry.width, y + geometry.height)
            })
        });

        // Republish the IME surface every painted frame while focused **and
        // engaged**, so a platform mirror reseeded behind our back converges.
        // `ime_engaged` is load-bearing: `press` requests focus on every `Down`,
        // so gating on focus alone would put the keyboard back up behind the
        // tap branch's back.
        if let Some(seam) = self.input.as_ref()
            && !self.read_only
            && ctx.has_focus()
            && self.ime_engaged
        {
            let state = seam.ime_state(self.caret_rect);
            ctx.publish_ime_state(state);
        }

        // Blink bookkeeping runs exactly once per frame, before content paints,
        // so the loupe's re-paint reuses the same answer instead of requesting
        // a second frame or blinking out of phase with itself.
        let lit = self.cursor_lit_now(ctx, focused);

        // The derived geometry already fits the content box, so the clip is a
        // backstop — and keeps the loupe inside the rect.
        scene.push_clip(origin, size);
        scene.fill_rect(origin, size, self.palette.background);
        self.paint_content(scene, origin, geometry, focused, lit, None);
        self.paint_loupe(scene, origin, size, geometry, focused, lit);
        scene.pop_clip();
        // No `request_frame` for content: the pump's write is the wake (rule 2).
    }

    /// The keystroke source's event half, the selection gesture's hands, **and**
    /// the touch state machine — one dispatch per event class, each arm's own
    /// method carrying the ordering it owes the others.
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        match event {
            InputEvent::Pointer(pointer) => self.pointer_event(ctx, pointer),
            // The desktop wheel folds into the touch drag's own accumulator.
            InputEvent::Scroll { position, delta } => self.wheel_event(*position, *delta),
            InputEvent::Key(_) | InputEvent::Ime(_) => self.key_event(ctx, event),
            InputEvent::Housekeeping => EventResult::Ignored,
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // One node for the whole grid, labelled with the session/pane it shows.
        // Deliberately NOT the screen's text as a value: a several-thousand
        // character string rebuilt 30 times a second is a leak with a screen
        // reader attached, not an accessibility feature.
        ctx.push_node(Role::Terminal, |node| {
            if !self.label.is_empty() {
                node.set_label(self.label.as_str());
            }
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
    /// cursor cell (see [`CursorCell`]). The `Rc` is cloned out first so the
    /// `RefCell` borrow does not also borrow `self`.
    fn reshape(&mut self, ctx: &mut LayoutCtx, geometry: CellGeometry) {
        let emulator = Rc::clone(&self.emulator);
        let palette = self.palette;
        let font_family = self.font_family.clone();
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

        // One `TextContext::layout` per run — one vello `draw_glyphs` downstream.
        let text_ctx = ctx.text_context::<TextContext>();
        for run in self.batch.active_mut() {
            let (fg, _) = run.style().resolve(&palette);
            let style = run.style().text_style(geometry.font_size, fg, &font_family);
            let layout = text_ctx.layout(run.text(), &style, None);
            run.set_layout(layout);
        }

        self.cursor_cell = (!cursor_hidden).then(|| {
            // The glyph under a focused block cursor, re-inked in the background.
            let inverse_glyph = cursor_glyph.map(|(style, text)| {
                let inverse =
                    style.text_style(geometry.font_size, palette.background, &font_family);
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
    /// all), then the touch state machine. The hand-over is `Handled`, not "the
    /// selection had an opinion" — its arms answer `Ignored` for phases it has
    /// nothing to do with, exactly what a scroll or a tap is made of.
    fn pointer_event(&mut self, ctx: &mut EventCtx, pointer: &PointerEvent) -> EventResult {
        if let Some(seam) = self.selection.clone()
            && let Some(result) = self.selection_event(ctx, pointer, &seam)
            && matches!(result, EventResult::Handled)
        {
            // A phase the selection owned — a clearing tap's `Down`, or any
            // phase of a live selection drag. Disarm rather than race it: no
            // content tap or wheel step comes out of a gesture the selection
            // is driving.
            self.grab = Grab::Idle;
            return result;
        }
        match pointer.phase {
            PointerPhase::Down => self.press(ctx, pointer.position),
            PointerPhase::Move => self.drag(pointer.position),
            PointerPhase::Up => self.release(ctx, pointer.position),
            // Flags only — a `Cancel` never touches app state.
            PointerPhase::Cancel => {
                self.grab = Grab::Idle;
                EventResult::Ignored
            }
        }
    }

    /// A `Down`: arm the touch machine, then claim the focus session — the
    /// claim is invariant across all three outcomes.
    fn press(&mut self, ctx: &mut EventCtx, at: Point) -> EventResult {
        // Armed only inside the grid's own box: the surface stack is
        // full-bleed, so a press on the sub-cell remainder strip is not a cell.
        let inside_grid = inside(at, ctx.size());
        self.grab = if inside_grid {
            Grab::Armed { origin: at }
        } else {
            Grab::Idle
        };
        self.scroll_carry = 0.0;

        // Cloned, not borrowed: the arm below writes `self.focus_claim`.
        let Some(seam) = self.input.clone() else {
            return EventResult::Ignored;
        };
        if self.read_only || !inside_grid {
            return EventResult::Ignored;
        }
        // Every `Down` re-claims the focus session, unconditionally and before
        // this widget knows whether the gesture is a tap, scroll, or hold,
        // because frust-core reads a `Down` that requested no focus as a blur.
        // The claim is recorded so the next blur edge can tell a real blur from
        // the key bar taking the surface (`note_focus_edge`). No IME publish
        // here: raising the keyboard is `release`'s call.
        self.focus_claim = seam.claim_focus();
        ctx.request_focus();
        ctx.request_redraw();
        EventResult::Handled
    }

    /// A `Move` the selection did not take: the slop test once, then scroll input.
    fn drag(&mut self, at: Point) -> EventResult {
        match self.grab {
            // No press of ours: a desktop hover move, or an already-disarmed one.
            Grab::Idle => EventResult::Ignored,
            Grab::Armed { origin } => {
                if (at - origin).hypot() <= TOUCH_SLOP {
                    return EventResult::Ignored;
                }
                // Past the slop before the long-press threshold — a scroll for
                // the rest of the gesture. The accumulator starts at the press
                // **origin**, so a drag's steps count its whole travel.
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

    /// An `Up` the selection did not take: the tap branch — the cell report,
    /// and the one place the keyboard can come up. A gesture that turned into a
    /// scroll (or never armed inside the grid) does nothing here. A qualifying
    /// tap reports its cell and obeys the sink's answer; with no sink to ask,
    /// it raises.
    fn release(&mut self, ctx: &mut EventCtx, at: Point) -> EventResult {
        let tapped = matches!(
            self.grab,
            Grab::Armed { origin } if (at - origin).hypot() <= TOUCH_SLOP
        );
        self.grab = Grab::Idle;
        if !tapped {
            return EventResult::Ignored;
        }
        let reported = match (self.on_content_tap.clone(), self.cell_at(at)) {
            (Some(sink), Some(cell)) => Some(sink(u32::from(cell.col), u32::from(cell.row))),
            // Nobody to ask (no hook, or no geometry): the tap answers itself.
            _ => None,
        };
        let raised = reported.unwrap_or(true) && self.engage_ime(ctx);
        if reported.is_some() || raised {
            EventResult::Handled
        } else {
            EventResult::Ignored
        }
    }

    /// Raise the keyboard: latch [`ime_engaged`](Self::ime_engaged) and publish
    /// the surface, reporting whether anything was published. Gated exactly as
    /// the paint-time republish is — a grid with no [`InputSeam`], or a
    /// `read_only` one, is not an IME client however it was tapped.
    fn engage_ime(&mut self, ctx: &mut EventCtx) -> bool {
        let Some(seam) = self.input.clone() else {
            return false;
        };
        if self.read_only {
            return false;
        }
        self.ime_engaged = true;
        let state = seam.ime_state(self.caret_rect);
        ctx.publish_ime_state(state);
        ctx.request_redraw();
        true
    }

    /// The desktop wheel, through the **same** step machinery a touch drag goes
    /// through. A line delta already *is* steps; a precision (trackpad) delta
    /// is pixels and divides by the cell height like a finger's `dy`. Either
    /// way, positive `y` is content moving **down**, so both reach
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
    /// line it crosses, at the cell under `at`, keeping the remainder. Positive
    /// `lines` is content moving **down**: earlier lines are revealed, which is
    /// a wheel **up** step.
    fn wheel_steps(&mut self, lines: f64, at: Point) -> EventResult {
        if !lines.is_finite() || lines == 0.0 {
            return EventResult::Ignored;
        }
        let (Some(sink), Some(cell)) = (self.on_wheel.clone(), self.cell_at(at)) else {
            // Nothing to report to: the accumulator stays put.
            return EventResult::Ignored;
        };
        // Clamped so a degenerate delta cannot spin the loop below.
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

    /// The measured cell height, once `layout` has solved a usable divisor.
    fn line_height(&self) -> Option<f64> {
        self.geometry
            .map(|(_, _, geometry)| geometry.height)
            .filter(|height| height.is_finite() && *height > 0.0)
    }

    /// The zero-based cell under a widget-local `at`, clamped to the grid.
    fn cell_at(&self, at: Point) -> Option<GridPoint> {
        self.geometry
            .map(|(_, _, geometry)| selection::cell_at(at, geometry))
    }

    /// One **focus-routed** event folded into the shared keystroke source.
    fn key_event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let Some(seam) = self.input.clone() else {
            return EventResult::Ignored;
        };
        if self.read_only || !ctx.has_focus() {
            return EventResult::Ignored;
        }
        // One shared fold, so the key bar's caps derive bytes the same way.
        if seam.absorb(event).is_none() {
            return EventResult::Ignored;
        }
        // A keystroke arrived, so a keyboard is up whatever this widget decided.
        self.ime_engaged = true;
        // Unconditional and idempotent: a stale editing state left behind would
        // let a shell's steady-active reconcile push *pre-keystroke* text back
        // into the platform mirror.
        let state = seam.ime_state(self.caret_rect);
        ctx.publish_ime_state(state);
        ctx.request_redraw();
        EventResult::Handled
    }

    /// The selection half of `event` — see [`pointer_event`](Self::pointer_event)
    /// for the ordering. `Some(result)` when this phase was the selection's to
    /// answer, `None` to fall through. Deliberately **no** `capture_pointer`
    /// here: the enclosing [`GestureDetector`](frust::GestureDetector) already
    /// captures and forwards every `Move`/`Up` to its child.
    fn selection_event(
        &mut self,
        ctx: &mut EventCtx,
        pointer: &PointerEvent,
        seam: &SelectionSeam,
    ) -> Option<EventResult> {
        match pointer.phase {
            PointerPhase::Down => {
                seam.track(pointer.position, true);
                // BEFORE anything else: a tap that dismisses a selection must
                // not also claim focus and summon the keyboard.
                if seam.clear_if_active() {
                    ctx.request_redraw();
                    // `Handled` does not spare this `Down` the root's focus
                    // bookkeeping: ANY pointer `Down` requesting no focus reads
                    // as a blur, which would scrub the input baseline out from
                    // under a session still being typed into. So it claims the
                    // seam's focus counter (the key-cap hand-off mechanism)
                    // without calling `request_focus`: the counter moves while
                    // `self.focus_claim` stays stale, so `note_focus_edge` reads
                    // a hand-off and skips the scrub. No keyboard: nothing here
                    // publishes a surface.
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
                // Unthrottled and signal-free: both endpoints re-resolve and
                // the frame repaints, which also refreshes the loupe.
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
            // Flags only. The loupe has to go (the gesture is over), but the
            // selection it produced stays until the next tap clears it.
            PointerPhase::Cancel => {
                seam.track(pointer.position, false);
                seam.cancel_drag();
                ctx.request_redraw();
                Some(EventResult::Ignored)
            }
        }
    }

    /// One end of a focus edge, as `paint` observed it: the **blur scrub**, and
    /// the claim bookkeeping that keeps it honest.
    ///
    /// A blur is where buffered keystrokes stop being live, so it's a scrub
    /// boundary — but two things move the focus path without the session
    /// stopping: a tap on a key bar cap (claims the seam itself), and a
    /// clearing tap this widget consumed (frust-core reads any focus-less
    /// `Down` as a blur regardless of `Handled`, so `selection_event` claims
    /// the seam there for exactly this reason). Scrubbing either would
    /// silently cost the user their next Backspace, so the seam's claim
    /// counter is the discriminator: unchanged across the edge means a real
    /// blur.
    ///
    /// **Residual:** a cap holding the focus path when the keyboard is
    /// dismissed tears down with the bar and reports no blur, so that path's
    /// scrub waits for the pane's teardown or the grid's next real blur. Every
    /// path that *submits* a line already scrubs at the source. The same edge
    /// drops the IME latch.
    fn note_focus_edge(&mut self, focused: bool) {
        let Some(seam) = self.input.clone() else {
            return;
        };
        if focused {
            self.focus_claim = seam.focus_claim();
        } else if seam.focus_claim() == self.focus_claim {
            self.ime_engaged = false;
            seam.scrub();
        }
    }

    /// Whether the caret is lit this frame, doing the blink cycle's bookkeeping
    /// exactly once per paint — split out so the loupe's re-paint reuses the
    /// answer instead of requesting a second paced frame.
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
        // Paced at the blink's own half-period, not the cosmetic cap: a 1.1s
        // two-state blink needs one paint per visibility flip, and the gate
        // repaints the full grid per paced frame — ~2 rather than ~30 grid
        // paints a second at focused idle. `cursor_lit` derives phase from the
        // frame clock, so cadence never shifts the flip points.
        ctx.request_frame_paced_at(Duration::from_millis((CURSOR_BLINK_PERIOD_MS / 2.0) as u64));
        cursor_lit(since)
    }

    /// Paint the grid's cells, the selection wash and the caret at `origin`.
    /// `cull` (grid-local) drops everything outside a region — how the loupe
    /// re-paints its window without submitting a whole screen of glyph runs the
    /// clip would throw away; `None` paints everything. Takes `&self` because it
    /// is called **twice** per frame (grid, then loupe), so it carries no
    /// per-frame bookkeeping.
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
    /// [`style`](super::style)'s selection note for why).
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

    /// Paint the caret — filled and `lit` when focused, a hollow outline when not.
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
            // Hollow outline, inset by half the stroke. No blink, no frame request.
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

    /// Paint the magnifier, if a selection drag is live. A **re-render**, not a
    /// backdrop sample: the same runs painted again under a [`LOUPE_SCALE`]
    /// transform inside a rounded clip, so the user sees real text re-rasterized
    /// rather than a blown-up bitmap. Paint only — `event` knows nothing of it.
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
        // the scene's current transform.
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
        // The content window in grid-local coordinates, centred on the finger.
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

        // The border last, over the magnified content, inset by half the stroke.
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
    //! Deliberately **below** the mounted suites (`tests/term_widget.rs`,
    //! `tests/term_selection.rs`), which own the assembly: what is pinned here is
    //! the discrimination itself. A test stands in for the detector by calling
    //! [`SelectionSeam::begin`](super::SelectionSeam::begin) where the long-press
    //! threshold would have fired.

    use std::any::Any;
    use std::cell::RefCell;

    use frust::authoring::PointerButton;

    use crate::features::terminal::domain::feed::TerminalFeed;

    use super::super::{SharedFeed, TerminalEmulator};
    use super::*;

    /// The font size every test lays out at — the app's own terminal default.
    const FONT_SIZE: f32 = 13.0;

    /// A phone-shaped content box, in logical px.
    const PHONE: Size = Size::new(392.7, 872.7);

    /// The screen every test points at: two lines of ordinary words.
    const FIXTURE: &[u8] = b"cargo build release\r\nusr local bin";

    /// A feed that neither produces nor accepts anything.
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
        /// The cell box `layout` solved — every coordinate below derives from it.
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

        /// …with neither wired, over a read-only session.
        fn unwired_read_only() -> Self {
            Self::build(false, true)
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
                        tapped.borrow_mut().push((col, row));
                        // Whether an IME surface reaches `RenderRoot` is a
                        // root-level fact pinned by `tests/terminal_live.rs`.
                        true
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

    // The tap branch.

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
        // The read-only gate is the *input* gate: it stops keystrokes, not looking
        // at a pane.
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

    // The drag-scroll branch.

    #[test]
    fn a_vertical_drag_emits_one_step_per_line_height_and_carries_the_remainder() {
        let mut probe = Probe::new();
        let start = probe.cell(4, 10);
        // 5.4 cell heights: five whole lines and a remainder that must survive.
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

        // 0.6 more lines completes the sixth step — the 0.4 remainder was kept.
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
        // gesture stays in scroll mode, and nothing arms a selection — the
        // enclosing detector disarmed its own long-press at the same slop.
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

    // The long-press branch.

    #[test]
    fn a_long_press_within_the_slop_selects_and_reports_neither_tap_nor_step() {
        let mut probe = Probe::new();
        // Inside "cargo" on the top row.
        let at = probe.cell(2, 0);
        probe.pointer(PointerPhase::Down, at);

        // What the enclosing GestureDetector does at the threshold.
        probe.selection.begin();
        assert!(probe.selection.dragging(), "the selection drag is live");

        // A drag that now crosses the slop belongs to the selection, not the scroll.
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
        // dismissal must not also address a pane.
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

    // The desktop wheel.

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

    // Unwired: the grid with no hooks.

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
        // …but the drag past the slop is no longer a tap, so the release reports
        // and raises nothing.
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
    fn an_unasked_tap_still_raises_the_keyboard_unless_the_session_is_read_only() {
        // With no hook there is nobody to ask, so the tap answers for itself and
        // publishes. `Handled` is the observable half here: the published surface
        // itself is a root-level fact (see the sink comment in `Probe::build`).
        let mut probe = Probe::unwired();
        let at = probe.cell(3, 3);
        probe.pointer(PointerPhase::Down, at);
        assert_eq!(
            probe.pointer(PointerPhase::Up, at),
            EventResult::Handled,
            "an unasked tap raises the keyboard"
        );
        assert!(probe.taps().is_empty(), "…and still reports no cell");

        // A read-only grid is not an IME client at all, however it is tapped.
        let mut probe = Probe::unwired_read_only();
        let at = probe.cell(3, 3);
        probe.pointer(PointerPhase::Down, at);
        assert_eq!(
            probe.pointer(PointerPhase::Up, at),
            EventResult::Ignored,
            "a read-only session publishes nothing however it is tapped"
        );
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
