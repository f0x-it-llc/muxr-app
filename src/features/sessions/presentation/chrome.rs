//! The Sessions screen's hand-rolled primitives: a filled/bordered **panel**
//! (single-child, with the card's left status accent and the `conn-row`'s
//! bottom rule as options), a fixed-size **swatch** leaf (the strip's success
//! dot, glow included), and the **dashed box** the "+ new session" row needs.
//!
//! `docs/redesign.html`'s sessions block is mostly boxes and hairlines —
//! `.sess-card`'s surface + `border-bright` + 2px `border-left`, `.pane-pill`'s
//! `bg-base` tile, `.conn-row`'s `border-bottom`, `.dot-ok`'s glowing dot, and
//! `.new-sess-btn`'s dashed outline. No facade widget paints an arbitrary
//! filled/bordered rect, so these [`View`]/[`Widget`] pairs — authored through
//! [`frust::authoring`], the same seam `crate::ui::backdrop`/`crate::ui::sheet`
//! use — fill the gap.
//!
//! **Feature-local, and knowingly parallel to `features::connect`'s own
//! `chrome`.** The two screens each own a panel/swatch pair rather than sharing
//! one, because a feature never imports a sibling's `presentation`
//! (`docs/ARCHITECTURE.md`'s cross-feature rule) and promoting Connect's copy
//! into `crate::ui` would mean editing that screen — a separate task's file.
//! When Connect's own redesign sweep lands, the two should be promoted together
//! (`examples/huddle` promoted its `fill_box` to the shared layer on exactly
//! this trigger: the second consumer).
//!
//! Like Connect's, every one of these takes already-resolved [`Color`]s:
//! [`super::pieces`] resolves each from the active [`frust::Theme`] once per
//! build, so these stay dumb about theming.
//!
//! # Frust gaps these work around
//!
//! - **No dash pattern on `PaintScene`.** `stroke_path`/`stroke_line` take a
//!   plain width; `kurbo::Stroke`'s dash array never reaches the scene. So
//!   [`DashedBoxWidget`] walks each edge itself in on/off steps — see its own
//!   note, and **delete it the day frust grows dash support**.
//! - **No per-corner radius.** The card's `border-left` accent is therefore
//!   painted as an accent-colored rounded rect the surface fill sits 2px
//!   inside of, which reproduces the source's curved accent without one.

use frust::authoring::{
    AnyView, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx,
    EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, RoundedRect, SemanticsCtx,
    Shape, Size, View, Widget, any, build_child, rebuild_child, route_event_single, teardown_child,
};

/// Hairline border width in logical px (`1px solid` on every bordered rule in
/// `docs/redesign.html`). Matches `frust-widgets`' own `BORDER_WIDTH`.
pub(super) const HAIRLINE: f64 = 1.0;
/// `.sess-card{border-left:2px solid}` — the status accent's weight.
pub(super) const ACCENT_WIDTH: f64 = 2.0;
/// `--radius-full: 999px` — clamped to half the shorter side at paint time, so
/// a caller can spell "pill" without knowing the box's measured height.
pub(super) const RADIUS_FULL: f64 = 999.0;
/// Flattening tolerance for the rounded-rect border path — the value
/// `frust-widgets`' `Button`/`material::card` border stroke uses.
const BORDER_TOLERANCE: f64 = 0.1;
/// `.new-sess-btn{border:1px dashed}` — the on/off step of the hand-drawn dash,
/// in logical px. CSS leaves `dashed`'s period to the UA; 4/4 is the common
/// rendering and reads correctly at this scale.
const DASH_ON: f64 = 4.0;
const DASH_OFF: f64 = 4.0;
/// `.dot-ok{box-shadow:0 0 5px var(--success)}` — the blur radius, as a
/// gaussian std-dev for [`PaintScene::draw_shadow`], plus how far the glow
/// bleeds past the dot on each side.
const GLOW_STD_DEV: f64 = 2.5;
const GLOW_SPREAD: f64 = 2.0;

/// The effective corner radius for a box of `size` — [`RADIUS_FULL`] (or any
/// over-large value) collapses to a true pill rather than overshooting.
fn effective_radius(radius: f64, size: Size) -> f64 {
    radius.min(size.width / 2.0).min(size.height / 2.0).max(0.0)
}

/// Stroke a hairline border just inside `origin`/`size`.
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

// ---------------------------------------------------------------------------
// Panel — a filled, optionally bordered/accented box behind one child
// ---------------------------------------------------------------------------

/// A rounded fill (plus optional hairline border, left status accent and
/// bottom rule) painted *behind* a child it sizes itself to: `.sess-card`,
/// `.pane-pill`, `.conn-row`, and the screen's own `--bg-base` page tone.
pub(super) struct PanelView<State: 'static> {
    child: AnyView<State>,
    fill: Color,
    radius: f64,
    border: Option<Color>,
    accent: Option<Color>,
    /// Whether the left accent's width is reserved even when it paints
    /// nothing — see [`PanelView::left_accent`].
    accent_reserved: bool,
    bottom_rule: Option<Color>,
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
        accent: None,
        accent_reserved: false,
        bottom_rule: None,
    }
}

impl<State: 'static> PanelView<State> {
    /// Stroke a hairline border in `color` just inside the panel's bounds.
    pub(super) fn border(mut self, color: Color) -> Self {
        self.border = Some(color);
        self
    }

    /// Give the panel `.sess-card`'s [`ACCENT_WIDTH`]-wide left status bar,
    /// painted in `color`.
    ///
    /// Calling this **reserves** the accent's width whether or not there is a
    /// colour to paint there — the source's `border-left:2px solid transparent`
    /// — so an idle card's surface starts exactly where a running card's does
    /// and the list's left edge never jogs by 2px from row to row. A panel that
    /// never calls this (the pane pills, the strip) reserves nothing.
    pub(super) fn left_accent(mut self, color: Option<Color>) -> Self {
        self.accent = color;
        self.accent_reserved = true;
        self
    }

    /// Paint a `.conn-row`-style hairline along the panel's bottom edge.
    pub(super) fn bottom_rule(mut self, color: Color) -> Self {
        self.bottom_rule = Some(color);
        self
    }
}

/// The retained half of a [`PanelView`].
pub(super) struct PanelWidget {
    child: ChildPod,
    fill: Color,
    radius: f64,
    border: Option<Color>,
    accent: Option<Color>,
    accent_reserved: bool,
    bottom_rule: Option<Color>,
}

impl<State: 'static> View<State> for PanelView<State> {
    type Element = PanelWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> PanelWidget {
        PanelWidget {
            child: build_child(&self.child, ctx),
            fill: self.fill,
            radius: self.radius,
            border: self.border,
            accent: self.accent,
            accent_reserved: self.accent_reserved,
            bottom_rule: self.bottom_rule,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut PanelWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.fill != self.fill
            || prev.radius != self.radius
            || prev.border != self.border
            || prev.accent != self.accent
            || prev.accent_reserved != self.accent_reserved
            || prev.bottom_rule != self.bottom_rule
        {
            element.fill = self.fill;
            element.radius = self.radius;
            element.border = self.border;
            element.accent = self.accent;
            element.accent_reserved = self.accent_reserved;
            element.bottom_rule = self.bottom_rule;
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
        let radius = effective_radius(self.radius, size);
        // The accent is the *outermost* layer, with the surface fill inset by
        // its width — `PaintScene` has no per-corner radius, and this
        // reproduces `border-left`'s curved ends without one (module docs).
        // The inset is reserved whenever the panel *can* carry an accent, so an
        // accentless card lines its content up with an accented one's.
        let inset = if self.accent_reserved {
            ACCENT_WIDTH
        } else {
            0.0
        };
        if let Some(accent) = self.accent {
            scene.fill_rounded_rect(origin, size, radius, accent);
        }
        let body_origin = Point::new(origin.x + inset, origin.y);
        let body_size = Size::new((size.width - inset).max(0.0), size.height);
        if self.fill != Color::TRANSPARENT {
            scene.fill_rounded_rect(body_origin, body_size, radius, self.fill);
        }
        if let Some(border) = self.border {
            stroke_border(scene, body_origin, body_size, radius, border);
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
        // it backs is silently unreachable (`features::connect`'s `panel`
        // documents the same rule): the card's gesture detector sits under
        // one of these.
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // A transparent container still MUST forward, or the child's whole
        // subtree drops out of the accessibility tree.
        self.child.semantics_child(ctx);
    }
}

// ---------------------------------------------------------------------------
// Swatch — a fixed-size filled leaf, optionally glowing
// ---------------------------------------------------------------------------

/// A fixed-size filled rounded rect: `.dot-ok`, the strip's 7px success dot. A
/// circle is a swatch whose `radius` is half its edge (or just
/// [`RADIUS_FULL`]).
pub(super) struct SwatchView {
    size: Size,
    fill: Color,
    radius: f64,
    glow: Option<Color>,
}

/// A `size`-sized box filled with `fill`, corner-rounded by `radius`.
pub(super) fn swatch(size: Size, fill: Color, radius: f64) -> SwatchView {
    SwatchView {
        size,
        fill,
        radius,
        glow: None,
    }
}

impl SwatchView {
    /// Bloom a soft `color` halo behind the swatch (`.dot-ok`'s
    /// `box-shadow:0 0 5px var(--success)`), through
    /// [`PaintScene::draw_shadow`].
    pub(super) fn glow(mut self, color: Color) -> Self {
        self.glow = Some(color);
        self
    }
}

/// The retained half of a [`SwatchView`].
pub(super) struct SwatchWidget {
    size: Size,
    fill: Color,
    radius: f64,
    glow: Option<Color>,
}

impl<State: 'static> View<State> for SwatchView {
    type Element = SwatchWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> SwatchWidget {
        SwatchWidget {
            size: self.size,
            fill: self.fill,
            radius: self.radius,
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
        if prev.fill != self.fill || prev.radius != self.radius || prev.glow != self.glow {
            element.fill = self.fill;
            element.radius = self.radius;
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
        let radius = effective_radius(self.radius, size);
        if let Some(glow) = self.glow {
            let spread = Size::new(
                size.width + 2.0 * GLOW_SPREAD,
                size.height + 2.0 * GLOW_SPREAD,
            );
            scene.draw_shadow(
                Point::new(origin.x - GLOW_SPREAD, origin.y - GLOW_SPREAD),
                spread,
                effective_radius(self.radius, spread),
                GLOW_STD_DEV,
                glow,
            );
        }
        if self.fill != Color::TRANSPARENT {
            scene.fill_rounded_rect(origin, size, radius, self.fill);
        }
    }
}

// ---------------------------------------------------------------------------
// Dashed box — `.new-sess-btn`
// ---------------------------------------------------------------------------

/// A dashed rounded outline around one child — `.new-sess-btn`.
///
/// **Hand-drawn, and meant to be deleted.** `PaintScene` carries no dash
/// pattern (a known, already-filed frust gap), so this walks the four straight
/// edges in [`DASH_ON`]/[`DASH_OFF`] steps with [`PaintScene::stroke_line`] —
/// the same workaround `frust::glyph::empty_state` makes for its own dashed
/// panel (`stroke_dashed_rect`), which is the framework's own precedent that
/// this is the available shape rather than a shortcut. The corner arcs are
/// left open (the catalog's version has no radius to worry about): dashing a
/// curve needs the path walking the missing pattern support would give us, and
/// at a 4px period an open corner reads as one more gap. **Replace the whole
/// widget with a dashed `Stroke` the day frust grows dash support.**
pub(super) struct DashedBoxView<State: 'static> {
    child: AnyView<State>,
    color: Color,
    radius: f64,
}

/// Wrap `child` in a dashed `color` outline with corner `radius`.
pub(super) fn dashed_box<State: 'static, V: View<State>>(
    child: V,
    color: Color,
    radius: f64,
) -> DashedBoxView<State> {
    DashedBoxView {
        child: any(child),
        color,
        radius,
    }
}

/// The retained half of a [`DashedBoxView`].
pub(super) struct DashedBoxWidget {
    child: ChildPod,
    color: Color,
    radius: f64,
}

impl<State: 'static> View<State> for DashedBoxView<State> {
    type Element = DashedBoxWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> DashedBoxWidget {
        DashedBoxWidget {
            child: build_child(&self.child, ctx),
            color: self.color,
            radius: self.radius,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut DashedBoxWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.color != self.color || prev.radius != self.radius {
            element.color = self.color;
            element.radius = self.radius;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut DashedBoxWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for DashedBoxWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        let radius = effective_radius(self.radius, size);
        let half = HAIRLINE / 2.0;
        let (left, top) = (origin.x + half, origin.y + half);
        let (right, bottom) = (origin.x + size.width - half, origin.y + size.height - half);
        // Horizontal edges, then vertical ones — each inset by the corner
        // radius, since the arcs themselves stay undashed (see the type docs).
        for y in [top, bottom] {
            dash_run(scene, left + radius, right - radius, self.color, |x| {
                (Point::new(x.0, y), Point::new(x.1, y))
            });
        }
        for x in [left, right] {
            dash_run(scene, top + radius, bottom - radius, self.color, |y| {
                (Point::new(x, y.0), Point::new(x, y.1))
            });
        }
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
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

    #[test]
    fn a_full_radius_collapses_to_a_pill_rather_than_overshooting() {
        let chip = Size::new(60.0, 18.0);
        assert_eq!(effective_radius(RADIUS_FULL, chip), 9.0);
        // …while an ordinary radius is left alone.
        assert_eq!(effective_radius(10.0, Size::new(300.0, 80.0)), 10.0);
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
    fn a_swatch_takes_its_declared_size_and_clamps_to_the_incoming_maximum() {
        let mut widget = SwatchWidget {
            size: Size::new(7.0, 7.0),
            fill: Color::WHITE,
            radius: RADIUS_FULL,
            glow: None,
        };
        let mut ctx = LayoutCtx::new();
        assert_eq!(
            widget.layout(&mut ctx, &BoxConstraints::loose(Size::new(340.0, 650.0))),
            Size::new(7.0, 7.0)
        );
    }

    #[test]
    fn an_accented_panel_insets_its_surface_by_the_accent_width() {
        // The card's left accent is painted *under* the surface fill, which
        // starts 2px in — that inset is what makes the accent visible at all.
        // A zero-sized swatch stands in for the card's content: this test is
        // about the panel's own two fills, not what it wraps.
        let stand_in = SwatchWidget {
            size: Size::ZERO,
            fill: Color::TRANSPARENT,
            radius: 0.0,
            glow: None,
        };
        let mut widget = PanelWidget {
            child: ChildPod::new(Box::new(stand_in)),
            fill: Color::from_rgb8(0x16, 0x1a, 0x23),
            radius: 10.0,
            border: None,
            accent: Some(Color::from_rgb8(0x5f, 0xd8, 0x8f)),
            accent_reserved: true,
            bottom_rule: None,
        };
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::ZERO, Size::new(300.0, 80.0));
        widget.paint(&mut ctx, &mut scene);

        assert_eq!(scene.rounded.len(), 2, "accent then surface");
        assert_eq!(scene.rounded[0].0, Point::ZERO, "the accent spans the card");
        assert_eq!(scene.rounded[1].0, Point::new(ACCENT_WIDTH, 0.0));
        assert_eq!(
            scene.rounded[1].1,
            Size::new(300.0 - ACCENT_WIDTH, 80.0),
            "the surface fill is the card minus the accent"
        );
    }

    #[test]
    fn an_accentless_card_still_reserves_the_accents_width() {
        // `.sess-card{border-left:2px solid transparent}` — an idle card's
        // content must line up with a running one's, so the *reservation* is
        // independent of there being a colour to paint.
        let stand_in = SwatchWidget {
            size: Size::ZERO,
            fill: Color::TRANSPARENT,
            radius: 0.0,
            glow: None,
        };
        let mut widget = PanelWidget {
            child: ChildPod::new(Box::new(stand_in)),
            fill: Color::from_rgb8(0x16, 0x1a, 0x23),
            radius: 10.0,
            border: None,
            accent: None,
            accent_reserved: true,
            bottom_rule: None,
        };
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::ZERO, Size::new(300.0, 80.0));
        widget.paint(&mut ctx, &mut scene);

        assert_eq!(scene.rounded.len(), 1, "no accent is painted");
        assert_eq!(
            scene.rounded[0].0,
            Point::new(ACCENT_WIDTH, 0.0),
            "…but the surface still starts where an accented card's does"
        );
    }
}
