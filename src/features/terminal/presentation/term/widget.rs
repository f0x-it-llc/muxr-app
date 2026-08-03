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
//! # Not here
//!
//! No selection, no scrollback view, no key repeat. Those are their own tasks.
//! An `InputEvent::Housekeeping` broadcast is still `Ignored` — a leaf has no
//! children to forward one to and must never consume one.

use std::rc::Rc;

use frust::authoring::text::{TextContext, TextLayout};
use frust::authoring::{
    BoxConstraints, Brush, BuildCtx, ChangeFlags, EventCtx, EventResult, InputEvent, LayoutCtx,
    PaintCtx, PaintScene, Point, PointerPhase, Rect, Role, SemanticsCtx, Shape, Size, View, Widget,
};
use frust::{FrameTime, Theme};

use super::batch::{RunBatch, batch_screen};
use super::geometry::{CellGeometry, measure_cell, solve_cell_geometry};
use super::style::{CellStyle, TerminalPalette};
use super::{CursorStyle, InputSeam, SharedEmulator};

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

/// Whether the caret is lit `elapsed_ms` into its blink cycle.
pub fn cursor_lit(elapsed_ms: f64) -> bool {
    (elapsed_ms % CURSOR_BLINK_PERIOD_MS) < CURSOR_BLINK_PERIOD_MS / 2.0
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
    /// The caret cell in **absolute** coordinates, captured during `paint` (the
    /// only pass that knows the grid's absolute origin — `EventCtx::origin` is
    /// parent-relative) and published as the platform's candidate-placement
    /// hint.
    caret_rect: Option<Rect>,
    /// Effective focus as of the last paint, for the blink-reset edge.
    was_focused: bool,
}

impl<State: 'static> View<State> for TerminalGridView {
    type Element = TerminalGridWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> TerminalGridWidget {
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
            caret_rect: None,
            was_focused: self.focused,
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
        let clienthood =
            prev.input.is_some() != self.input.is_some() || prev.read_only != self.read_only;
        element.input = self.input.clone();
        if clienthood {
            element.read_only = self.read_only;
            flags |= ChangeFlags::PAINT;
        }

        flags
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

        // The derived geometry already fits the content box, so nothing should
        // reach this clip — it stays as the backstop that makes "nothing
        // outside the grid can paint" true by construction (a font floored at
        // MIN_FONT_SIZE in a hopeless box is the one case that leans on it).
        scene.push_clip(origin, size);
        scene.fill_rect(origin, size, self.palette.background);

        for run in self.batch.active() {
            let x = origin.x + f64::from(run.col()) * geometry.width;
            let y = origin.y + f64::from(run.row()) * geometry.height;
            let (fg, bg) = run.style().resolve(&self.palette);
            if let Some(bg) = bg {
                scene.fill_rect(
                    Point::new(x, y),
                    Size::new(f64::from(run.cells()) * geometry.width, geometry.height),
                    bg,
                );
            }
            if let Some(layout) = run.layout() {
                for glyphs in layout.to_scene_runs(Point::new(x, y)) {
                    scene.draw_glyph_run(glyphs);
                }
            }
            if run.style().underline {
                let uy = y + geometry.height * TEXT_UNDERLINE_Y_FRACTION;
                scene.stroke_line(
                    Point::new(x, uy),
                    Point::new(x + f64::from(run.cells()) * geometry.width, uy),
                    TEXT_UNDERLINE_WIDTH,
                    fg,
                );
            }
        }

        self.paint_cursor(ctx, scene, geometry, focused);

        scene.pop_clip();
        // No `request_frame` for content: the feed pump's generation write is
        // the sole wake source (module docs, rule 2).
    }

    /// The keystroke source's event half — see the [module docs](self).
    ///
    /// A grid with no [`InputSeam`], or a `read_only` one, falls straight
    /// through to `Ignored`: it claims no focus, so the focus-routed `Key`/`Ime`
    /// events can never reach it in the first place.
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let Some(seam) = self.input.as_ref() else {
            return EventResult::Ignored;
        };
        if self.read_only {
            return EventResult::Ignored;
        }
        match event {
            InputEvent::Pointer(pointer) => {
                // No capture: the grid has no drag semantic yet (selection is
                // its own task), and a `Cancel` must never touch state.
                if pointer.phase != PointerPhase::Down || !inside(pointer.position, ctx.size()) {
                    return EventResult::Ignored;
                }
                // Every tap re-claims focus and re-publishes `active: true`, so
                // a keyboard the user dismissed comes back on the next tap
                // rather than needing a blur first.
                ctx.request_focus();
                let state = seam.ime_state(self.caret_rect);
                ctx.publish_ime_state(state);
                ctx.request_redraw();
                EventResult::Handled
            }
            InputEvent::Key(_) | InputEvent::Ime(_) => {
                if !ctx.has_focus() {
                    return EventResult::Ignored;
                }
                // One shared fold, so the key bar's caps derive bytes exactly
                // the same way this widget does (see [`InputSeam::absorb`]).
                if seam.absorb(event).is_none() {
                    return EventResult::Ignored;
                }
                // The publish is unconditional and idempotent: leaving a stale
                // editing state behind would make a shell's steady-active
                // reconcile push the *pre-keystroke* text back into the
                // platform mirror.
                let state = seam.ime_state(self.caret_rect);
                ctx.publish_ime_state(state);
                ctx.request_redraw();
                EventResult::Handled
            }
            InputEvent::Scroll { .. } => EventResult::Ignored,
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

    /// Paint the caret — the one thing here that may ask for another frame, and
    /// only ever as a paced cosmetic loop (see the [module docs](self)).
    fn paint_cursor(
        &mut self,
        ctx: &mut PaintCtx,
        scene: &mut dyn PaintScene,
        geo: CellGeometry,
        focused: bool,
    ) {
        let Some(cell) = &self.cursor_cell else {
            return;
        };
        if cell.row >= geo.rows || cell.col >= geo.cols {
            return;
        }
        let origin = ctx.origin();
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

        let reduce_motion =
            Theme::from_paint_ctx(ctx).is_some_and(|theme| theme.motion.reduce_motion);
        let lit = if reduce_motion {
            // Frozen lit: a caret is a position cue, so it must not vanish for
            // a user who asked motion to stop.
            true
        } else {
            let from = *self.blink_from.get_or_insert(ctx.frame_time());
            let since = ctx.frame_time().saturating_sub(from).as_secs_f64() * 1000.0;
            // Paced, not unpaced: a 1.1s two-state blink is exactly the
            // "decorative loop whose exact cadence is imperceptible" class the
            // mobile frame gate may throttle.
            ctx.request_frame_paced();
            cursor_lit(since)
        };
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
