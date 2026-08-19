//! The app's bar furniture: the pieces `crate::shell`'s route-meta adapter
//! feeds to Glyph's own `AppBarView` (the brand row, a bar icon slot), plus
//! `icon_button`, the app's one vector press-target primitive, and the top
//! clearance a page reserves for the bar.
//!
//! The bar itself is Glyph's — its surface, its inset consumption, its title
//! crossfade and its layout all belong to `frust_glyph::app_bar`, mounted in
//! the shell scaffold's `app_bar` slot.
use std::rc::Rc;

use frust::authoring::text::LineHeight;
use frust::authoring::{
    Action, AnyView, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, ErasedCallback,
    EventCtx, EventResult, InputEvent, LayoutCtx, PRESSED_OPACITY, PaintCtx, PaintScene, Point,
    PointerPhase, Role, SemanticsCtx, Size, View, Widget, any, build_child, erase_callback,
    rebuild_child, teardown_child,
};
use frust::{
    Axis, CrossAxisAlignment, EdgeInsets, FlexView, IconData, Padding, SizedBox, icon, inflexible,
    safe_area, text,
};

use crate::ui::palette::active_theme;

/// The top clearance a bar-bearing page reserves for itself
/// ([`topbar_clearance`]) — and the band `crate::features::terminal`'s own
/// chrome measures against.
pub const BAR_HEIGHT: f64 = 46.0;
const MARK_SIZE: f64 = 16.0;
const MARK_GAP: f64 = 2.0;
const MARK_CELL: f64 = (MARK_SIZE - MARK_GAP) / 2.0;
const MARK_RADIUS: f64 = 1.0;
const BRAND_GAP: f64 = 7.0;
const BRAND_SIZE: f32 = 13.0;
const BRAND_LINE_HEIGHT: f32 = 18.0;

/// The Glyph app bar's icon hit target (`.ab-icon{width:40px;height:40px}`).
const BAR_ICON_SLOT: f64 = 40.0;
/// The mark drawn inside that slot.
const BAR_ICON_SIZE: f64 = 20.0;
const ICON_DESIGN_BOX: f64 = 24.0;

const BRAND: &str = "muxr";

pub const BACK_LABEL: &str = "Back";

/// The bar's leading brand row: the four-cell mark beside the wordmark
/// (`docs/redesign.html`'s `.topbar` brand), laid out at its natural width in
/// the app bar's loose leading slot.
pub fn brand_leading<State: 'static>() -> AnyView<State> {
    let theme = active_theme();
    let scheme = theme.scheme();
    let mut brand_style = theme.type_scale.display_small.clone();
    brand_style.size = BRAND_SIZE;
    brand_style.line_height = LineHeight::Absolute(BRAND_LINE_HEIGHT);
    brand_style.color = scheme.primary;
    any(FlexView::new(
        Axis::Horizontal,
        vec![
            inflexible(BrandMark {
                color: scheme.primary_container,
            }),
            inflexible(SizedBox::<State>(Some(BRAND_GAP), None)),
            inflexible(text(BRAND).style(brand_style)),
        ],
    )
    .cross_axis(CrossAxisAlignment::Center))
}

/// A vector affordance sized for the app bar's icon slot: `mark` at
/// [`BAR_ICON_SIZE`] centred in a [`BAR_ICON_SLOT`] tap target, inked
/// `on_surface` and rounded by the theme's small shape token.
pub fn bar_icon<State: 'static, F: Fn(&mut State) + 'static>(
    mark: impl Into<IconData>,
    label: impl Into<String>,
    on_press: F,
) -> IconButtonView<State> {
    let theme = active_theme();
    icon_button(mark, label, on_press)
        .ink(theme.scheme().on_surface)
        .icon_size(BAR_ICON_SIZE)
        .box_size(Size::new(BAR_ICON_SLOT, BAR_ICON_SLOT))
        .radius(theme.shape.small)
}

/// `body` with the top clearance a bar-bearing page owes reserved above it:
/// the status-bar inset **plus** [`BAR_HEIGHT`].
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
        // button. Either way a tight incoming constraint wins outright, and the
        // mark centres in whatever results.
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
    fn the_clearance_reserves_exactly_the_bars_own_box() {
        // What a bar-bearing page pads at its top must be exactly the band the
        // bar occupies. At a zero window inset that is the 46px band and
        // nothing else — the inset half of the sum is the *same*
        // `WindowInsets::padding().top` both sides resolve, which
        // `LayoutCtx::new` cannot fake here (the setter is `pub(crate)`).
        const CHILD: f64 = 100.0;
        let view: AnyView<()> = topbar_clearance(SizedBox::<()>(Some(CHILD), Some(CHILD)));
        let mut next_id = 0u64;
        let mut widget = view.build(&mut BuildCtx::new(&mut next_id));
        let mut ctx = LayoutCtx::new();
        let size = widget.layout(&mut ctx, &BoxConstraints::loose(Size::new(390.0, 844.0)));

        assert_eq!(size, Size::new(CHILD, CHILD + BAR_HEIGHT));
    }

    #[test]
    fn the_slot_mark_fits_the_slot_that_holds_it() {
        // The tap target is Glyph's own 40×40 `.ab-icon` box; the vector is
        // drawn inside it, so a mark bigger than its box would be clipped by
        // the bar's band before anyone noticed.
        const { assert!(BAR_ICON_SIZE < BAR_ICON_SLOT) };
    }

    #[test]
    fn the_back_affordance_announces_a_word_and_not_a_mark() {
        // The a11y half of the vector rule: the slot used to be labelled by the
        // glyph it drew, which a screen reader would read out as the name of
        // the character. A label is a *name*.
        assert!(
            BACK_LABEL.chars().all(|c| c.is_ascii_alphabetic()),
            "{BACK_LABEL:?} is a name, not a mark"
        );
    }
}
