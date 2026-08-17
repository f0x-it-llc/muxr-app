//! `muxr_topbar` — the app's branded top bar (`docs/redesign.html`'s
//! `.topbar`), mounted **once** by [`crate::shell`] as persistent chrome
//! floating over the navigator
use std::rc::Rc;

use frust::authoring::text::LineHeight;
use frust::authoring::{
    any, build_child, erase_callback, rebuild_child, route_event, teardown_child, Action, AnyView,
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, ErasedCallback, EventCtx, EventResult,
    InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, PointerPhase, Role, SemanticsCtx, Size,
    View, Widget, PRESSED_OPACITY,
};
use frust::motion::patterns::{PatternLayer, TransitionPattern};
use frust::motion::switcher::{pattern_switcher, REDUCE_MOTION_DURATION};
use frust::{
    icon, inflexible, safe_area, text, AnimationController, Axis, ChildKey, CrossAxisAlignment,
    Curve, EdgeInsets, FlexView, IconData, IconSource, Padding, SizedBox, Theme, Timing,
};
use frust_glyph::motion::{GLYPH_SLIDE_ENTER, GLYPH_SLIDE_EXIT};

pub const BAR_HEIGHT: f64 = 46.0;
const PAD_X: f64 = 16.0;
const MARK_SIZE: f64 = 16.0;
const MARK_GAP: f64 = 2.0;
const MARK_CELL: f64 = (MARK_SIZE - MARK_GAP) / 2.0;
const MARK_RADIUS: f64 = 1.0;
const BRAND_GAP: f64 = 7.0;
const BRAND_SIZE: f32 = 13.0;
const BRAND_LINE_HEIGHT: f32 = 18.0;
const ICON_SLOT: f64 = 30.0;
const ICON_SIZE: f64 = 20.0;
const TITLE_GUTTER: f64 = 8.0;
const HAIRLINE: f64 = 1.0;

const BRAND: &str = "muxr";

pub const BACK_LABEL: &str = "Back";

/// The back affordance's mark: a chevron pointing at the leading edge, mirrored
/// from `frust::icons::CHEVRON_RIGHT` about the centre of its own 24×24 design
/// box (`x' = 24 - x`; the relative segments negate their `dx`).
///
/// **Why an app-supplied source rather than `icons::ARROW_BACK`.** The design
/// (`docs/redesign.html`'s settings topbar) draws a *chevron*, and the vendored
/// Material Symbols starter set carries `chevron_right` but no `chevron_left`,
/// so the collection cannot express the mark the mock asks for. Mirroring the
/// one it does carry keeps the stroke weight, the corner cuts and the optical
/// size identical to the framework's own chevron — a hand-drawn substitute
/// would match none of the three. `IconData::from_path` is the documented
/// app-supply seam; an `IconSource` is the cheaper half of it (a `d` string
/// needs no `kurbo` in app code), and the same seam the Connect FAB uses.
pub const CHEVRON_LEFT: IconSource = IconSource {
    d: "M 11.4 12 16 7.4 l -1.4 -1.4 -6 6 6 6 1.4 -1.4 -4.6 -4.6 Z",
    design: ICON_DESIGN_BOX,
};

const ICON_DESIGN_BOX: f64 = 24.0;

const TITLE_SHIFT: f64 = 14.0;
const LEADING_SHIFT: f64 = 6.0;
const CROSSFADE_MS: u64 = 220;
const CROSSFADE_CURVE: Curve = Curve::Cubic(0.16, 1.0, 0.3, 1.0);

fn crossfade_timing() -> Timing {
    Timing::Duration(
        std::time::Duration::from_millis(CROSSFADE_MS),
        CROSSFADE_CURVE,
    )
}

// ---------------------------------------------------------------------------
// The bar's own entrance/exit
// ---------------------------------------------------------------------------

const REVEAL_SPATIAL_CURVE: Curve = Curve::Cubic(0.05, 0.7, 0.1, 1.0);
const REVEAL_EFFECTS_CURVE: Curve = Curve::Cubic(0.2, 0.0, 0.0, 1.0);
const REVEAL_EXIT_CURVE: Curve = Curve::Cubic(0.3, 0.0, 1.0, 1.0);

fn reveal_layer(p: f64, showing: bool, extent: f64, reduce_motion: bool) -> PatternLayer {
    let p = p.clamp(0.0, 1.0);
    if reduce_motion {
        return PatternLayer {
            dx: 0.0,
            dy: 0.0,
            alpha: p as f32,
            scale: 1.0,
        };
    }
    let (travelled, alpha) = if showing {
        (
            REVEAL_SPATIAL_CURVE.transform(p),
            REVEAL_EFFECTS_CURVE.transform(p),
        )
    } else {
        // The exit runs on one curve for both channels, measured from the
        // still-shown state: at `p == 1.0` nothing has moved yet.
        let gone = REVEAL_EXIT_CURVE.transform(1.0 - p);
        (1.0 - gone, 1.0 - gone)
    };
    PatternLayer {
        dx: 0.0,
        dy: -(1.0 - travelled) * extent,
        alpha: alpha as f32,
        scale: 1.0,
    }
}

/// One in-flight reveal: a `0→1` driver plus the visual range it maps onto.

struct Reveal {
    driver: AnimationController,
    from: f64,
    to: f64,
    reduced: bool,
}

impl Reveal {
    fn start(from: f64, to: f64, reduce_motion: bool) -> Self {
        let showing = to > from;
        let duration = if reduce_motion {
            REDUCE_MOTION_DURATION
        } else if showing {
            GLYPH_SLIDE_ENTER
        } else {
            GLYPH_SLIDE_EXIT
        };
        let mut driver = AnimationController::new(duration);
        driver.forward();
        Reveal {
            driver,
            from,
            to,
            reduced: reduce_motion,
        }
    }

    fn showing(&self) -> bool {
        self.to > self.from
    }
}

#[derive(Clone, Copy)]
struct TitleShift;

impl TransitionPattern for TitleShift {
    fn resolve(&self, p: f64, reverse: bool, _size: Size) -> (PatternLayer, PatternLayer) {
        let pc = p.clamp(0.0, 1.0);
        let sign = if reverse { -1.0 } else { 1.0 };
        let incoming = PatternLayer {
            dx: sign * (1.0 - pc) * TITLE_SHIFT,
            dy: 0.0,
            alpha: pc as f32,
            scale: 1.0,
        };
        let exiting = PatternLayer {
            dx: -sign * pc * TITLE_SHIFT,
            dy: 0.0,
            alpha: (1.0 - pc) as f32,
            scale: 1.0,
        };
        (incoming, exiting)
    }
}

#[derive(Clone, Copy)]
struct LeadingSwap;

impl TransitionPattern for LeadingSwap {
    fn resolve(&self, p: f64, reverse: bool, _size: Size) -> (PatternLayer, PatternLayer) {
        let pc = p.clamp(0.0, 1.0);
        let sign = if reverse { -1.0 } else { 1.0 };
        let incoming = PatternLayer {
            dx: 0.0,
            dy: sign * (1.0 - pc) * LEADING_SHIFT,
            alpha: pc as f32,
            scale: 1.0,
        };
        let exiting = PatternLayer {
            dx: 0.0,
            dy: -sign * pc * LEADING_SHIFT,
            alpha: (1.0 - pc) as f32,
            scale: 1.0,
        };
        (incoming, exiting)
    }
}

pub fn muxr_topbar<State: 'static>(title: impl Into<String>) -> TopBarView<State> {
    let theme = active_theme();
    let scheme = theme.scheme();
    let mut brand_style = theme.type_scale.display_small.clone();
    brand_style.size = BRAND_SIZE;
    brand_style.line_height = LineHeight::Absolute(BRAND_LINE_HEIGHT);
    brand_style.color = scheme.primary;
    let mut title_style = theme.type_scale.label_large.clone();
    title_style.color = scheme.on_surface;
    let title = title.into();
    TopBarView {
        leading: Rc::new(any(FlexView::new(
            Axis::Horizontal,
            vec![
                inflexible(BrandMark {
                    color: scheme.primary_container,
                }),
                inflexible(SizedBox::<State>(Some(BRAND_GAP), None)),
                inflexible(text(BRAND).style(brand_style)),
            ],
        )
        .cross_axis(CrossAxisAlignment::Center))),
        leading_key: ChildKey::new(BRAND),
        back: false,
        title: Rc::new(any(text(title.clone()).style(title_style))),
        title_key: ChildKey::new(&title),
        action: None,
        direction_reverse: false,
        hidden: false,
        surface: scheme.surface,
        rule: scheme.outline_variant,
        on_surface: scheme.on_surface,
        slot_radius: theme.shape.small,
    }
}

fn active_theme() -> Theme {
    frust::use_context::<Theme>().unwrap_or_else(frust_glyph::baseline)
}

/// The y a slot `height` tall takes to sit centered in the content band —
/// **below** the status-bar inset, which the bar consumes on the content's
/// behalf (see the [module docs](self)) rather than letting it squash the
/// band.
fn content_y(top_inset: f64, height: f64) -> f64 {
    top_inset + (BAR_HEIGHT - height) / 2.0
}

/// `body` with the top clearance a bar-bearing page owes reserved above it:
/// the status-bar inset **plus** [`BAR_HEIGHT`], which is exactly the box the
/// floating bar occupies.

pub fn topbar_clearance<State: 'static, V: View<State>>(body: V) -> AnyView<State> {
    any(safe_area(Padding(
        EdgeInsets {
            left: 0.0,
            top: BAR_HEIGHT,
            right: 0.0,
            bottom: 0.0,
        },
        body,
    ))
    .left(false)
    .right(false)
    .bottom(false))
}

pub struct TopBarView<State: 'static> {
    leading: Rc<AnyView<State>>,
    leading_key: ChildKey,
    back: bool,
    title: Rc<AnyView<State>>,
    title_key: ChildKey,
    action: Option<AnyView<State>>,
    direction_reverse: bool,
    hidden: bool,
    surface: Color,
    rule: Color,
    on_surface: Color,
    slot_radius: f64,
}

impl<State: 'static> TopBarView<State> {
    pub fn back<F: Fn(&mut State) + 'static>(mut self, on_press: F) -> Self {
        self.leading = Rc::new(any(self.slot_icon(CHEVRON_LEFT, BACK_LABEL, on_press)));
        self.leading_key = ChildKey::new(BACK_LABEL);
        self.back = true;
        self
    }

    /// Put a vector `mark` (`icons::SETTINGS`, `icons::MORE_VERT`) in the
    /// trailing icon slot under the accessible name `label`, firing
    /// `on_press` when it is tapped.

    pub fn action<F: Fn(&mut State) + 'static>(
        mut self,
        mark: impl Into<IconData>,
        label: impl Into<String>,
        on_press: F,
    ) -> Self {
        self.action = Some(any(self.slot_icon(mark, label, on_press)));
        self
    }

    fn slot_icon<F: Fn(&mut State) + 'static>(
        &self,
        mark: impl Into<IconData>,
        label: impl Into<String>,
        on_press: F,
    ) -> IconButtonView<State> {
        icon_button(mark, label, on_press)
            .ink(self.on_surface)
            .icon_size(ICON_SIZE)
            .radius(self.slot_radius)
    }

    pub fn reverse(mut self, reverse: bool) -> Self {
        self.direction_reverse = reverse;
        self
    }

    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    /// The leading slot, wrapped in its crossfade switcher.
    fn staged_leading(&self) -> AnyView<State> {
        any(pattern_switcher(
            self.leading_key,
            LeadingSwap,
            Slot(Rc::clone(&self.leading)),
        )
        .timing(crossfade_timing())
        .reverse(self.direction_reverse))
    }

    /// The title slot, wrapped in its crossfade switcher.
    fn staged_title(&self) -> AnyView<State> {
        any(
            pattern_switcher(self.title_key, TitleShift, Slot(Rc::clone(&self.title)))
                .timing(crossfade_timing())
                .reverse(self.direction_reverse),
        )
    }
}

/// A `'static`, cheaply clonable handle to one slot's [`AnyView`].

struct Slot<State: 'static>(Rc<AnyView<State>>);

impl<State: 'static> View<State> for Slot<State> {
    type Element = Box<dyn Widget>;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> Box<dyn Widget> {
        self.0.build(ctx)
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut Box<dyn Widget>,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        self.0.rebuild(&prev.0, element, ctx)
    }

    fn teardown(&self, element: &mut Box<dyn Widget>, ctx: &mut BuildCtx<'_>) {
        self.0.teardown(element, ctx);
    }
}

// ---------------------------------------------------------------------------
// The brand mark
// ---------------------------------------------------------------------------

/// `.brand-mark`: four amber cells on a 2×2 grid in a 16×16 box.
struct BrandMark {
    color: Color,
}

/// The retained half of [`BrandMark`].
struct BrandMarkWidget {
    color: Color,
}

impl<State: 'static> View<State> for BrandMark {
    type Element = BrandMarkWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> BrandMarkWidget {
        BrandMarkWidget { color: self.color }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut BrandMarkWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        if prev.color != self.color {
            element.color = self.color;
            return ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

impl Widget for BrandMarkWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(MARK_SIZE, MARK_SIZE))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        for (col, row) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            scene.fill_rounded_rect(
                Point::new(
                    origin.x + col * (MARK_CELL + MARK_GAP),
                    origin.y + row * (MARK_CELL + MARK_GAP),
                ),
                Size::new(MARK_CELL, MARK_CELL),
                MARK_RADIUS,
                self.color,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Icon button — the app's one vector affordance
// ---------------------------------------------------------------------------

/// A pressable **vector** icon: `mark` centred in a box, labelled `label` for
/// accessibility, firing `on_press` on a release inside its bounds.

pub fn icon_button<State: 'static, F: Fn(&mut State) + 'static>(
    mark: impl Into<IconData>,
    label: impl Into<String>,
    on_press: F,
) -> IconButtonView<State> {
    IconButtonView {
        mark: mark.into(),
        label: label.into(),
        ink: None,
        fill: Color::TRANSPARENT,
        icon_size: ICON_DESIGN_BOX,
        box_size: None,
        radius: 0.0,
        on_press: Rc::new(on_press),
    }
}

/// The declarative half of [`icon_button`].
pub struct IconButtonView<State: 'static> {
    mark: IconData,
    label: String,
    /// The mark's ink; `None` lets [`frust::icon`] resolve `on_surface` from
    /// the threaded theme (its own documented default).
    ink: Option<Color>,
    fill: Color,
    icon_size: f64,
    box_size: Option<Size>,
    radius: f64,
    on_press: Rc<dyn Fn(&mut State)>,
}

impl<State: 'static> IconButtonView<State> {
    /// Ink the mark explicitly, overriding the theme's `on_surface` default.
    pub fn ink(mut self, ink: Color) -> Self {
        self.ink = Some(ink);
        self
    }

    /// Fill the button's box behind the mark (a FAB's accent disc).
    pub fn fill(mut self, fill: Color) -> Self {
        self.fill = fill;
        self
    }

    /// Draw the mark at `size` logical px (default: its own 24px design box).
    pub fn icon_size(mut self, size: f64) -> Self {
        self.icon_size = size;
        self
    }

    /// Give the button a fixed tap target rather than shrink-wrapping the mark
    /// — the platform 44/48px minimum, or a design's own box.
    pub fn box_size(mut self, size: Size) -> Self {
        self.box_size = Some(size);
        self
    }

    /// Round the fill and the pressed wash by `radius`.
    pub fn radius(mut self, radius: f64) -> Self {
        self.radius = radius;
        self
    }

    /// The mark, as the pod this button paints and lays out.
    fn mark_view(&self) -> AnyView<State> {
        let mut view = icon(self.mark.clone()).size(self.icon_size);
        if let Some(ink) = self.ink {
            view = view.color(ink);
        }
        any(view)
    }
}

/// The retained half of [`icon_button`].
pub struct IconButtonWidget {
    mark: ChildPod,
    label: String,
    fill: Color,
    ink: Option<Color>,
    icon_size: f64,
    box_size: Option<Size>,
    radius: f64,
    /// The pressed *visual*; follows the pointer in and out while captured.
    pressed: bool,
    /// Armed by a `Down` (alongside `capture_pointer`), cleared on
    /// `Up`/`Cancel` — gates `Move`/`Up` so a hover never latches or fires.
    captured: bool,
    on_press: ErasedCallback,
}

impl<State: 'static> View<State> for IconButtonView<State> {
    type Element = IconButtonWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> IconButtonWidget {
        IconButtonWidget {
            mark: build_child(&self.mark_view(), ctx),
            label: self.label.clone(),
            fill: self.fill,
            ink: self.ink,
            icon_size: self.icon_size,
            box_size: self.box_size,
            radius: self.radius,
            pressed: false,
            captured: false,
            on_press: erase_callback(&self.on_press),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut IconButtonWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // Closures aren't comparable; reinstall the adapter every time.
        element.on_press = erase_callback(&self.on_press);
        // The icon pod diffs itself (`IconView::rebuild` skips re-parsing an
        // unchanged `d`), so this just re-supplies the current mark.
        let mut flags = rebuild_child(&prev.mark_view(), &self.mark_view(), &mut element.mark, ctx);
        if prev.box_size != self.box_size || prev.icon_size != self.icon_size {
            element.box_size = self.box_size;
            element.icon_size = self.icon_size;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.fill != self.fill || prev.ink != self.ink || prev.radius != self.radius {
            element.fill = self.fill;
            element.ink = self.ink;
            element.radius = self.radius;
            flags |= ChangeFlags::PAINT;
        }
        if prev.label != self.label {
            element.label = self.label.clone();
            // Semantics are recomputed off the paint pass, so a label change
            // republishes through PAINT.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut IconButtonWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.mark_view(), &mut element.mark, ctx);
    }
}

/// Whether `pos` (widget-local) falls inside a `size`-sized box.
fn inside(pos: Point, size: Size) -> bool {
    pos.x >= 0.0 && pos.y >= 0.0 && pos.x < size.width && pos.y < size.height
}

impl Widget for IconButtonWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let mark_bc = BoxConstraints::tight(Size::new(self.icon_size, self.icon_size));
        let mark = self.mark.layout_child(ctx, &mark_bc);
        // A declared box is the tap target; without one the mark *is* the
        // button. Either way a tight incoming constraint (a `.topbar-icon`
        // slot) wins outright, and the mark centres in whatever results.
        let size = bc.constrain(self.box_size.unwrap_or(mark));
        self.mark.set_origin(Point::new(
            (size.width - mark.width) / 2.0,
            (size.height - mark.height) / 2.0,
        ));
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        if self.fill != Color::TRANSPARENT {
            scene.fill_rounded_rect(origin, size, self.radius, self.fill);
        }
        self.mark.paint_child(ctx, scene);
        if self.pressed {
            // Washed in the mark's own ink, so a ghost slot on a dark bar and
            // an accent disc both darken rather than one of them glowing.
            let wash = self.ink.unwrap_or(Color::BLACK);
            scene.fill_rounded_rect(origin, size, self.radius, wash.with_alpha(PRESSED_OPACITY));
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
                if !self.captured {
                    return EventResult::Ignored;
                }
                self.pressed = false;
                self.captured = false;
                ctx.request_redraw();
                EventResult::Handled
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // One labelled Button leaf, like `frust`'s own `Button` over its label
        // pod: an icon is decorative on its own (`IconView` publishes nothing
        // unlabelled), and the *name* of this control is what a screen reader
        // needs — never the shape of its mark.
        ctx.push_node(Role::Button, |node| {
            node.set_label(self.label.as_str());
            node.add_action(Action::Click);
        });
    }
}

// ---------------------------------------------------------------------------
// The bar itself
// ---------------------------------------------------------------------------

/// The retained half of the top bar. See [`muxr_topbar`].
pub struct TopBarWidget {
    children: Vec<ChildPod>,
    rest_origins: Vec<Point>,
    back: bool,
    hidden: bool,
    surface: Color,
    rule: Color,
    reveal: f64,
    reveal_anim: Option<Reveal>,
    reveal_pending: bool,
    extent: f64,
}

/// The index of each fixed pod in [`TopBarWidget::children`].
const LEADING_POD: usize = 0;
const TITLE_POD: usize = 1;
const ACTION_POD: usize = 2;

impl<State: 'static> View<State> for TopBarView<State> {
    type Element = TopBarWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> TopBarWidget {
        let mut children = vec![
            build_child(&self.staged_leading(), ctx),
            build_child(&self.staged_title(), ctx),
        ];
        if let Some(action) = &self.action {
            children.push(build_child(action, ctx));
        }
        TopBarWidget {
            children,
            rest_origins: Vec::new(),
            back: self.back,
            hidden: self.hidden,
            surface: self.surface,
            rule: self.rule,
            // A first mount never animates: the bar is simply already where the
            // route says it belongs — this is also the frame a cold start's
            // splash covers.
            reveal: if self.hidden { 0.0 } else { 1.0 },
            reveal_anim: None,
            reveal_pending: false,
            extent: 0.0,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut TopBarWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(
            &prev.staged_leading(),
            &self.staged_leading(),
            &mut element.children[LEADING_POD],
            ctx,
        );
        flags |= rebuild_child(
            &prev.staged_title(),
            &self.staged_title(),
            &mut element.children[TITLE_POD],
            ctx,
        );
        // The action slot's presence is route-driven, so unlike the two staged
        // slots it really can appear and disappear; a shape change rebuilds the
        // pod list outright rather than pretending to diff it.
        match (&prev.action, &self.action) {
            (Some(prev_action), Some(action)) => {
                flags |= rebuild_child(prev_action, action, &mut element.children[ACTION_POD], ctx);
            }
            (None, Some(action)) => {
                element.children.push(build_child(action, ctx));
                flags |= ChangeFlags::LAYOUT;
            }
            (Some(prev_action), None) => {
                let mut pod = element.children.remove(ACTION_POD);
                teardown_child(prev_action, &mut pod, ctx);
                flags |= ChangeFlags::LAYOUT;
            }
            (None, None) => {}
        }
        if prev.back != self.back {
            element.back = self.back;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.hidden != self.hidden {
            // A visibility flip is a *reveal*, never a jump: the next paint
            // resolves the theme's motion policy and starts the animation from
            // wherever the bar is painting right now (see `Reveal`).
            element.hidden = self.hidden;
            element.reveal_pending = true;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.surface != self.surface || prev.rule != self.rule {
            element.surface = self.surface;
            element.rule = self.rule;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut TopBarWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(
            &self.staged_leading(),
            &mut element.children[LEADING_POD],
            ctx,
        );
        teardown_child(&self.staged_title(), &mut element.children[TITLE_POD], ctx);
        if let Some(action) = &self.action {
            teardown_child(action, &mut element.children[ACTION_POD], ctx);
        }
    }
}

impl TopBarWidget {
    /// Whether a reveal is staged or running — the bar is on screen and moving.
    fn revealing(&self) -> bool {
        self.reveal_pending || self.reveal_anim.is_some()
    }

    /// Whether the bar occupies a box at all: shown, or mid-reveal in either
    /// direction. A **fully** hidden bar is `false` and lays out to nothing.
    fn occupies_a_box(&self) -> bool {
        !self.hidden || self.revealing()
    }

    /// Whether the bar is settled on screen — the one state in which it takes
    /// input and publishes semantics (see the [module docs](self)).
    fn settled_open(&self) -> bool {
        !self.hidden && !self.revealing()
    }
}

impl Widget for TopBarWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        if !self.occupies_a_box() {
            // Zero-height, children unlaid — `paint`/`event`/`semantics` all
            // short-circuit on the same predicate, so nothing reads their
            // geometry. (A reveal that *ends* hidden leaves the last full box
            // behind until the next relayout: harmless, since a bar that is
            // neither painting nor routing has nothing to give away, and any
            // later rebuild corrects it.)
            self.rest_origins.clear();
            return bc.constrain(Size::ZERO);
        }
        let width = if bc.max().width.is_finite() {
            bc.max().width
        } else {
            0.0
        };
        let content_top = ctx.window_insets().padding().top;
        // Every slot lays out inside the 46px content band, never against the
        // inset-extended height.
        let slot_bc = BoxConstraints::loose(Size::new(width, BAR_HEIGHT));
        let icon_bc = BoxConstraints::tight(Size::new(ICON_SLOT, ICON_SLOT));
        let centered = |height: f64| content_y(content_top, height);

        // Leading: the brand row at its natural width, or the `‹`'s 30×30 box
        // flush against the leading edge (`docs/redesign.html`'s settings bar).
        let leading_bc = if self.back { icon_bc } else { slot_bc };
        let leading_size = self.children[LEADING_POD].layout_child(ctx, &leading_bc);
        self.children[LEADING_POD].set_origin(Point::new(PAD_X, centered(leading_size.height)));
        let leading_end = PAD_X + leading_size.width;

        // Trailing: the icon slot, flush against the trailing edge. Its box is
        // the source's 30×30 (tight, so the `Align` inside centers the button
        // in it) rather than whatever the button happens to measure. A back-led
        // bar with no action still reserves the box — the mock's
        // `<div style="width:30px">` spacer — so the title's clamp stays
        // symmetric and a later trailing icon slots in without the title
        // jumping.
        let trailing_start = match self.children.get_mut(ACTION_POD) {
            Some(action) => {
                let size = action.layout_child(ctx, &icon_bc);
                let x = (width - PAD_X - size.width).max(leading_end);
                action.set_origin(Point::new(x, centered(size.height)));
                x
            }
            None if self.back => (width - PAD_X - ICON_SLOT).max(leading_end),
            None => width - PAD_X,
        };

        // The title is centered on the **bar**, not on the gap between the
        // slots (the source's `left:50%;translateX(-50%)`), so its width is
        // clamped by whichever side crowds it first.
        let center = width / 2.0;
        let half = (center - leading_end).min(trailing_start - center).max(0.0);
        let title_max = (2.0 * half - TITLE_GUTTER).max(0.0);
        let title_size = self.children[TITLE_POD].layout_child(
            ctx,
            &BoxConstraints::loose(Size::new(title_max, BAR_HEIGHT)),
        );
        self.children[TITLE_POD].set_origin(Point::new(
            (width - title_size.width) / 2.0,
            centered(title_size.height),
        ));

        // The rest pose `paint` slides away from, and the distance it slides.
        self.rest_origins = self.children.iter().map(ChildPod::origin).collect();
        self.extent = content_top + BAR_HEIGHT;
        bc.constrain(Size::new(width, self.extent))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        if !self.occupies_a_box() {
            return;
        }
        let theme = Theme::from_paint_ctx(ctx);
        // Start a staged reveal now a theme is in scope: `reduce_motion` and
        // the entrance/exit duration are paint-time reads (module docs).
        if self.reveal_pending {
            let reduce = theme
                .map(|theme| theme.motion.reduce_motion)
                .unwrap_or(false);
            let to = if self.hidden { 0.0 } else { 1.0 };
            self.reveal_anim = Some(Reveal::start(self.reveal, to, reduce));
            self.reveal_pending = false;
        }
        // Advance it, and let the frame clock — not the rebuild — decide when
        // the bar has arrived.
        let mut layer = PatternLayer {
            dx: 0.0,
            dy: 0.0,
            alpha: 1.0,
            scale: 1.0,
        };
        if let Some(reveal) = self.reveal_anim.as_mut() {
            let animating = reveal.driver.advance(ctx.frame_time());
            self.reveal = reveal.from + (reveal.to - reveal.from) * reveal.driver.value_clamped();
            layer = reveal_layer(self.reveal, reveal.showing(), self.extent, reveal.reduced);
            if animating {
                ctx.request_frame();
            } else {
                self.reveal = reveal.to;
                self.reveal_anim = None;
                // One more frame so the settled pose (and the semantics/input
                // that come back with it) is the one on screen.
                ctx.request_frame();
            }
        }

        let origin = Point::new(ctx.origin().x, ctx.origin().y + layer.dy);
        let size = ctx.size();
        let faded = layer.alpha < 1.0;
        if faded {
            scene.push_layer(ctx.origin(), size, layer.alpha);
        }
        // The surface runs the full extended height, so it sits under the
        // status bar (see the module docs' inset note).
        scene.fill_rect(origin, size, self.surface);
        // `border-bottom`: a hairline centered on the bar's last row.
        let y = origin.y + size.height - HAIRLINE / 2.0;
        scene.stroke_line(
            Point::new(origin.x, y),
            Point::new(origin.x + size.width, y),
            HAIRLINE,
            self.rule,
        );

        // Slide the slots with the surface. Pod origins (rather than a
        // `push_transform`) for the same reason `frust::motion`'s switcher
        // stages its children that way: paint and hit-testing move together.
        let aligned = self.rest_origins.len() == self.children.len();
        for (index, child) in self.children.iter_mut().enumerate() {
            if aligned {
                let rest = self.rest_origins[index];
                child.set_origin(Point::new(rest.x, rest.y + layer.dy));
            }
            child.paint_child(ctx, scene);
        }

        if faded {
            scene.pop_layer();
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        if !self.settled_open() {
            // Hidden or mid-reveal: a bar that is only half-arrived must not
            // eat a tap meant for the page it floats over. Ignoring rather
            // than swallowing is what lets that page treat the press as its
            // own. (A press *under an open overlay* never reaches here at all
            // — the root host routes to its top page only; module docs.)
            return EventResult::Ignored;
        }
        // Only the leading/trailing slots are interactive, but routing to every
        // child is what keeps that true as slots grow — `route_event` owns the
        // capture/focus fast paths.
        route_event(&mut self.children, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        if !self.settled_open() {
            // `/` has no bar at all, in the accessibility tree as on screen —
            // and a mid-reveal bar routes nothing, so it publishes nothing: an
            // accessibility node whose affordances route nowhere is worse than
            // no node. (Under an open overlay the host omits this whole subtree
            // for the same reason, one level up; module docs.)
            return;
        }
        ctx.push_container(
            Role::TitleBar,
            |node| node.set_label(BRAND),
            |ctx| {
                for child in &self.children {
                    child.semantics_child(ctx);
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_brand_mark_cells_tile_its_box_exactly() {
        // A tripwire on the derived cell size: two cells plus the gap must be
        // the 16px mark, or the grid drifts off `.brand-mark`'s box.
        assert!((2.0 * MARK_CELL + MARK_GAP - MARK_SIZE).abs() < 1e-9);
    }

    #[test]
    fn a_slot_centers_in_the_band_below_the_status_bar_inset() {
        // The inset-consumption contract, as arithmetic: content sits *below*
        // the inset, centered in the 46px band — never centered in the
        // inset-extended height (which would push the bar's content down by
        // half the status bar).
        assert_eq!(content_y(0.0, ICON_SLOT), (BAR_HEIGHT - ICON_SLOT) / 2.0);
        assert_eq!(
            content_y(47.0, MARK_SIZE),
            47.0 + (BAR_HEIGHT - MARK_SIZE) / 2.0
        );
    }

    #[test]
    fn the_title_crossfade_matches_the_appbars_documented_staging() {
        // `glyph/appbar.rs`: the new run enters from +14px at 0 opacity and
        // rests at 0 fully opaque, while the old run leaves the opposite way.
        let (incoming, exiting) = TitleShift.resolve(0.0, false, Size::ZERO);
        assert_eq!(incoming.dx, TITLE_SHIFT);
        assert_eq!(incoming.alpha, 0.0);
        assert_eq!(exiting.dx, 0.0);
        assert_eq!(exiting.alpha, 1.0);

        let (incoming, exiting) = TitleShift.resolve(1.0, false, Size::ZERO);
        assert_eq!(incoming.dx, 0.0);
        assert_eq!(incoming.alpha, 1.0);
        assert_eq!(exiting.dx, -TITLE_SHIFT);
        assert_eq!(exiting.alpha, 0.0);

        // …and neither run ever moves vertically or scales.
        for p in [0.0, 0.5, 1.0] {
            let (incoming, exiting) = TitleShift.resolve(p, false, Size::ZERO);
            assert_eq!((incoming.dy, incoming.scale), (0.0, 1.0));
            assert_eq!((exiting.dy, exiting.scale), (0.0, 1.0));
        }
    }

    #[test]
    fn a_back_navigation_mirrors_the_title_shift() {
        let (forward, _) = TitleShift.resolve(0.0, false, Size::ZERO);
        let (back, _) = TitleShift.resolve(0.0, true, Size::ZERO);
        assert_eq!(forward.dx, TITLE_SHIFT);
        assert_eq!(back.dx, -TITLE_SHIFT, "a pop enters from the leading edge");
    }

    #[test]
    fn the_clearance_reserves_exactly_the_bars_own_box() {
        // The seam three screens share with the bar: what a bar-bearing page
        // pads at its top must be exactly what the floating bar occupies. At a
        // zero window inset that is the 46px band and nothing else — the inset
        // half of the sum is the *same* `WindowInsets::padding().top` both
        // sides resolve (`TopBarWidget::layout` adds it, the wrapper's own
        // top-only `safe_area` contributes it), which `LayoutCtx::new` cannot
        // fake here (the setter is `pub(crate)`).
        const CHILD: f64 = 100.0;
        let view: AnyView<()> = topbar_clearance(SizedBox::<()>(Some(CHILD), Some(CHILD)));
        let mut next_id = 0u64;
        let mut widget = view.build(&mut BuildCtx::new(&mut next_id));
        let mut ctx = LayoutCtx::new();
        let size = widget.layout(&mut ctx, &BoxConstraints::loose(Size::new(390.0, 844.0)));

        assert_eq!(size, Size::new(CHILD, CHILD + BAR_HEIGHT));
    }

    #[test]
    fn the_reveal_starts_fully_above_the_bar_and_settles_flush() {
        // The entrance's endpoints, as geometry: at rest the bar is exactly
        // where layout put it and fully opaque; at progress zero it is its own
        // whole height above the top edge (never merely nudged) and invisible.
        const EXTENT: f64 = 46.0 + 47.0;
        let settled = reveal_layer(1.0, true, EXTENT, false);
        assert_eq!((settled.dy, settled.alpha), (0.0, 1.0));
        assert_eq!(settled.dx, 0.0, "the reveal is vertical only");

        let start = reveal_layer(0.0, true, EXTENT, false);
        assert_eq!(start.dy, -EXTENT);
        assert_eq!(start.alpha, 0.0);

        // …and the exit runs the same range backwards: still flush at 1.0
        // (nothing has left yet), fully gone at 0.0.
        let leaving = reveal_layer(1.0, false, EXTENT, false);
        assert_eq!((leaving.dy, leaving.alpha), (0.0, 1.0));
        let gone = reveal_layer(0.0, false, EXTENT, false);
        assert_eq!((gone.dy, gone.alpha), (-EXTENT, 0.0));
    }

    #[test]
    fn reduced_motion_fades_the_bar_without_moving_it() {
        // Glyph's hard accessibility rule, at this seam: a reduced reveal is a
        // pure linear fade — the bar never travels, in either direction.
        for showing in [true, false] {
            for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let layer = reveal_layer(p, showing, 93.0, true);
                assert_eq!(layer.dy, 0.0, "reduced motion never moves the bar");
                assert_eq!(layer.dx, 0.0);
                assert_eq!(layer.alpha, p as f32, "…it only fades it, linearly");
            }
        }
    }

    #[test]
    fn the_exit_clears_faster_than_the_entrance_arrives() {
        // GlyphSlide's own asymmetry, inherited: exits run on the faster exit
        // curve over a shorter duration, so at the same fraction of its own
        // timeline a leaving bar has travelled further than an arriving one.
        assert!(GLYPH_SLIDE_EXIT < GLYPH_SLIDE_ENTER);
        let arriving = reveal_layer(0.5, true, 100.0, false);
        let leaving = reveal_layer(0.5, false, 100.0, false);
        assert!(
            leaving.dy < arriving.dy,
            "half-way through, the exit is further gone ({}) than the entrance \
             is arrived ({})",
            leaving.dy,
            arriving.dy
        );
    }

    // -----------------------------------------------------------------------
    // Vector chrome
    // -----------------------------------------------------------------------

    #[test]
    fn the_back_chevron_is_the_frameworks_own_chevron_mirrored() {
        use frust::authoring::{BezPath, Shape};

        let mine = BezPath::from_svg(CHEVRON_LEFT.d).expect("the back chevron parses");
        let theirs =
            BezPath::from_svg(frust::icons::CHEVRON_RIGHT.d).expect("the vendored one parses");
        let (mine, theirs) = (mine.bounding_box(), theirs.bounding_box());

        // A mirror about the design box's centre maps `x` to `design - x`, so
        // the two bounding boxes swap across it and share their y band exactly.
        // (The framework's own icon test already pins `theirs` inside the box;
        // this pins that the transcription is a *reflection* and not a redraw.)
        let design = CHEVRON_LEFT.design;
        assert_eq!(CHEVRON_LEFT.design, frust::icons::CHEVRON_RIGHT.design);
        assert!((mine.x0 - (design - theirs.x1)).abs() < 1e-9, "{mine:?}");
        assert!((mine.x1 - (design - theirs.x0)).abs() < 1e-9, "{mine:?}");
        assert!((mine.y0 - theirs.y0).abs() < 1e-9, "{mine:?}");
        assert!((mine.y1 - theirs.y1).abs() < 1e-9, "{mine:?}");
    }

    #[test]
    fn the_slot_mark_fits_the_slot_that_holds_it() {
        // The tap target is the source's 30×30 `.topbar-icon` box; the vector
        // is drawn inside it, so a mark bigger than its box would be clipped
        // by the bar's own band before anyone noticed.
        const { assert!(ICON_SIZE < ICON_SLOT) };
        const { assert!(ICON_SLOT <= BAR_HEIGHT) };
    }

    #[test]
    fn the_back_affordance_announces_a_word_and_not_a_mark() {
        // The a11y half of the vector switch: the slot used to be labelled by
        // the glyph it drew, which a screen reader would read out as the name
        // of the character. A label is a *name*.
        assert!(
            BACK_LABEL.chars().all(|c| c.is_ascii_alphabetic()),
            "{BACK_LABEL:?} is a name, not a mark"
        );
    }

    // -----------------------------------------------------------------------
    // Input and semantics follow the reveal, and nothing else
    // -----------------------------------------------------------------------

    #[test]
    fn a_bar_mid_reveal_paints_but_takes_no_input() {
        // The one predicate `event`/`semantics` gate on. A half-arrived bar
        // still paints (that is the whole point of the reveal) but must not
        // eat a tap meant for the page it floats over — and there is no
        // *modality* term in it any more: an overlay is a page of the root
        // host, which routes input to its top page only, so this bar never
        // sees the press to begin with (module docs).
        let mut widget = TopBarWidget {
            children: Vec::new(),
            rest_origins: Vec::new(),
            back: false,
            hidden: false,
            surface: Color::BLACK,
            rule: Color::BLACK,
            reveal: 1.0,
            reveal_anim: None,
            reveal_pending: false,
            extent: BAR_HEIGHT,
        };
        assert!(widget.settled_open(), "a settled bar takes input");

        widget.hidden = true;
        widget.reveal_pending = true;
        assert!(
            !widget.settled_open(),
            "…and stops the frame the exit is staged, before it has moved"
        );
        assert!(
            widget.occupies_a_box(),
            "it still paints — leaving, not gone"
        );

        widget.reveal_pending = false;
        assert!(
            !widget.occupies_a_box(),
            "a fully hidden bar lays out to nothing at all"
        );
    }

    #[test]
    fn the_leading_swap_crosses_over_vertically() {
        // The brand ↔ `‹` swap is the appbar's leading↔close treatment: ±6px on
        // y, never on x.
        let (incoming, exiting) = LeadingSwap.resolve(0.0, false, Size::ZERO);
        assert_eq!(incoming.dy, LEADING_SHIFT);
        assert_eq!(incoming.dx, 0.0);
        assert_eq!(exiting.dy, 0.0);
        assert_eq!(exiting.alpha, 1.0);

        let (incoming, exiting) = LeadingSwap.resolve(1.0, false, Size::ZERO);
        assert_eq!(incoming.dy, 0.0);
        assert_eq!(incoming.alpha, 1.0);
        assert_eq!(exiting.dy, -LEADING_SHIFT);
        assert_eq!(exiting.alpha, 0.0);
    }
}
