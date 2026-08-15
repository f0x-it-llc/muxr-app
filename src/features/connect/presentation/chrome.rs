//! The Connect screen's hand-rolled primitives: a bordered, rounded, filled
//! **panel** (single-child), a fixed-size **swatch** (leaf, optionally
//! glowing), a **centered paragraph**, and the **control-semantics** wrapper
//! behind [`switch_semantics`]/[`radio_semantics`]. No facade widget paints
//! an arbitrary filled/bordered rect, so these [`View`]/[`Widget`] pairs —
//! authored through [`frust::authoring`], the same seam `examples/huddle`'s
//! `ui::fill_box` uses — fill the gap.
//!
//! [`switch_semantics`]/[`radio_semantics`] close an accessibility gap over
//! whatever art or widget they wrap. [`radio_semantics`] closes a total hole:
//! a trust row assembled from swatches and a `GestureDetector` contributes
//! **no** accessibility node at all — delete it the day `frust::glyph` grows
//! a real radio with a themeable label. [`switch_semantics`] closes a
//! narrower one: the remember toggle wraps the real `frust::switch`, which
//! contributes its own `Toggled`/`Action::Click` node but no label
//! (`SwitchWidget` takes none) — delete it the day `switch` grows one.
//! [`centered_paragraph`] works around a different gap: `TextContext::layout`
//! hardcodes `parley::Alignment::Start`, so a wrapped run's second line
//! always hugs the leading edge; this line-breaks and centers each line
//! itself, and can go the day `text` grows an alignment.
//!
//! **Feature-local on purpose**, knowingly parallel with the other features'
//! own primitive modules: they live beside the pieces that need them rather
//! than in `crate::ui`, promoted to the shared layer only once a second
//! feature needs one, same as `examples/huddle`'s `ui::fill_box` precedent.
//! Each takes already-resolved [`Color`]s/[`TextStyle`]s — their callers
//! ([`super::widgets`]) resolve every one from the active [`frust::Theme`]
//! once per build, so these stay dumb about theming.

use frust::authoring::text::{TextContext, TextLayout, TextStyle};
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

/// Stroke a `width`-thick border just inside `origin`/`size`.
///
/// Inset by half the stroke width so the border paints fully inside the
/// widget's own bounds (a stroke is centered on its path) — the
/// `frust-widgets` `Button` precedent, re-derived here because that helper is
/// crate-private. `width` is [`HAIRLINE`] for every rule in the mock but the
/// `.radio-dot` ring, which asks for 1.5px.
fn stroke_border(
    scene: &mut dyn PaintScene,
    origin: Point,
    size: Size,
    radius: f64,
    color: Color,
    width: f64,
) {
    let half = width / 2.0;
    let rr = RoundedRect::new(
        half,
        half,
        size.width - half,
        size.height - half,
        (radius - half).max(0.0),
    );
    let path: BezPath = rr.to_path(BORDER_TOLERANCE);
    scene.stroke_path(origin, &path, width, &Brush::Solid(color));
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
            stroke_border(scene, origin, size, self.radius, border, HAIRLINE);
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
    border_width: f64,
    glow: Option<Color>,
}

/// A `size`-sized box filled with `fill`, corner-rounded by `radius`.
pub fn swatch(size: Size, fill: Color, radius: f64) -> SwatchView {
    SwatchView {
        size,
        fill,
        radius,
        border: None,
        border_width: HAIRLINE,
        glow: None,
    }
}

impl SwatchView {
    /// Stroke a hairline border in `color` just inside the swatch's bounds.
    pub fn border(mut self, color: Color) -> Self {
        self.border = Some(color);
        self
    }

    /// Stroke this swatch's border at `width` rather than [`HAIRLINE`] —
    /// `.radio-dot`'s `border:1.5px`, the mock's one non-hairline rule.
    pub fn border_width(mut self, width: f64) -> Self {
        self.border_width = width;
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
    border_width: f64,
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
            border_width: self.border_width,
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
            || prev.border_width != self.border_width
            || prev.glow != self.glow
        {
            element.fill = self.fill;
            element.radius = self.radius;
            element.border = self.border;
            element.border_width = self.border_width;
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
            stroke_border(scene, origin, size, self.radius, border, self.border_width);
        }
    }
}

// ---------------------------------------------------------------------------
// Control semantics — an accessibility node over hand-composed control art
// ---------------------------------------------------------------------------

/// Which control a [`ControlSemanticsView`] stands in for, plus that control's
/// own state. One wrapper serves both because the switch and the trust rows
/// differ *only* in the role/state pair they announce — everything else
/// (labelled leaf node, `Click` action, pass-through layout/paint/events) is
/// identical.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ControlKind {
    /// A `Role::Switch` reading on/off — the remember toggle.
    Switch(bool),
    /// A `Role::RadioButton` reading selected/unselected — a trust row.
    Radio(bool),
}

/// Give `child` — today, the real `frust::switch` the remember toggle wraps
/// (`super::widgets::trust::remember_row`) — the one accessibility detail it cannot
/// grow itself: a **labelled**, toggled [`Role::Switch`] advertising the
/// [`Action::Click`] it fires.
///
/// Layout, paint and event routing are pure pass-through; only
/// [`Widget::semantics`] does anything, and it fully replaces whatever node
/// `child` would have contributed on its own (never both) — see the
/// [module docs](self).
pub fn switch_semantics<State: 'static, V: View<State>>(
    child: V,
    label: impl Into<String>,
    on: bool,
) -> ControlSemanticsView<State> {
    ControlSemanticsView {
        child: any(child),
        label: label.into(),
        kind: ControlKind::Switch(on),
    }
}

/// [`switch_semantics`]' sibling for a hand-composed radio row: a labelled
/// [`Role::RadioButton`] carrying `selected` and the [`Action::Click`] the
/// row's own recognizer fires.
///
/// The trust selector hand-composes its rows (the stock `frust::radio` styles
/// its label with `TextStyle::default()` — 16px, no theme role — which the
/// design's 12px `--fg` row cannot be reached from), so the rows would
/// otherwise contribute no accessibility node at all. See the
/// [module docs](self).
pub fn radio_semantics<State: 'static, V: View<State>>(
    child: V,
    label: impl Into<String>,
    selected: bool,
) -> ControlSemanticsView<State> {
    ControlSemanticsView {
        child: any(child),
        label: label.into(),
        kind: ControlKind::Radio(selected),
    }
}

/// The declarative half of [`switch_semantics`]/[`radio_semantics`].
pub struct ControlSemanticsView<State: 'static> {
    child: AnyView<State>,
    label: String,
    kind: ControlKind,
}

/// The retained half of [`switch_semantics`]/[`radio_semantics`].
pub struct ControlSemanticsWidget {
    child: ChildPod,
    label: String,
    kind: ControlKind,
}

impl<State: 'static> View<State> for ControlSemanticsView<State> {
    type Element = ControlSemanticsWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> ControlSemanticsWidget {
        ControlSemanticsWidget {
            child: build_child(&self.child, ctx),
            label: self.label.clone(),
            kind: self.kind,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut ControlSemanticsWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.label != self.label || prev.kind != self.kind {
            element.label = self.label.clone();
            element.kind = self.kind;
            // Semantics-only: the accessibility tree is rebuilt off a paint
            // pass, so PAINT is what republishes the node.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut ControlSemanticsWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for ControlSemanticsWidget {
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
        // announce, and exposing four unlabelled swatches inside a switch (or a
        // ring + a label run inside a radio) would be noise rather than detail.
        let (role, label) = (self.kind.role(), self.label.as_str());
        ctx.push_node(role, |node| {
            node.set_label(label);
            match self.kind {
                ControlKind::Switch(on) => {
                    node.set_toggled(if on { Toggled::True } else { Toggled::False })
                }
                ControlKind::Radio(selected) => node.set_selected(selected),
            }
            node.add_action(Action::Click);
        });
    }
}

impl ControlKind {
    /// The accessibility role this control announces.
    fn role(self) -> Role {
        match self {
            ControlKind::Switch(_) => Role::Switch,
            ControlKind::Radio(_) => Role::RadioButton,
        }
    }
}

// ---------------------------------------------------------------------------
// Centered paragraph — per-line centering `text` cannot express
// ---------------------------------------------------------------------------

/// A block of text whose every *laid-out line* is centered in the width it is
/// given — `.sub`'s `text-align:center` on a paragraph that wraps.
///
/// # Why this exists
///
/// `TextView` exposes no paragraph alignment, and `TextContext::layout`
/// hardcodes `parley::layout::Alignment::Start` for every run in the
/// framework. So a wrapped run is always start-aligned *inside its own block*,
/// and `Align(CENTER, text(..))` can only center that block as a whole: the
/// first line looks centered, every line after it hugs the leading edge. That
/// is the defect this closes, and it cannot be closed from composition —
/// hence a widget. **Delete it the day `text` grows an alignment**, and note
/// the gap is a framework one, not an app decision.
///
/// # How
///
/// Greedy word wrap against measured widths (the same algorithm parley runs,
/// re-run here because the per-line boxes are what has to move), then one
/// centered draw per line. Re-wrapped only when the width, the style or the
/// content changes — the same cache shape `TextWidget`/`glyph::badge`'s own
/// label uses.
pub fn centered_paragraph(content: impl Into<String>, style: TextStyle) -> ParagraphView {
    ParagraphView {
        content: content.into(),
        style,
    }
}

/// The declarative half of [`centered_paragraph`].
pub struct ParagraphView {
    content: String,
    style: TextStyle,
}

/// The retained half of [`centered_paragraph`].
pub struct ParagraphWidget {
    content: String,
    style: TextStyle,
    /// One shaped layout per wrapped line, in order. Empty until the first
    /// layout pass.
    lines: Vec<TextLayout>,
    /// The style [`Self::lines`] were wrapped/shaped at (a color-only change
    /// re-shapes too — the layout-time-baked-color contract).
    laid_out_style: Option<TextStyle>,
    /// The width [`Self::lines`] were wrapped at; `None` means unbounded (one
    /// line, no wrapping).
    laid_out_max_width: Option<f32>,
}

impl<State: 'static> View<State> for ParagraphView {
    type Element = ParagraphWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> ParagraphWidget {
        ParagraphWidget {
            content: self.content.clone(),
            style: self.style.clone(),
            lines: Vec::new(),
            laid_out_style: None,
            laid_out_max_width: None,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut ParagraphWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = ChangeFlags::NONE;
        if prev.content != self.content || prev.style != self.style {
            element.content = self.content.clone();
            element.style = self.style.clone();
            // Invalidate the wrap: `laid_out_style` is what the layout pass
            // compares against, so clearing it forces a re-wrap.
            element.laid_out_style = None;
            flags |= ChangeFlags::LAYOUT;
        }
        flags
    }
}

impl Widget for ParagraphWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let max_width = bc.max().width.is_finite().then(|| bc.max().width as f32);
        if self.laid_out_max_width != max_width || self.laid_out_style.as_ref() != Some(&self.style)
        {
            let text_ctx = ctx.text_context::<TextContext>();
            self.lines = wrap_lines(text_ctx, &self.content, &self.style, max_width);
            self.laid_out_max_width = max_width;
            self.laid_out_style = Some(self.style.clone());
        }
        let height: f64 = self.lines.iter().map(|line| line.size().height).sum();
        let widest = self
            .lines
            .iter()
            .map(|line| line.size().width)
            .fold(0.0_f64, f64::max);
        // A bounded width is the box the lines center *within*, so take all of
        // it; an unbounded one shrink-wraps to the widest line.
        let width = max_width.map_or(widest, f64::from);
        bc.constrain(Size::new(width, height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let width = ctx.size().width;
        let mut y = origin.y;
        for line in &self.lines {
            let line_size = line.size();
            let x = origin.x + ((width - line_size.width) / 2.0).max(0.0);
            for run in line.to_scene_runs(Point::new(x, y)) {
                scene.draw_glyph_run(run);
            }
            y += line_size.height;
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // The same node a `TextWidget` contributes for the same content: a
        // `Role::Label` carrying its run in `value` (accesskit guidance), so
        // the wrap is invisible to a screen reader.
        ctx.push_node(Role::Label, |node| {
            node.set_value(self.content.as_str());
        });
    }
}

/// Greedily break `content` into lines no wider than `max_width`, each shaped
/// with `style`, in order.
///
/// Breaks on whitespace only: a single word wider than the line overflows
/// rather than being split mid-word, which is parley's own behavior at these
/// sizes and is not reachable here anyway (the one paragraph this carries is
/// ordinary prose). An unbounded `max_width` yields exactly one line.
fn wrap_lines(
    text_ctx: &mut TextContext,
    content: &str,
    style: &TextStyle,
    max_width: Option<f32>,
) -> Vec<TextLayout> {
    let Some(max_width) = max_width else {
        return vec![text_ctx.layout(content, style, None)];
    };
    let mut lines = Vec::new();
    let mut current: Option<(String, TextLayout)> = None;
    for word in content.split_whitespace() {
        let candidate = match &current {
            Some((line, _)) => format!("{line} {word}"),
            None => word.to_string(),
        };
        // Measured unbounded on purpose: this asks "how wide is this line",
        // and a bounded layout would answer with the wrapped block's width.
        let laid = text_ctx.layout(&candidate, style, None);
        if laid.size().width > f64::from(max_width) && current.is_some() {
            let (_, full) = current.take().expect("checked is_some");
            lines.push(full);
            current = Some((word.to_string(), text_ctx.layout(word, style, None)));
        } else {
            current = Some((candidate, laid));
        }
    }
    if let Some((_, last)) = current {
        lines.push(last);
    }
    lines
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
            border_width: HAIRLINE,
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
            border_width: HAIRLINE,
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
            border_width: HAIRLINE,
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

    // -----------------------------------------------------------------------
    // Centered paragraph
    // -----------------------------------------------------------------------

    /// The subtitle's own copy, at the caption role's size.
    const PROSE: &str = "Enter credentials to attach to a remote zellij or herdr session.";

    #[test]
    fn a_paragraph_wraps_greedily_and_every_line_fits_its_width() {
        let mut cx = TextContext::new();
        let style = TextStyle::new(11.0, Color::WHITE);
        let max_width = 120.0_f32;
        let lines = wrap_lines(&mut cx, PROSE, &style, Some(max_width));

        assert!(
            lines.len() > 1,
            "a 64-character run must wrap at {max_width}px"
        );
        for (index, line) in lines.iter().enumerate() {
            // A wrapped line may exceed the width only when it is a single
            // unbreakable word — never the case in this prose.
            assert!(
                line.size().width <= f64::from(max_width),
                "line {index} is {:.1}px wide, past the {max_width}px box",
                line.size().width
            );
        }
    }

    #[test]
    fn an_unbounded_paragraph_stays_one_line() {
        // The shrink-wrap case (no finite width to wrap against): one line, so
        // nothing is silently broken where there is no box to break to.
        let mut cx = TextContext::new();
        let style = TextStyle::new(11.0, Color::WHITE);
        assert_eq!(wrap_lines(&mut cx, PROSE, &style, None).len(), 1);
    }
}
