//! The terminal key bar: Compact while the IME is up, Tablet behind "Show key bar". Its slot is
//! always mounted (an appearing child shifts the grid's index and drops focus), and every cap
//! claims focus on tap (an unclaimed `Down` blurs and closes the keyboard).

use std::rc::Rc;

use frust::authoring::text::TextStyle;
use frust::authoring::{
    Action, Affine, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, ErasedCallback,
    EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, PointerPhase, Role,
    SemanticsCtx, Size, Toggled, Vec2, View, Widget, WindowInsets, build_child, erase_callback,
    rebuild_child, route_event_single, teardown_child,
};
use frust::{
    Align, Alignment, Axis, CrossAxisAlignment, EdgeInsets, Either, FlexChild, FlexView, Get,
    GetUntracked, IconSource, Padding, RwSignal, Set, SizedBox, Theme, column, flexible, icon,
    icons, inflexible, row, text,
};

use crate::responsive::WidthBand;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;

use super::term::input::BarKey;
use super::term::{InputSeam, ModifierLatch};
use super::widgets::card::CardInk;

// The Compact bar has no mock: these are original values.

pub const CAP_HEIGHT: f64 = 34.0;
const BAR_PAD_Y: f64 = 6.0;
const BAR_PAD_X: f64 = 4.0;

pub const KEY_BAR_HEIGHT: f64 = CAP_HEIGHT + 2.0 * BAR_PAD_Y;

/// Replaced, not added to, by a group rule at the three group boundaries.
const CAP_GAP: f64 = 2.0;
const CAP_RADIUS: f64 = 8.0;
/// Twelve caps spend `24 ×` this of a ~360 lp row, which [`LABEL_SIZE`] pays for.
pub const CAP_PAD_X: f64 = 4.0;
/// The largest half-step leaving every cap its full padding at 360 lp; `PgUp` binds.
pub const LABEL_SIZE: f32 = 8.5;
/// IBM Plex Mono's 0.6em pitch plus `label_small`'s 0.475px tracking.
const LABEL_ADVANCE: f64 = 0.6 * LABEL_SIZE as f64 + 0.475;
pub const ICON_SIZE: f64 = 13.0;
const SEPARATOR_W: f64 = 1.0;
const SEPARATOR_H: f64 = CAP_HEIGHT - 14.0;
const SEPARATOR_MARGIN: f64 = 3.0;
const PRESS_SCALE: f64 = 0.92;
/// Only when a parent offers an unbounded width; every real frame is bounded.
const FALLBACK_CAP_WIDTH: f64 = 32.0;
const DANGER_BORDER_ALPHA: f32 = 0.35;

// Tablet geometry, from `docs/tablet.html`'s `.key`, `.ksep`, `.arrows` and `.frow .key`.

pub const TABLET_CAP_HEIGHT: f64 = 44.0;
const TABLET_CAP_MIN_W: f64 = 36.0;
const TABLET_CAP_PAD_X: f64 = 7.0;
pub const TABLET_LABEL_SIZE: f32 = 10.5;
const TABLET_LABEL_ADVANCE: f64 = 0.6 * TABLET_LABEL_SIZE as f64 + 0.475;
const TABLET_ICON_SIZE: f64 = 14.0;
const TABLET_SEPARATOR_H: f64 = 24.0;
const ARROW_GAP: f64 = spacing::MICRO;
pub const ARROW_CAP_HEIGHT: f64 = (TABLET_CAP_HEIGHT - ARROW_GAP) / 2.0;
pub const FN_CAP_HEIGHT: f64 = 36.0;

#[derive(Clone, Copy, PartialEq)]
struct CapShape {
    height: f64,
    pad_x: f64,
    icon: f64,
    label: f32,
}

const COMPACT_CAP: CapShape = CapShape {
    height: CAP_HEIGHT,
    pad_x: CAP_PAD_X,
    icon: ICON_SIZE,
    label: LABEL_SIZE,
};
const TABLET_CAP: CapShape = CapShape {
    height: TABLET_CAP_HEIGHT,
    pad_x: TABLET_CAP_PAD_X,
    icon: TABLET_ICON_SIZE,
    label: TABLET_LABEL_SIZE,
};
const ARROW_CAP: CapShape = CapShape {
    height: ARROW_CAP_HEIGHT,
    pad_x: 0.0,
    ..TABLET_CAP
};
const FN_CAP: CapShape = CapShape {
    height: FN_CAP_HEIGHT,
    ..TABLET_CAP
};

fn arrow_mark(key: BarKey) -> Option<IconSource> {
    match key {
        BarKey::Up => Some(icons::ARROW_UPWARD),
        BarKey::Down => Some(icons::ARROW_DOWNWARD),
        BarKey::Left => Some(icons::ARROW_BACK),
        BarKey::Right => Some(icons::ARROW_FORWARD),
        _ => None,
    }
}

/// A flex weight, not a size: the row fills its width and never overflows, so a narrower
/// phone shrinks every cap by the same share.
fn cap_weight(key: BarKey) -> u32 {
    let content = match arrow_mark(key) {
        Some(_) => ICON_SIZE,
        None => key.label().chars().count() as f64 * LABEL_ADVANCE,
    };
    (content + 2.0 * CAP_PAD_X).round() as u32
}

/// Reads `seam.revision` tracked, so a latch change repaints the armed pips.
pub fn key_bar<State: 'static>(active: bool, seam: &InputSeam) -> impl View<State> + use<State> {
    // Read before the early-out, so the subscription exists while the bar is hidden.
    let _revision = seam.revision.get();
    if !active {
        return Either::Left(SizedBox::<State>(None, Some(0.0)));
    }

    let kit = Kit::new(seam);
    let mut children: Vec<FlexChild<State>> = Vec::with_capacity(BarKey::ORDER.len() * 2);
    for (index, key) in BarKey::ORDER.iter().copied().enumerate() {
        if key.separator_before() {
            children.push(inflexible(separator::<State>(
                &kit.colors,
                SEPARATOR_H,
                SEPARATOR_MARGIN,
            )));
        } else if index > 0 {
            children.push(inflexible(SizedBox::<State>(Some(CAP_GAP), None)));
        }
        children.push(flexible(cap_weight(key), kit.cap(key, COMPACT_CAP)));
    }

    let row = FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center);
    Either::Right(kit.band(BAR_PAD_X, BAR_PAD_Y, row))
}

fn keyboard_inset() -> f64 {
    frust::use_context::<WindowInsets>()
        .map(|insets| insets.view_insets.bottom)
        .unwrap_or(0.0)
}

/// Hidden, it keeps only the keyboard's inset. Read-only is re-checked on the seam: this tier
/// never waits for an IME, whose absence keeps the Compact tier off a read-only session.
pub fn tablet_key_bar<State: 'static>(
    shown: bool,
    band: WidthBand,
    fn_row: RwSignal<bool>,
    seam: &InputSeam,
) -> impl View<State> + use<State> {
    let _revision = seam.revision.get();
    let fn_open = fn_row.get();
    if !shown || seam.read_only() {
        return Either::Left(SizedBox::<State>(None, Some(keyboard_inset())));
    }

    let kit = Kit::new(seam);
    let mut row: Vec<FlexChild<State>> = Vec::with_capacity(BarKey::TABLET_ORDER.len() * 2 + 2);
    let keys = BarKey::TABLET_ORDER
        .iter()
        .copied()
        .filter(|key| band == WidthBand::Full || !matches!(key, BarKey::Home | BarKey::End));
    for key in keys {
        if key.tablet_separator_before() {
            row.push(inflexible(separator::<State>(
                &kit.colors,
                TABLET_SEPARATOR_H,
                spacing::SM,
            )));
        } else if !row.is_empty() {
            row.push(inflexible(SizedBox::<State>(Some(spacing::TIGHT), None)));
        }
        let cap = if key == BarKey::Fn {
            let toggle = Rc::new(move |_state: &mut State| fn_row.set(!fn_row.get_untracked()));
            kit.toggle_cap(key, TABLET_CAP, fn_open, toggle)
        } else {
            kit.cap(key, TABLET_CAP)
        };
        let width = tablet_cap_width(key);
        row.push(inflexible(SizedBox::<State>(Some(width), None).child(cap)));
    }
    row.push(flexible(1, SizedBox::<State>(None, None)));
    row.push(inflexible(arrow_cluster::<State>(&kit)));

    let mut rows = vec![inflexible(
        FlexView::new(Axis::Horizontal, row).cross_axis(CrossAxisAlignment::Center),
    )];
    if fn_open {
        let mut keys: Vec<FlexChild<State>> = Vec::with_capacity(BarKey::FN_ROW.len() * 2);
        for key in BarKey::FN_ROW {
            if !keys.is_empty() {
                keys.push(inflexible(SizedBox::<State>(Some(spacing::TIGHT), None)));
            }
            keys.push(flexible(1, kit.cap(key, FN_CAP)));
        }
        rows.push(inflexible(SizedBox::<State>(None, Some(spacing::XS))));
        rows.push(inflexible(FlexView::new(Axis::Horizontal, keys)));
    }
    let column = FlexView::new(Axis::Vertical, rows).cross_axis(CrossAxisAlignment::Stretch);
    Either::Right(kit.band(spacing::BASE, spacing::SM, column))
}

fn tablet_cap_width(key: BarKey) -> f64 {
    let label = key.label().chars().count() as f64 * TABLET_LABEL_ADVANCE;
    (label + 2.0 * TABLET_CAP_PAD_X).max(TABLET_CAP_MIN_W)
}

fn arrow_cluster<State: 'static>(kit: &Kit<'_>) -> impl View<State> {
    let cell = |key: Option<BarKey>| {
        let slot = SizedBox::<State>(Some(TABLET_CAP_MIN_W), Some(ARROW_CAP_HEIGHT));
        inflexible(match key {
            Some(key) => slot.child(kit.cap(key, ARROW_CAP)),
            None => slot,
        })
    };
    let gap = || inflexible(SizedBox::<State>(Some(ARROW_GAP), Some(ARROW_GAP)));
    let line = |keys: [Option<BarKey>; 3]| {
        let [a, b, c] = keys.map(cell);
        inflexible(row().push(a).push(gap()).push(b).push(gap()).push(c))
    };
    column()
        .push(line([None, Some(BarKey::Up), None]))
        .push(gap())
        .push(line([
            Some(BarKey::Left),
            Some(BarKey::Down),
            Some(BarKey::Right),
        ]))
}

fn separator<State: 'static>(colors: &Palette, height: f64, margin: f64) -> impl View<State> {
    Padding(
        EdgeInsets::symmetric(margin, 0.0),
        BarBackdropView {
            fill: colors.outline,
            hairline: Color::TRANSPARENT,
            child: SizedBox::<State>(Some(SEPARATOR_W), Some(height)),
        },
    )
}

struct Kit<'a> {
    theme: Theme,
    colors: Palette,
    ink: CardInk,
    danger: Color,
    latch: ModifierLatch,
    seam: &'a InputSeam,
}

impl<'a> Kit<'a> {
    fn new(seam: &'a InputSeam) -> Self {
        let theme = active_theme();
        Self {
            colors: Palette::from_theme(&theme),
            ink: CardInk::from_theme(&theme),
            danger: theme.scheme().error,
            latch: seam.latch(),
            theme,
            seam,
        }
    }

    fn band<State: 'static, Content: View<State>>(
        &self,
        pad_x: f64,
        pad_y: f64,
        content: Content,
    ) -> impl View<State> + use<State, Content> {
        BarBackdropView {
            fill: self.ink.head,
            hairline: self.ink.card_border,
            child: Padding(
                EdgeInsets {
                    left: pad_x,
                    top: pad_y,
                    right: pad_x,
                    bottom: pad_y + keyboard_inset(),
                },
                content,
            ),
        }
    }

    fn cap<State: 'static>(&self, key: BarKey, shape: CapShape) -> KeyCapView<State> {
        let pressed_seam = self.seam.clone();
        // Via `press_bar_key`, never hand-rolled: it holds the read-only gate.
        let on_press = Rc::new(move |_state: &mut State| {
            pressed_seam.press_bar_key(key);
        });
        self.view(key, shape, key.armed(self.latch), key.is_latch(), on_press)
    }

    fn toggle_cap<State: 'static>(
        &self,
        key: BarKey,
        shape: CapShape,
        on: bool,
        on_press: Rc<dyn Fn(&mut State)>,
    ) -> KeyCapView<State> {
        self.view(key, shape, on, true, on_press)
    }

    fn view<State: 'static>(
        &self,
        key: BarKey,
        shape: CapShape,
        armed: bool,
        toggle: bool,
        on_press: Rc<dyn Fn(&mut State)>,
    ) -> KeyCapView<State> {
        let colors = &self.colors;
        let (fill, border, ink) = if armed {
            (colors.amber_faint, colors.amber_border, colors.amber)
        } else if key.is_danger() {
            (
                colors.raised,
                self.danger.with_alpha(DANGER_BORDER_ALPHA),
                self.danger,
            )
        } else {
            (colors.raised, colors.outline, colors.fg_muted)
        };
        let mut label = self.theme.type_scale.label_small.clone();
        label.size = shape.label;
        label.color = ink;
        KeyCapView {
            key,
            shape,
            label,
            ink,
            fill,
            border,
            armed,
            toggle,
            seam: self.seam.clone(),
            on_press,
        }
    }
}

/// Hand-rolled: `frust::container` strokes a full rounded border, not one square top edge.
struct BarBackdropView<V> {
    fill: Color,
    hairline: Color,
    child: V,
}

struct BarBackdropWidget {
    fill: Color,
    hairline: Color,
    child: ChildPod,
}

impl<State: 'static, V: View<State>> View<State> for BarBackdropView<V> {
    type Element = BarBackdropWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> BarBackdropWidget {
        BarBackdropWidget {
            fill: self.fill,
            hairline: self.hairline,
            child: build_child(&self.child, ctx),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut BarBackdropWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.fill != self.fill || prev.hairline != self.hairline {
            element.fill = self.fill;
            element.hairline = self.hairline;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut BarBackdropWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for BarBackdropWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        if self.fill != Color::TRANSPARENT {
            scene.fill_rect(origin, size, self.fill);
        }
        if self.hairline != Color::TRANSPARENT {
            scene.fill_rect(origin, Size::new(size.width, SEPARATOR_W), self.hairline);
        }
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // A decorative container still routes, or every cap under it is unreachable.
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

struct KeyCapView<State: 'static> {
    key: BarKey,
    shape: CapShape,
    label: TextStyle,
    ink: Color,
    fill: Color,
    border: Color,
    armed: bool,
    toggle: bool,
    seam: InputSeam,
    on_press: Rc<dyn Fn(&mut State)>,
}

impl<State: 'static> KeyCapView<State> {
    fn content(&self) -> impl View<State> + use<State> {
        let inner = match arrow_mark(self.key) {
            Some(mark) => Either::Left(icon(mark).size(self.shape.icon).color(self.ink)),
            None => Either::Right(text(self.key.label()).style(self.label.clone())),
        };
        Padding(
            EdgeInsets::symmetric(self.shape.pad_x, 0.0),
            Align(Alignment::CENTER, inner),
        )
    }
}

struct KeyCapWidget {
    /// A word, never the arrow glyph the cap draws.
    name: &'static str,
    toggled: Option<bool>,
    height: f64,
    fill: Color,
    border: Color,
    content: ChildPod,
    seam: InputSeam,
    pressed: bool,
    /// Set on `Down` with pointer capture; gates `Move`/`Up` so a hover never latches or fires.
    captured: bool,
    on_press: ErasedCallback,
}

impl<State: 'static> View<State> for KeyCapView<State> {
    type Element = KeyCapWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> KeyCapWidget {
        KeyCapWidget {
            name: self.key.name(),
            toggled: self.toggle.then_some(self.armed),
            height: self.shape.height,
            fill: self.fill,
            border: self.border,
            content: build_child(&self.content(), ctx),
            seam: self.seam.clone(),
            pressed: false,
            captured: false,
            on_press: erase_callback(&self.on_press),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut KeyCapWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // Closures and the `Rc`-backed seam aren't comparable; reinstall both every time.
        element.on_press = erase_callback(&self.on_press);
        element.seam = self.seam.clone();
        let mut flags = rebuild_child(&prev.content(), &self.content(), &mut element.content, ctx);
        if prev.shape != self.shape {
            element.height = self.shape.height;
            flags |= ChangeFlags::LAYOUT;
        }
        if prev.key != self.key
            || prev.fill != self.fill
            || prev.border != self.border
            || prev.armed != self.armed
        {
            element.name = self.key.name();
            element.fill = self.fill;
            element.border = self.border;
            element.toggled = self.toggle.then_some(self.armed);
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut KeyCapWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.content(), &mut element.content, ctx);
    }
}

impl Widget for KeyCapWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let width = if bc.max().width.is_finite() {
            bc.max().width
        } else {
            FALLBACK_CAP_WIDTH
        };
        let size = bc.constrain(Size::new(width, self.height));
        self.content.layout_child(ctx, &BoxConstraints::tight(size));
        self.content.set_origin(Point::ZERO);
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        // Paint owns the press: a scale transform keeps hit testing at full size.
        if self.pressed {
            let centre = Vec2::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0);
            scene.push_transform(
                Affine::translate(centre) * Affine::scale(PRESS_SCALE) * Affine::translate(-centre),
            );
        }
        let radius = CAP_RADIUS.min(size.width / 2.0).min(size.height / 2.0);
        scene.fill_rounded_rect(origin, size, radius, self.fill);
        if self.border != Color::TRANSPARENT {
            // `PaintScene` has no stroked rounded rect: border full-size, fill inset over it.
            scene.fill_rounded_rect(origin, size, radius, self.border);
            let inset = Point::new(origin.x + SEPARATOR_W, origin.y + SEPARATOR_W);
            let inner = Size::new(
                (size.width - 2.0 * SEPARATOR_W).max(0.0),
                (size.height - 2.0 * SEPARATOR_W).max(0.0),
            );
            scene.fill_rounded_rect(inset, inner, (radius - SEPARATOR_W).max(0.0), self.fill);
        }
        self.content.paint_child(ctx, scene);
        if self.pressed {
            scene.pop_transform();
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(pointer) = event else {
            // Focus-routed `Key`/`Ime` fold into the shared source exactly as the grid's do.
            if !ctx.has_focus() {
                return EventResult::Ignored;
            }
            if self.seam.absorb(event).is_none() {
                return EventResult::Ignored;
            }
            ctx.publish_ime_state(self.seam.ime_state(None));
            ctx.request_redraw();
            return EventResult::Handled;
        };
        match pointer.phase {
            PointerPhase::Down => {
                // Claim focus first: a hand-off, not a session blur, so the grid must not scrub.
                self.seam.claim_focus();
                ctx.request_focus();
                ctx.publish_ime_state(self.seam.ime_state(None));
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
            // No key repeat and no long-press: a bar key fires once per tap.
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
            node.set_label(self.name);
            node.add_action(Action::Click);
            if let Some(armed) = self.toggled {
                node.set_toggled(if armed { Toggled::True } else { Toggled::False });
            }
        });
    }
}

fn inside(position: Point, size: Size) -> bool {
    position.x >= 0.0 && position.y >= 0.0 && position.x < size.width && position.y < size.height
}
