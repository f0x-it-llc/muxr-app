//! `.set-row`/`.set-group-lbl` — the row primitive every group is built from,
//! plus the shared layout helpers ([`h_gap`]/[`spacer`]/[`row`]/[`column`])
//! and the screen's page tone ([`page_tone`]) every other `widgets` module
//! draws with.
//!
//! Geometry transcribed from `docs/redesign.html`'s settings block (CSS lines
//! 153–179). [`RowMark`]/[`MARK_*`] cover every group's tile: a vendored
//! `frust::icons` vector, or a single-font-safe text mark (ASCII or Geometric
//! Shapes only) for the two backend initials and the mock's literal glyphs —
//! **never an emoji**, which resolves through platform font fallback (the
//! tofu-on-iPhone defect this app has hit before).
//!
//! [`rounded_fill`] is this module's own primitive — the sliver of the
//! former `chrome::panel` `frust::container` cannot reach: a paint-only
//! bleed (`.pick-row.sel{margin:0 -20px}`, `widgets::pickers`' own row) and a
//! zero-radius fill that stays classified as a plain rect rather than a
//! rounded one, which [`page_tone`] below needs. Every other option that
//! module used to carry — fill/border/radius, a dashed border, a fixed
//! centered size — is `frust::container` directly at each call site now; a
//! bottom hairline composes as a trailing `frust::divider` instead, except on
//! `widgets::pickers`' own rows, whose uniform row-to-row pitch depends on
//! the rule staying a paint overlay rather than added layout height (see
//! [`rounded_fill`]'s own doc).

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult, InputEvent,
    LayoutCtx, PaintCtx, PaintScene, Point, SemanticsCtx, Size, View, Widget, build_child,
    rebuild_child, route_event_single, teardown_child,
};
use frust::icons::{CHEVRON_RIGHT, FORMAT_SIZE, LINK, LOCK, NOTIFICATIONS, REFRESH};
use frust::{
    AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexChild, FlexView, GestureDetector,
    IconSource, Padding, SizedBox, any, container, divider, flexible, icon, inflexible, text,
};

use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::pickers::PickerKind;

/// `.settings-scroll{padding:6px 0 30px}`.
pub(in super::super) const SCROLL_PAD_TOP: f64 = 6.0;
pub(in super::super) const SCROLL_PAD_BOTTOM: f64 = spacing::HERO;
/// `.set-group-lbl{padding:16px 16px 8px}`.
pub(super) const GROUP_PAD_TOP: f64 = spacing::XL;
pub(super) const GROUP_PAD_BOTTOM: f64 = spacing::SM;
/// `.set-row{padding:11px 16px; gap:12px}` — also `.token-item`'s padding.
pub(super) const ROW_PAD_X: f64 = spacing::XL;
pub(super) const ROW_PAD_Y: f64 = 11.0;
pub(super) const ROW_GAP: f64 = spacing::BASE;
/// `.set-ic{width:28px;height:28px}`; the vector inside it (`font-size:12px`
/// in the mock, sized up to match a Material Symbols vector's drawn weight).
const MARK_BOX: f64 = 28.0;
const MARK_ICON: f64 = spacing::LG;
/// `.set-value{margin-top:2px}`.
pub(super) const ROW_LINE_GAP: f64 = spacing::MICRO;
/// `.set-chev{font-size:13px}`.
const CHEVRON: f64 = 13.0;
/// Hairline width in logical px (`1px solid` on every bordered rule in
/// `docs/redesign.html`) — [`rounded_fill`]'s own bottom rule, and a chip's
/// optional border at each call site. Matches `frust-widgets`' own
/// `BORDER_WIDTH`.
pub(super) const HAIRLINE: f64 = 1.0;
/// `--radius-full: 999px` — kurbo's own `RoundedRect` clamps an oversized
/// radius to half the shorter side, so a caller can spell "pill" without
/// knowing the box's measured height.
pub(super) const RADIUS_FULL: f64 = 999.0;

/// A `gap`-wide horizontal spacer.
pub(super) fn h_gap<S: 'static>(gap: f64) -> FlexChild<S> {
    inflexible(SizedBox::<S>(Some(gap), None))
}

/// A flexible spacer pushing what follows it to the trailing edge — Flex v1
/// has no `space-between`.
pub(super) fn spacer<S: 'static>() -> FlexChild<S> {
    flexible(1, SizedBox::<S>(None, None))
}

/// A vertically-centered row.
pub(super) fn row<S: 'static>(children: Vec<FlexChild<S>>) -> FlexView<S> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

/// A column whose children each fill the available width.
pub(super) fn column<S: 'static>(children: Vec<AnyView<S>>) -> FlexView<S> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Stretch)
}

/// The screen's page tone behind `body` — `--bg-base`, which the shell's own
/// clear colour (`--bg-surface`) is not. Zero radius: [`rounded_fill`] keeps
/// this out of every rounded-rect probe (see its own doc).
pub(in super::super) fn page_tone(
    body: impl View<SettingsState>,
    colors: &Palette,
) -> AnyView<SettingsState> {
    any(rounded_fill(body, colors.base, 0.0))
}

// ---------------------------------------------------------------------------
// `rounded_fill` — the one primitive `frust::container` cannot quite reach
// ---------------------------------------------------------------------------

/// A rounded fill behind a child it hugs exactly (the container's own size is
/// always the child's), with two options `frust::container` has no knob for:
///
/// - [`RoundedFillView::bleed`] widens the *painted* fill by `bleed` logical
///   px on each side without moving layout — `.pick-row.sel{margin:0 -20px}`.
///   `frust::Padding` clamps a negative inset, and `Container` carries no
///   bleed knob; its own module doc names this the deferred v1 boundary and
///   says to compose it as app-side layering over a plain `Container`
///   instead, which is exactly what this is.
/// - At `radius == 0.0`, paint goes through [`PaintScene::fill_rect`] rather
///   than [`PaintScene::fill_rounded_rect`]. `Container` always emits the
///   rounded call regardless of radius, so a zero-radius `Container` fill
///   still reads as *rounded* to a paint-recording test scene — this
///   screen's own [`page_tone`] must not, since `tests/navigation_motion.rs`'s
///   `leaving_page_x` finds the *largest rounded rect* on screen to track a
///   leaving page, and this screen's full-bleed tone would out-measure it.
///
/// [`RoundedFillView::bottom_rule`] is the third option: a hairline painted
/// along the bottom edge, *overlapping* the last logical px rather than
/// adding to the box's own height. `widgets::pickers`' rows need this
/// distinction specifically — every `.pick-row` must stay exactly one line
/// tall regardless of whether it rules, since `tests/settings.rs` locates an
/// unopened row by that uniform pitch. Everywhere else a bottom hairline
/// composes as a trailing `frust::divider` instead (see this module's own
/// doc).
pub(super) struct RoundedFillView<State: 'static> {
    child: AnyView<State>,
    fill: Color,
    radius: f64,
    bleed: f64,
    bottom_rule: Option<Color>,
}

/// Wrap `child` in a `fill`-filled box with corner `radius`, no bleed and no
/// bottom rule. See [`RoundedFillView`].
pub(super) fn rounded_fill<State: 'static, V: View<State>>(
    child: V,
    fill: Color,
    radius: f64,
) -> RoundedFillView<State> {
    RoundedFillView {
        child: any(child),
        fill,
        radius,
        bleed: 0.0,
        bottom_rule: None,
    }
}

impl<State: 'static> RoundedFillView<State> {
    /// See [`RoundedFillView`].
    pub(super) fn bleed(mut self, bleed: f64) -> Self {
        self.bleed = bleed;
        self
    }

    /// See [`RoundedFillView`].
    pub(super) fn bottom_rule(mut self, color: Color) -> Self {
        self.bottom_rule = Some(color);
        self
    }
}

/// The retained half of a [`RoundedFillView`].
pub(super) struct RoundedFillWidget {
    child: ChildPod,
    fill: Color,
    radius: f64,
    bleed: f64,
    bottom_rule: Option<Color>,
}

impl<State: 'static> View<State> for RoundedFillView<State> {
    type Element = RoundedFillWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> RoundedFillWidget {
        RoundedFillWidget {
            child: build_child(&self.child, ctx),
            fill: self.fill,
            radius: self.radius,
            bleed: self.bleed,
            bottom_rule: self.bottom_rule,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut RoundedFillWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if prev.fill != self.fill
            || prev.radius != self.radius
            || prev.bleed != self.bleed
            || prev.bottom_rule != self.bottom_rule
        {
            element.fill = self.fill;
            element.radius = self.radius;
            element.bleed = self.bleed;
            element.bottom_rule = self.bottom_rule;
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut RoundedFillWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for RoundedFillWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        bc.constrain(size)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let origin = ctx.origin();
        let size = ctx.size();
        if self.fill != Color::TRANSPARENT {
            // The bleed widens only what is painted — this widget's own
            // bounds (and its child's origin) are untouched, so nothing
            // reflows when a selection moves (see [`RoundedFillView`]).
            let fill_origin = Point::new(origin.x - self.bleed, origin.y);
            let fill_size = Size::new(size.width + 2.0 * self.bleed, size.height);
            if self.radius == 0.0 {
                scene.fill_rect(fill_origin, fill_size, self.fill);
            } else {
                scene.fill_rounded_rect(fill_origin, fill_size, self.radius, self.fill);
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
        // it backs is silently unreachable.
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        // A transparent container still MUST forward, or the child's whole
        // subtree drops out of the accessibility tree.
        self.child.semantics_child(ctx);
    }
}

/// The band above each group — 10px dim, letter-spaced, uppercased here since
/// `text` carries no CSS `text-transform`.
pub(in super::super) fn group_label(label: &str, colors: &Palette) -> AnyView<SettingsState> {
    let theme = active_theme();
    any(Padding(
        EdgeInsets {
            left: ROW_PAD_X,
            top: GROUP_PAD_TOP,
            right: ROW_PAD_X,
            bottom: GROUP_PAD_BOTTOM,
        },
        text(label.to_uppercase()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
    ))
}

/// What sits in a row's 28px `.set-ic` tile — a vendored vector, or a
/// single-font-safe text mark (`abc`, `▮`, a backend initial).
#[derive(Clone, Copy)]
pub(super) enum RowMark {
    Vector(IconSource),
    Text(&'static str),
}

/// The semantic hue a `.set-value` prints in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ValueTone {
    /// `--fg-dim` — every value that is merely a fact.
    Plain,
    /// `.set-value.ok` — the live endpoint, and a validated TLS posture.
    Ok,
    /// `.set-value.warn` — the posture that skips certificate validation.
    Warn,
}

/// What a row *does*, and so what sits in its trailing slot. The bell row is
/// not a `RowAction` — the real switch owns its own tap target (see
/// `widgets::misc::bell_notifications_row`).
#[derive(Clone, Copy)]
pub(in super::super) enum RowAction {
    /// Inert: no chevron, no gesture (the version rows and connection info).
    Static,
    /// A chevron row that opens a picker sheet.
    Picker(PickerKind),
}

/// One `.set-row`: the mark tile, the label over its value, and a trailing
/// chevron for anything that leads somewhere. For [`RowAction::Picker`] the
/// **whole row** is the tap target — the mock draws no separate hit area, and
/// a 28px chevron alone would be unreachable on a phone.
pub(super) fn settings_row(
    mark: RowMark,
    label: &'static str,
    value: impl Into<String>,
    tone: ValueTone,
    action: RowAction,
    colors: &Palette,
) -> AnyView<SettingsState> {
    let theme = active_theme();
    let value_ink = match tone {
        ValueTone::Plain => colors.fg_dim,
        ValueTone::Ok => colors.success,
        ValueTone::Warn => colors.warning,
    };
    let body = column(vec![
        any(text(label).style(inked(&theme.type_scale.label_large, colors.fg))),
        any(SizedBox::<SettingsState>(None, Some(ROW_LINE_GAP))),
        any(text(value.into()).style(inked(&theme.type_scale.body_small, value_ink))),
    ]);

    let mut children = vec![
        inflexible(mark_tile(mark, colors)),
        h_gap(ROW_GAP),
        flexible(1, any(body)),
    ];
    if let Some(trailing) = trailing_slot(action, colors) {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(trailing));
    }

    // No fill, so this composes as plain content over a trailing
    // `frust::divider` — the row's own `border-bottom` — rather than reaching
    // for `rounded_fill` at all (see this module's own doc).
    let box_ = column(vec![
        any(Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            row(children),
        )),
        any(divider(colors.rule)),
    ]);

    match action {
        RowAction::Static => any(box_),
        RowAction::Picker(_) => any(GestureDetector(box_)
            .on_tap(move |state: &mut SettingsState| pages::row_tapped(state, action))),
    }
}

/// `.set-ic` — the 28px raised tile holding a row's mark. Shared with
/// `widgets::misc::bell_notifications_row`, the one row outside this file
/// that also uses it.
pub(super) fn mark_tile(mark: RowMark, colors: &Palette) -> AnyView<SettingsState> {
    let theme = active_theme();
    let content: AnyView<SettingsState> = match mark {
        RowMark::Vector(source) => any(icon(source).size(MARK_ICON).color(colors.fg_muted)),
        RowMark::Text(glyph) => {
            any(text(glyph).style(inked(&theme.type_scale.label_small, colors.fg_muted)))
        }
    };
    any(container(content)
        .fill(colors.raised)
        .radius(theme.shape.extra_small)
        .size_centered(MARK_BOX, MARK_BOX))
}

/// A row's trailing slot: the faintest chevron for anything that leads
/// somewhere, nothing for a static fact.
fn trailing_slot(action: RowAction, colors: &Palette) -> Option<AnyView<SettingsState>> {
    match action {
        RowAction::Static => None,
        RowAction::Picker(_) => Some(any(icon(CHEVRON_RIGHT)
            .size(CHEVRON)
            .color(colors.fg_faintest))),
    }
}

// ---------------------------------------------------------------------------
// The mark set, named once
// ---------------------------------------------------------------------------

/// `.set-ic` for `server url` — a link, standing in for the mock's 🌐.
pub(super) const MARK_SERVER_URL: RowMark = RowMark::Vector(LINK);
/// …for `tls certificate` — the mock's 🔓 as a vector. Not state-dependent:
/// the row's *value* carries the posture, so a second signal in the tile
/// would say the same thing twice.
pub(super) const MARK_TLS: RowMark = RowMark::Vector(LOCK);
/// …for `zellij version`/`herdr version` — the backend's initial, the same
/// "initial in a tile" device `.sess-avatar` uses.
pub(super) const MARK_ZELLIJ: RowMark = RowMark::Text("z");
pub(super) const MARK_HERDR: RowMark = RowMark::Text("h");
/// …for `font family` — the mock's literal `abc`, kept as text.
pub(super) const MARK_FONT_FAMILY: RowMark = RowMark::Text("abc");
/// …for `font size` — the vendored `format_size` vector rather than the
/// mock's literal `12`, which would go stale the moment the picker changed
/// it.
pub(super) const MARK_FONT_SIZE: RowMark = RowMark::Vector(FORMAT_SIZE);
/// …for `cursor style` — `▮` (U+25AE), the mock's own mark, from the same
/// Geometric Shapes block the sessions screen already ships on device.
pub(super) const MARK_CURSOR: RowMark = RowMark::Text("\u{25ae}");
/// …for `layout refresh` — the mock's `↻` as a vector.
pub(super) const MARK_REFRESH: RowMark = RowMark::Vector(REFRESH);
/// …for `bell notifications` — the mock's 🔔 as a vector.
pub(super) const MARK_BELL: RowMark = RowMark::Vector(NOTIFICATIONS);

#[cfg(test)]
mod tests {
    use super::*;

    /// A recording scene distinguishing plain fills from rounded ones — the
    /// distinction [`RoundedFillView`]'s zero-radius branch exists to control.
    #[derive(Default)]
    struct RecScene {
        rects: Vec<(Point, Size)>,
        rounded: Vec<(Point, Size, f64)>,
        lines: Vec<(Point, Point)>,
    }

    impl PaintScene for RecScene {
        fn fill_rect(&mut self, origin: Point, size: Size, _color: Color) {
            self.rects.push((origin, size));
        }
        fn fill_rounded_rect(&mut self, origin: Point, size: Size, radius: f64, _color: Color) {
            self.rounded.push((origin, size, radius));
        }
        fn stroke_line(&mut self, p0: Point, p1: Point, _width: f64, _color: Color) {
            self.lines.push((p0, p1));
        }
        fn draw_text(&mut self, _origin: Point, _text: &str) {}
    }

    /// The smallest possible leaf — lays out to `size`, paints nothing, so a
    /// paint test measures the fill's own chrome rather than whatever it
    /// wraps.
    struct StandIn(Size);

    impl Widget for StandIn {
        fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
            bc.constrain(self.0)
        }
        fn paint(&mut self, _ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {}
    }

    fn empty_fill(fill: Color, radius: f64, bleed: f64) -> RoundedFillWidget {
        RoundedFillWidget {
            child: ChildPod::new(Box::new(StandIn(Size::ZERO))),
            fill,
            radius,
            bleed,
            bottom_rule: None,
        }
    }

    #[test]
    fn a_zero_radius_fill_paints_a_plain_rect_not_a_rounded_one() {
        // The invariant `page_tone` depends on: `tests/navigation_motion.rs`'s
        // `leaving_page_x` finds the *largest rounded rect* on screen, and
        // this screen's own full-page tone must never be a candidate.
        let mut widget = empty_fill(Color::from_rgb8(0x10, 0x13, 0x1a), 0.0, 0.0);
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::ZERO, Size::new(800.0, 1700.0));
        widget.paint(&mut ctx, &mut scene);

        assert_eq!(scene.rects, vec![(Point::ZERO, Size::new(800.0, 1700.0))]);
        assert!(scene.rounded.is_empty(), "{:?}", scene.rounded);
    }

    #[test]
    fn a_nonzero_radius_fill_paints_a_rounded_rect() {
        let mut widget = empty_fill(Color::from_rgb8(0xff, 0xb6, 0x27), 6.0, 0.0);
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::new(20.0, 100.0), Size::new(300.0, 44.0));
        widget.paint(&mut ctx, &mut scene);

        assert!(scene.rects.is_empty(), "{:?}", scene.rects);
        assert_eq!(
            scene.rounded,
            vec![(Point::new(20.0, 100.0), Size::new(300.0, 44.0), 6.0)]
        );
    }

    #[test]
    fn a_bleeding_fill_paints_wider_than_it_measures() {
        // `.pick-row.sel{margin:0 -20px}` — the fill escapes the sheet's
        // content gutter on both sides while layout stays put.
        let mut widget = empty_fill(Color::from_rgb8(0xff, 0xb6, 0x27), 6.0, 20.0);
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::new(20.0, 100.0), Size::new(300.0, 44.0));
        widget.paint(&mut ctx, &mut scene);

        assert_eq!(scene.rounded.len(), 1, "one fill: {:?}", scene.rounded);
        assert_eq!(scene.rounded[0].0, Point::new(0.0, 100.0), "20px left");
        assert_eq!(scene.rounded[0].1, Size::new(340.0, 44.0), "…and right");
    }

    #[test]
    fn an_unbled_fill_paints_exactly_its_own_box() {
        let mut widget = empty_fill(Color::from_rgb8(0x1e, 0x23, 0x30), 6.0, 0.0);
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::new(20.0, 100.0), Size::new(300.0, 44.0));
        widget.paint(&mut ctx, &mut scene);

        assert_eq!(scene.rounded[0].0, Point::new(20.0, 100.0));
        assert_eq!(scene.rounded[0].1, Size::new(300.0, 44.0));
    }

    #[test]
    fn a_transparent_fill_paints_only_its_bottom_rule() {
        // Every `.set-row` composing through `rounded_fill` directly (rather
        // than a trailing `divider`) would be one of these: transparent, with
        // only the rule. A fill would show as a seam over the page tone.
        let mut widget = empty_fill(Color::TRANSPARENT, 0.0, 0.0);
        widget.bottom_rule = Some(Color::WHITE);
        let mut scene = RecScene::default();
        let mut ctx = PaintCtx::new(Point::ZERO, Size::new(300.0, 44.0));
        widget.paint(&mut ctx, &mut scene);

        assert!(scene.rects.is_empty(), "{:?}", scene.rects);
        assert!(scene.rounded.is_empty(), "{:?}", scene.rounded);
        assert_eq!(scene.lines.len(), 1, "just the rule");
        assert_eq!(scene.lines[0].0.y, scene.lines[0].1.y, "…and it is level");
    }

    #[test]
    fn no_mark_is_an_emoji() {
        // A text mark must stay inside the BMP ranges the app's one bundled
        // family actually covers — ASCII or Geometric Shapes — because
        // anything else resolves through platform font fallback.
        for mark in [
            MARK_SERVER_URL,
            MARK_TLS,
            MARK_ZELLIJ,
            MARK_HERDR,
            MARK_FONT_FAMILY,
            MARK_FONT_SIZE,
            MARK_CURSOR,
            MARK_REFRESH,
            MARK_BELL,
        ] {
            let RowMark::Text(glyph) = mark else {
                continue;
            };
            for c in glyph.chars() {
                let cp = c as u32;
                assert!(
                    c.is_ascii() || (0x25A0..=0x25FF).contains(&cp),
                    "{glyph:?} carries U+{cp:04X}, outside ASCII/Geometric Shapes"
                );
            }
        }
    }
}
