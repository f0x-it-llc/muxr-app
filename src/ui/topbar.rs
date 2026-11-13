//! The brand row and icon slot Glyph's app bar wears, plus `icon_button`, the app's one vector
//! press target. A bar-bearing page reserves no clearance: the scaffold body sizes around the bar.
use std::rc::Rc;

use frust::authoring::text::LineHeight;
use frust::authoring::{
    Action, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, ErasedCallback, EventCtx,
    EventResult, InputEvent, LayoutCtx, PRESSED_OPACITY, PaintCtx, PaintScene, Point, PointerPhase,
    Role, SemanticsCtx, Size, View, Widget, build_child, erase_callback, rebuild_child,
    teardown_child,
};
use frust::{CrossAxisAlignment, IconData, SizedBox, icon, row, text};

use crate::ui::palette::active_theme;

/// Glyph's compact app bar content height (`.appbar{height:52px}`), excluding the top window
/// inset; the terminal chrome locates the bar's trailing slot with it.
pub const BAR_HEIGHT: f64 = 52.0;
const MARK_SIZE: f64 = 16.0;
const MARK_GAP: f64 = 2.0;
const MARK_CELL: f64 = (MARK_SIZE - MARK_GAP) / 2.0;
const MARK_RADIUS: f64 = 1.0;
const BRAND_GAP: f64 = 7.0;
const BRAND_SIZE: f32 = 13.0;
const BRAND_LINE_HEIGHT: f32 = 18.0;

/// `.ab-icon{width:40px;height:40px}`.
const BAR_ICON_SLOT: f64 = 40.0;
const BAR_ICON_SIZE: f64 = 20.0;
const ICON_DESIGN_BOX: f64 = 24.0;

const BRAND: &str = "muxr";

pub const BACK_LABEL: &str = "Back";

pub fn brand_leading<State: 'static>() -> impl View<State> {
    let theme = active_theme();
    let scheme = theme.scheme();
    let mut brand_style = theme.type_scale.display_small.clone();
    brand_style.size = BRAND_SIZE;
    brand_style.line_height = LineHeight::Absolute(BRAND_LINE_HEIGHT);
    brand_style.color = scheme.primary;
    row()
        .child(BrandMark {
            color: scheme.primary_container,
        })
        .child(SizedBox::<State>(Some(BRAND_GAP), None))
        .child(text(BRAND).style(brand_style))
        .cross_axis(CrossAxisAlignment::Center)
}

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

/// `.brand-mark`: four cells on a 2×2 grid in a 16×16 box.
struct BrandMark {
    color: Color,
}

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

pub struct IconButtonView<State: 'static> {
    mark: IconData,
    label: String,
    /// `None` lets [`frust::icon`] resolve `on_surface` from the theme.
    ink: Option<Color>,
    fill: Color,
    icon_size: f64,
    box_size: Option<Size>,
    radius: f64,
    on_press: Rc<dyn Fn(&mut State)>,
}

impl<State: 'static> IconButtonView<State> {
    pub fn ink(mut self, ink: Color) -> Self {
        self.ink = Some(ink);
        self
    }

    pub fn fill(mut self, fill: Color) -> Self {
        self.fill = fill;
        self
    }

    pub fn icon_size(mut self, size: f64) -> Self {
        self.icon_size = size;
        self
    }

    pub fn box_size(mut self, size: Size) -> Self {
        self.box_size = Some(size);
        self
    }

    pub fn radius(mut self, radius: f64) -> Self {
        self.radius = radius;
        self
    }

    fn mark_view(&self) -> impl View<State> + use<State> {
        let mut view = icon(self.mark.clone()).size(self.icon_size);
        if let Some(ink) = self.ink {
            view = view.color(ink);
        }
        view
    }
}

pub struct IconButtonWidget {
    mark: ChildPod,
    label: String,
    fill: Color,
    ink: Option<Color>,
    icon_size: f64,
    box_size: Option<Size>,
    radius: f64,
    pressed: bool,
    /// Gates `Move`/`Up` so a hover never latches or fires.
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
        // Closures are not comparable; reinstall the adapter every time.
        element.on_press = erase_callback(&self.on_press);
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
            // Semantics are recomputed off the paint pass, so a label change needs PAINT.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut IconButtonWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.mark_view(), &mut element.mark, ctx);
    }
}

fn inside(pos: Point, size: Size) -> bool {
    pos.x >= 0.0 && pos.y >= 0.0 && pos.x < size.width && pos.y < size.height
}

impl Widget for IconButtonWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let mark_bc = BoxConstraints::tight(Size::new(self.icon_size, self.icon_size));
        let mark = self.mark.layout_child(ctx, &mark_bc);
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
            // Washed in the mark's own ink so both a ghost slot and an accent disc darken.
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
            // A `Cancel` arm clears flags only, never touching state.
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
        // One labelled Button leaf: a screen reader needs the control's name, never its mark.
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
        assert!((2.0 * MARK_CELL + MARK_GAP - MARK_SIZE).abs() < 1e-9);
    }

    #[test]
    fn the_slot_mark_fits_the_slot_that_holds_it() {
        const { assert!(BAR_ICON_SIZE < BAR_ICON_SLOT) };
    }

    #[test]
    fn the_back_affordance_announces_a_word_and_not_a_mark() {
        assert!(
            BACK_LABEL.chars().all(|c| c.is_ascii_alphabetic()),
            "{BACK_LABEL:?} is a name, not a mark"
        );
    }
}
