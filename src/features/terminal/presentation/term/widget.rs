//! The retained grid. A new generation reports `LAYOUT | PAINT`, never bare `PAINT`: shaping
//! happens in `layout` and `paint` only replays it. Content requests no frames (the pump's one
//! signal write is the wake); only the cursor blink requests paced frames.

use std::rc::Rc;
use std::time::Duration;

use frust::authoring::text::{FontFamily, TextContext, TextLayout};
use frust::authoring::{
    Affine, BoxConstraints, Brush, BuildCtx, ChangeFlags, Color, EditCommand, EventCtx,
    EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, PointerEvent, PointerPhase,
    Rect, Role, RoundedRect, ScrollDelta, SemanticsCtx, Shape, Size, Vec2, View, Widget,
};
use frust::input::TOUCH_SLOP;
use frust::{FrameTime, Theme};

use super::batch::{RunBatch, batch_screen};
use super::geometry::{CellGeometry, measure_cell, solve_cell_geometry};
use super::selection::{self, GridPoint, Segment};
use super::style::{CellStyle, TerminalPalette};
use super::{ContentTapSink, CursorStyle, InputSeam, SelectionSeam, SharedEmulator, WheelSink};

/// One on/off cycle in ms, matching the mock's `@keyframes blink` in `docs/terminal.html`.
pub const CURSOR_BLINK_PERIOD_MS: f64 = 1100.0;

/// Fraction of the cell height, floored at one logical px.
const CURSOR_UNDERLINE_FRACTION: f64 = 0.12;
/// Fraction of the cell width, floored at one logical px.
const CURSOR_BAR_FRACTION: f64 = 0.16;
const CURSOR_OUTLINE_WIDTH: f64 = 1.0;
const OUTLINE_TOLERANCE: f64 = 0.1;
/// SGR 4: stroke width in logical px, and its offset down the cell as a fraction of the height.
const TEXT_UNDERLINE_WIDTH: f64 = 1.0;
const TEXT_UNDERLINE_Y_FRACTION: f64 = 0.92;

/// A guard: a degenerate delta (an enormous `dy`, a near-zero cell height) cannot spin the loop.
const MAX_WHEEL_STEPS: f64 = 256.0;

pub fn cursor_lit(elapsed_ms: f64) -> bool {
    (elapsed_ms % CURSOR_BLINK_PERIOD_MS) < CURSOR_BLINK_PERIOD_MS / 2.0
}

// Loupe geometry, in logical px.

pub const LOUPE_W: f64 = 116.0;
pub const LOUPE_H: f64 = 46.0;
pub const LOUPE_SCALE: f64 = 1.75;
/// Gap between the finger and the window's bottom edge.
pub const LOUPE_LIFT: f64 = 28.0;
pub const LOUPE_MARGIN: f64 = 4.0;
const LOUPE_RADIUS: f64 = 4.0;
const LOUPE_BORDER: f64 = 1.0;
const LOUPE_SHADOW_BLUR: f64 = 6.0;
const LOUPE_SHADOW_ALPHA: f32 = 0.4;
const LOUPE_TOLERANCE: f64 = 0.1;

/// `focal` is widget-local px; `max` after `min` so a too-small rect pins the window top-left.
pub fn loupe_box(focal: Point, bounds: Size) -> Rect {
    let x = (focal.x - LOUPE_W / 2.0)
        .min(bounds.width - LOUPE_MARGIN - LOUPE_W)
        .max(LOUPE_MARGIN);
    let y = (focal.y - LOUPE_LIFT - LOUPE_H)
        .min(bounds.height - LOUPE_MARGIN - LOUPE_H)
        .max(LOUPE_MARGIN);
    Rect::new(x, y, x + LOUPE_W, y + LOUPE_H)
}

/// Uses the clamped centre, so a window pinned at an edge still shows the cell under the finger.
pub fn loupe_content_offset(focal: Point, window: Rect) -> Vec2 {
    focal.to_vec2() - window.center().to_vec2()
}

/// Both points are absolute paint-space, like every `PaintScene` coordinate.
fn loupe_transform(focal: Point, window: Rect) -> Affine {
    Affine::translate(window.center().to_vec2())
        * Affine::scale(LOUPE_SCALE)
        * Affine::translate(-focal.to_vec2())
}

/// Captured in `layout`, the only pass that can shape, so `paint` touches no text engine.
struct CursorCell {
    row: u16,
    col: u16,
    /// Re-shaped in the background ink for the focused block cursor.
    inverse_glyph: Option<TextLayout>,
}

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

/// `generation` must be a tracked read of the pump's signal in the caller's `build`.
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
    #[must_use]
    pub fn cursor(mut self, cursor: CursorStyle) -> Self {
        self.cursor = cursor;
        self
    }

    #[must_use]
    pub fn font_family(mut self, family: FontFamily) -> Self {
        self.font_family = family;
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn input(mut self, seam: InputSeam) -> Self {
        self.input = Some(seam);
        self
    }

    /// The focus gate is the read-only gate: a grid that refuses focus never receives `Key`/`Ime`.
    #[must_use]
    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Not gated on [`read_only`](Self::read_only).
    #[must_use]
    pub fn selection(mut self, seam: SelectionSeam) -> Self {
        self.selection = Some(seam);
        self
    }

    /// Tap branch only, after the `Down`'s focus claim; not gated on read-only.
    #[must_use]
    pub fn on_content_tap(mut self, sink: ContentTapSink) -> Self {
        self.on_content_tap = Some(sink);
        self
    }

    #[must_use]
    pub fn on_wheel(mut self, sink: WheelSink) -> Self {
        self.on_wheel = Some(sink);
        self
    }
}

#[derive(Clone, Copy, Debug)]
enum Grab {
    Idle,
    Armed { origin: Point },
    Scrolling { last: Point },
}

pub struct TerminalGridWidget {
    generation: u64,
    emulator: SharedEmulator,
    palette: TerminalPalette,
    font_size: f32,
    font_family: FontFamily,
    cursor: CursorStyle,
    focused: bool,
    label: String,
    batch: RunBatch,
    shaped: Option<u64>,
    geometry: Option<(Size, f32, CellGeometry)>,
    /// Beside `geometry` because `FontFamily` isn't `Copy`; `None` exactly when `geometry` is.
    geometry_family: Option<FontFamily>,
    cursor_cell: Option<CursorCell>,
    /// Reset on a focus change so a freshly focused grid shows a lit caret at once.
    blink_from: Option<FrameTime>,
    input: Option<InputSeam>,
    read_only: bool,
    selection: Option<SelectionSeam>,
    /// Absolute coordinates, captured in `paint`, the only pass that knows the origin.
    caret_rect: Option<Rect>,
    was_focused: bool,
    focus_claim: u64,
    /// Raised by a qualifying tap or a keystroke, dropped only on a real blur. Gating on focus
    /// alone would raise the keyboard on every `Down`, since `press` requests focus.
    ime_engaged: bool,
    grab: Grab,
    /// Sub-step remainder in lines, carried so a slow drag scrolls smoothly; reset at every press.
    scroll_carry: f64,
    on_content_tap: Option<ContentTapSink>,
    on_wheel: Option<WheelSink>,
}

impl<State: 'static> View<State> for TerminalGridView {
    type Element = TerminalGridWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> TerminalGridWidget {
        if let Some(seam) = self.selection.as_ref() {
            seam.observe_generation(self.generation);
        }
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

        // Anything that changes what is shaped forces LAYOUT, never a bare PAINT.
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

        if prev.cursor != self.cursor {
            element.cursor = self.cursor;
            flags |= ChangeFlags::PAINT;
        }
        if prev.focused != self.focused {
            element.focused = self.focused;
            element.blink_from = None;
            flags |= ChangeFlags::PAINT;
        }
        if prev.label != self.label {
            element.label.clear();
            element.label.push_str(&self.label);
            flags |= ChangeFlags::PAINT;
        }

        // Seams are not comparable, so they are reinstalled every rebuild.
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
        // Every rebuild: the seam outlives this element and `set_read_only` is idempotent.
        if let Some(seam) = self.input.as_ref() {
            seam.set_read_only(self.read_only);
        }

        if let Some(seam) = self.selection.as_ref() {
            seam.observe_generation(self.generation);
        }

        flags
    }

    /// The unmount scrub. Not in `on_cleanup`, which needs `Send + Sync`; teardown runs before the
    /// owner is disposed.
    fn teardown(&self, element: &mut TerminalGridWidget, _ctx: &mut BuildCtx<'_>) {
        if let Some(seam) = element.input.as_ref() {
            seam.scrub();
        }
    }
}

impl Widget for TerminalGridWidget {
    /// Takes exactly the grid's own box; a tight parent gets its size instead, with the grid
    /// anchored top-left over the terminal background.
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let available = bc.max();
        // A different family can measure a different advance at the same size.
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
            self.shaped = None;
            geometry
        };

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

        if let Some(seam) = self.selection.as_ref() {
            seam.set_metrics(geometry);
        }

        bc.constrain(geometry.grid_size())
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let Some((_, _, geometry)) = self.geometry else {
            return;
        };
        let origin = ctx.origin();
        let size = ctx.size();

        // `has_focus` also catches a container-routed blur that never reaches `event`, so a blurred
        // grid stops blinking on the next frame.
        let focused = self.focused || ctx.has_focus();
        if focused != self.was_focused {
            self.was_focused = focused;
            self.blink_from = None;
            self.note_focus_edge(focused);
        }

        self.caret_rect = self.cursor_cell.as_ref().and_then(|cell| {
            (cell.row < geometry.rows && cell.col < geometry.cols).then(|| {
                let x = origin.x + f64::from(cell.col) * geometry.width;
                let y = origin.y + f64::from(cell.row) * geometry.height;
                Rect::new(x, y, x + geometry.width, y + geometry.height)
            })
        });

        // Republished each focused, engaged frame so a reseeded platform mirror converges.
        if let Some(seam) = self.input.as_ref()
            && !self.read_only
            && ctx.has_focus()
            && self.ime_engaged
        {
            let state = seam.ime_state(self.caret_rect);
            ctx.publish_ime_state(state);
        }

        // Once per frame, before content paints: the loupe's re-paint reuses the answer.
        let lit = self.cursor_lit_now(ctx, focused);

        // The geometry already fits the box; the clip is a backstop that also bounds the loupe.
        scene.push_clip(origin, size);
        scene.fill_rect(origin, size, self.palette.background);
        self.paint_content(scene, origin, geometry, focused, lit, None);
        self.paint_loupe(scene, origin, size, geometry, focused, lit);
        scene.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        match event {
            InputEvent::Pointer(pointer) => self.pointer_event(ctx, pointer),
            InputEvent::Scroll { position, delta } => self.wheel_event(*position, *delta),
            InputEvent::Key(_) | InputEvent::Ime(_) => self.key_event(ctx, event),
            InputEvent::EditCommand(_) => self.edit_command(ctx, event),
            InputEvent::Housekeeping => EventResult::Ignored,
            // The Copy chip is a sibling in `terminal_surface`, not an overlay this grid owns.
            InputEvent::Overlay(_) => EventResult::Ignored,
            InputEvent::PointerContact { .. } | InputEvent::Scale(_) => EventResult::Ignored,
            InputEvent::FileDrop(_) => EventResult::Ignored,
            InputEvent::Shortcut(_) => EventResult::Ignored,
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // One node, never the screen text as a value: a several-thousand-character string rebuilt
        // 30 times a second is a leak with a screen reader attached.
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

fn inside(position: Point, size: Size) -> bool {
    position.x >= 0.0 && position.y >= 0.0 && position.x < size.width && position.y < size.height
}

/// `Rect::intersect` returns a possibly negative-extent rect, not an `Option`.
fn overlaps(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

impl TerminalGridWidget {
    /// The `Rc` is cloned out first so the `RefCell` borrow does not also borrow `self`.
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

        let text_ctx = ctx.text_context::<TextContext>();
        for run in self.batch.active_mut() {
            let (fg, _) = run.style().resolve(&palette);
            let style = run.style().text_style(geometry.font_size, fg, &font_family);
            let layout = text_ctx.layout(run.text(), &style, None);
            run.set_layout(layout);
        }

        self.cursor_cell = (!cursor_hidden).then(|| {
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

    /// The selection arms see every phase first (they work read-only and with no [`InputSeam`]);
    /// only their `Handled` stops the touch state machine.
    fn pointer_event(&mut self, ctx: &mut EventCtx, pointer: &PointerEvent) -> EventResult {
        if let Some(seam) = self.selection.clone()
            && let Some(result) = self.selection_event(ctx, pointer, &seam)
            && matches!(result, EventResult::Handled)
        {
            // Disarm: no tap or wheel step comes out of a gesture the selection drives.
            self.grab = Grab::Idle;
            return result;
        }
        match pointer.phase {
            PointerPhase::Down => self.press(ctx, pointer.position),
            PointerPhase::Move => self.drag(pointer.position),
            PointerPhase::Up => self.release(ctx, pointer.position),
            PointerPhase::Cancel => {
                self.grab = Grab::Idle;
                EventResult::Ignored
            }
        }
    }

    fn press(&mut self, ctx: &mut EventCtx, at: Point) -> EventResult {
        // The surface is full-bleed: a press on the sub-cell remainder strip is not a cell.
        let inside_grid = inside(at, ctx.size());
        self.grab = if inside_grid {
            Grab::Armed { origin: at }
        } else {
            Grab::Idle
        };
        self.scroll_carry = 0.0;

        let Some(seam) = self.input.clone() else {
            return EventResult::Ignored;
        };
        if self.read_only || !inside_grid {
            return EventResult::Ignored;
        }
        // Every `Down` re-claims focus before the gesture is known: frust reads a `Down` that
        // requests no focus as a blur. Raising the keyboard is `release`'s call.
        self.focus_claim = seam.claim_focus();
        ctx.request_focus();
        ctx.request_redraw();
        EventResult::Handled
    }

    fn drag(&mut self, at: Point) -> EventResult {
        match self.grab {
            Grab::Idle => EventResult::Ignored,
            Grab::Armed { origin } => {
                if (at - origin).hypot() <= TOUCH_SLOP {
                    return EventResult::Ignored;
                }
                // The accumulator starts at the press origin, so the steps count the whole travel.
                self.grab = Grab::Scrolling { last: origin };
                self.scroll_to(at)
            }
            Grab::Scrolling { .. } => self.scroll_to(at),
        }
    }

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

    /// The one place a tap can raise the keyboard; with no sink to ask, it raises.
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
            _ => None,
        };
        let raised = reported.unwrap_or(true) && self.engage_ime(ctx);
        if reported.is_some() || raised {
            EventResult::Handled
        } else {
            EventResult::Ignored
        }
    }

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

    /// A line delta already is steps; a pixel (trackpad) delta divides by the cell height. Positive
    /// `y` is content moving down in both.
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

    /// Positive `lines` is content moving down, revealing earlier lines: a wheel-up step.
    fn wheel_steps(&mut self, lines: f64, at: Point) -> EventResult {
        if !lines.is_finite() || lines == 0.0 {
            return EventResult::Ignored;
        }
        let (Some(sink), Some(cell)) = (self.on_wheel.clone(), self.cell_at(at)) else {
            return EventResult::Ignored;
        };
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

    fn line_height(&self) -> Option<f64> {
        self.geometry
            .map(|(_, _, geometry)| geometry.height)
            .filter(|height| height.is_finite() && *height > 0.0)
    }

    fn cell_at(&self, at: Point) -> Option<GridPoint> {
        self.geometry
            .map(|(_, _, geometry)| selection::cell_at(at, geometry))
    }

    fn key_event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let Some(seam) = self.input.clone() else {
            return EventResult::Ignored;
        };
        if self.read_only || !ctx.has_focus() {
            return EventResult::Ignored;
        }
        if seam.absorb(event).is_none() {
            return EventResult::Ignored;
        }
        self.ime_engaged = true;
        // Unconditional: a stale editing state would let a shell's steady-active reconcile push
        // pre-keystroke text back into the platform mirror.
        let state = seam.ime_state(self.caret_rect);
        ctx.publish_ime_state(state);
        ctx.request_redraw();
        EventResult::Handled
    }

    /// `Copy`/`Cut`/`SelectAll` are reads and ignore read-only; `Paste` is input bytes and passes
    /// [`InputSeam::absorb`]'s gate like every other emitted byte.
    fn edit_command(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::EditCommand(cmd) = event else {
            return EventResult::Ignored;
        };
        match cmd {
            // A terminal has nothing local to delete, so `Cut` copies.
            EditCommand::Copy | EditCommand::Cut => self.copy_selection(),
            // The word-based selection model has no select-everything resolution.
            EditCommand::SelectAll => EventResult::Ignored,
            EditCommand::Paste(_) => {
                let Some(seam) = self.input.clone() else {
                    return EventResult::Ignored;
                };
                if !ctx.has_focus() {
                    return EventResult::Ignored;
                }
                if seam.absorb(event).is_none() {
                    return EventResult::Ignored;
                }
                self.ime_engaged = true;
                let state = seam.ime_state(self.caret_rect);
                ctx.publish_ime_state(state);
                ctx.request_redraw();
                EventResult::Handled
            }
        }
    }

    /// Nothing selected answers `Ignored` and never calls the sink. Shares the chip's clipboard
    /// sink, so both routes write and redact alike.
    fn copy_selection(&mut self) -> EventResult {
        let Some(seam) = self.selection.as_ref() else {
            return EventResult::Ignored;
        };
        if !seam.is_active() {
            return EventResult::Ignored;
        }
        let text = seam.text();
        let _ = super::chip::system_clipboard_sink()(&text);
        seam.clear();
        EventResult::Handled
    }

    /// `None` falls through. No `capture_pointer`: the enclosing detector already captures.
    fn selection_event(
        &mut self,
        ctx: &mut EventCtx,
        pointer: &PointerEvent,
        seam: &SelectionSeam,
    ) -> Option<EventResult> {
        match pointer.phase {
            PointerPhase::Down => {
                seam.pinch_ended();
                seam.track(pointer.position, true);
                // First: dismissing a selection must not claim focus and raise the keyboard.
                if seam.clear_if_active() {
                    ctx.request_redraw();
                    // A focus-less `Down` reads as a blur; bumping the seam's claim without
                    // `request_focus` makes `note_focus_edge` see a hand-off, not a blur.
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
            // The selection survives until the next tap clears it.
            PointerPhase::Cancel => {
                seam.track(pointer.position, false);
                seam.cancel_drag();
                ctx.request_redraw();
                Some(EventResult::Ignored)
            }
        }
    }

    /// The blur scrub. A key-cap tap and a consumed clearing tap move the focus path without the
    /// session stopping, so only an unchanged seam claim across the edge is a real blur.
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

    fn cursor_lit_now(&mut self, ctx: &mut PaintCtx, focused: bool) -> bool {
        if !focused {
            return false;
        }
        if Theme::from_paint_ctx(ctx).is_some_and(|theme| theme.motion.reduce_motion) {
            // Frozen lit: a caret is a position cue.
            return true;
        }
        let from = *self.blink_from.get_or_insert(ctx.frame_time());
        let since = ctx.frame_time().saturating_sub(from).as_secs_f64() * 1000.0;
        // Paced at the blink's half-period, one paint per visibility flip; `cursor_lit` derives the
        // phase from the frame clock, so cadence never shifts the flips.
        ctx.request_frame_paced_at(Duration::from_millis((CURSOR_BLINK_PERIOD_MS / 2.0) as u64));
        cursor_lit(since)
    }

    /// `cull` (grid-local) skips everything outside a region, for the loupe. Called twice per
    /// frame, so it carries no per-frame bookkeeping.
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

    /// Painted over the runs it covers.
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

    /// A re-render under a [`LOUPE_SCALE`] transform, not a backdrop sample, so text re-rasterizes.
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
        // Absolute: `push_transform` composes with the scene's current transform.
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

        // The border paints last, over the magnified content.
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

    pub fn geometry(&self) -> Option<CellGeometry> {
        self.geometry.map(|(_, _, geometry)| geometry)
    }

    pub fn run_count(&self) -> usize {
        self.batch.count()
    }

    pub fn run_capacity(&self) -> usize {
        self.batch.capacity()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    //! A test calls `SelectionSeam::begin` where the detector's long-press threshold would fire.

    use std::any::Any;
    use std::cell::RefCell;

    use frust::authoring::PointerButton;

    use crate::features::terminal::domain::feed::TerminalFeed;

    use super::super::{SharedFeed, TerminalEmulator};
    use super::*;

    const FONT_SIZE: f32 = 13.0;

    const PHONE: Size = Size::new(392.7, 872.7);

    const FIXTURE: &[u8] = b"cargo build release\r\nusr local bin";

    struct NullFeed;

    impl TerminalFeed for NullFeed {
        fn drain_output(&mut self, _sink: &mut dyn FnMut(&[u8])) -> usize {
            0
        }

        fn send_input(&mut self, _bytes: &[u8]) {}

        fn resize(&mut self, _cols: u16, _rows: u16) {}
    }

    struct Probe {
        widget: TerminalGridWidget,
        geo: CellGeometry,
        taps: Rc<RefCell<Vec<(u32, u32)>>>,
        steps: Rc<RefCell<Vec<(bool, u32, u32)>>>,
        input: InputSeam,
        selection: SelectionSeam,
    }

    impl Probe {
        fn new() -> Self {
            Self::build(true, false)
        }

        fn unwired() -> Self {
            Self::build(false, false)
        }

        fn unwired_read_only() -> Self {
            Self::build(false, true)
        }

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

        fn pointer(&mut self, phase: PointerPhase, at: Point) -> EventResult {
            let event = InputEvent::Pointer(PointerEvent {
                phase,
                position: at,
                button: PointerButton::Primary,
            });
            self.dispatch(&event)
        }

        fn scroll(&mut self, delta: ScrollDelta, at: Point) -> EventResult {
            self.dispatch(&InputEvent::Scroll {
                position: at,
                delta,
            })
        }

        fn edit_command(&mut self, cmd: EditCommand) -> EventResult {
            self.dispatch(&InputEvent::EditCommand(cmd))
        }

        fn dispatch(&mut self, event: &InputEvent) -> EventResult {
            let mut state = ();
            let mut ctx = EventCtx::new(&mut state, Point::ZERO, self.geo.grid_size());
            self.widget.event(&mut ctx, event)
        }

        fn cell(&self, col: u16, row: u16) -> Point {
            Point::new(
                (f64::from(col) + 0.5) * self.geo.width,
                (f64::from(row) + 0.5) * self.geo.height,
            )
        }

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

    #[test]
    fn a_vertical_drag_emits_one_step_per_line_height_and_carries_the_remainder() {
        let mut probe = Probe::new();
        let start = probe.cell(4, 10);
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

        assert_eq!(
            probe.pointer(PointerPhase::Move, dragged),
            EventResult::Ignored
        );
        assert_eq!(probe.steps().len(), 2, "a stationary Move adds no step");
        assert!(
            probe.selection.range().is_none(),
            "no selection comes out of a scroll"
        );

        assert_eq!(
            probe.pointer(PointerPhase::Up, dragged),
            EventResult::Ignored
        );
        assert!(probe.taps().is_empty());
    }

    #[test]
    fn a_long_press_within_the_slop_selects_and_reports_neither_tap_nor_step() {
        let mut probe = Probe::new();
        let at = probe.cell(2, 0);
        probe.pointer(PointerPhase::Down, at);

        probe.selection.begin();
        assert!(probe.selection.dragging(), "the selection drag is live");

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
    fn an_unasked_tap_still_raises_the_keyboard_unless_the_session_is_read_only() {
        let mut probe = Probe::unwired();
        let at = probe.cell(3, 3);
        probe.pointer(PointerPhase::Down, at);
        assert_eq!(
            probe.pointer(PointerPhase::Up, at),
            EventResult::Handled,
            "an unasked tap raises the keyboard"
        );
        assert!(probe.taps().is_empty(), "…and still reports no cell");

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
        let mut probe = Probe::new();
        let outside = Point::new(probe.geo.grid_width() + 4.0, probe.geo.grid_height() + 4.0);
        assert_eq!(
            probe.pointer(PointerPhase::Down, outside),
            EventResult::Ignored
        );
        probe.pointer(PointerPhase::Up, outside);
        assert!(probe.taps().is_empty());
    }

    // `Paste`'s focused path needs a real `has_focus()`; `tests/terminal.rs` pins it.

    #[test]
    fn select_all_is_not_yet_implemented_and_ignored_either_way() {
        for mut probe in [Probe::unwired(), Probe::unwired_read_only()] {
            assert_eq!(
                probe.edit_command(EditCommand::SelectAll),
                EventResult::Ignored
            );
        }
    }

    #[test]
    fn copy_and_cut_write_nothing_when_nothing_is_selected() {
        // The sink is the real system clipboard, so this test stays on the no-selection path.
        let mut probe = Probe::unwired();
        assert!(
            probe.selection.range().is_none(),
            "sanity: nothing selected"
        );
        assert_eq!(
            probe.edit_command(EditCommand::Copy),
            EventResult::Ignored,
            "no selection, no sink call"
        );
        assert_eq!(
            probe.edit_command(EditCommand::Cut),
            EventResult::Ignored,
            "Cut shares Copy's guard"
        );
    }
}
