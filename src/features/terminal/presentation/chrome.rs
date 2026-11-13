//! Hand-rolled Terminal primitives: the link notice strip, the expand button, the animated card
//! stage and the card body's text run. Every piece takes resolved colours and styles.

use std::rc::Rc;
use std::time::Duration;

use frust::authoring::TickClass;
use frust::authoring::text::{TextContext, TextLayout, TextStyle};
use frust::authoring::{
    Action, Affine, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, Color,
    ErasedCallback, EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point,
    PointerPhase, Rect, Role, RoundedRect, SemanticsCtx, Shape, Size, Vec2, View, Widget,
    build_child, erase_callback, rebuild_child, route_event_single, teardown_child,
};
use frust::{
    AnimationController, Axis, CrossAxisAlignment, Curve, EdgeInsets, FlexView, FrameTime,
    IconData, Padding, Theme, container, flexible, icon, inflexible, text,
};

use crate::features::terminal::domain::entities::{TermLine, TermLineKind};
use crate::ui::topbar::BAR_HEIGHT;

use crate::ui::paint::{HAIRLINE, RADIUS_FULL};

/// The framework's own border-stroke flattening tolerance.
const BORDER_TOLERANCE: f64 = 0.1;

fn effective_radius(radius: f64, size: Size) -> f64 {
    radius.min(size.width / 2.0).min(size.height / 2.0).max(0.0)
}

fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

fn stroke_border(scene: &mut dyn PaintScene, origin: Point, size: Size, radius: f64, color: Color) {
    let half = HAIRLINE / 2.0;
    let rr = RoundedRect::new(
        half,
        half,
        size.width - half,
        size.height - half,
        (radius - half).max(0.0),
    );
    let path: BezPath = rr.to_path(BORDER_TOLERANCE);
    scene.stroke_path(origin, &path, HAIRLINE, &Brush::Solid(color));
}

fn inside(pos: Point, size: Size) -> bool {
    pos.x >= 0.0 && pos.y >= 0.0 && pos.x < size.width && pos.y < size.height
}

/// Must equal the status row's 18px lead in `widgets`: the card has one left edge.
const STRIP_PAD_LEFT: f64 = 18.0;
const STRIP_PAD_RIGHT: f64 = 14.0;
const STRIP_PAD_Y: f64 = 10.0;
const STRIP_GAP: f64 = 8.0;

/// A band rather than an overlay: a reconnecting session's output stays readable.
pub(super) fn status_strip<State: 'static>(
    message: impl Into<String>,
    style: TextStyle,
    fill: Color,
    action: Option<impl View<State>>,
) -> impl View<State> {
    let mut children = vec![flexible(1, text(message.into()).style(style))];
    if let Some(action) = action {
        children.push(inflexible(frust::SizedBox::<State>(Some(STRIP_GAP), None)));
        children.push(inflexible(action));
    }
    container(Padding(
        EdgeInsets {
            left: STRIP_PAD_LEFT,
            top: STRIP_PAD_Y,
            right: STRIP_PAD_RIGHT,
            bottom: STRIP_PAD_Y,
        },
        FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center),
    ))
    .fill(fill)
}

struct GlyphLabel {
    content: String,
    layout: Option<TextLayout>,
    laid_out: Option<(TextStyle, String)>,
}

impl GlyphLabel {
    fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            layout: None,
            laid_out: None,
        }
    }

    fn layout(&mut self, ctx: &mut LayoutCtx, style: &TextStyle, max_width: Option<f32>) -> Size {
        let key = (style.clone(), self.content.clone());
        if let Some(cached) = &self.layout
            && self.laid_out.as_ref() == Some(&key)
        {
            return cached.size();
        }
        let text_ctx = ctx.text_context::<TextContext>();
        let laid = text_ctx.layout(&self.content, style, max_width);
        let size = laid.size();
        self.layout = Some(laid);
        self.laid_out = Some(key);
        size
    }

    fn paint(&self, origin: Point, scene: &mut dyn PaintScene) {
        if let Some(layout) = &self.layout {
            for run in layout.to_scene_runs(origin) {
                scene.draw_glyph_run(run);
            }
        }
    }
}

/// `.expand-btn{width:28px;height:28px}`.
pub(super) const EXPAND_SIZE: f64 = 28.0;
/// Not from the mock: a Material-weight outline at the mock's 12px reads too thin.
const EXPAND_ICON_SIZE: f64 = 15.0;
/// `.expand-btn:active{transform:scale(0.9)}`.
const PRESS_SCALE: f64 = 0.9;

/// Hand-rolled for the hairline border and the `:active` scale press `icon_button` lacks.
pub(super) struct ExpandButtonView<State: 'static> {
    mark: IconData,
    label: String,
    fill: Color,
    ink: Color,
    border: Color,
    on: bool,
    on_press: Rc<dyn Fn(&mut State)>,
}

pub(super) fn expand_button<State: 'static, F: Fn(&mut State) + 'static>(
    mark: impl Into<IconData>,
    label: impl Into<String>,
    on_press: F,
) -> ExpandButtonView<State> {
    ExpandButtonView {
        mark: mark.into(),
        label: label.into(),
        fill: Color::TRANSPARENT,
        ink: Color::WHITE,
        border: Color::TRANSPARENT,
        on: false,
        on_press: Rc::new(on_press),
    }
}

impl<State: 'static> ExpandButtonView<State> {
    pub(super) fn tones(mut self, fill: Color, ink: Color, border: Color) -> Self {
        self.fill = fill;
        self.ink = ink;
        self.border = border;
        self
    }

    /// Semantics only; the caller passes the active colours through [`tones`](Self::tones).
    pub(super) fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    fn mark_view(&self) -> impl View<State> + use<State> {
        icon(self.mark.clone())
            .size(EXPAND_ICON_SIZE)
            .color(self.ink)
    }
}

pub(super) struct ExpandButtonWidget {
    mark: ChildPod,
    label: String,
    fill: Color,
    ink: Color,
    border: Color,
    on: bool,
    pressed: bool,
    /// Set on `Down` with pointer capture; gates `Move`/`Up` so a hover never latches or fires.
    captured: bool,
    on_press: ErasedCallback,
}

impl<State: 'static> View<State> for ExpandButtonView<State> {
    type Element = ExpandButtonWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> ExpandButtonWidget {
        ExpandButtonWidget {
            mark: build_child(&self.mark_view(), ctx),
            label: self.label.clone(),
            fill: self.fill,
            ink: self.ink,
            border: self.border,
            on: self.on,
            pressed: false,
            captured: false,
            on_press: erase_callback(&self.on_press),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut ExpandButtonWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // Closures aren't comparable; reinstall the adapter every time.
        element.on_press = erase_callback(&self.on_press);
        let mut flags = rebuild_child(&prev.mark_view(), &self.mark_view(), &mut element.mark, ctx);
        if prev.fill != self.fill
            || prev.ink != self.ink
            || prev.border != self.border
            || prev.on != self.on
            || prev.label != self.label
        {
            element.fill = self.fill;
            element.ink = self.ink;
            element.border = self.border;
            element.on = self.on;
            element.label = self.label.clone();
            // Semantics are recomputed on paint, so a label or state change needs PAINT.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut ExpandButtonWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.mark_view(), &mut element.mark, ctx);
    }
}

impl Widget for ExpandButtonWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let mark_bc = BoxConstraints::tight(Size::new(EXPAND_ICON_SIZE, EXPAND_ICON_SIZE));
        let mark = self.mark.layout_child(ctx, &mark_bc);
        let size = bc.constrain(Size::new(EXPAND_SIZE, EXPAND_SIZE));
        self.mark.set_origin(Point::new(
            (size.width - mark.width) / 2.0,
            (size.height - mark.height) / 2.0,
        ));
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        // Paint owns the press: a scale transform keeps hit testing on the full circle, as the CSS does.
        if self.pressed {
            let centre = Vec2::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
            scene.push_transform(
                Affine::translate(centre) * Affine::scale(PRESS_SCALE) * Affine::translate(-centre),
            );
        }
        let radius = effective_radius(RADIUS_FULL, size);
        if self.fill != Color::TRANSPARENT {
            scene.fill_rounded_rect(origin, size, radius, self.fill);
        }
        if self.border != Color::TRANSPARENT {
            stroke_border(scene, origin, size, radius, self.border);
        }
        self.mark.paint_child(ctx, scene);
        if self.pressed {
            scene.pop_transform();
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(pointer) = event else {
            return EventResult::Ignored;
        };
        match pointer.phase {
            PointerPhase::Down => {
                self.pressed = true;
                self.captured = true;
                ctx.capture_pointer();
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Move => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                self.pressed = inside(pointer.position, ctx.size());
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Up => {
                if !self.captured {
                    return EventResult::Ignored;
                }
                if inside(pointer.position, ctx.size()) {
                    (self.on_press)(ctx);
                }
                self.pressed = false;
                self.captured = false;
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Cancel => {
                self.pressed = false;
                self.captured = false;
                ctx.request_redraw();
                EventResult::Handled
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_node(Role::Button, |node| {
            node.set_label(self.label.as_str());
            node.add_action(Action::Click);
            if self.on {
                node.set_toggled(frust::accesskit::Toggled::True);
            }
        });
    }
}

/// `.term-wrap{padding:14px}`; fullscreen animates it to zero.
pub(super) const WRAP_PAD: f64 = 14.0;
/// `--radius-xl` (22px): the Glyph shape scale has no token above `large`.
pub(super) const CARD_RADIUS: f64 = 22.0;
/// `.term-card{box-shadow:0 20px 50px rgba(0,0,0,0.4)}`; CSS blur radius is 2σ.
const SHADOW_DY: f64 = 20.0;
const SHADOW_STD_DEV: f64 = 25.0;
const SHADOW_ALPHA: f32 = 0.4;

/// The app bar's band above this box. The shell mounts no bar for exactly as long as the
/// screen is fullscreen, flipped in the same rebuild as `fullscreen`.
fn chrome_band(fullscreen: bool, inset_top: f64) -> f64 {
    if fullscreen {
        0.0
    } else {
        inset_top + BAR_HEIGHT
    }
}

/// Both ends keep the status-bar inset: the pane title never sits under a notch.
fn card_top(p: f64, inset_top: f64, gutter_top: f64) -> f64 {
    inset_top + (BAR_HEIGHT + gutter_top) * (1.0 - p)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct StageBox {
    /// Local; negative through a collapse, while the card still covers the remounted bar's band.
    top: f64,
    left: f64,
    right: f64,
    bottom: f64,
}

/// Placed in window space, then localized: the box's top jumps by the bar band the frame the
/// bar unmounts, so a box-local path would jump with it.
fn stage_box(
    p: f64,
    inset_top: f64,
    inset_bottom: f64,
    gutters: EdgeInsets,
    fullscreen: bool,
) -> StageBox {
    let rest = 1.0 - p;
    StageBox {
        top: card_top(p, inset_top, gutters.top) - chrome_band(fullscreen, inset_top),
        left: (WRAP_PAD + gutters.left) * rest,
        right: (WRAP_PAD + gutters.right) * rest,
        bottom: (WRAP_PAD + gutters.bottom) * rest + inset_bottom,
    }
}

/// Eases the driver's progress before mapping onto `from..to`: `spatial` overshoots, so easing
/// the position stalls a collapse at `1.0`. Timings are the theme's, not the mock's.
struct FullscreenStage {
    driver: AnimationController,
    from: f64,
    to: f64,
    curve: Curve,
}

/// Glyph's `durations.slow`.
const FALLBACK_SLOW: Duration = Duration::from_millis(340);
/// Glyph's `spatial` easing.
const FALLBACK_SPATIAL: Curve = Curve::Cubic(0.34, 1.35, 0.64, 1.0);

impl FullscreenStage {
    /// `from` must be the last painted progress, never a raw driver `t`, or an interruption jumps.
    fn start(from: f64, to: f64, theme: Option<&Theme>) -> Self {
        let duration = theme.map_or(FALLBACK_SLOW, |theme| {
            Duration::from_secs_f64(theme.motion.durations.slow / 1000.0)
        });
        let mut driver = AnimationController::new(duration);
        driver.forward();
        FullscreenStage {
            driver,
            from,
            to,
            curve: theme.map_or(FALLBACK_SPATIAL, |theme| theme.motion.easing.spatial),
        }
    }

    /// Clamped once, at the end: `spatial` overshoots and a negative wrap padding pushes the card off screen.
    fn value(&self) -> f64 {
        let eased = self.curve.transform(self.driver.value_clamped());
        (self.from + (self.to - self.from) * eased).clamp(0.0, 1.0)
    }
}

/// One widget because padding, radius and clearance animate off one progress and an animated
/// padding is a layout fact; `gutters` is an input for the same reason.
pub(super) struct TerminalStageView<V> {
    child: V,
    fullscreen: bool,
    gutters: EdgeInsets,
    card: Color,
    border: Color,
}

pub(super) fn terminal_stage<V>(
    child: V,
    fullscreen: bool,
    gutters: EdgeInsets,
    card: Color,
    border: Color,
) -> TerminalStageView<V> {
    TerminalStageView {
        child,
        fullscreen,
        gutters,
        card,
        border,
    }
}

pub(super) struct TerminalStageWidget {
    child: ChildPod,
    fullscreen: bool,
    gutters: EdgeInsets,
    card: Color,
    border: Color,
    /// `0.0` windowed, `1.0` fullscreen; read by layout, written by paint.
    progress: f64,
    /// Created on the first paint after a flip: a `BuildCtx` carries no theme for `reduce_motion`.
    stage: Option<FullscreenStage>,
    pending: bool,
    card_rect: Rect,
    card_radius: f64,
}

impl<State: 'static, V: View<State>> View<State> for TerminalStageView<V> {
    type Element = TerminalStageWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> TerminalStageWidget {
        TerminalStageWidget {
            child: build_child(&self.child, ctx),
            fullscreen: self.fullscreen,
            gutters: self.gutters,
            card: self.card,
            border: self.border,
            progress: if self.fullscreen { 1.0 } else { 0.0 },
            stage: None,
            pending: false,
            card_rect: Rect::ZERO,
            card_radius: if self.fullscreen { 0.0 } else { CARD_RADIUS },
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut TerminalStageWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.fullscreen != self.fullscreen {
            element.fullscreen = self.fullscreen;
            element.pending = true;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.gutters != self.gutters {
            element.gutters = self.gutters;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.card != self.card || prev.border != self.border {
            element.card = self.card;
            element.border = self.border;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut TerminalStageWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for TerminalStageWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let insets = ctx.window_insets().padding();
        let width = finite_or_zero(bc.max().width);
        let height = finite_or_zero(bc.max().height);
        let p = self.progress.clamp(0.0, 1.0);

        let card = stage_box(p, insets.top, insets.bottom, self.gutters, self.fullscreen);
        let card_w = (width - card.left - card.right).max(0.0);
        let card_h = (height - card.top - card.bottom).max(0.0);
        self.child
            .layout_child(ctx, &BoxConstraints::tight(Size::new(card_w, card_h)));
        self.child.set_origin(Point::new(card.left, card.top));
        self.card_rect = Rect::new(card.left, card.top, card.left + card_w, card.top + card_h);
        // Taken from the layout `p`: paint has already advanced the driver one frame ahead.
        self.card_radius = CARD_RADIUS * (1.0 - p);

        bc.constrain(Size::new(width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        // `reduce_motion` snaps both directions.
        if self.pending {
            self.pending = false;
            let target = if self.fullscreen { 1.0 } else { 0.0 };
            if theme.is_some_and(|theme| theme.motion.reduce_motion) {
                self.progress = target;
                self.stage = None;
                // The staging pass laid out the old progress, so the snap needs its own layout.
                ctx.request_layout();
            } else {
                self.stage = Some(FullscreenStage::start(self.progress, target, theme));
            }
        }
        if let Some(stage) = self.stage.as_mut() {
            let animating = stage.driver.advance(ctx.frame_time());
            self.progress = stage.value();
            // `progress` is a layout input: a bare `request_frame` freezes the geometry.
            if animating {
                ctx.request_layout();
            } else {
                self.progress = if self.fullscreen { 1.0 } else { 0.0 };
                self.stage = None;
                // One more pass so the settled geometry is the one on screen.
                ctx.request_layout();
            }
        }

        let origin = Point::new(
            ctx.origin().x + self.card_rect.x0,
            ctx.origin().y + self.card_rect.y0,
        );
        let size = Size::new(self.card_rect.width(), self.card_rect.height());
        let radius = effective_radius(self.card_radius, size);

        scene.draw_shadow(
            Point::new(origin.x, origin.y + SHADOW_DY),
            size,
            radius,
            SHADOW_STD_DEV,
            Color::BLACK.with_alpha(SHADOW_ALPHA),
        );
        scene.fill_rounded_rect(origin, size, radius, self.card);
        stroke_border(scene, origin, size, radius, self.border);

        // Without this clip the status row's square band corners poke through the rounding.
        scene.push_clip_rounded(origin, size, radius);
        self.child.paint_child(ctx, scene);
        scene.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

/// `.term-line` stagger step (`setTimeout(.., i*140 + 60)`).
pub(super) const REVEAL_STEP_MS: f64 = 140.0;
pub(super) const REVEAL_LEAD_MS: f64 = 60.0;
/// `.term-line{transition:opacity .18s, transform .18s}`.
pub(super) const REVEAL_DURATION_MS: f64 = 180.0;
/// `.term-line{transform:translateY(4px)}`.
pub(super) const REVEAL_RISE: f64 = 4.0;
/// `.cur{animation:blink 1.1s steps(1) infinite}` — one full on/off cycle.
pub(super) const BLINK_PERIOD_MS: f64 = 1100.0;
/// `.cur{width:7px;height:13px}`.
pub(super) const CURSOR_WIDTH: f64 = 7.0;
pub(super) const CURSOR_HEIGHT: f64 = 13.0;
/// `.cur{vertical-align:-2px}`.
const CURSOR_DROP: f64 = 2.0;

/// Glyph's `effects` easing; it never overshoots.
const FALLBACK_EFFECTS: Curve = Curve::Cubic(0.16, 1.0, 0.3, 1.0);

#[derive(Clone, PartialEq)]
pub(super) struct LineStyles {
    pub(super) prompt: TextStyle,
    pub(super) output: TextStyle,
    pub(super) comment: TextStyle,
}

impl LineStyles {
    fn of(&self, kind: TermLineKind) -> &TextStyle {
        match kind {
            TermLineKind::Prompt => &self.prompt,
            TermLineKind::Output => &self.output,
            TermLineKind::Comment => &self.comment,
        }
    }
}

/// The reveal replays whenever the lines or `epoch` change; `epoch` is the controller's replay
/// token, since per-frame progress is not a signal.
pub(super) struct TermBodyView {
    lines: Vec<TermLine>,
    epoch: u64,
    styles: LineStyles,
    cursor: Color,
}

pub(super) fn term_body(
    lines: Vec<TermLine>,
    epoch: u64,
    styles: LineStyles,
    cursor: Color,
) -> TermBodyView {
    TermBodyView {
        lines,
        epoch,
        styles,
        cursor,
    }
}

struct LineRun {
    kind: TermLineKind,
    text: String,
    cursor: bool,
    run: GlyphLabel,
    size: Size,
}

impl LineRun {
    fn new(line: &TermLine) -> Self {
        Self {
            kind: line.kind,
            text: line.text.clone(),
            cursor: line.cursor,
            run: GlyphLabel::new(line.text.clone()),
            size: Size::ZERO,
        }
    }
}

pub(super) struct TermBodyWidget {
    lines: Vec<LineRun>,
    epoch: u64,
    styles: LineStyles,
    cursor: Color,
    reveal: Option<AnimationController>,
    replay: bool,
    /// Seeded on the first paint that draws a caret, so it is lit the instant a tab appears.
    blink_from: Option<FrameTime>,
}

fn reveal_total_ms(n: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    (n as f64 - 1.0) * REVEAL_STEP_MS + REVEAL_LEAD_MS + REVEAL_DURATION_MS
}

fn line_progress(index: usize, elapsed_ms: f64) -> f64 {
    let start = index as f64 * REVEAL_STEP_MS + REVEAL_LEAD_MS;
    ((elapsed_ms - start) / REVEAL_DURATION_MS).clamp(0.0, 1.0)
}

fn caret_lit(elapsed_ms: f64) -> bool {
    (elapsed_ms % BLINK_PERIOD_MS) < BLINK_PERIOD_MS / 2.0
}

impl<State: 'static> View<State> for TermBodyView {
    type Element = TermBodyWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> TermBodyWidget {
        TermBodyWidget {
            lines: self.lines.iter().map(LineRun::new).collect(),
            epoch: self.epoch,
            styles: self.styles.clone(),
            cursor: self.cursor,
            reveal: None,
            replay: true,
            blink_from: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut TermBodyWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if prev.lines != self.lines {
            element.lines = self.lines.iter().map(LineRun::new).collect();
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.epoch != self.epoch || flags.contains(ChangeFlags::LAYOUT) {
            element.epoch = self.epoch;
            element.replay = true;
            element.blink_from = None;
            flags |= ChangeFlags::PAINT;
        }
        if prev.styles != self.styles || prev.cursor != self.cursor {
            element.styles = self.styles.clone();
            element.cursor = self.cursor;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        flags
    }
}

impl Widget for TermBodyWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let max_width = bc.max().width;
        let mut width = 0.0f64;
        let mut height = 0.0f64;
        for line in &mut self.lines {
            let reserved = if line.cursor { CURSOR_WIDTH } else { 0.0 };
            let wrap_at = if max_width.is_finite() {
                Some(((max_width - reserved).max(0.0)) as f32)
            } else {
                None
            };
            let style = self.styles.of(line.kind).clone();
            line.size = line.run.layout(ctx, &style, wrap_at);
            width = width.max(line.size.width + reserved);
            height += line.size.height;
        }
        bc.constrain(Size::new(
            if max_width.is_finite() {
                max_width
            } else {
                width
            },
            height,
        ))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        let reduce_motion = theme.is_some_and(|theme| theme.motion.reduce_motion);
        let easing = theme.map_or(FALLBACK_EFFECTS, |theme| theme.motion.easing.effects);

        // Armed at paint, where a theme is in scope; reduced motion shows every line at once.
        if self.replay {
            self.replay = false;
            self.reveal = (!reduce_motion).then(|| {
                let total = reveal_total_ms(self.lines.len());
                let mut driver = AnimationController::new(Duration::from_secs_f64(total / 1000.0))
                    .with_curve(Curve::Linear);
                driver.forward();
                driver
            });
        }
        let total_ms = reveal_total_ms(self.lines.len());
        let elapsed_ms = match self.reveal.as_mut() {
            None => f64::INFINITY,
            Some(driver) => {
                let animating = driver.advance(ctx.frame_time());
                let elapsed = driver.value_clamped() * total_ms;
                if animating {
                    ctx.request_frame();
                } else {
                    self.reveal = None;
                }
                elapsed
            }
        };

        // `reduce_motion` freezes the caret lit: it is a position cue.
        let lit = if reduce_motion {
            true
        } else {
            let from = *self.blink_from.get_or_insert(ctx.frame_time());
            let since = ctx.frame_time().saturating_sub(from).as_secs_f64() * 1000.0;
            // A cosmetic loop the frame gate may throttle: the one paint-only exception to LAYOUT|PAINT.
            ctx.request_frame_class(TickClass::CosmeticLoop);
            caret_lit(since)
        };

        let origin = ctx.origin();
        let mut y = origin.y;
        for (index, line) in self.lines.iter().enumerate() {
            let progress = easing
                .transform(line_progress(index, elapsed_ms))
                .clamp(0.0, 1.0);
            if progress > 0.0 {
                let dy = (1.0 - progress) * REVEAL_RISE;
                let faded = progress < 1.0;
                if faded {
                    scene.push_layer(
                        Point::new(origin.x, y),
                        Size::new(ctx.size().width, line.size.height),
                        progress as f32,
                    );
                }
                let at = Point::new(origin.x, y + dy);
                line.run.paint(at, scene);
                if line.cursor && lit {
                    scene.fill_rect(
                        Point::new(
                            at.x + line.size.width,
                            at.y + (line.size.height - CURSOR_HEIGHT) / 2.0 + CURSOR_DROP,
                        ),
                        Size::new(CURSOR_WIDTH, CURSOR_HEIGHT),
                        self.cursor,
                    );
                }
                if faded {
                    scene.pop_layer();
                }
            }
            y += line.size.height;
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // One label for the whole body: separate runs would lose that they are one transcript.
        let text = self
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        ctx.push_node(Role::Label, |node| {
            node.set_value(text.as_str());
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_radius_collapses_to_a_pill_rather_than_overshooting() {
        let pill = Size::new(60.0, 18.0);
        assert_eq!(effective_radius(RADIUS_FULL, pill), 9.0);
        assert_eq!(effective_radius(CARD_RADIUS, Size::new(340.0, 600.0)), 22.0);
    }

    #[test]
    fn a_wrapped_row_keeps_an_unbounded_height_unbounded() {
        let unbounded = BoxConstraints::loose(Size::new(600.0, f64::INFINITY));
        let child_bc = BoxConstraints::loose(Size::new(
            finite_or_zero(unbounded.max().width),
            unbounded.max().height,
        ));
        assert_eq!(child_bc.max().width, 600.0);
        assert!(child_bc.max().height.is_infinite());
        assert_eq!(finite_or_zero(f64::INFINITY), 0.0);
    }

    #[test]
    fn the_reveal_window_is_the_mocks_stagger() {
        // `i*140 + 60` then 180ms: line 0 spans 60..240ms, line 3 opens at 480ms.
        assert_eq!(line_progress(0, 0.0), 0.0);
        assert_eq!(line_progress(0, 60.0), 0.0);
        assert_eq!(line_progress(0, 150.0), 0.5);
        assert_eq!(line_progress(0, 240.0), 1.0);
        assert_eq!(line_progress(3, 400.0), 0.0, "line 3 has not started");
        assert_eq!(line_progress(3, 660.0), 1.0);
    }

    #[test]
    fn the_whole_reveal_ends_when_its_last_line_does() {
        assert_eq!(reveal_total_ms(0), 0.0);
        assert_eq!(reveal_total_ms(1), REVEAL_LEAD_MS + REVEAL_DURATION_MS);
        // 3*140 + 60 + 180.
        assert_eq!(reveal_total_ms(4), 660.0);
        for index in 0..4 {
            assert_eq!(line_progress(index, reveal_total_ms(4)), 1.0);
        }
    }

    #[test]
    fn the_caret_is_lit_for_half_of_each_cycle() {
        assert!(caret_lit(0.0), "a fresh caret is on");
        assert!(caret_lit(500.0));
        assert!(!caret_lit(600.0));
        assert!(!caret_lit(1_099.0));
        assert!(caret_lit(1_101.0), "and the cycle repeats");
    }

    #[test]
    fn the_fullscreen_stage_ends_flush_at_both_ends() {
        let theme = frust_glyph::baseline();
        let entering = FullscreenStage::start(0.0, 1.0, Some(&theme));
        assert_eq!(entering.value(), 0.0);
        let leaving = FullscreenStage::start(1.0, 0.0, Some(&theme));
        assert_eq!(leaving.value(), 1.0);
    }

    fn painted_mid_entrance(theme: &Theme) -> f64 {
        let mut entering = FullscreenStage::start(0.0, 1.0, Some(theme));
        // The first `advance` only seeds the clock; the second elapses 60ms.
        entering.driver.advance(FrameTime::from_nanos(0));
        entering.driver.advance(FrameTime::from_nanos(60_000_000));
        entering.value()
    }

    #[test]
    fn collapsing_leaves_1_0_well_before_half_the_duration_rather_than_stalling() {
        let theme = frust_glyph::baseline();
        let mut collapsing = FullscreenStage::start(1.0, 0.0, Some(&theme));
        collapsing.driver.advance(FrameTime::from_nanos(0));
        collapsing.driver.advance(FrameTime::from_nanos(80_000_000));
        let value = collapsing.value();
        assert!(
            value < 0.9,
            "80ms in, the collapse should already have left 1.0: {value}"
        );
    }

    #[test]
    fn an_interrupted_stage_starts_exactly_at_the_painted_value_it_seeds_from() {
        let theme = frust_glyph::baseline();
        let seed = painted_mid_entrance(&theme);
        assert!((0.0..1.0).contains(&seed), "60ms in, mid-flight: {seed}");

        let interrupted = FullscreenStage::start(seed, 0.0, Some(&theme));
        assert_eq!(
            interrupted.value(),
            seed,
            "a freshly-seeded stage must read exactly what was last painted, \
             not that value curved a second time"
        );
    }

    #[test]
    fn an_interrupted_collapse_never_lets_its_value_climb_back_toward_the_seed() {
        let theme = frust_glyph::baseline();
        let seed = painted_mid_entrance(&theme);
        let mut collapsing = FullscreenStage::start(seed, 0.0, Some(&theme));
        assert_eq!(collapsing.value(), seed);

        let mut last = seed;
        for cumulative_ms in [0u64, 10, 30, 50, 80, 120, 170, 250, 340, 500] {
            collapsing
                .driver
                .advance(FrameTime::from_nanos(cumulative_ms * 1_000_000));
            let value = collapsing.value();
            assert!((0.0..=1.0).contains(&value), "{value}");
            assert!(
                value <= last + 1e-9,
                "value climbed back from {last} to {value} at {cumulative_ms}ms \
                 after the interrupt"
            );
            last = value;
        }
        assert!(last.abs() < 1e-6, "settles flush at the target: {last}");
    }

    #[test]
    fn the_stages_geometry_never_overshoots_its_own_screen() {
        let theme = frust_glyph::baseline();
        let stage = FullscreenStage::start(0.0, 1.0, Some(&theme));
        for raw in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let eased = stage.curve.transform(raw).clamp(0.0, 1.0);
            assert!((0.0..=1.0).contains(&eased), "{eased}");
            assert!(WRAP_PAD * (1.0 - eased) >= 0.0);
        }
    }

    const INSET_TOP: f64 = 44.0;
    const INSET_BOTTOM: f64 = 34.0;
    const GUTTER_TOP: f64 = 8.0;
    const WIDE_GUTTER: f64 = 8.0;

    fn portrait_gutters() -> EdgeInsets {
        EdgeInsets {
            left: 0.0,
            top: GUTTER_TOP,
            right: 0.0,
            bottom: 0.0,
        }
    }

    /// Transcribes the scaffold's body origin rather than reading [`chrome_band`], so drift fails.
    fn on_screen_top(p: f64, fullscreen: bool) -> f64 {
        let body_top = if fullscreen {
            0.0
        } else {
            INSET_TOP + BAR_HEIGHT
        };
        body_top + stage_box(p, INSET_TOP, INSET_BOTTOM, portrait_gutters(), fullscreen).top
    }

    const WINDOWED_TOP: f64 = INSET_TOP + BAR_HEIGHT + GUTTER_TOP;
    const FULLSCREEN_TOP: f64 = INSET_TOP;

    fn painted_tops(fullscreen: bool, theme: &Theme) -> Vec<f64> {
        let (from, to) = if fullscreen { (0.0, 1.0) } else { (1.0, 0.0) };
        let mut stage = FullscreenStage::start(from, to, Some(theme));
        let mut tops = vec![on_screen_top(from, fullscreen)];
        for frame in 0..30u64 {
            stage
                .driver
                .advance(FrameTime::from_nanos(frame * 16_000_000));
            tops.push(on_screen_top(stage.value(), fullscreen));
        }
        tops
    }

    #[test]
    fn the_bar_leaving_moves_the_box_but_never_the_card() {
        assert_eq!(on_screen_top(0.0, false), on_screen_top(0.0, true));
        assert_eq!(on_screen_top(1.0, true), on_screen_top(1.0, false));
        assert_eq!(on_screen_top(0.0, false), WINDOWED_TOP);
        assert_eq!(on_screen_top(1.0, true), FULLSCREEN_TOP);
        let inset_of = |fullscreen| {
            stage_box(0.0, INSET_TOP, INSET_BOTTOM, portrait_gutters(), fullscreen).bottom
        };
        assert_eq!(inset_of(false), inset_of(true));
    }

    #[test]
    fn entering_fullscreen_walks_the_card_up_without_a_step_back() {
        let tops = painted_tops(true, &frust_glyph::baseline());
        assert_eq!(tops[0], WINDOWED_TOP, "the flip frame has not moved yet");
        for pair in tops.windows(2) {
            assert!(
                pair[1] <= pair[0] + 1e-9,
                "the card dropped back down: {pair:?}"
            );
            assert!(
                (FULLSCREEN_TOP..=WINDOWED_TOP).contains(&pair[1]),
                "{pair:?}"
            );
        }
        assert!(
            tops.iter()
                .any(|top| (FULLSCREEN_TOP..WINDOWED_TOP).contains(top)),
            "…and it travelled rather than snapped: {tops:?}"
        );
        assert_eq!(tops.last().copied(), Some(FULLSCREEN_TOP));
    }

    #[test]
    fn leaving_fullscreen_walks_it_back_down_the_same_way() {
        let tops = painted_tops(false, &frust_glyph::baseline());
        assert_eq!(tops[0], FULLSCREEN_TOP, "the flip frame has not moved yet");
        for pair in tops.windows(2) {
            assert!(
                pair[1] >= pair[0] - 1e-9,
                "the card climbed back up: {pair:?}"
            );
            assert!(
                (FULLSCREEN_TOP..=WINDOWED_TOP).contains(&pair[1]),
                "{pair:?}"
            );
        }
        assert_eq!(tops.last().copied(), Some(WINDOWED_TOP));
    }

    #[test]
    fn a_reduced_motion_flip_lands_on_the_endpoint_the_animation_walks_to() {
        let theme = frust_glyph::baseline();
        assert_eq!(
            on_screen_top(1.0, true),
            painted_tops(true, &theme).last().copied().expect("stepped")
        );
        assert_eq!(
            on_screen_top(0.0, false),
            painted_tops(false, &theme)
                .last()
                .copied()
                .expect("stepped")
        );
    }

    #[test]
    fn the_gutters_ride_the_same_progress_the_card_does() {
        let windowed = stage_box(0.0, INSET_TOP, INSET_BOTTOM, portrait_gutters(), false);
        assert_eq!(windowed.left, WRAP_PAD);
        assert_eq!(windowed.bottom, WRAP_PAD + INSET_BOTTOM);
        let full = stage_box(1.0, INSET_TOP, INSET_BOTTOM, portrait_gutters(), true);
        assert_eq!((full.left, full.right), (0.0, 0.0));
        // Edge to edge never puts the last output line under the home bar.
        assert_eq!(full.bottom, INSET_BOTTOM);

        let wide = EdgeInsets::all(WIDE_GUTTER);
        assert_eq!(
            stage_box(0.0, INSET_TOP, INSET_BOTTOM, wide, false).left,
            WRAP_PAD + WIDE_GUTTER
        );
        assert_eq!(
            stage_box(1.0, INSET_TOP, INSET_BOTTOM, wide, true).left,
            0.0
        );
    }

    #[test]
    fn the_unthemed_fallbacks_are_the_glyph_tokens_they_stand_in_for() {
        let motion = frust_glyph::baseline().motion;
        assert_eq!(FALLBACK_SLOW.as_millis() as f64, motion.durations.slow);
        assert_eq!(FALLBACK_SPATIAL, motion.easing.spatial);
        assert_eq!(FALLBACK_EFFECTS, motion.easing.effects);
    }
}
