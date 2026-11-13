//! `.sess-card`: the whole box is the control. A tap attaches (on Tablet it selects); a long-press
//! opens the management sheet and never also fires the tap.

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult, InputEvent,
    LayoutCtx, PaintCtx, PaintScene, Role, SemanticsCtx, Size, Widget, build_child, rebuild_child,
    route_event_single, teardown_child,
};
use frust::icons::NOTIFICATIONS;
use frust::motion::AnimatedOpacity;
use frust::{
    AnyView, Axis, CrossAxisAlignment, EdgeInsets, Either, FlexChild, FlexView, GestureDetector,
    Padding, SizedBox, View, any, container, flexible, icon, inflexible, text,
};
use frust_glyph::{BadgeVariant, avatar, badge};

use crate::agents::AgentSummary;
use crate::features::sessions::domain::entities::{Session, SessionStatus};
use crate::ui::agent_marks::{count_pills, kind_tiles, summary_line};
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

use super::super::pages::{self, SessionsState};
use super::{active_theme, centered_row, h_gap};

const AVATAR_SIZE: f64 = 26.0;
const CARD_PAD: f64 = spacing::LG;
const ACCENT_WIDTH: f64 = 2.0;
const ROW_GAP: f64 = spacing::MD;
const PILL_GAP: f64 = spacing::XS;
const META_GAP: f64 = spacing::BASE;
const CARD_LINE_GAP: f64 = spacing::MD;
const PILL_PAD_X: f64 = spacing::MD;
const PILL_PAD_Y: f64 = spacing::SM;
/// The whole-card dim for a session that isn't running, reused for read-only create affordances.
pub(super) const IDLE_OPACITY: f64 = 0.55;
const MAX_PREVIEW_PILLS: usize = 2;
pub(super) const BELL_ICON: f64 = 14.0;
const MAX_KIND_TILES: usize = 4;
const NEEDS_YOU: &str = "needs you";
const THIS_DEVICE: &str = "\u{25cf} this device";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::features::sessions::presentation) enum CardMode {
    Compact,
    Tablet { selected: bool, previews: bool },
}

/// The surface plus, for a live session, the 2px `success` left accent: an accent-filled container
/// behind an `ACCENT_WIDTH`-inset surface, since `Container` has no left-accent knob.
///
/// The inset is unconditional so an idle card's content starts where a running card's does.
/// `fill` must be opaque: a translucent wash lets the accent through.
fn accented_card<State: 'static>(
    body: impl View<State>,
    fill: Color,
    radius: f64,
    border: Color,
    accent: Option<Color>,
) -> impl View<State> {
    let surface = container(body)
        .fill(fill)
        .border(border, HAIRLINE)
        .radius(radius);
    let inset = Padding(
        EdgeInsets {
            left: ACCENT_WIDTH,
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
        },
        surface,
    );
    let mut outer = container(inset).radius(radius);
    if let Some(accent) = accent {
        outer = outer.fill(accent);
    }
    outer
}

pub(in crate::features::sessions::presentation) fn session_card(
    session: &Session,
    colors: &Palette,
    mode: CardMode,
) -> impl View<SessionsState> {
    let theme = active_theme();
    let live = session.status.is_live();
    let (avatar_bg, avatar_ink) = if live {
        (colors.success_faint, colors.success)
    } else {
        (colors.raised, colors.fg_muted)
    };

    let mut top = vec![
        inflexible(
            avatar(session.initial())
                .size(AVATAR_SIZE)
                .background(avatar_bg)
                .foreground(avatar_ink)
                // `.sess-avatar` has no border; `avatar` always strokes one, so it is stroked in nothing.
                .border_color(Color::TRANSPARENT),
        ),
        h_gap(ROW_GAP),
        flexible(
            1,
            text(session.name.clone()).style(inked(&theme.type_scale.label_large, colors.fg)),
        ),
    ];
    if session.has_bell {
        top.push(h_gap(PILL_GAP));
        top.push(inflexible(bell_mark(colors)));
    }
    let agents = session
        .agents
        .as_ref()
        .filter(|summary| live && summary.total > 0);
    let needs_you = agents.is_some_and(|summary| summary.blocked > 0);
    let (status_label, status_tone) = if needs_you {
        (NEEDS_YOU, BadgeVariant::Warning)
    } else {
        (session.status.label(), status_variant(session.status))
    };
    top.extend([
        h_gap(PILL_GAP),
        inflexible(badge(status_label, status_tone)),
        h_gap(PILL_GAP),
        inflexible(badge(session.backend.label(), BadgeVariant::Neutral)),
    ]);
    let top = centered_row(top);

    let meta_style = inked(&theme.type_scale.label_small, colors.fg_dim);
    let mut meta = vec![
        inflexible(text(session.layout.panes_label()).style(meta_style.clone())),
        h_gap(META_GAP),
        inflexible(text(session.layout.tabs_label()).style(meta_style.clone())),
        h_gap(META_GAP),
        inflexible(text(session.layout.third.label()).style(meta_style)),
    ];
    if session.is_current {
        meta.push(h_gap(META_GAP));
        meta.push(inflexible(
            text(THIS_DEVICE).style(inked(&theme.type_scale.label_small, colors.amber)),
        ));
    }
    meta.push(spacer());
    let meta = centered_row(meta);

    let (selected, previews) = match mode {
        CardMode::Compact => (false, true),
        CardMode::Tablet { selected, previews } => (selected, previews),
    };
    let mut lines = vec![any(top), any(meta)];
    if let Some(summary) = agents {
        lines.push(any(agent_strip(summary, colors)));
    }
    if live && previews && !session.layout.preview.is_empty() {
        lines.push(any(pane_pills(&session.layout.preview, colors)));
    }
    let body = FlexView::new(Axis::Vertical, interleave(lines, CARD_LINE_GAP))
        .cross_axis(CrossAxisAlignment::Stretch);

    let (fill, border, accent) = if selected {
        (colors.amber_wash, colors.amber_border, Some(colors.amber))
    } else {
        (
            colors.surface,
            colors.outline,
            live.then_some(if needs_you {
                colors.warning
            } else {
                colors.success
            }),
        )
    };
    let card = accented_card(
        Padding(EdgeInsets::all(CARD_PAD), body),
        fill,
        theme.shape.medium,
        border,
        accent,
    );

    let tap_id = session.id.clone();
    let manage_id = session.id.clone();
    let pressable = GestureDetector(card)
        .on_tap(move |state: &mut SessionsState| match mode {
            CardMode::Compact => pages::attach(state, &tap_id),
            CardMode::Tablet { .. } => pages::select(state, &tap_id),
        })
        .on_long_press(move |state: &mut SessionsState| {
            pages::open_manage_sheet(state, &manage_id)
        });

    if live {
        Either::Left(pressable)
    } else {
        // No static opacity wrapper exists; `AnimatedOpacity` settles at its initial target with no animate-in.
        Either::Right(AnimatedOpacity(IDLE_OPACITY, pressable))
    }
}

pub(super) fn bell_mark(colors: &Palette) -> impl View<SessionsState> {
    icon(NOTIFICATIONS).size(BELL_ICON).color(colors.warning)
}

/// Never amber: a chip is not a selection.
pub(super) fn status_variant(status: SessionStatus) -> BadgeVariant {
    if status.is_live() {
        BadgeVariant::Success
    } else {
        BadgeVariant::Neutral
    }
}

fn pane_pills(preview: &[String], colors: &Palette) -> FlexView<SessionsState> {
    let theme = active_theme();
    let mut children = Vec::new();
    for title in preview.iter().take(MAX_PREVIEW_PILLS) {
        children.push(flexible(
            1,
            container(Padding(
                EdgeInsets::symmetric(PILL_PAD_X, PILL_PAD_Y),
                text(title.clone()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
            ))
            .fill(colors.base)
            .border(colors.rule, HAIRLINE)
            .radius(theme.shape.extra_small),
        ));
    }
    centered_row(children).gap(PILL_GAP)
}

fn agent_strip(summary: &AgentSummary, colors: &Palette) -> impl View<SessionsState> {
    let theme = active_theme();
    let noun = if summary.total == 1 {
        "agent"
    } else {
        "agents"
    };
    let mut children = vec![
        inflexible(kind_tiles::<SessionsState>(&summary.kinds, MAX_KIND_TILES)),
        h_gap(PILL_GAP),
        inflexible(
            text(format!("{} {noun}", summary.total))
                .style(inked(&theme.type_scale.label_small, colors.fg_dim)),
        ),
        spacer(),
    ];
    for (index, pill) in count_pills::<SessionsState>(summary)
        .into_iter()
        .enumerate()
    {
        if index > 0 {
            children.push(h_gap(PILL_GAP));
        }
        children.push(inflexible(pill));
    }
    Labeled {
        child: centered_row(children),
        label: summary_line(summary),
    }
}

struct Labeled<V> {
    child: V,
    label: String,
}

struct LabeledWidget {
    child: ChildPod,
    label: String,
}

impl<V: View<SessionsState>> View<SessionsState> for Labeled<V> {
    type Element = LabeledWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> LabeledWidget {
        LabeledWidget {
            child: build_child(&self.child, ctx),
            label: self.label.clone(),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut LabeledWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        if element.label != self.label {
            element.label.clone_from(&self.label);
            // Semantics-only, but PAINT is what dirties the semantics gate.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut LabeledWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for LabeledWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        self.child.layout_child(ctx, bc)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let label = self.label.clone();
        ctx.push_container(
            Role::Group,
            |node| node.set_label(label),
            |ctx| self.child.semantics_child(ctx),
        );
    }
}

fn spacer() -> FlexChild<SessionsState> {
    flexible(1, SizedBox::<SessionsState>(None, None))
}

fn interleave(views: Vec<AnyView<SessionsState>>, gap: f64) -> Vec<FlexChild<SessionsState>> {
    let mut children = Vec::with_capacity(views.len() * 2);
    for (index, view) in views.into_iter().enumerate() {
        if index > 0 {
            children.push(inflexible(SizedBox::<SessionsState>(None, Some(gap))));
        }
        children.push(inflexible(view));
    }
    children
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn liveness_is_success_and_never_the_brand_hue() {
        assert_eq!(
            status_variant(SessionStatus::Running),
            BadgeVariant::Success
        );
        assert_eq!(
            status_variant(SessionStatus::Resurrectible),
            BadgeVariant::Neutral
        );
    }

    #[test]
    fn the_idle_dim_matches_the_source() {
        assert!((IDLE_OPACITY - 0.55).abs() < 1e-9, "{IDLE_OPACITY}");
    }

    #[test]
    fn the_cards_meta_row_gap_is_the_sources_and_not_the_chip_gap() {
        // Two different 6/12px gaps live a few lines apart; a swap is invisible in review and obvious on screen.
        assert_eq!(META_GAP, 12.0);
        assert_eq!(PILL_GAP, 6.0);
    }
}
