//! The Connect screen's two hand-rolled paint primitives: a bordered,
//! rounded, filled **panel** (single-child) and a fixed-size **swatch**
//! (leaf).
//!
//! `docs/splash.html`'s Connect mock is mostly borders and washes —
//! `.connect-mark`'s tile and its four grid cells, `.connect-badge`'s box,
//! `.toggle`'s pill and knob, `.dot-ok`, and the `background:var(--bg-void)`
//! backing every full-width piece carries so the amber rain can't bleed
//! through its text while it lands. No facade widget paints an arbitrary
//! filled/bordered rect, so these two [`View`]/[`Widget`] pairs — authored
//! through [`frust::authoring`], the same seam
//! `examples/huddle`'s `ui::fill_box` uses — fill the gap.
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
    AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, Color, LayoutCtx,
    PaintCtx, PaintScene, Point, RoundedRect, SemanticsCtx, Shape, Size, View, Widget, any,
    build_child, rebuild_child, teardown_child,
};

/// Hairline border width in logical px (`1px solid` on every bordered rule in
/// `docs/splash.html`). Matches `frust-widgets`' own `BORDER_WIDTH`.
const HAIRLINE: f64 = 1.0;
/// Flattening tolerance for the rounded-rect border path — the value
/// `frust-widgets`' `Button`/`material::card` border stroke uses.
const BORDER_TOLERANCE: f64 = 0.1;

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

    // No `event` impl: every Connect piece is static presentation today (the
    // interactive upgrade lands in Phase 2), and the two interactive widgets
    // the pieces *do* nest (`text_input`, `button`) are reached through their
    // own parents, never through a panel.

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // A transparent container still MUST forward, or the child's whole
        // subtree drops out of the accessibility tree.
        self.child.semantics_child(ctx);
    }
}

// ---------------------------------------------------------------------------
// Swatch — a fixed-size filled leaf
// ---------------------------------------------------------------------------

/// A fixed-size filled rounded rect with an optional hairline border — the
/// `.connect-mark span` grid cells, the `.toggle` pill and its knob, and
/// `.dot-ok`. A circle is a swatch whose `radius` is half its edge.
pub struct SwatchView {
    size: Size,
    fill: Color,
    radius: f64,
    border: Option<Color>,
}

/// A `size`-sized box filled with `fill`, corner-rounded by `radius`.
pub fn swatch(size: Size, fill: Color, radius: f64) -> SwatchView {
    SwatchView {
        size,
        fill,
        radius,
        border: None,
    }
}

impl SwatchView {
    /// Stroke a hairline border in `color` just inside the swatch's bounds.
    pub fn border(mut self, color: Color) -> Self {
        self.border = Some(color);
        self
    }
}

/// The retained half of a [`SwatchView`].
pub struct SwatchWidget {
    size: Size,
    fill: Color,
    radius: f64,
    border: Option<Color>,
}

impl<State: 'static> View<State> for SwatchView {
    type Element = SwatchWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> SwatchWidget {
        SwatchWidget {
            size: self.size,
            fill: self.fill,
            radius: self.radius,
            border: self.border,
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
        if prev.fill != self.fill || prev.radius != self.radius || prev.border != self.border {
            element.fill = self.fill;
            element.radius = self.radius;
            element.border = self.border;
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
        if self.fill != Color::TRANSPARENT {
            scene.fill_rounded_rect(origin, size, self.radius, self.fill);
        }
        if let Some(border) = self.border {
            stroke_border(scene, origin, size, self.radius, border);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_swatch_takes_its_declared_size_and_clamps_to_the_incoming_maximum() {
        let mut widget = SwatchWidget {
            size: Size::new(60.0, 60.0),
            fill: Color::BLACK,
            radius: 4.0,
            border: None,
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
