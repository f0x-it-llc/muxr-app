//! The Terminal screen's hand-rolled primitives — the boxes, the animated
//! terminal card, the body's own text pipeline, and the three vector marks
//! `frust::icons` does not carry.
//!
//! `docs/terminal.html` (v2) is mostly boxes, hairlines and monospace runs:
//! `.term-card`'s filled + bordered + *animating* wrap, `.term-statusrow`'s
//! band, `.pill`/`.pane-item`'s tiles (some dashed), and a `.term-body` whose
//! lines stagger in behind a blinking block caret. No facade widget paints an
//! arbitrary filled/bordered rect, a dashed outline, an ellipsized single-line
//! run, or a per-line staggered reveal, so these [`View`]/[`Widget`] pairs —
//! authored through [`frust::authoring`], the seam `crate::ui::backdrop`/
//! `crate::ui::topbar` already use — fill the gap.
//!
//! | Piece | Mock element |
//! |---|---|
//! | [`term_box`] | `.pill`, `.pane-item`, `.new-pane-btn`, `.term-statusrow`, `.sheet-close` — one filled/bordered box, optionally dashed |
//! | [`clipped_label`] | `.term-pane-title` — a single-line run, ellipsized to fit |
//! | [`status_strip`] | the card's one-line link notice and its affordance — no mock element; a live session's own |
//! | [`expand_button`] | `.expand-btn` — the 28px circle that toggles fullscreen |
//! | [`terminal_stage`] | `.term-wrap` + `.term-card` — the animated clearance/padding/radius, the card fill/border/shadow, and the bar's anchor rect |
//! | [`term_body`] | `.term-body` — typed lines, the staggered reveal, the blinking caret |
//!
//! **Feature-local, and knowingly parallel to `features::sessions`' own
//! `chrome`.** The two screens each own their box primitives rather than
//! sharing one, because a feature never imports a sibling's `presentation`
//! (`docs/ARCHITECTURE.md`'s cross-feature rule). Promoting them into
//! `crate::ui` is a job for the sweep that touches both screens at once —
//! `examples/huddle` promoted its `fill_box` on exactly that trigger.
//!
//! Like the other two screens', every one of these takes already-resolved
//! [`Color`]s/[`TextStyle`]s: [`super::pieces`] resolves each from the active
//! [`frust::Theme`] once per build, so these stay dumb about theming. The one
//! exception is [`terminal_stage`], which reads `reduce_motion` and the motion
//! tokens from the **paint-time** theme, because that is the only pass where a
//! theme is in scope when a transition actually starts (the deferred-driver
//! idiom `crate::ui::topbar` documents).
//!
//! # Frust gaps these work around
//!
//! - **No dash pattern on `PaintScene`** — `stroke_path`/`stroke_line` take a
//!   plain width. [`TermBoxWidget`] walks each edge in on/off steps, the same
//!   workaround `frust::glyph::empty_state` and `features::sessions`' own
//!   `dashed_box` make.
//! - **No text overflow/ellipsis vocabulary** — `frust::text` has no
//!   `max_lines`/`overflow` builder, so a too-long pane title would wrap
//!   instead of ellipsizing. [`ClippedLabelWidget`] measures and truncates
//!   itself.
//! - **No per-corner radius** — the card's status row would poke square
//!   corners through the card's 22px rounding, so the stage clips its child to
//!   its own rounded rect (`push_clip_rounded`) rather than compositing two
//!   radii.

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
    IconData, IconSource, Padding, Theme, flexible, icon, inflexible, text,
};

use crate::features::terminal::domain::entities::{TermLine, TermLineKind};
use crate::ui::topbar::BAR_HEIGHT;

// ---------------------------------------------------------------------------
// App-supplied vector marks (the "chrome icons are vectors" rule)
// ---------------------------------------------------------------------------

/// The side length of the square design box every mark here is authored
/// against — Material Symbols' own 24, which [`frust::icons`]' generated
/// entries all carry.
const ICON_DESIGN_BOX: f64 = 24.0;

/// `.expand-btn`'s resting mark (the mock's `⛶`): the four corner brackets of
/// Material Icons' `fullscreen`.
///
/// **Why an app-supplied source.** `frust::icons` is a 42-entry starter set
/// with no fullscreen pair at all, so the collection cannot express the mark
/// the mock asks for — and `⛶` (`U+26F6`) is absent from the bundled Glyph
/// faces, which is exactly the font-fallback trap `crate::ui::topbar`'s
/// "Chrome icons are vectors" rule exists to close (the `⚙` it replaced was
/// simply invisible on iPhone). Transcribed from the Material Icons 24px
/// `fullscreen` glyph, so its stroke weight and corner cuts match the
/// vendored set beside it. [`CHEVRON_LEFT`](crate::ui::topbar::CHEVRON_LEFT)
/// is the app's first use of this same `IconSource` seam.
pub const FULLSCREEN_EXPAND: IconSource = IconSource {
    d: "M 7 14 H 5 v 5 h 5 v -2 H 7 v -3 Z M 5 10 h 2 V 7 h 3 V 5 H 5 v 5 Z \
        M 17 17 h -3 v 2 h 5 v -5 h -2 v 3 Z M 14 5 v 2 h 3 v 3 h 2 V 5 h -5 Z",
    design: ICON_DESIGN_BOX,
};

/// `.expand-btn`'s on-state mark (the mock's `⤡`): Material Icons 24px
/// `fullscreen_exit`, the brackets turned inward. See [`FULLSCREEN_EXPAND`].
pub const FULLSCREEN_EXIT: IconSource = IconSource {
    d: "M 5 16 h 3 v 3 h 2 v -5 H 5 v 2 Z M 8 8 H 5 v 2 h 5 V 5 H 8 v 3 Z \
        M 14 19 h 2 v -3 h 3 v -2 h -5 v 5 Z M 16 8 V 5 h -2 v 5 h 5 V 8 h -3 Z",
    design: ICON_DESIGN_BOX,
};

/// The pane mark (the mock's `▤`, "square with horizontal fill"): three
/// stacked bars in the 24px box.
///
/// Drawn rather than set as text for the same reason as the pair above —
/// `▤` (`U+25A4`) is a Geometric Shapes glyph absent from IBM Plex Mono, so on
/// device it resolves through each platform's own fallback chain (or not at
/// all). Three separated bars read as the source's striped square at the 12px
/// the sheet draws it, and as a filled square at a glance. Authored here (no
/// Material Symbols counterpart is in the vendored set).
pub const PANE_MARK: IconSource = IconSource {
    d: "M 4 5 h 16 v 4 H 4 V 5 Z M 4 10 h 16 v 4 H 4 v -4 Z M 4 15 h 16 v 4 H 4 v -4 Z",
    design: ICON_DESIGN_BOX,
};

// ---------------------------------------------------------------------------
// Shared paint helpers
// ---------------------------------------------------------------------------

/// Hairline border width in logical px (`1px solid` on every bordered rule in
/// `docs/terminal.html`). Matches `frust-widgets`' own `BORDER_WIDTH`.
pub(super) const HAIRLINE: f64 = 1.0;
/// `--radius-full: 999px` — clamped to half the shorter side at paint time, so
/// a caller can spell "pill" without knowing the box's measured height.
pub(super) const RADIUS_FULL: f64 = 999.0;
/// Flattening tolerance for a rounded-rect border path — the value
/// `frust-widgets`' own border strokes use.
const BORDER_TOLERANCE: f64 = 0.1;
/// `border-style:dashed` — the on/off step of the hand-drawn dash, in logical
/// px. CSS leaves the period to the UA; 4/4 is the common rendering and the
/// value `features::sessions`' own dashed row uses.
const DASH_ON: f64 = 4.0;
const DASH_OFF: f64 = 4.0;

/// The effective corner radius for a box of `size` — [`RADIUS_FULL`] (or any
/// over-large value) collapses to a true pill rather than overshooting.
fn effective_radius(radius: f64, size: Size) -> f64 {
    radius.min(size.width / 2.0).min(size.height / 2.0).max(0.0)
}

/// Coerce a possibly-infinite constraint dimension to a finite value (mirrors
/// the helper of the same name `frust::glyph`'s own modal widgets carry).
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

/// Stroke `from`→`to` as a dashed run, `segment` mapping each `(start, end)`
/// scalar pair onto one dash's endpoints.
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

/// Whether `pos` (widget-local) falls inside a `size`-sized box.
fn inside(pos: Point, size: Size) -> bool {
    pos.x >= 0.0 && pos.y >= 0.0 && pos.x < size.width && pos.y < size.height
}

// ---------------------------------------------------------------------------
// `term_box` — a filled/bordered (optionally dashed) box behind one child
// ---------------------------------------------------------------------------

/// A rounded fill plus an optional hairline border, painted *behind* a child
/// it sizes itself to. Every tile on this screen is one of these.
pub(super) struct TermBoxView<State: 'static> {
    child: AnyView<State>,
    fill: Color,
    radius: f64,
    border: Option<Color>,
    dashed: bool,
}

/// Wrap `child` in a `fill`-filled box with corner `radius`.
pub(super) fn term_box<State: 'static, V: View<State>>(
    child: V,
    fill: Color,
    radius: f64,
) -> TermBoxView<State> {
    TermBoxView {
        child: any(child),
        fill,
        radius,
        border: None,
        dashed: false,
    }
}

impl<State: 'static> TermBoxView<State> {
    /// Stroke a hairline border in `color` just inside the box's bounds.
    pub(super) fn border(mut self, color: Color) -> Self {
        self.border = Some(color);
        self
    }

    /// Draw that border dashed (`.pill.add`, `.new-pane-btn`) — see the
    /// [module docs](self)'s frust-gap note.
    pub(super) fn dashed(mut self, dashed: bool) -> Self {
        self.dashed = dashed;
        self
    }
}

/// The retained half of a [`TermBoxView`].
pub(super) struct TermBoxWidget {
    child: ChildPod,
    fill: Color,
    radius: f64,
    border: Option<Color>,
    dashed: bool,
}

impl<State: 'static> View<State> for TermBoxView<State> {
    type Element = TermBoxWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> TermBoxWidget {
        TermBoxWidget {
            child: build_child(&self.child, ctx),
            fill: self.fill,
            radius: self.radius,
            border: self.border,
            dashed: self.dashed,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut TermBoxWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.fill != self.fill
            || prev.radius != self.radius
            || prev.border != self.border
            || prev.dashed != self.dashed
        {
            element.fill = self.fill;
            element.radius = self.radius;
            element.border = self.border;
            element.dashed = self.dashed;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut TermBoxWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for TermBoxWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        let radius = effective_radius(self.radius, size);
        if self.fill != Color::TRANSPARENT {
            scene.fill_rounded_rect(origin, size, radius, self.fill);
        }
        if let Some(border) = self.border {
            if self.dashed {
                let half = HAIRLINE / 2.0;
                let (left, top) = (origin.x + half, origin.y + half);
                let (right, bottom) = (origin.x + size.width - half, origin.y + size.height - half);
                // The corner arcs stay undashed (dashing a curve needs the
                // pattern support frust lacks); at a 4px period an open corner
                // reads as one more gap.
                for y in [top, bottom] {
                    dash_run(scene, left + radius, right - radius, border, |x| {
                        (Point::new(x.0, y), Point::new(x.1, y))
                    });
                }
                for x in [left, right] {
                    dash_run(scene, top + radius, bottom - radius, border, |y| {
                        (Point::new(x, y.0), Point::new(x, y.1))
                    });
                }
            } else {
                stroke_border(scene, origin, size, radius, border);
            }
        }
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        // A decorative single-child container still MUST route, or everything
        // it backs is silently unreachable (the sessions `panel` precedent):
        // every pill's gesture detector sits under one of these.
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // A transparent container still MUST forward, or the child's whole
        // subtree drops out of the accessibility tree.
        self.child.semantics_child(ctx);
    }
}

// ---------------------------------------------------------------------------
// `wrap_row` — `.pill-row{display:flex; gap:8px; flex-wrap:wrap}`
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The card's status strip
// ---------------------------------------------------------------------------

/// The strip's own gutters — the status row's (`.term-statusrow`'s 18px lead,
/// 14px trail), transcribed a second time because that row's constants are
/// private to [`super::pieces`]; the two are the card's one left edge, so they
/// have to agree.
const STRIP_PAD_LEFT: f64 = 18.0;
const STRIP_PAD_RIGHT: f64 = 14.0;
/// Tighter than the status row's 14px: this band is a transient notice above a
/// grid, and every row it steals is a row of terminal.
const STRIP_PAD_Y: f64 = 10.0;
/// Gap between the message and the affordance beside it — the sheet's own pill
/// gap, so the card and its sheet breathe alike.
const STRIP_GAP: f64 = 8.0;

/// A one-line notice band across the card, optionally ending in one
/// affordance: what the screen surfaces a live session's link state through.
///
/// A band rather than an overlay because it must not cover terminal output the
/// user is still reading — a reconnecting session is *stale*, not gone, and
/// what it last printed is often exactly what explains why. Square-cornered
/// and full-bleed for the same reason the status row above it is: the card
/// clips to its own radius, so a band inside it never needs one of its own.
///
/// `action` is an already-built view rather than a label plus a callback, so
/// this stays free of a closure type parameter and the caller keeps whatever
/// button vocabulary the rest of its screen uses.
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
    any(term_box(
        Padding(
            EdgeInsets {
                left: STRIP_PAD_LEFT,
                top: STRIP_PAD_Y,
                right: STRIP_PAD_RIGHT,
                bottom: STRIP_PAD_Y,
            },
            FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center),
        ),
        fill,
        0.0,
    ))
}

/// A row of children that wraps onto further lines when it runs out of width,
/// with `gap` between neighbours on both axes.
///
/// Hand-rolled because **frust's `Flex` has no wrap** (`FlexView` is a single
/// line, `MainAxisAlignment::Start` only) — another entry in the same
/// framework-gap list as the dash pattern above. The sheet's tab row is what
/// forces the issue: five pills at 11px come to roughly 390 logical px, which
/// is wider than the panel on every phone this app targets, and an unwrapped
/// row would simply run its last pill off the edge.
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
            // erased types no longer line up), so the row is rebuilt whole —
            // the same rule `crate::shell::root_view`'s stable-`Stack`
            // invariant states from the other side.
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
        // The **height** is passed through unclamped, infinity included: a
        // vertical `Flex` hands its children an unbounded main axis, and
        // coercing that to zero (as the width has to be, to wrap against it)
        // would flatten every child to nothing.
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

// ---------------------------------------------------------------------------
// `clipped_label` — a single-line run, ellipsized to fit
// ---------------------------------------------------------------------------

/// The mark appended to a truncated run (`text-overflow:ellipsis`).
const ELLIPSIS: char = '\u{2026}';

/// A minimal retained text run — `frust-widgets`' own per-module
/// `GlyphLabel` helper, re-derived here because it is crate-private (the same
/// re-derivation `examples/glyph-catalog` makes for `build_child`).
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
    /// changed. `max_width` is only ever `None` here: every caller either
    /// measures unconstrained (and truncates itself) or lets the line wrap.
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

    /// Measure `content` at `style` without disturbing the cached layout.
    fn measure(ctx: &mut LayoutCtx, content: &str, style: &TextStyle) -> Size {
        ctx.text_context::<TextContext>()
            .layout(content, style, None)
            .size()
    }

    fn paint(&self, origin: Point, scene: &mut dyn PaintScene) {
        if let Some(layout) = &self.layout {
            for run in layout.to_scene_runs(origin) {
                scene.draw_glyph_run(run);
            }
        }
    }
}

/// A single-line run truncated with an ellipsis when it does not fit —
/// `.term-pane-title`'s `white-space:nowrap; text-overflow:ellipsis`.
///
/// Hand-rolled because `frust::text` carries no overflow vocabulary at all
/// (see the [module docs](self)): left to it, a long pane title would wrap to
/// a second line and grow the card's status row.
pub(super) struct ClippedLabelView {
    content: String,
    style: TextStyle,
}

/// A single-line, ellipsized run of `content` in `style`.
pub(super) fn clipped_label(content: impl Into<String>, style: TextStyle) -> ClippedLabelView {
    ClippedLabelView {
        content: content.into(),
        style,
    }
}

/// The retained half of a [`ClippedLabelView`].
pub(super) struct ClippedLabelWidget {
    /// The full text, for semantics — a screen reader gets the whole title
    /// even when the pixels are cut short.
    content: String,
    style: TextStyle,
    /// The run actually shaped this layout (possibly truncated).
    run: GlyphLabel,
}

impl<State: 'static> View<State> for ClippedLabelView {
    type Element = ClippedLabelWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> ClippedLabelWidget {
        ClippedLabelWidget {
            content: self.content.clone(),
            style: self.style.clone(),
            run: GlyphLabel::new(self.content.clone()),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut ClippedLabelWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        if prev.content != self.content || prev.style != self.style {
            element.content = self.content.clone();
            element.style = self.style.clone();
            element.run = GlyphLabel::new(self.content.clone());
            return ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        ChangeFlags::NONE
    }
}

impl ClippedLabelWidget {
    /// The longest prefix of `content` that fits `max_width` once an ellipsis
    /// is appended, or the whole string when it already fits.
    ///
    /// A linear walk over character boundaries rather than a binary search:
    /// a pane title is a handful of characters, and each step is a cached
    /// shaping call.
    fn fitted(&self, ctx: &mut LayoutCtx, max_width: f64) -> String {
        if GlyphLabel::measure(ctx, &self.content, &self.style).width <= max_width {
            return self.content.clone();
        }
        let mut fitted = String::new();
        for (index, _) in self.content.char_indices() {
            let candidate = format!("{}{ELLIPSIS}", &self.content[..index]);
            if GlyphLabel::measure(ctx, &candidate, &self.style).width > max_width {
                break;
            }
            fitted = candidate;
        }
        if fitted.is_empty() {
            fitted.push(ELLIPSIS);
        }
        fitted
    }
}

impl Widget for ClippedLabelWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let max_width = finite_or_zero(bc.max().width);
        let fitted = self.fitted(ctx, max_width);
        if self.run.content != fitted {
            self.run = GlyphLabel::new(fitted);
        }
        let size = self.run.layout(ctx, &self.style, None);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.run.paint(ctx.origin(), scene);
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // The **untruncated** text, in `value` — frust's own `text` widget's
        // convention (accesskit guidance for `Role::Label`).
        ctx.push_node(Role::Label, |node| {
            node.set_value(self.content.as_str());
        });
    }
}

// ---------------------------------------------------------------------------
// `expand_button` — `.expand-btn`, the fullscreen toggle
// ---------------------------------------------------------------------------

/// `.expand-btn{width:28px;height:28px}`.
pub(super) const EXPAND_SIZE: f64 = 28.0;
/// The drawn size of the vector inside that circle. An original value: the
/// source sets its glyph at 12px, and a Material-weight outline at 12px reads
/// thinner than a glyph does, so the mark is drawn a step up.
const EXPAND_ICON_SIZE: f64 = 15.0;
/// `.expand-btn:active{transform:scale(0.9)}`.
const PRESS_SCALE: f64 = 0.9;

/// The 28px circle on the card's status row: the **only** fullscreen
/// affordance, and the one that stays in the same place in both states ("one
/// button, always in the same place" — the mock's own note).
///
/// Hand-rolled rather than `crate::ui::topbar::icon_button` because it needs
/// two things that leaf deliberately does not have: a hairline **border** (the
/// source's `1px solid var(--term-border)`) and the `:active` **scale** press
/// treatment rather than a tone wash. Everything else — one vector mark, one
/// labelled `Role::Button` node, fire-on-up-inside — is the same contract.
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
    /// border — resolved by [`super::pieces`], like every other colour here.
    pub(super) fn tones(mut self, fill: Color, ink: Color, border: Color) -> Self {
        self.fill = fill;
        self.ink = ink;
        self.border = border;
        self
    }

    /// Whether the button is in its `.on` state (fullscreen is engaged).
    /// Purely a *semantics* input — the caller passes the amber trio through
    /// [`tones`](Self::tones) — so a screen reader announces the toggle state.
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
        // `:active{transform:scale(0.9)}` about the button's own centre. A
        // transform (rather than a smaller rect) so the mark shrinks with the
        // circle; hit testing deliberately stays at full size, exactly as the
        // CSS does.
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

// ---------------------------------------------------------------------------
// `terminal_stage` — `.term-wrap` + `.term-card`, both directions animated
// ---------------------------------------------------------------------------

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

/// `.topbar{padding:0 16px}`/`.topbar-icon{width:30px}`, transcribed from
/// `crate::ui::topbar` (whose own constants are private) so this screen can
/// hand `frust::glyph::show_glyph_menu` the window-coordinate rect of the bar
/// slot its `⋮` lives in. [`BAR_HEIGHT`] itself *is* `pub` and is imported
/// rather than transcribed.
const BAR_PAD_X: f64 = 16.0;
const BAR_SLOT: f64 = 30.0;

/// One in-flight fullscreen transition: a linear `0→1` driver, the curve that
/// eases its progress, and the `from..to` range the eased progress is then
/// interpolated across.
///
/// The same shape (and for the same reason) as `crate::ui::topbar`'s `Reveal`:
/// the range is what makes an
/// *interrupted* transition value-continuous — a second tap mid-flight starts a
/// fresh driver from wherever the card actually is (see [`start`](Self::start)),
/// the "value continuity, not velocity continuity" contract `frust::motion`'s
/// implicit animations document.
///
/// **Ease the progress, not the position.** [`value`](Self::value) applies
/// `curve` to the driver's own `0..=1` progress *before* mapping the result
/// onto `from..to` — curving the already-interpolated position instead
/// (`curve.transform(from + (to - from) · t)`) curves the wrong quantity.
/// Glyph's `spatial` curve is an authored overshoot that exceeds `1.0` on
/// roughly `t ∈ [0.45, 1.0]` (e.g. `y(0.5) ≈ 1.01`, `y(0.65) ≈ 1.04`); fed a
/// *position* input, a collapse (`from = 1, to = 0`) would sit in that
/// overshoot band for the whole first half of the driver's run and clamp
/// flush to `1.0` there — the card stalling in place before it visibly moves,
/// rather than animating through its full duration as this type's continuity
/// claim (and the entry direction's mirror-image symmetry) requires.
///
/// **Timings are the theme's, not the mock's.** `docs/terminal.html` animates
/// on `.32–.38s cubic-bezier(0.34,1.4,0.55,1)`; Glyph's `durations.slow`
/// (340ms) with `easing.spatial` (`Cubic(0.34,1.35,0.64,1)`) is the same
/// motion expressed in tokens, which is why this reaches for the authored
/// duration/easing pair rather than the `default_spatial` **spring** slot
/// beside it — `frust_theme`'s own Glyph motion docs say the spring slots are
/// hand-tuned approximations of these very beziers and that "a consumer that
/// wants the authored feel should prefer the durations/easing tokens".
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
    ///
    /// `from` must be the last **painted** progress (`self.progress` at the
    /// call site below, never a raw driver `t`) — an interrupted stage's own
    /// `value()` reads `from` at `t = 0` (see below), so seeding anything
    /// other than the on-screen value would itself reintroduce a visible
    /// jump on interruption.
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

    /// The eased `0..=1` geometry fraction this frame.
    ///
    /// Eases the driver's own progress, **then** interpolates `from..to` —
    /// see the type docs for why the reverse order is wrong. `t = 0` always
    /// returns exactly `from` and `t = 1` always returns exactly `to`
    /// (`curve.transform` is endpoint-preserving), which is what makes a
    /// freshly-[`start`](Self::start)ed stage value-continuous with whatever
    /// was last painted.
    ///
    /// Clamped once, at the end: Glyph's `spatial` curve is an authored
    /// overshoot, and "past fullscreen" is not a geometry that exists — a
    /// negative wrap padding would push the card off its own screen.
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
/// **same** progress in the same gesture (the mock transitions
/// `.app-bar`'s collapse, `.term-wrap`'s padding and `.term-card`'s radius
/// together), and because a padding that animates is a *layout* fact — frust
/// has no animated-padding widget, and the paint-origin trick
/// `crate::ui::topbar` uses cannot resize a child.
///
/// It also records the app bar's trailing-slot rect into
/// [`anchor`](TerminalStageView::anchor) — see [`BAR_PAD_X`]: the overflow
/// menu is anchored to a slot inside a *sibling* widget that this page cannot
/// query (`show_glyph_menu` takes a caller-reported window rect, and there is
/// no ancestor-bounds query), so the one widget that already knows both the
/// window box and the top inset computes it.
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
    /// The in-flight transition, `None` at rest. Created on the first
    /// **paint** after a flip, not on the rebuild that staged it: a `BuildCtx`
    /// carries no theme to resolve `reduce_motion`/the motion tokens from (the
    /// deferred-driver idiom `crate::ui::topbar` documents).
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

        // The clearance the floating bar occupies, shrinking to just the
        // status-bar inset as the bar animates away (`crate::shell` drives the
        // bar itself off the same flag). The status bar's own inset is kept in
        // **both** states, so the pane title never sits under a notch.
        let top = insets.top + BAR_HEIGHT * (1.0 - p);
        let pad = WRAP_PAD * (1.0 - p);
        // The gesture-nav/home-indicator inset is likewise kept in both
        // states: "edge to edge" must not mean "the last output line is under
        // the home bar".
        let bottom = pad + insets.bottom;

        let card_w = (width - 2.0 * pad).max(0.0);
        let card_h = (height - top - bottom).max(0.0);
        self.child
            .layout_child(ctx, &BoxConstraints::tight(Size::new(card_w, card_h)));
        self.child.set_origin(Point::new(pad, top));
        self.card_rect = Rect::new(pad, top, pad + card_w, top + card_h);
        // Recorded **here**, off the same `p` the box was built from, rather
        // than re-derived in paint: paint has already advanced the driver by
        // then, so a radius read there would run one frame ahead of the box it
        // rounds — visible on a 22px corner mid-transition.
        self.card_radius = CARD_RADIUS * (1.0 - p);

        // The bar's trailing `⋮` slot, in window coordinates — this page is
        // laid out at the full surface (the navigator never shrinks it), so
        // local and window coordinates coincide here. See the type docs.
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
        // **snaps** both directions — the documented app-wide policy, and the
        // only honest collapse for a transition that is entirely geometric.
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
            // computed in `layout`), so every step must request a layout
            // pass, not just a repaint — shells honoring the layout-skip
            // contract (Android) freeze the geometry on a bare
            // `request_frame`. `request_layout` implies the frame.
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

        // The status row's band is a plain rect, so without this its square
        // top corners would poke through the card's rounding (frust has no
        // per-corner radius — see the module docs).
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

// ---------------------------------------------------------------------------
// `term_body` — the card's own lines, their reveal, and the caret
// ---------------------------------------------------------------------------

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
/// [`super::pieces`] from `GlyphInk`.
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

/// `.term-body`'s lines: one shaped run each, revealed in a stagger and
/// followed — on the line that carries it — by the blinking block caret.
///
/// The reveal restarts whenever the lines change **or** `epoch` does, which is
/// what makes a tab switch replay it (the mock's `renderTab` ends in
/// `playReveal`). `epoch` is the controller's replay token; see its docs for
/// why per-frame progress is not a signal.
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
    /// Drives the whole stagger — one linear `0→1` controller sized to the
    /// last line's finish, exactly the shape `frust::glyph::term_block` uses
    /// for its own one-shot reveal.
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

        // Arm (or, under `reduce_motion`, deliberately *don't* arm) the
        // reveal now a theme is in scope. Reduced motion shows every line
        // immediately — a cascade is decorative, and this one carries no
        // information the static lines don't.
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

        // The caret's own clock, independent of the reveal (the mock's
        // `@keyframes blink` runs forever). `reduce_motion` freezes it lit —
        // a caret is a *position* cue, so it must not disappear.
        let lit = if reduce_motion {
            true
        } else {
            let from = *self.blink_from.get_or_insert(ctx.frame_time());
            let since = ctx.frame_time().saturating_sub(from).as_secs_f64() * 1000.0;
            // A paceable request: a 1.1s two-state blink is exactly the
            // "decorative loop whose exact cadence is imperceptible" class the
            // mobile frame gate may throttle (`PaintCtx::request_frame_paced`
            // names a caret blink as its example).
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
        // The body reads as one block of text — `frust::glyph::term_block`'s
        // own choice, and the right one: a screen reader announcing four
        // separate unlabelled runs would lose the fact that they are one
        // transcript.
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
        // The regression: a vertical `Flex` hands its children `INFINITY` on
        // the main axis, and the wrap has to coerce the *width* to a finite
        // number to wrap against it. Coercing the height the same way flattens
        // every pill to zero — which is what the sheet's first frame did.
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
        let theme = Theme::glyph_baseline();
        let entering = FullscreenStage::start(0.0, 1.0, Some(&theme));
        // A fresh driver has not advanced, so it is still where it started.
        assert_eq!(entering.value(), 0.0);
        let leaving = FullscreenStage::start(1.0, 0.0, Some(&theme));
        assert_eq!(leaving.value(), 1.0);
    }

    /// `60ms` into a fresh entrance: mid-flight, inside Glyph's `spatial`
    /// overshoot band, and never past `1.0` before the final clamp — the
    /// number the interruption tests below seed from.
    fn painted_mid_entrance(theme: &Theme) -> f64 {
        let mut entering = FullscreenStage::start(0.0, 1.0, Some(theme));
        // The first `advance` after `start` only seeds the clock (a fresh
        // driver's own documented zero-delta rule) — the second is what
        // actually elapses 60ms.
        entering.driver.advance(FrameTime::from_nanos(0));
        entering.driver.advance(FrameTime::from_nanos(60_000_000));
        entering.value()
    }

    #[test]
    fn collapsing_leaves_1_0_well_before_half_the_duration_rather_than_stalling() {
        // The bug this guards against: curving the *interpolated position*
        // (`curve.transform(from + (to - from) * t)`) feeds a collapse's
        // `from = 1.0` into the curve's own `[0.45, 1.0]` overshoot band for
        // the whole first half of `durations.slow`, clamping flush to `1.0`
        // there — the card sits motionless before it ever visibly moves.
        let theme = Theme::glyph_baseline();
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
        // The bug this guards against: seeding a fresh stage's `from` with an
        // already-*eased* value and curving it a second time. Since the
        // painted value of an in-flight entrance often lands inside Glyph's
        // `spatial` overshoot band, re-easing it snaps the card straight to
        // fullscreen the instant a collapse interrupts it — before any time
        // has passed.
        let theme = Theme::glyph_baseline();
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
        let theme = Theme::glyph_baseline();
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
        // Glyph's `spatial` easing overshoots past 1.0 — which for a padding
        // that reaches zero would mean a *negative* one.
        let theme = Theme::glyph_baseline();
        let stage = FullscreenStage::start(0.0, 1.0, Some(&theme));
        for raw in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let eased = stage.curve.transform(raw).clamp(0.0, 1.0);
            assert!((0.0..=1.0).contains(&eased), "{eased}");
            assert!(WRAP_PAD * (1.0 - eased) >= 0.0);
        }
    }

    #[test]
    fn the_unthemed_fallbacks_are_the_glyph_tokens_they_stand_in_for() {
        // A tripwire on the transcription: a headless frame with no theme
        // threaded must stage on the same numbers a real device does.
        let motion = Theme::glyph_baseline().motion;
        assert_eq!(FALLBACK_SLOW.as_millis() as f64, motion.durations.slow);
        assert_eq!(FALLBACK_SPATIAL, motion.easing.spatial);
        assert_eq!(FALLBACK_EFFECTS, motion.easing.effects);
    }

    #[test]
    fn every_app_supplied_mark_parses_and_fits_its_design_box() {
        use frust::authoring::Shape;

        for (name, source) in [
            ("FULLSCREEN_EXPAND", FULLSCREEN_EXPAND),
            ("FULLSCREEN_EXIT", FULLSCREEN_EXIT),
            ("PANE_MARK", PANE_MARK),
        ] {
            let path = BezPath::from_svg(source.d).unwrap_or_else(|e| panic!("{name}: {e}"));
            let bounds = path.bounding_box();
            assert_eq!(source.design, ICON_DESIGN_BOX, "{name}");
            assert!(bounds.x0 >= 0.0 && bounds.y0 >= 0.0, "{name}: {bounds:?}");
            assert!(
                bounds.x1 <= source.design && bounds.y1 <= source.design,
                "{name}: {bounds:?}"
            );
        }
    }

    #[test]
    fn the_two_fullscreen_marks_are_distinct_shapes() {
        // The icon *swap* is the whole feedback the button gives beyond its
        // tone change — two identical paths would be a silent regression.
        assert_ne!(FULLSCREEN_EXPAND.d, FULLSCREEN_EXIT.d);
    }
}
