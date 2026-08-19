//! The Terminal screen's remaining hand-rolled primitives: [`status_strip`]
//! (the link notice), [`expand_button`] (`.expand-btn`), [`terminal_stage`]
//! (`.term-wrap` + `.term-card`, animated), [`term_body`] (`.term-body`, its own
//! text pipeline) and [`wrap_row`] (a wrapping row, since `frust::Flex` has
//! none).
//!
//! **Dissolved into the framework since this module first shipped**: the
//! `.pill`/`.pane-item`/`.term-statusrow` box (now every caller's own
//! `frust::container`, `.border_style(BorderStyle::Dashed(..))` for the dashed
//! edges) and the `.term-pane-title` single-line ellipsis (now
//! `frust::text(..).max_lines(1).overflow(TextOverflow::Ellipsis)`, in
//! [`super::widgets::status`]) — both closed the framework gaps their own prior
//! doc comments named (no dash pattern on `PaintScene`, no text
//! overflow/ellipsis vocabulary). The three vector marks this module used to
//! author (`⛶`/`⤡`/`▤`) are now [`frust::icons`]'
//! `FULLSCREEN_EXPAND`/`FULLSCREEN_EXIT`/`PANE_MARK` — the catalog's own art,
//! not a transcription of `docs/terminal.html`'s marks (Ed device-gates the
//! swap).
//!
//! **Feature-local, and knowingly parallel to `features::sessions`' own
//! `chrome`**: a feature never imports a sibling's `presentation`, so promoting
//! these into `crate::ui` is a job for a sweep that touches both screens.
//!
//! Every piece takes already-resolved [`Color`]s/[`TextStyle`]s —
//! [`super::widgets`] resolves them from the active [`frust::Theme`] once per
//! build. The exception is [`terminal_stage`], which reads `reduce_motion` and
//! the motion tokens from the **paint-time** theme, the only pass where a theme
//! is in scope when a transition starts (the deferred-driver idiom
//! `crate::ui::topbar` documents).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use frust::authoring::TickClass;
use frust::authoring::text::{TextContext, TextLayout, TextStyle};
use frust::authoring::{
    Action, Affine, AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod,
    Color, ErasedCallback, EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene,
    Point, PointerPhase, Rect, Role, RoundedRect, SemanticsCtx, Shape, Size, Vec2, View, Widget,
    any, build_child, erase_callback, rebuild_child, route_event, route_event_single,
    teardown_child,
};
use frust::{
    AnimationController, Axis, CrossAxisAlignment, Curve, EdgeInsets, FlexView, FrameTime,
    IconData, Padding, Theme, container, flexible, icon, inflexible, text,
};

use crate::features::terminal::domain::entities::{TermLine, TermLineKind};
use crate::ui::topbar::BAR_HEIGHT;

pub(super) use crate::ui::paint::{DASH_OFF, DASH_ON, HAIRLINE, RADIUS_FULL};

// Shared paint helpers.

/// Flattening tolerance for a rounded-rect border path — the value
/// `frust-widgets`' own border strokes use.
const BORDER_TOLERANCE: f64 = 0.1;

/// The effective corner radius for a box of `size` — [`RADIUS_FULL`] (or any
/// over-large value) collapses to a true pill rather than overshooting.
fn effective_radius(radius: f64, size: Size) -> f64 {
    radius.min(size.width / 2.0).min(size.height / 2.0).max(0.0)
}

/// Coerce a possibly-infinite constraint dimension to a finite value (mirrors
/// the helper of the same name `frust_glyph`'s own modal widgets carry).
fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() { v } else { 0.0 }
}

/// Stroke a hairline border just inside `origin`/`size`, inset by half the
/// stroke width so it paints fully inside the box's own bounds.
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

/// Whether `pos` (widget-local) falls inside a `size`-sized box.
fn inside(pos: Point, size: Size) -> bool {
    pos.x >= 0.0 && pos.y >= 0.0 && pos.x < size.width && pos.y < size.height
}

/// The strip's own gutters — the status row's 18px lead/14px trail, transcribed a
/// second time because that row's constants are private to [`super::widgets`];
/// the two are the card's one left edge, so they have to agree.
const STRIP_PAD_LEFT: f64 = 18.0;
const STRIP_PAD_RIGHT: f64 = 14.0;
/// Tighter than the status row's 14px: this band is a transient notice above a
/// grid, and every row it steals is a row of terminal.
const STRIP_PAD_Y: f64 = 10.0;
/// Gap between the message and the affordance beside it — the sheet's own pill
/// gap, so the card and its sheet breathe alike.
const STRIP_GAP: f64 = 8.0;

/// A one-line notice band across the card, optionally ending in one affordance:
/// how the screen surfaces a live session's link state. A band rather than an
/// overlay because it must not cover output the user is still reading — a
/// reconnecting session is *stale*, not gone. Square-cornered and full-bleed,
/// since the card clips to its own radius. `action` is an already-built view
/// rather than a label plus a callback, so this stays free of a type parameter.
pub(super) fn status_strip<State: 'static>(
    message: impl Into<String>,
    style: TextStyle,
    fill: Color,
    action: Option<AnyView<State>>,
) -> AnyView<State> {
    let mut children = vec![flexible(1, text(message.into()).style(style))];
    if let Some(action) = action {
        children.push(inflexible(frust::SizedBox::<State>(Some(STRIP_GAP), None)));
        children.push(inflexible(action));
    }
    any(container(Padding(
        EdgeInsets {
            left: STRIP_PAD_LEFT,
            top: STRIP_PAD_Y,
            right: STRIP_PAD_RIGHT,
            bottom: STRIP_PAD_Y,
        },
        FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center),
    ))
    .fill(fill))
}

/// A row of children that wraps onto further lines when it runs out of width, with
/// `gap` between neighbours on both axes. Hand-rolled because **frust's `Flex` has
/// no wrap**: the sheet's five tab pills come to roughly 390 logical px, wider
/// than the panel on every phone this app targets.
pub(super) struct WrapRowView<State: 'static> {
    children: Vec<AnyView<State>>,
    gap: f64,
}

/// `children` laid out left to right, wrapping at the available width.
pub(super) fn wrap_row<State: 'static>(children: Vec<AnyView<State>>, gap: f64) -> AnyView<State> {
    any(WrapRowView { children, gap })
}

/// The retained half of a [`wrap_row`].
pub(super) struct WrapRowWidget {
    children: Vec<ChildPod>,
    gap: f64,
}

impl<State: 'static> View<State> for WrapRowView<State> {
    type Element = WrapRowWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> WrapRowWidget {
        WrapRowWidget {
            children: self
                .children
                .iter()
                .map(|child| build_child(child, ctx))
                .collect(),
            gap: self.gap,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut WrapRowWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if prev.children.len() != self.children.len() {
            // A changed child *count* cannot be reconciled positionally (the
            // erased types no longer line up), so the row is rebuilt whole.
            for (view, pod) in prev.children.iter().zip(element.children.iter_mut()) {
                teardown_child(view, pod, ctx);
            }
            element.children = self
                .children
                .iter()
                .map(|child| build_child(child, ctx))
                .collect();
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        } else {
            for ((prev_child, child), pod) in prev
                .children
                .iter()
                .zip(self.children.iter())
                .zip(element.children.iter_mut())
            {
                flags |= rebuild_child(prev_child, child, pod, ctx);
            }
        }
        if prev.gap != self.gap {
            element.gap = self.gap;
            flags |= ChangeFlags::LAYOUT;
        }
        flags
    }

    fn teardown(&self, element: &mut WrapRowWidget, ctx: &mut BuildCtx<'_>) {
        for (view, pod) in self.children.iter().zip(element.children.iter_mut()) {
            teardown_child(view, pod, ctx);
        }
    }
}

impl Widget for WrapRowWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let max_width = finite_or_zero(bc.max().width);
        // The **height** is passed through unclamped, infinity included: a vertical
        // `Flex` hands its children an unbounded main axis, and coercing that to
        // zero (as the width has to be, to wrap against it) would flatten them.
        let child_bc = BoxConstraints::loose(Size::new(max_width, bc.max().height));
        let (mut x, mut y) = (0.0f64, 0.0f64);
        let mut line_height = 0.0f64;
        let mut widest = 0.0f64;
        for child in &mut self.children {
            let size = child.layout_child(ctx, &child_bc);
            if x > 0.0 && x + size.width > max_width {
                x = 0.0;
                y += line_height + self.gap;
                line_height = 0.0;
            }
            child.set_origin(Point::new(x, y));
            x += size.width + self.gap;
            widest = widest.max(x - self.gap);
            line_height = line_height.max(size.height);
        }
        bc.constrain(Size::new(widest.min(max_width), y + line_height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        for child in &mut self.children {
            child.paint_child(ctx, scene);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event(&mut self.children, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        for child in &self.children {
            child.semantics_child(ctx);
        }
    }
}

/// A minimal retained text run — `frust-widgets`' own per-module `GlyphLabel`
/// helper, re-derived here because it is crate-private. [`term_body`]'s own
/// per-line shaping is this module's one remaining consumer: the ellipsized
/// single-line run this type used to *also* back (`.term-pane-title`) is now
/// `frust::text(..).max_lines(1).overflow(TextOverflow::Ellipsis)`, in
/// [`super::widgets::status`].
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

    /// Shape `content` at `style`, reusing the cached layout when neither has
    /// changed. `max_width` is only ever `None` here.
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
/// The drawn size of the vector inside that circle. An original value: the source
/// sets its glyph at 12px, and a Material-weight outline reads thinner than a
/// glyph does at that size.
const EXPAND_ICON_SIZE: f64 = 15.0;
/// `.expand-btn:active{transform:scale(0.9)}`.
const PRESS_SCALE: f64 = 0.9;

/// The 28px circle on the card's status row: the **only** fullscreen affordance,
/// in the same place in both states. Hand-rolled rather than
/// `crate::ui::topbar::icon_button` because it needs a hairline border and the
/// `:active` scale press treatment rather than a tone wash.
pub(super) struct ExpandButtonView<State: 'static> {
    mark: IconData,
    label: String,
    /// Resting fill/ink/border, and the on-state trio the mock swaps in.
    fill: Color,
    ink: Color,
    border: Color,
    on: bool,
    on_press: Rc<dyn Fn(&mut State)>,
}

/// The `.expand-btn`, drawing `mark` and firing `on_press` when tapped.
/// `label` is its accessible name (never the shape of its mark).
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
    /// The resting (or, with [`on`](Self::on), the active) fill, ink and
    /// border — resolved by [`super::widgets`], like every other colour here.
    pub(super) fn tones(mut self, fill: Color, ink: Color, border: Color) -> Self {
        self.fill = fill;
        self.ink = ink;
        self.border = border;
        self
    }

    /// Whether the button is in its `.on` state. Purely a *semantics* input —
    /// the caller passes the amber trio through [`tones`](Self::tones).
    pub(super) fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    /// The mark, as the pod this button paints and lays out.
    fn mark_view(&self) -> AnyView<State> {
        any(icon(self.mark.clone())
            .size(EXPAND_ICON_SIZE)
            .color(self.ink))
    }
}

/// The retained half of an [`expand_button`].
pub(super) struct ExpandButtonWidget {
    mark: ChildPod,
    label: String,
    fill: Color,
    ink: Color,
    border: Color,
    on: bool,
    /// The pressed *visual*; follows the pointer in and out while captured.
    pressed: bool,
    /// Armed by a `Down` (alongside `capture_pointer`), cleared on
    /// `Up`/`Cancel` — gates `Move`/`Up` so a hover never latches or fires.
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
            // Semantics are recomputed off the paint pass, so a label/state
            // change republishes through PAINT.
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
        // `:active{transform:scale(0.9)}` about the button's own centre: a
        // transform rather than a smaller rect, so the mark shrinks with the
        // circle and hit testing stays at full size, as the CSS does.
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
            // Flags only — a `Cancel` arm never touches state
            // (`docs/CODE_STANDARDS.md`'s Interaction Semantics).
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

/// `.term-wrap{padding:14px}` — the gap around the card in the windowed state,
/// which fullscreen animates to zero.
pub(super) const WRAP_PAD: f64 = 14.0;
/// `.term-card{border-radius:var(--radius-xl)}` — 22px. A **local** constant:
/// Glyph's own shape scale tops out at `large` (16) and reaches for `full`
/// above it, so there is no token carrying the mock's `--radius-xl`.
pub(super) const CARD_RADIUS: f64 = 22.0;
/// `.term-card{box-shadow:0 20px 50px rgba(0,0,0,0.4)}` — the offset, the
/// gaussian σ (CSS blur radius is 2σ), and the ink.
const SHADOW_DY: f64 = 20.0;
const SHADOW_STD_DEV: f64 = 25.0;
const SHADOW_ALPHA: f32 = 0.4;

/// `.appbar{padding:0 6px}`/`.ab-icon{width:40px;height:40px}`, transcribed
/// from `frust_glyph::appbar` (whose own copies are private) so this screen
/// can hand `frust_glyph::show_glyph_menu` the window-coordinate rect of the
/// bar slot its `⋮` lives in. [`BAR_HEIGHT`] itself *is* `pub` and is
/// imported — one shared source for the bar's own content height, the one
/// dimension `crate::ui::topbar` already needs to transcribe too.
const BAR_PAD_X: f64 = 6.0;
const BAR_SLOT: f64 = 40.0;

/// One in-flight fullscreen transition: a linear `0→1` driver, the curve that
/// eases its progress, and the `from..to` range that eased progress is then
/// interpolated across. The range is what makes an *interrupted* transition
/// value-continuous — a second tap mid-flight starts a fresh driver from wherever
/// the card actually is (see [`start`](Self::start)).
///
/// **Ease the progress, not the position.** [`value`](Self::value) applies `curve`
/// to the driver's `0..=1` progress *before* mapping onto `from..to`. Glyph's
/// `spatial` curve is an authored overshoot exceeding `1.0` on roughly
/// `t ∈ [0.45, 1.0]`, so curving the already-interpolated position would leave a
/// collapse (`from = 1, to = 0`) clamped flush at `1.0` for the first half of its
/// run — the card stalling in place before it visibly moves.
///
/// **Timings are the theme's, not the mock's**: Glyph's `durations.slow` (340ms)
/// with `easing.spatial`. The authored duration/easing pair, not the
/// `default_spatial` spring slot beside it — the springs are hand-tuned
/// approximations of these very beziers.
struct FullscreenStage {
    driver: AnimationController,
    from: f64,
    to: f64,
    curve: Curve,
}

/// Unthemed-fallback transition duration — Glyph's `durations.slow`.
const FALLBACK_SLOW: Duration = Duration::from_millis(340);
/// Unthemed-fallback `spatial` easing — the Glyph curve.
const FALLBACK_SPATIAL: Curve = Curve::Cubic(0.34, 1.35, 0.64, 1.0);

impl FullscreenStage {
    /// Stage a transition from `from` to `to` under `theme`'s motion policy.
    /// `from` must be the last **painted** progress, never a raw driver `t`:
    /// `value()` reads `from` at `t = 0`, so seeding anything but the on-screen
    /// value reintroduces a jump on interruption.
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

    /// The eased `0..=1` geometry fraction this frame: eases the driver's
    /// progress, **then** interpolates `from..to`. `t = 0` returns exactly
    /// `from` and `t = 1` exactly `to`, which is what makes a freshly-started
    /// stage value-continuous. Clamped once, at the end: the `spatial` curve
    /// overshoots, and a negative wrap padding would push the card off screen.
    fn value(&self) -> f64 {
        let eased = self.curve.transform(self.driver.value_clamped());
        (self.from + (self.to - self.from) * eased).clamp(0.0, 1.0)
    }
}

/// The screen's whole geometry: the animated top clearance, the animated wrap
/// padding, and the card (fill, hairline, animated radius, drop shadow) around
/// `child`.
///
/// One widget rather than three nested ones because all three animate off the
/// **same** progress in one gesture, and because an animated padding is a *layout*
/// fact. It also records the app bar's trailing-slot rect into
/// [`anchor`](TerminalStageView::anchor): the overflow menu anchors to a slot
/// inside a sibling widget this page cannot query, so the one widget that knows
/// both the window box and the top inset computes it.
pub(super) struct TerminalStageView<State: 'static> {
    child: AnyView<State>,
    fullscreen: bool,
    card: Color,
    border: Color,
    anchor: Rc<Cell<Rect>>,
}

/// The `.term-wrap`/`.term-card` stage around `child`.
pub(super) fn terminal_stage<State: 'static, V: View<State>>(
    child: V,
    fullscreen: bool,
    card: Color,
    border: Color,
    anchor: Rc<Cell<Rect>>,
) -> TerminalStageView<State> {
    TerminalStageView {
        child: any(child),
        fullscreen,
        card,
        border,
        anchor,
    }
}

/// The retained half of a [`terminal_stage`].
pub(super) struct TerminalStageWidget {
    child: ChildPod,
    fullscreen: bool,
    card: Color,
    border: Color,
    anchor: Rc<Cell<Rect>>,
    /// The painted transition: `0.0` windowed, `1.0` fullscreen. Read by
    /// **layout** (the padding/clearance it produces) and written by paint.
    progress: f64,
    /// The in-flight transition, `None` at rest. Created on the first **paint**
    /// after a flip, not on the rebuild that staged it: a `BuildCtx` carries no
    /// theme to resolve `reduce_motion`/the motion tokens from.
    stage: Option<FullscreenStage>,
    /// A flip the last rebuild staged, awaiting that first paint.
    pending: bool,
    /// The card's rect in this widget's own local coordinates, recorded by
    /// layout for paint.
    card_rect: Rect,
    /// …and the corner radius that box was built with — see `layout`.
    card_radius: f64,
}

impl<State: 'static> View<State> for TerminalStageView<State> {
    type Element = TerminalStageWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> TerminalStageWidget {
        TerminalStageWidget {
            child: build_child(&self.child, ctx),
            fullscreen: self.fullscreen,
            card: self.card,
            border: self.border,
            anchor: Rc::clone(&self.anchor),
            // A first mount never animates: the card is simply already where
            // the screen says it belongs.
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
        element.anchor = Rc::clone(&self.anchor);
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.fullscreen != self.fullscreen {
            element.fullscreen = self.fullscreen;
            element.pending = true;
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

        // The shell scaffold's app-bar slot (`crate::shell::shell_page`)
        // already reserves the bar's own band above this widget's box when
        // the bar is mounted — adding it again here would double-pad the
        // card. The one clearance this widget still owes is the status-bar
        // inset itself, phased in as the card grows toward fullscreen: at
        // `p = 0` the scaffold has already placed this box below the bar (no
        // extra offset needed), and at `p = 1` the bar is unmounted and the
        // box starts at the window's own top edge, so the card alone must
        // keep the pane title out from under a notch.
        let top = insets.top * p;
        let pad = WRAP_PAD * (1.0 - p);
        let bottom = pad + insets.bottom;

        let card_w = (width - 2.0 * pad).max(0.0);
        let card_h = (height - top - bottom).max(0.0);
        self.child
            .layout_child(ctx, &BoxConstraints::tight(Size::new(card_w, card_h)));
        self.child.set_origin(Point::new(pad, top));
        self.card_rect = Rect::new(pad, top, pad + card_w, top + card_h);
        // Recorded off the same `p` the box was built from: paint has already
        // advanced the driver, so a radius read there runs one frame ahead.
        self.card_radius = CARD_RADIUS * (1.0 - p);

        // The bar's trailing `⋮` slot, in window coordinates — this page is
        // laid out at the full surface, so local and window coordinates
        // coincide here.
        let slot_x = width - BAR_PAD_X - BAR_SLOT;
        let slot_y = insets.top + (BAR_HEIGHT - BAR_SLOT) / 2.0;
        self.anchor.set(Rect::new(
            slot_x,
            slot_y,
            slot_x + BAR_SLOT,
            slot_y + BAR_SLOT,
        ));

        bc.constrain(Size::new(width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let theme = Theme::from_paint_ctx(ctx);
        // Start a staged transition now a theme is in scope. `reduce_motion`
        // **snaps** both directions — the documented app-wide policy.
        if self.pending {
            self.pending = false;
            let target = if self.fullscreen { 1.0 } else { 0.0 };
            if theme.is_some_and(|theme| theme.motion.reduce_motion) {
                self.progress = target;
                self.stage = None;
            } else {
                self.stage = Some(FullscreenStage::start(self.progress, target, theme));
            }
        }
        if let Some(stage) = self.stage.as_mut() {
            let animating = stage.driver.advance(ctx.frame_time());
            self.progress = stage.value();
            // `progress` is a LAYOUT input (padding/clearance/radius are
            // computed in `layout`), so every step must request a layout pass:
            // a shell honoring the layout-skip contract freezes the geometry on
            // a bare `request_frame`. `request_layout` implies the frame.
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

        // `box-shadow:0 20px 50px rgba(0,0,0,0.4)`.
        scene.draw_shadow(
            Point::new(origin.x, origin.y + SHADOW_DY),
            size,
            radius,
            SHADOW_STD_DEV,
            Color::BLACK.with_alpha(SHADOW_ALPHA),
        );
        scene.fill_rounded_rect(origin, size, radius, self.card);
        stroke_border(scene, origin, size, radius, self.border);

        // The status row's band is a plain rect, so without this clip its
        // square top corners poke through the card's rounding.
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
/// …and its lead-in.
pub(super) const REVEAL_LEAD_MS: f64 = 60.0;
/// `.term-line{transition:opacity .18s, transform .18s}`.
pub(super) const REVEAL_DURATION_MS: f64 = 180.0;
/// `.term-line{transform:translateY(4px)}` — how far a line rises as it
/// arrives.
pub(super) const REVEAL_RISE: f64 = 4.0;
/// `.cur{animation:blink 1.1s steps(1) infinite}` — one full on/off cycle.
pub(super) const BLINK_PERIOD_MS: f64 = 1100.0;
/// `.cur{width:7px;height:13px}`.
pub(super) const CURSOR_WIDTH: f64 = 7.0;
pub(super) const CURSOR_HEIGHT: f64 = 13.0;
/// `.cur{vertical-align:-2px}` — how far the block sits below the run's own
/// baseline box.
const CURSOR_DROP: f64 = 2.0;

/// Unthemed-fallback `effects` easing — the Glyph curve, which never
/// overshoots, and what a line's fade/rise is shaped by.
const FALLBACK_EFFECTS: Curve = Curve::Cubic(0.16, 1.0, 0.3, 1.0);

/// The three inks a line can print in, resolved once per build by
/// [`super::widgets`] from `GlyphInk`.
#[derive(Clone, PartialEq)]
pub(super) struct LineStyles {
    /// `.term-line .p` — `GlyphInk::terminal_prompt`.
    pub(super) prompt: TextStyle,
    /// `.term-line .out` — `GlyphInk::terminal_out`.
    pub(super) output: TextStyle,
    /// `.term-line .cm` — `GlyphInk::terminal_comment`, italic.
    pub(super) comment: TextStyle,
}

impl LineStyles {
    /// The style `kind` prints in.
    fn of(&self, kind: TermLineKind) -> &TextStyle {
        match kind {
            TermLineKind::Prompt => &self.prompt,
            TermLineKind::Output => &self.output,
            TermLineKind::Comment => &self.comment,
        }
    }
}

/// `.term-body`'s lines: one shaped run each, revealed in a stagger and followed —
/// on the line that carries it — by the blinking block caret. The reveal restarts
/// whenever the lines change **or** `epoch` does, which is what makes a tab switch
/// replay it; `epoch` is the controller's replay token, published because
/// per-frame progress is deliberately not a signal.
pub(super) struct TermBodyView {
    lines: Vec<TermLine>,
    epoch: u64,
    styles: LineStyles,
    cursor: Color,
}

/// The card body over `lines`, replaying its reveal whenever `epoch` changes.
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

/// One retained line: its shaped run plus the size that run measured.
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

/// The retained half of a [`term_body`].
pub(super) struct TermBodyWidget {
    lines: Vec<LineRun>,
    epoch: u64,
    styles: LineStyles,
    cursor: Color,
    /// Drives the whole stagger — one linear `0→1` controller sized to the last
    /// line's finish, the shape `frust_glyph::term_block` uses.
    reveal: Option<AnimationController>,
    /// Set by a rebuild that changed the lines or the epoch; the next paint
    /// arms a fresh driver (or, under `reduce_motion`, none at all).
    replay: bool,
    /// When the caret's blink cycle started. Seeded on the first paint that
    /// draws one, so the caret is always *on* the instant a tab appears.
    blink_from: Option<FrameTime>,
}

/// The whole reveal's duration for `n` lines: the last line's delay plus its
/// own fade.
fn reveal_total_ms(n: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    (n as f64 - 1.0) * REVEAL_STEP_MS + REVEAL_LEAD_MS + REVEAL_DURATION_MS
}

/// Line `index`'s own `0..=1` progress at `elapsed_ms` into the reveal —
/// `0.0` before its window opens, `1.0` once its 180ms fade has run.
fn line_progress(index: usize, elapsed_ms: f64) -> f64 {
    let start = index as f64 * REVEAL_STEP_MS + REVEAL_LEAD_MS;
    ((elapsed_ms - start) / REVEAL_DURATION_MS).clamp(0.0, 1.0)
}

/// Whether the caret is lit `elapsed_ms` into its cycle — `steps(1)`, so it is
/// on for the first half and off for the second, with no fade.
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
            // The caret rides at the end of its own line, so that line has
            // that much less room for text before it wraps.
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

        // Arm (or, under `reduce_motion`, deliberately don't arm) the reveal now
        // a theme is in scope: reduced motion shows every line at once.
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
            // No driver: either the reveal has settled or `reduce_motion`
            // never armed one, and both mean "every line is fully shown".
            None => f64::INFINITY,
            Some(driver) => {
                let animating = driver.advance(ctx.frame_time());
                let elapsed = driver.value_clamped() * total_ms;
                if animating {
                    ctx.request_frame();
                } else {
                    // Settled: drop the driver so later frames cost nothing.
                    self.reveal = None;
                }
                elapsed
            }
        };

        // The caret's own clock, independent of the reveal. `reduce_motion`
        // freezes it lit — a caret is a *position* cue.
        let lit = if reduce_motion {
            true
        } else {
            let from = *self.blink_from.get_or_insert(ctx.frame_time());
            let since = ctx.frame_time().saturating_sub(from).as_secs_f64() * 1000.0;
            // Paceable: a 1.1s two-state blink is the "decorative loop whose
            // cadence is imperceptible" class the mobile frame gate may throttle
            // — the one paint-only exception to the LAYOUT|PAINT contract.
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
        // The body reads as one block of text: four separate unlabelled runs
        // would lose the fact that they are one transcript.
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
        // A vertical `Flex` hands its children `INFINITY` on the main axis, and
        // the wrap must coerce the *width* to a finite number to wrap against.
        let unbounded = BoxConstraints::loose(Size::new(600.0, f64::INFINITY));
        let child_bc = BoxConstraints::loose(Size::new(
            finite_or_zero(unbounded.max().width),
            unbounded.max().height,
        ));
        assert_eq!(child_bc.max().width, 600.0);
        assert!(child_bc.max().height.is_infinite());
        // …while an unbounded *width* still collapses, since there is nothing
        // to wrap against.
        assert_eq!(finite_or_zero(f64::INFINITY), 0.0);
    }

    #[test]
    fn the_reveal_window_is_the_mocks_stagger() {
        // `setTimeout(.., i*140 + 60)` then a 180ms transition: line 0 opens at
        // 60ms and is done at 240ms; line 3 opens at 480ms.
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
        // Four lines: 3*140 + 60 + 180.
        assert_eq!(reveal_total_ms(4), 660.0);
        // …and every line is fully in by then.
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
        // A fresh driver has not advanced, so it is still where it started.
        assert_eq!(entering.value(), 0.0);
        let leaving = FullscreenStage::start(1.0, 0.0, Some(&theme));
        assert_eq!(leaving.value(), 1.0);
    }

    /// `60ms` into a fresh entrance: mid-flight, inside Glyph's `spatial`
    /// overshoot band, and never past `1.0` before the final clamp.
    fn painted_mid_entrance(theme: &Theme) -> f64 {
        let mut entering = FullscreenStage::start(0.0, 1.0, Some(theme));
        // The first `advance` after `start` only seeds the clock (a fresh
        // driver's documented zero-delta rule); the second elapses 60ms.
        entering.driver.advance(FrameTime::from_nanos(0));
        entering.driver.advance(FrameTime::from_nanos(60_000_000));
        entering.value()
    }

    #[test]
    fn collapsing_leaves_1_0_well_before_half_the_duration_rather_than_stalling() {
        // Guards the curve-the-position bug: feeding a collapse's `from = 1.0`
        // through the curve's overshoot band clamps it flush at `1.0` for the
        // first half of the run, so the card sits motionless.
        let theme = frust_glyph::baseline();
        let mut collapsing = FullscreenStage::start(1.0, 0.0, Some(&theme));
        collapsing.driver.advance(FrameTime::from_nanos(0));
        // 80ms of the 340ms `durations.slow` — comfortably inside the band
        // the old formula stalled through.
        collapsing.driver.advance(FrameTime::from_nanos(80_000_000));
        let value = collapsing.value();
        assert!(
            value < 0.9,
            "80ms in, the collapse should already have left 1.0: {value}"
        );
    }

    #[test]
    fn an_interrupted_stage_starts_exactly_at_the_painted_value_it_seeds_from() {
        // Guards the double-easing bug: an in-flight entrance's painted value
        // often lands inside the `spatial` overshoot band, so re-easing it snaps
        // the card to fullscreen the instant a collapse interrupts.
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
        // Cumulative milliseconds since the interrupt — the last two are
        // past the 340ms duration, so the driver settles exactly on target.
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
        // `spatial` overshoots past 1.0 — a *negative* wrap padding.
        let theme = frust_glyph::baseline();
        let stage = FullscreenStage::start(0.0, 1.0, Some(&theme));
        for raw in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let eased = stage.curve.transform(raw).clamp(0.0, 1.0);
            assert!((0.0..=1.0).contains(&eased), "{eased}");
            assert!(WRAP_PAD * (1.0 - eased) >= 0.0);
        }
    }

    #[test]
    fn the_unthemed_fallbacks_are_the_glyph_tokens_they_stand_in_for() {
        // A headless frame with no theme must stage on the device's numbers.
        let motion = frust_glyph::baseline().motion;
        assert_eq!(FALLBACK_SLOW.as_millis() as f64, motion.durations.slow);
        assert_eq!(FALLBACK_SPATIAL, motion.easing.spatial);
        assert_eq!(FALLBACK_EFFECTS, motion.easing.effects);
    }

    // The three app-supplied marks this module used to author
    // (`FULLSCREEN_EXPAND`/`FULLSCREEN_EXIT`/`PANE_MARK`, plus their
    // parse/design-box/distinct-shapes coverage) moved to
    // `frust::icons::{FULLSCREEN_EXPAND, FULLSCREEN_EXIT, PANE_MARK}` — the
    // catalog's own generated set carries the equivalent conformance tests
    // (`frust-widgets`' `icon.rs`), so there is nothing app-local left to
    // assert here.
}
