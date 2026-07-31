//! The Connect screen's hand-rolled primitives: a bordered, rounded, filled
//! **panel** (single-child), a fixed-size **swatch** (leaf, optionally
//! glowing), and a **switch-semantics** wrapper.
//!
//! The Connect mock is mostly borders and washes — `.connect-mark`'s tile and
//! its four grid cells, `.connect-badge`'s box, `.toggle`'s pill and knob,
//! `.dot-ok` (glow included), and the page-tone backing every full-width piece
//! carries so the amber rain can't bleed through its text while it lands (see
//! [`super::pieces`]'s `Palette::page`). No facade widget paints an arbitrary
//! filled/bordered rect, so these [`View`]/[`Widget`] pairs — authored
//! through [`frust::authoring`], the same seam
//! `examples/huddle`'s `ui::fill_box` uses — fill the gap.
//!
//! [`switch_semantics`] closes the accessibility hole that composition opens:
//! a toggle assembled from swatches and a `GestureDetector` paints and presses
//! correctly but contributes **no** accessibility node at all (a
//! `GestureDetector` only forwards its child's, and a swatch has none), so a
//! screen reader would find no control there. It contributes the one
//! `Role::Switch` node the composed pile is worth. Delete it the day
//! `frust::glyph` grows a real switch.
//!
//! **Feature-local on purpose.** They live beside the pieces that need them
//! rather than in `crate::ui`; `examples/huddle` promoted its own `fill_box`
//! to the shared layer only once a second feature needed it, and nothing else
//! in muxr does yet.
//!
//! Both take already-resolved [`Color`]s: their callers
//! ([`super::pieces`]) resolve every one from the active [`frust::Theme`]
//! once per build, so these stay dumb about theming.

use frust::authoring::{
    Action, AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, Color,
    EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, Role, RoundedRect,
    SemanticsCtx, Shape, Size, Toggled, View, Widget, any, build_child, rebuild_child,
    route_event_single, teardown_child,
};

/// Hairline border width in logical px (`1px solid` on every bordered rule in
/// `docs/splash.html`). Matches `frust-widgets`' own `BORDER_WIDTH`.
const HAIRLINE: f64 = 1.0;
/// Flattening tolerance for the rounded-rect border path — the value
/// `frust-widgets`' `Button`/`material::card` border stroke uses.
const BORDER_TOLERANCE: f64 = 0.1;
/// `.dot-ok{box-shadow:0 0 5px var(--success)}` — the blur radius as a gaussian
/// std-dev for [`PaintScene::draw_shadow`], plus how far the glow bleeds past
/// the swatch on each side. The same pair the Sessions screen's own `.dot-ok`
/// uses, so one dot does not glow differently from the other.
const GLOW_STD_DEV: f64 = 2.5;
const GLOW_SPREAD: f64 = 2.0;

/// Stroke a hairline border just inside `origin`/`size`.
///
/// Inset by half the stroke width so the hairline paints fully inside the
/// widget's own bounds (a stroke is centered on its path) — the
/// `frust-widgets` `Button` precedent, re-derived here because that helper is
/// crate-private.
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

// ---------------------------------------------------------------------------
// Panel — a filled, optionally bordered, rounded box behind one child
// ---------------------------------------------------------------------------

/// A rounded fill (plus optional hairline border) painted *behind* a child it
/// sizes itself to — the piece backing, `.connect-mark`'s tile and
/// `.connect-badge`'s box.
///
/// Deliberately **not** `glyph::glyph_card`: that widget brings its own
/// header/description/footer slot structure and inset, where every one of
/// these is a bare box around already-composed content.
pub struct PanelView<State: 'static> {
    child: AnyView<State>,
    fill: Color,
    radius: f64,
    border: Option<Color>,
}

/// Wrap `child` in a `fill`-filled box with corner `radius`.
pub fn panel<State: 'static, V: View<State>>(
    child: V,
    fill: Color,
    radius: f64,
) -> PanelView<State> {
    PanelView {
        child: any(child),
        fill,
        radius,
        border: None,
    }
}

impl<State: 'static> PanelView<State> {
    /// Stroke a hairline border in `color` just inside the panel's bounds.
    pub fn border(mut self, color: Color) -> Self {
        self.border = Some(color);
        self
    }
}

/// The retained half of a [`PanelView`].
pub struct PanelWidget {
    child: ChildPod,
    fill: Color,
    radius: f64,
    border: Option<Color>,
}

impl<State: 'static> View<State> for PanelView<State> {
    type Element = PanelWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> PanelWidget {
        PanelWidget {
            child: build_child(&self.child, ctx),
            fill: self.fill,
            radius: self.radius,
            border: self.border,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut PanelWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.fill != self.fill || prev.radius != self.radius || prev.border != self.border {
            element.fill = self.fill;
            element.radius = self.radius;
            element.border = self.border;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut PanelWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for PanelWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        if self.fill != Color::TRANSPARENT {
            scene.fill_rounded_rect(origin, size, self.radius, self.fill);
        }
        if let Some(border) = self.border {
            stroke_border(scene, origin, size, self.radius, border);
        }
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // A decorative single-child container still MUST route, or every
        // control it backs is silently unreachable: the panel paints a fill and
        // nothing else, so a widget nested under one (the URL/token fields, the
        // remember toggle, the trust rows, a saved-server row) is only pressable
        // through here. `route_event_single` owns the capture/focus fast paths
        // and the blur-on-outside-tap rule — never re-hit-test a captured child
        // by hand.
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // A transparent container still MUST forward, or the child's whole
        // subtree drops out of the accessibility tree.
        self.child.semantics_child(ctx);
    }
}

// ---------------------------------------------------------------------------
// Swatch — a fixed-size filled leaf
// ---------------------------------------------------------------------------

/// A fixed-size filled rounded rect with an optional hairline border and an
/// optional soft halo — the `.connect-mark span` grid cells, the `.toggle`
/// pill and its knob, and `.dot-ok`. A circle is a swatch whose `radius` is
/// half its edge.
pub struct SwatchView {
    size: Size,
    fill: Color,
    radius: f64,
    border: Option<Color>,
    glow: Option<Color>,
}

/// A `size`-sized box filled with `fill`, corner-rounded by `radius`.
pub fn swatch(size: Size, fill: Color, radius: f64) -> SwatchView {
    SwatchView {
        size,
        fill,
        radius,
        border: None,
        glow: None,
    }
}

impl SwatchView {
    /// Stroke a hairline border in `color` just inside the swatch's bounds.
    pub fn border(mut self, color: Color) -> Self {
        self.border = Some(color);
        self
    }

    /// Bloom a soft `color` halo behind the swatch — `.dot-ok`'s
    /// `box-shadow:0 0 5px var(--success)`, through
    /// [`PaintScene::draw_shadow`]. Painted *under* the fill, so the dot
    /// itself stays crisp.
    pub fn glow(mut self, color: Color) -> Self {
        self.glow = Some(color);
        self
    }
}

/// The retained half of a [`SwatchView`].
pub struct SwatchWidget {
    size: Size,
    fill: Color,
    radius: f64,
    border: Option<Color>,
    glow: Option<Color>,
}

impl<State: 'static> View<State> for SwatchView {
    type Element = SwatchWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> SwatchWidget {
        SwatchWidget {
            size: self.size,
            fill: self.fill,
            radius: self.radius,
            border: self.border,
            glow: self.glow,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut SwatchWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if prev.size != self.size {
            element.size = self.size;
            flags |= ChangeFlags::LAYOUT;
        }
        if prev.fill != self.fill
            || prev.radius != self.radius
            || prev.border != self.border
            || prev.glow != self.glow
        {
            element.fill = self.fill;
            element.radius = self.radius;
            element.border = self.border;
            element.glow = self.glow;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }
}

impl Widget for SwatchWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(self.size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        if let Some(glow) = self.glow {
            let spread = Size::new(
                size.width + 2.0 * GLOW_SPREAD,
                size.height + 2.0 * GLOW_SPREAD,
            );
            scene.draw_shadow(
                Point::new(origin.x - GLOW_SPREAD, origin.y - GLOW_SPREAD),
                spread,
                self.radius + GLOW_SPREAD,
                GLOW_STD_DEV,
                glow,
            );
        }
        if self.fill != Color::TRANSPARENT {
            scene.fill_rounded_rect(origin, size, self.radius, self.fill);
        }
        if let Some(border) = self.border {
            stroke_border(scene, origin, size, self.radius, border);
        }
    }
}

// ---------------------------------------------------------------------------
// Switch semantics — an accessibility node over a hand-composed toggle
// ---------------------------------------------------------------------------

/// Give `child` — a hand-composed toggle's art plus its press recognizer — the
/// one accessibility node it cannot grow itself: a labelled, toggled
/// [`Role::Switch`] advertising the [`Action::Click`] it fires.
///
/// Layout, paint and event routing are pure pass-through; only
/// [`Widget::semantics`] does anything. See the [module docs](self).
pub fn switch_semantics<State: 'static, V: View<State>>(
    child: V,
    label: impl Into<String>,
    on: bool,
) -> SwitchSemanticsView<State> {
    SwitchSemanticsView {
        child: any(child),
        label: label.into(),
        on,
    }
}

/// The declarative half of [`switch_semantics`].
pub struct SwitchSemanticsView<State: 'static> {
    child: AnyView<State>,
    label: String,
    on: bool,
}

/// The retained half of [`switch_semantics`].
pub struct SwitchSemanticsWidget {
    child: ChildPod,
    label: String,
    on: bool,
}

impl<State: 'static> View<State> for SwitchSemanticsView<State> {
    type Element = SwitchSemanticsWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> SwitchSemanticsWidget {
        SwitchSemanticsWidget {
            child: build_child(&self.child, ctx),
            label: self.label.clone(),
            on: self.on,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut SwitchSemanticsWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.label != self.label || prev.on != self.on {
            element.label = self.label.clone();
            element.on = self.on;
            // Semantics-only: the accessibility tree is rebuilt off a paint
            // pass, so PAINT is what republishes the node.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut SwitchSemanticsWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for SwitchSemanticsWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // A leaf node, deliberately: the composed art underneath has nothing to
        // announce, and exposing four unlabelled swatches inside a switch would
        // be noise rather than detail.
        ctx.push_node(Role::Switch, |node| {
            node.set_label(self.label.as_str());
            node.set_toggled(if self.on {
                Toggled::True
            } else {
                Toggled::False
            });
            node.add_action(Action::Click);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recording scene that remembers filled rounded rects and blurred
    /// halos, in paint order.
    #[derive(Default)]
    struct RecScene {
        rounded: Vec<(Point, Size)>,
        shadows: Vec<(Point, Size, f64)>,
    }

    impl PaintScene for RecScene {
        fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
        fn fill_rounded_rect(&mut self, origin: Point, size: Size, _radius: f64, _color: Color) {
            self.rounded.push((origin, size));
        }
        fn draw_text(&mut self, _origin: Point, _text: &str) {}
        fn draw_shadow(
            &mut self,
            origin: Point,
            size: Size,
            _radius: f64,
            std_dev: f64,
            _color: Color,
        ) {
            self.shadows.push((origin, size, std_dev));
        }
    }

    #[test]
    fn a_glowing_swatch_blooms_a_halo_under_its_own_fill() {
        // `.dot-ok{box-shadow:0 0 5px var(--success)}` — the halo spreads
        // symmetrically past the 7px dot and is painted *first*, so the dot
        // itself stays crisp on top of it.
        let mut widget = SwatchWidget {
            size: Size::new(7.0, 7.0),
            fill: Color::WHITE,
            radius: 3.5,
            border: None,
            glow: Some(Color::WHITE),
        };
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::new(20.0, 40.0), Size::new(7.0, 7.0));
        widget.paint(&mut ctx, &mut scene);

        assert_eq!(
            scene.shadows,
            vec![(
                Point::new(20.0 - GLOW_SPREAD, 40.0 - GLOW_SPREAD),
                Size::new(7.0 + 2.0 * GLOW_SPREAD, 7.0 + 2.0 * GLOW_SPREAD),
                GLOW_STD_DEV,
            )]
        );
        assert_eq!(
            scene.rounded,
            vec![(Point::new(20.0, 40.0), Size::new(7.0, 7.0))],
            "the dot itself is unmoved and unresized by its halo"
        );
    }

    #[test]
    fn a_swatch_with_no_glow_paints_no_halo() {
        let mut widget = SwatchWidget {
            size: Size::new(7.0, 7.0),
            fill: Color::WHITE,
            radius: 3.5,
            border: None,
            glow: None,
        };
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::ZERO, Size::new(7.0, 7.0));
        widget.paint(&mut ctx, &mut scene);
        assert!(scene.shadows.is_empty(), "the glow is opt-in");
    }

    #[test]
    fn a_swatch_takes_its_declared_size_and_clamps_to_the_incoming_maximum() {
        let mut widget = SwatchWidget {
            size: Size::new(60.0, 60.0),
            fill: Color::BLACK,
            radius: 4.0,
            border: None,
            glow: None,
        };
        let mut ctx = LayoutCtx::new();
        assert_eq!(
            widget.layout(&mut ctx, &BoxConstraints::loose(Size::new(340.0, 650.0))),
            Size::new(60.0, 60.0)
        );
        // A tight (stretch) constraint still wins — the Flex cross-axis case.
        assert_eq!(
            widget.layout(&mut ctx, &BoxConstraints::tight(Size::new(20.0, 20.0))),
            Size::new(20.0, 20.0)
        );
    }
}
