//! The Settings screen's hand-rolled primitives: a filled/bordered **panel**
//! (single-child, with the row's bottom hairline, the dashed create-token
//! outline and the picker sheet's full-bleed selected row as options), and the
//! **switch-semantics** wrapper the bell toggle needs. No facade widget paints
//! an arbitrary filled/bordered rect, so this [`View`]/[`Widget`] pair —
//! authored through [`frust::authoring`], the same seam `crate::ui::backdrop`
//! uses — fills the gap.
//!
//! **Feature-local, and knowingly the third copy.** `features::connect` and
//! `features::sessions` each own a panel of their own, since a feature never
//! imports a sibling's `presentation`; **three copies is the promotion
//! trigger** (`examples/huddle` promoted its own `fill_box` on the second
//! consumer), and the three panels here should be promoted to `crate::ui`
//! together in a task that owns all three screens' files — recorded rather
//! than done.
//!
//! [`switch_semantics`] closes the accessibility hole composition opens: a
//! toggle assembled from filled boxes and a `GestureDetector` contributes
//! **no** accessibility node at all — delete it the day `frust::glyph` grows
//! a real switch (the Material catalog's `Switch` is **not** compiled into
//! this app).
//!
//! **Frust gaps worked around here:** `PaintScene` has no dash pattern, so a
//! dashed panel walks each edge itself in on/off steps — delete that branch
//! the day frust grows dash support. No negative margins: `.pick-row.sel`'s
//! selected-row fill bleeds out past the sheet's content gutter, and frust's
//! `Padding` takes no negative inset, so [`PanelView::bleed`] widens the
//! *painted* fill instead, leaving layout untouched. No fixed-size
//! *container*: `SizedBox` tightens its child's constraints rather than
//! centering it in the box, so [`PanelView::fixed`] is the centering variant
//! (the same shape `frust::glyph`'s own avatar solves internally, with no
//! reusable container exposed for app chrome).

use frust::authoring::{
    Action, AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, Color,
    EventCtx, EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, Role, RoundedRect,
    SemanticsCtx, Shape, Size, Toggled, View, Widget, any, build_child, rebuild_child,
    route_event_single, teardown_child,
};

/// Hairline border width in logical px (`1px solid` on every bordered rule in
/// `docs/redesign.html`). Matches `frust-widgets`' own `BORDER_WIDTH`.
pub(super) const HAIRLINE: f64 = 1.0;
/// `--radius-full: 999px` — clamped to half the shorter side at paint time, so
/// a caller can spell "pill" without knowing the box's measured height.
pub(super) const RADIUS_FULL: f64 = 999.0;
/// Flattening tolerance for the rounded-rect border path — the value
/// `frust-widgets`' `Button`/`material::card` border stroke uses.
const BORDER_TOLERANCE: f64 = 0.1;
/// `.add-token-btn{border:1px dashed}` — the on/off step of the hand-drawn
/// dash, in logical px. CSS leaves `dashed`'s period to the UA; 4/4 is the
/// common rendering and reads correctly at this scale (the same pair
/// `features::sessions`' own dashed row picked).
const DASH_ON: f64 = 4.0;
const DASH_OFF: f64 = 4.0;

/// How a panel's border is stroked.
#[derive(Clone, Copy, PartialEq)]
enum BorderStyle {
    /// `1px solid` — every bordered box except the create-token row.
    Solid,
    /// `1px dashed` — `.add-token-btn`. See the [module docs](self)'s gap note.
    Dashed,
}

/// The effective corner radius for a box of `size` — [`RADIUS_FULL`] (or any
/// over-large value) collapses to a true pill rather than overshooting.
fn effective_radius(radius: f64, size: Size) -> f64 {
    radius.min(size.width / 2.0).min(size.height / 2.0).max(0.0)
}

// ---------------------------------------------------------------------------
// Panel — a filled box behind one child
// ---------------------------------------------------------------------------

/// A rounded fill (plus an optional hairline border — solid or dashed — and an
/// optional bottom rule) painted *behind* a child it sizes itself to:
/// `.set-row`, `.set-ic`, `.token-scope`, `.add-token-btn`, `.pick-row`, and
/// the screen's own `--bg-base` page tone.
pub(super) struct PanelView<State: 'static> {
    child: AnyView<State>,
    fill: Color,
    radius: f64,
    border: Option<(Color, BorderStyle)>,
    bottom_rule: Option<Color>,
    bleed: f64,
    fixed: Option<Size>,
}

/// Wrap `child` in a `fill`-filled box with corner `radius`.
pub(super) fn panel<State: 'static, V: View<State>>(
    child: V,
    fill: Color,
    radius: f64,
) -> PanelView<State> {
    PanelView {
        child: any(child),
        fill,
        radius,
        border: None,
        bottom_rule: None,
        bleed: 0.0,
        fixed: None,
    }
}

impl<State: 'static> PanelView<State> {
    /// Stroke a solid hairline border in `color` just inside the panel's
    /// bounds.
    pub(super) fn border(mut self, color: Color) -> Self {
        self.border = Some((color, BorderStyle::Solid));
        self
    }

    /// …and the dashed variant (`.add-token-btn`).
    pub(super) fn dashed_border(mut self, color: Color) -> Self {
        self.border = Some((color, BorderStyle::Dashed));
        self
    }

    /// Paint a hairline along the panel's bottom edge (`.set-row`'s
    /// `border-bottom`, `.pick-row`'s separator).
    pub(super) fn bottom_rule(mut self, color: Color) -> Self {
        self.bottom_rule = Some(color);
        self
    }

    /// Widen the painted fill by `bleed` logical px on each side **without**
    /// changing layout — `.pick-row.sel{margin:0 -20px}`. The bottom rule and
    /// the border keep the panel's own width; only the fill bleeds.
    pub(super) fn bleed(mut self, bleed: f64) -> Self {
        self.bleed = bleed;
        self
    }

    /// Force the panel to exactly `size`, **centering** the child inside it —
    /// `.set-ic`'s 28px tile, the toggle's 36×21 track, its 15px knob. See the
    /// [module docs](self)'s gap note for why `SizedBox` is not this.
    pub(super) fn fixed(mut self, size: Size) -> Self {
        self.fixed = Some(size);
        self
    }
}

/// The retained half of a [`PanelView`].
pub(super) struct PanelWidget {
    child: ChildPod,
    fill: Color,
    radius: f64,
    border: Option<(Color, BorderStyle)>,
    bottom_rule: Option<Color>,
    bleed: f64,
    fixed: Option<Size>,
}

impl<State: 'static> View<State> for PanelView<State> {
    type Element = PanelWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> PanelWidget {
        PanelWidget {
            child: build_child(&self.child, ctx),
            fill: self.fill,
            radius: self.radius,
            border: self.border,
            bottom_rule: self.bottom_rule,
            bleed: self.bleed,
            fixed: self.fixed,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut PanelWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.fixed != self.fixed {
            element.fixed = self.fixed;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        if prev.fill != self.fill
            || prev.radius != self.radius
            || prev.border != self.border
            || prev.bottom_rule != self.bottom_rule
            || prev.bleed != self.bleed
        {
            element.fill = self.fill;
            element.radius = self.radius;
            element.border = self.border;
            element.bottom_rule = self.bottom_rule;
            element.bleed = self.bleed;
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
        let Some(fixed) = self.fixed else {
            let size = self.child.layout_child(ctx, bc);
            self.child.set_origin(Point::ZERO);
            return bc.constrain(size);
        };
        // Fixed: the child takes its natural size under a loose box of `fixed`
        // and is centered in it (see `PanelView::fixed`).
        let child = self
            .child
            .layout_child(ctx, &BoxConstraints::new(Size::ZERO, fixed));
        self.child.set_origin(Point::new(
            ((fixed.width - child.width) / 2.0).max(0.0),
            ((fixed.height - child.height) / 2.0).max(0.0),
        ));
        bc.constrain(fixed)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        if self.fill != Color::TRANSPARENT {
            // The bleed widens only what is painted (see the module docs' gap
            // note): the child's origin and this widget's own bounds are
            // untouched, so nothing reflows when a selection moves.
            let fill_origin = Point::new(origin.x - self.bleed, origin.y);
            let fill_size = Size::new(size.width + 2.0 * self.bleed, size.height);
            let radius = effective_radius(self.radius, fill_size);
            // A zero radius **is** a rect, and `PaintScene` carries the
            // cheaper primitive for one. Emitting it as such also keeps this
            // screen's full-surface page tone out of every rounded-rect probe
            // in `tests/`, where "the largest rounded rect" is how a suite
            // identifies real chrome (see `tests/navigation_motion.rs`'s
            // `leaving_page_x`).
            if radius == 0.0 {
                scene.fill_rect(fill_origin, fill_size, self.fill);
            } else {
                scene.fill_rounded_rect(fill_origin, fill_size, radius, self.fill);
            }
        }
        if let Some((color, style)) = self.border {
            let radius = effective_radius(self.radius, size);
            match style {
                BorderStyle::Solid => stroke_border(scene, origin, size, radius, color),
                BorderStyle::Dashed => stroke_dashed_border(scene, origin, size, radius, color),
            }
        }
        if let Some(rule) = self.bottom_rule {
            let y = origin.y + size.height - HAIRLINE / 2.0;
            scene.stroke_line(
                Point::new(origin.x, y),
                Point::new(origin.x + size.width, y),
                HAIRLINE,
                rule,
            );
        }
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // A decorative single-child container still MUST route, or everything
        // it backs is silently unreachable (the same rule both sibling panels
        // document): a row's gesture detector sits under one of these.
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // A transparent container still MUST forward, or the child's whole
        // subtree drops out of the accessibility tree.
        self.child.semantics_child(ctx);
    }
}

/// Stroke a solid hairline border just inside `origin`/`size`.
///
/// Inset by half the stroke width so the hairline paints fully inside the
/// box's own bounds (a stroke is centered on its path) — the `frust-widgets`
/// `Button` precedent.
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

/// Stroke the same border as a dash run along each straight edge.
///
/// The corner arcs are left open (dashing a curve needs the pattern support
/// `PaintScene` doesn't have): at a 4px period an open corner reads as one more
/// gap. See the [module docs](self)'s gap note.
fn stroke_dashed_border(
    scene: &mut dyn PaintScene,
    origin: Point,
    size: Size,
    radius: f64,
    color: Color,
) {
    let half = HAIRLINE / 2.0;
    let (left, top) = (origin.x + half, origin.y + half);
    let (right, bottom) = (origin.x + size.width - half, origin.y + size.height - half);
    for y in [top, bottom] {
        dash_run(scene, left + radius, right - radius, color, |x| {
            (Point::new(x.0, y), Point::new(x.1, y))
        });
    }
    for x in [left, right] {
        dash_run(scene, top + radius, bottom - radius, color, |y| {
            (Point::new(x, y.0), Point::new(x, y.1))
        });
    }
}

/// Stroke `from`→`to` as a dashed run, `segment` mapping each `(start, end)`
/// scalar pair onto the two endpoints of one dash.
fn dash_run(
    scene: &mut dyn PaintScene,
    from: f64,
    to: f64,
    color: Color,
    segment: impl Fn((f64, f64)) -> (Point, Point),
) {
    let mut at = from;
    while at < to {
        let end = (at + DASH_ON).min(to);
        let (p0, p1) = segment((at, end));
        scene.stroke_line(p0, p1, HAIRLINE, color);
        at = end + DASH_OFF;
    }
}

// ---------------------------------------------------------------------------
// Switch semantics — the one node a composed toggle cannot grow itself
// ---------------------------------------------------------------------------

/// Give `child` — a hand-composed toggle's art plus its press recognizer — the
/// one accessibility node it cannot grow itself: a labelled, toggled
/// [`Role::Switch`] advertising the [`Action::Click`] it fires.
///
/// Layout, paint and event routing are pure pass-through; only
/// [`Widget::semantics`] does anything. See the [module docs](self).
pub(super) fn switch_semantics<State: 'static, V: View<State>>(
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
pub(super) struct SwitchSemanticsView<State: 'static> {
    child: AnyView<State>,
    label: String,
    on: bool,
}

/// The retained half of [`switch_semantics`].
pub(super) struct SwitchSemanticsWidget {
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
        // A leaf node, deliberately: the composed art underneath has nothing
        // to announce, and exposing two unlabelled boxes inside a switch would
        // be noise rather than detail.
        let (label, on) = (self.label.as_str(), self.on);
        ctx.push_node(Role::Switch, |node| {
            node.set_label(label);
            node.set_toggled(if on { Toggled::True } else { Toggled::False });
            node.add_action(Action::Click);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A recording scene that counts stroked lines and remembers filled rects.
    #[derive(Default)]
    struct RecScene {
        lines: Vec<(Point, Point)>,
        rounded: Vec<(Point, Size, f64)>,
    }

    impl PaintScene for RecScene {
        fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
        fn fill_rounded_rect(&mut self, origin: Point, size: Size, radius: f64, _color: Color) {
            self.rounded.push((origin, size, radius));
        }
        fn stroke_line(&mut self, p0: Point, p1: Point, _width: f64, _color: Color) {
            self.lines.push((p0, p1));
        }
        fn draw_text(&mut self, _origin: Point, _text: &str) {}
    }

    /// The smallest possible leaf — a widget that lays out to `size` and
    /// paints nothing, so a paint test measures the panel's own chrome rather
    /// than whatever it wraps.
    struct StandIn(Size);

    impl Widget for StandIn {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            bc.constrain(self.0)
        }
        fn paint(&mut self, _ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {}
    }

    /// A panel around a zero-sized stand-in.
    fn empty_panel(bleed: f64, fill: Color) -> PanelWidget {
        PanelWidget {
            child: ChildPod::new(Box::new(StandIn(Size::ZERO))),
            fill,
            radius: 6.0,
            border: None,
            bottom_rule: None,
            bleed,
            fixed: None,
        }
    }

    #[test]
    fn a_full_radius_collapses_to_a_pill_rather_than_overshooting() {
        // `.token-scope{border-radius:var(--radius-full)}` on an 18px chip.
        let chip = Size::new(60.0, 18.0);
        assert_eq!(effective_radius(RADIUS_FULL, chip), 9.0);
        // …while an ordinary radius is left alone.
        assert_eq!(effective_radius(6.0, Size::new(300.0, 40.0)), 6.0);
    }

    #[test]
    fn a_dash_run_alternates_on_and_off_and_never_overshoots() {
        let mut scene = RecScene::default();
        dash_run(&mut scene, 0.0, 20.0, Color::WHITE, |(a, b)| {
            (Point::new(a, 0.0), Point::new(b, 0.0))
        });
        assert_eq!(scene.lines.len(), 3, "20px / 8px period: {:?}", scene.lines);
        assert_eq!(scene.lines[0], (Point::new(0.0, 0.0), Point::new(4.0, 0.0)));
        assert_eq!(
            scene.lines[1],
            (Point::new(8.0, 0.0), Point::new(12.0, 0.0))
        );
        // The last dash is clipped to the edge rather than running past it.
        assert_eq!(
            scene.lines[2],
            (Point::new(16.0, 0.0), Point::new(20.0, 0.0))
        );
    }

    #[test]
    fn a_bleeding_panel_paints_wider_than_it_measures() {
        // `.pick-row.sel{margin:0 -20px}` — the fill escapes the sheet's
        // content gutter on both sides while layout stays put.
        let mut widget = empty_panel(20.0, Color::from_rgb8(0xff, 0xb6, 0x27));
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::new(20.0, 100.0), Size::new(300.0, 44.0));
        widget.paint(&mut ctx, &mut scene);

        assert_eq!(scene.rounded.len(), 1, "one fill: {:?}", scene.rounded);
        assert_eq!(scene.rounded[0].0, Point::new(0.0, 100.0), "20px left");
        assert_eq!(scene.rounded[0].1, Size::new(340.0, 44.0), "…and right");
    }

    #[test]
    fn an_unbled_panel_paints_exactly_its_own_box() {
        let mut widget = empty_panel(0.0, Color::from_rgb8(0x1e, 0x23, 0x30));
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::new(20.0, 100.0), Size::new(300.0, 44.0));
        widget.paint(&mut ctx, &mut scene);

        assert_eq!(scene.rounded[0].0, Point::new(20.0, 100.0));
        assert_eq!(scene.rounded[0].1, Size::new(300.0, 44.0));
    }

    #[test]
    fn a_transparent_panel_paints_no_fill_at_all() {
        // Every `.set-row` is one of these: transparent, with only its bottom
        // hairline. A fill would show as a seam over the page tone.
        let mut widget = empty_panel(0.0, Color::TRANSPARENT);
        widget.bottom_rule = Some(Color::WHITE);
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::ZERO, Size::new(300.0, 44.0));
        widget.paint(&mut ctx, &mut scene);

        assert!(scene.rounded.is_empty(), "{:?}", scene.rounded);
        assert_eq!(scene.lines.len(), 1, "just the rule");
        assert_eq!(scene.lines[0].0.y, scene.lines[0].1.y, "…and it is level");
    }

    #[test]
    fn a_fixed_panel_takes_its_declared_size_and_centers_what_it_wraps() {
        // `.set-ic` — a 14px vector centered in a 28px tile, not pinned to its
        // corner (which is what a `SizedBox` would give).
        let mut widget = PanelWidget {
            child: ChildPod::new(Box::new(StandIn(Size::new(14.0, 14.0)))),
            fill: Color::from_rgb8(0x1e, 0x23, 0x30),
            radius: 4.0,
            border: None,
            bottom_rule: None,
            bleed: 0.0,
            fixed: Some(Size::new(28.0, 28.0)),
        };
        let mut ctx = LayoutCtx::new();
        let size = widget.layout(&mut ctx, &BoxConstraints::loose(Size::new(340.0, 650.0)));

        assert_eq!(size, Size::new(28.0, 28.0));
        assert_eq!(widget.child.origin(), Point::new(7.0, 7.0), "centered");
    }
}
