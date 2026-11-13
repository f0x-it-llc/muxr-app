//! The `.set-row` primitive, the shared layout helpers and the page tone. A text mark stays in
//! ASCII or Geometric Shapes, never an emoji, which resolves through platform font fallback.

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult, InputEvent,
    LayoutCtx, PaintCtx, PaintScene, Point, SemanticsCtx, Size, View, Widget, build_child,
    rebuild_child, route_event_single, teardown_child,
};
use frust::icons::{CHEVRON_RIGHT, FORMAT_SIZE, LINK, LOCK, NOTIFICATIONS, REFRESH, SCHEDULE};
use frust::{
    AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, Either, FlexChild, FlexView,
    GestureDetector, IconSource, Padding, SizedBox, ViewSeq, any, button, column, container,
    divider, flexible, icon, inflexible, text,
};

use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::pickers::PickerKind;

pub(super) use crate::ui::paint::{HAIRLINE, RADIUS_FULL};

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
/// `.set-ic{width:28px;height:28px}`; the vector is sized above the mock's 12px to match weight.
const MARK_BOX: f64 = 28.0;
const MARK_ICON: f64 = spacing::LG;
/// `.set-value{margin-top:2px}`.
pub(super) const ROW_LINE_GAP: f64 = spacing::MICRO;
/// `.set-chev{font-size:13px}`.
const CHEVRON: f64 = 13.0;

pub(super) fn h_gap<S: 'static>(gap: f64) -> FlexChild<S> {
    inflexible(SizedBox::<S>(Some(gap), None))
}

pub(super) fn spacer<S: 'static>() -> FlexChild<S> {
    flexible(1, SizedBox::<S>(None, None))
}

pub(super) fn centered_row<S: 'static>(children: Vec<FlexChild<S>>) -> FlexView<S> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

pub(super) fn stretch_column<S: 'static, M>(children: impl ViewSeq<S, M>) -> FlexView<S> {
    column()
        .children(children)
        .cross_axis(CrossAxisAlignment::Stretch)
}

/// `--bg-base`, which the shell's clear colour is not; zero radius keeps it a plain rect.
pub(in super::super) fn page_tone<B: View<SettingsState>>(
    body: B,
    colors: &Palette,
) -> impl View<SettingsState> + use<B> {
    rounded_fill(body, colors.base, 0.0)
}

/// `bleed` widens only the painted fill (frust's `Padding` clamps a negative inset). Radius 0
/// paints a plain rect, since `tests/navigation_motion.rs` tracks the largest rounded rect.
/// `bottom_rule` overlaps the last px instead of adding height, so picker rows keep one pitch.
pub(super) struct RoundedFillView<V> {
    child: V,
    fill: Color,
    radius: f64,
    bleed: f64,
    bottom_rule: Option<Color>,
}

pub(super) fn rounded_fill<V>(child: V, fill: Color, radius: f64) -> RoundedFillView<V> {
    RoundedFillView {
        child,
        fill,
        radius,
        bleed: 0.0,
        bottom_rule: None,
    }
}

impl<V> RoundedFillView<V> {
    pub(super) fn bleed(mut self, bleed: f64) -> Self {
        self.bleed = bleed;
        self
    }

    pub(super) fn bottom_rule(mut self, color: Color) -> Self {
        self.bottom_rule = Some(color);
        self
    }
}

pub(super) struct RoundedFillWidget {
    child: ChildPod,
    fill: Color,
    radius: f64,
    bleed: f64,
    bottom_rule: Option<Color>,
}

impl<State: 'static, V: View<State>> View<State> for RoundedFillView<V> {
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
            // The bleed widens only paint; layout is untouched, so nothing reflows on selection.
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
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

pub(in super::super) fn group_label(label: &str, colors: &Palette) -> impl View<SettingsState> {
    let theme = active_theme();
    Padding(
        EdgeInsets {
            left: ROW_PAD_X,
            top: GROUP_PAD_TOP,
            right: ROW_PAD_X,
            bottom: GROUP_PAD_BOTTOM,
        },
        text(label.to_uppercase()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
    )
}

#[derive(Clone, Copy)]
pub(super) enum RowMark {
    Vector(IconSource),
    Text(&'static str),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ValueTone {
    Plain,
    Ok,
    Warn,
}

#[derive(Clone, Copy)]
pub(in super::super) enum RowAction {
    Static,
    Picker(PickerKind),
    UpgradeSheet,
}

/// The whole row is the tap target: a 28px chevron alone is unreachable on a phone.
pub(super) fn settings_row(
    mark: RowMark,
    label: &'static str,
    value: impl Into<String>,
    tone: ValueTone,
    action: RowAction,
    colors: &Palette,
) -> AnyView<SettingsState> // erasure: keep accumulator into Vec<AnyView>
{
    let theme = active_theme();
    let value_ink = match tone {
        ValueTone::Plain => colors.fg_dim,
        ValueTone::Ok => colors.success,
        ValueTone::Warn => colors.warning,
    };
    let body = stretch_column((
        text(label).style(inked(&theme.type_scale.label_large, colors.fg)),
        text(value.into()).style(inked(&theme.type_scale.body_small, value_ink)),
    ))
    .gap(ROW_LINE_GAP);

    let mut children = vec![inflexible(mark_tile(mark, colors)), flexible(1, body)];
    if let Some(trailing) = trailing_slot(action, colors) {
        children.push(inflexible(trailing));
    }

    let box_ = stretch_column((
        Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            centered_row(children).gap(ROW_GAP),
        ),
        divider(colors.rule),
    ));

    match action {
        RowAction::Static => any(box_),
        RowAction::Picker(_) | RowAction::UpgradeSheet => any(GestureDetector(box_)
            .on_tap(move |state: &mut SettingsState| pages::row_tapped(state, action))),
    }
}

pub(super) fn button_row(
    mark: RowMark,
    label: &'static str,
    subtitle: &'static str,
    action_label: &'static str,
    on_tap: impl Fn(&mut SettingsState) + 'static,
    colors: &Palette,
) -> AnyView<SettingsState> // erasure: keep accumulator into Vec<AnyView>
{
    let theme = active_theme();
    let body = stretch_column((
        text(label).style(inked(&theme.type_scale.label_large, colors.fg)),
        text(subtitle).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
    ))
    .gap(ROW_LINE_GAP);
    let children = vec![
        inflexible(mark_tile(mark, colors)),
        flexible(1, body),
        inflexible(button(action_label, on_tap).style(ButtonStyle::Ghost)),
    ];
    any(stretch_column((
        Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            centered_row(children).gap(ROW_GAP),
        ),
        divider(colors.rule),
    )))
}

pub(super) fn mark_tile(mark: RowMark, colors: &Palette) -> impl View<SettingsState> {
    let theme = active_theme();
    let content = match mark {
        RowMark::Vector(source) => {
            Either::Left(icon(source).size(MARK_ICON).color(colors.fg_muted))
        }
        RowMark::Text(glyph) => {
            Either::Right(text(glyph).style(inked(&theme.type_scale.label_small, colors.fg_muted)))
        }
    };
    container(content)
        .fill(colors.raised)
        .radius(theme.shape.extra_small)
        .size_centered(MARK_BOX, MARK_BOX)
}

fn trailing_slot(action: RowAction, colors: &Palette) -> Option<impl View<SettingsState>> {
    match action {
        RowAction::Static => None,
        RowAction::Picker(_) | RowAction::UpgradeSheet => {
            Some(icon(CHEVRON_RIGHT).size(CHEVRON).color(colors.fg_faintest))
        }
    }
}

pub(super) const MARK_SERVER_URL: RowMark = RowMark::Vector(LINK);
/// The row's value carries the TLS posture, so the tile is not state-dependent.
pub(super) const MARK_TLS: RowMark = RowMark::Vector(LOCK);
pub(super) const MARK_ZELLIJ: RowMark = RowMark::Text("z");
pub(super) const MARK_HERDR: RowMark = RowMark::Text("h");
pub(super) const MARK_FONT_FAMILY: RowMark = RowMark::Text("abc");
/// Not the mock's literal `12`, which would go stale when the size changes.
pub(super) const MARK_FONT_SIZE: RowMark = RowMark::Vector(FORMAT_SIZE);
pub(super) const MARK_CURSOR: RowMark = RowMark::Text("\u{25ae}");
pub(super) const MARK_REFRESH: RowMark = RowMark::Vector(REFRESH);
pub(super) const MARK_LATENCY: RowMark = RowMark::Vector(SCHEDULE);
pub(super) const MARK_BELL: RowMark = RowMark::Vector(NOTIFICATIONS);

#[cfg(test)]
mod tests {
    use super::*;

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
        // `tests/navigation_motion.rs` tracks the largest rounded rect; the page tone is never one.
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
        for mark in [
            MARK_SERVER_URL,
            MARK_TLS,
            MARK_ZELLIJ,
            MARK_HERDR,
            MARK_FONT_FAMILY,
            MARK_FONT_SIZE,
            MARK_CURSOR,
            MARK_REFRESH,
            MARK_LATENCY,
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
