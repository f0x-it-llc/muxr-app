//! `.sess-card` — one session card: the whole box is the control.
//!
//! **A tap attaches (on Tablet it selects instead); a long-press opens the
//! management sheet** — the redesign draws no per-card buttons.
//! [`GestureDetector`]'s long-press fires on the first pointer event after the
//! 500ms threshold and never also fires the tap, which is what makes
//! "long-press must not attach" a property of the framework rather than a
//! guard written here.
//!
//! Running and idle differ in four places: the 2px `success` left accent
//! (absent when idle, though its width is still reserved), the avatar's
//! wash and ink, the status chip, and — idle only — a whole-card dim to
//! [`IDLE_OPACITY`] plus no `.sess-panes` row at all. A bell mark follows the
//! name and an accent `● this device` span closes the meta row on every
//! class; a Tablet selection repaints the card in the amber selection wash.

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

/// `.sess-avatar{width:26px;height:26px}` — a widget dimension, not a
/// spacing gap, so it stays off the shared scale.
const AVATAR_SIZE: f64 = 26.0;
/// `.sess-card{padding:14px}`.
const CARD_PAD: f64 = spacing::LG;
/// `.sess-card{border-left:2px solid}` — the status accent's weight.
const ACCENT_WIDTH: f64 = 2.0;
/// `.sess-top{gap:10px}` — avatar to name.
const ROW_GAP: f64 = spacing::MD;
/// `.sess-badges{gap:6px}` / `.sess-panes{gap:6px}`.
const PILL_GAP: f64 = spacing::XS;
/// `.sess-meta{gap:12px}` — between the three meta spans.
const META_GAP: f64 = spacing::BASE;
/// `.sess-top{margin-bottom:10px}` / `.sess-meta{margin-bottom:10px}` — the
/// card's inner vertical rhythm.
const CARD_LINE_GAP: f64 = spacing::MD;
/// `.pane-pill{padding:8px 10px}`.
const PILL_PAD_X: f64 = spacing::MD;
const PILL_PAD_Y: f64 = spacing::SM;
/// `.sess-card.idle{opacity:0.55}` — the whole-card dim for a session that
/// isn't running, and (see [`super::list`]) the same "disabled" dim the
/// read-only create affordances reuse.
pub(super) const IDLE_OPACITY: f64 = 0.55;
/// How many `.pane-pill` previews a card shows (the redesign draws two).
const MAX_PREVIEW_PILLS: usize = 2;
/// The bell mark after a ringing session's name.
pub(super) const BELL_ICON: f64 = 14.0;
/// How many distinct agent kinds the strip draws as tiles before a `+N` tile.
const MAX_KIND_TILES: usize = 4;
/// The status badge's text when an agent is blocked on the user.
const NEEDS_YOU: &str = "needs you";
/// The meta span a session attached from this device carries.
const THIS_DEVICE: &str = "\u{25cf} this device";

/// How a card is placed: Compact's list, or the Tablet list column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::features::sessions::presentation) enum CardMode {
    /// A tap attaches.
    Compact,
    /// A tap selects; `previews` is off on the collapsed band.
    Tablet { selected: bool, previews: bool },
}

/// `.sess-card`'s surface (`fill`/`border`/`radius`) plus, for a live
/// session, the 2px `success` left status accent — an accent-filled
/// `container` behind an `ACCENT_WIDTH`-inset surface `container`, since
/// `frust::container` carries no left-accent knob by design (deferred: see
/// `frust-widgets::container`'s module docs' "Sizing + a child" section, "app
/// side layering over a plain `Container`"). The outer accent paints the
/// *whole* card's rounded rect first; the inner surface, offset right by
/// [`ACCENT_WIDTH`], paints on top of it — reproducing `border-left`'s
/// curved ends without a per-edge radius.
///
/// The inset is unconditional (`Padding` always reserves [`ACCENT_WIDTH`]),
/// independent of `accent` carrying a colour — `.sess-card{border-left:2px
/// solid transparent}` — so an idle card's content still starts exactly
/// where a running card's does and the list's left edge never jogs by 2px
/// row to row.
///
/// `fill` must be opaque: the inner surface is all that hides the accent's
/// solid colour, so a translucent wash here (the selected card once used
/// `amber_faint`) lets the accent through and the whole card reads as a
/// block of amber — hence the palette's opaque `amber_wash`.
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

/// See the [module docs](self).
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
                // `.sess-avatar` has no border of its own; `avatar` always
                // strokes one, so it is stroked in nothing.
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
    // The agent roll-up shows only on a live card with at least one agent.
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
    // An idle card draws no pills at all, so liveness gates the row as well
    // as the preview being non-empty.
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
        // Frust has no *static* opacity wrapper; `AnimatedOpacity` settles at
        // its initial target with no animate-in, so a pinned constant costs
        // one layer push and no extra frames. A card that later stops
        // running fades, which is the right behaviour anyway.
        Either::Right(AnimatedOpacity(IDLE_OPACITY, pressable))
    }
}

/// The warning-toned bell a ringing session or tab carries.
pub(super) fn bell_mark(colors: &Palette) -> impl View<SessionsState> {
    icon(NOTIFICATIONS).size(BELL_ICON).color(colors.warning)
}

/// The hue a status chip prints in — `success` for the live one, a neutral
/// wash for everything else. Never amber: a chip is not a selection.
pub(super) fn status_variant(status: SessionStatus) -> BadgeVariant {
    if status.is_live() {
        BadgeVariant::Success
    } else {
        BadgeVariant::Neutral
    }
}

/// `.sess-panes` — up to [`MAX_PREVIEW_PILLS`] equal-width (`flex:1`) pane
/// tiles.
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

/// `.sess-agents` — kind tiles and the `N agents` count at the left, the
/// per-status count pills at the right; announced as one
/// [`summary_line`].
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

/// Wraps `child` in a `Role::Group` semantics node carrying `label`; layout,
/// paint and events pass straight through.
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

/// A flexible spacer that pushes what follows it to the trailing edge — Flex
/// v1 has no `space-between`.
fn spacer() -> FlexChild<SessionsState> {
    flexible(1, SizedBox::<SessionsState>(None, None))
}

/// `[a, b, c]` with a `gap`-tall spacer between consecutive items, as flex
/// children.
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
        // The colour-role rule's own tripwire: a running chip is the
        // success variant, an idle one is neutral, and neither is amber.
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
        // `.sess-card.idle{opacity:0.55}` — a dim, not a disappearance.
        assert!((IDLE_OPACITY - 0.55).abs() < 1e-9, "{IDLE_OPACITY}");
    }

    #[test]
    fn the_cards_meta_row_gap_is_the_sources_and_not_the_chip_gap() {
        // Two different 6/12px gaps live a few lines apart in the source; a
        // swap is invisible in code review and obvious on screen.
        assert_eq!(META_GAP, 12.0);
        assert_eq!(PILL_GAP, 6.0);
    }
}
