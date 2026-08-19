//! `.sess-card` — one session card: the whole box is the control.
//!
//! **A tap attaches; a long-press opens the management sheet** — the
//! redesign draws no per-card buttons. [`GestureDetector`]'s long-press
//! fires on the first pointer event after the 500ms threshold and never
//! also fires the tap, which is what makes "long-press must not attach" a
//! property of the framework rather than a guard written here.
//!
//! Running and idle differ in four places: the 2px `success` left accent
//! (absent when idle, though its width is still reserved), the avatar's
//! wash and ink, the status chip, and — idle only — a whole-card dim to
//! [`IDLE_OPACITY`] plus no `.sess-panes` row at all.

use frust::authoring::Color;
use frust::motion::AnimatedOpacity;
use frust::{
    AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexChild, FlexView, GestureDetector, Padding,
    SizedBox, View, any, container, flexible, inflexible, text,
};
use frust_glyph::{BadgeVariant, avatar, badge};

use crate::features::sessions::domain::entities::{Session, SessionStatus};
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

use super::super::pages::{self, SessionsState};
use super::{active_theme, h_gap, row};

/// `.sess-avatar{width:26px;height:26px}` — a widget dimension, not a
/// spacing gap, so it stays off the shared scale.
const AVATAR_SIZE: f64 = 26.0;
/// `.sess-card{padding:14px}`.
const CARD_PAD: f64 = spacing::LG;
/// Hairline border width in logical px (`1px solid` on every bordered rule in
/// `docs/redesign.html`).
const HAIRLINE: f64 = 1.0;
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
fn accented_card<State: 'static>(
    body: impl View<State>,
    fill: Color,
    radius: f64,
    border: Color,
    accent: Option<Color>,
) -> AnyView<State> {
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
    any(outer)
}

/// See the [module docs](self).
pub(in crate::features::sessions::presentation) fn session_card(
    session: &Session,
    colors: &Palette,
) -> AnyView<SessionsState> {
    let theme = active_theme();
    let live = session.status.is_live();
    let (avatar_bg, avatar_ink) = if live {
        (colors.success_faint, colors.success)
    } else {
        (colors.raised, colors.fg_muted)
    };

    let top = row(vec![
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
        h_gap(PILL_GAP),
        inflexible(badge(
            session.status.label(),
            status_variant(session.status),
        )),
        h_gap(PILL_GAP),
        inflexible(badge(session.backend.label(), BadgeVariant::Neutral)),
    ]);

    let meta_style = inked(&theme.type_scale.label_small, colors.fg_dim);
    let meta = row(vec![
        inflexible(text(session.layout.panes_label()).style(meta_style.clone())),
        h_gap(META_GAP),
        inflexible(text(session.layout.tabs_label()).style(meta_style.clone())),
        h_gap(META_GAP),
        inflexible(text(session.layout.third.label()).style(meta_style)),
        spacer(),
    ]);

    let mut lines = vec![any(top), any(meta)];
    // An idle card draws no pills at all, so liveness gates the row as well
    // as the preview being non-empty.
    if live && !session.layout.preview.is_empty() {
        lines.push(any(pane_pills(&session.layout.preview, colors)));
    }
    let body = FlexView::new(Axis::Vertical, interleave(lines, CARD_LINE_GAP))
        .cross_axis(CrossAxisAlignment::Stretch);

    let card = accented_card(
        Padding(EdgeInsets::all(CARD_PAD), body),
        colors.surface,
        theme.shape.medium,
        colors.outline,
        live.then_some(colors.success),
    );

    let attach_id = session.id.clone();
    let manage_id = session.id.clone();
    let pressable = GestureDetector(card)
        .on_tap(move |state: &mut SessionsState| pages::attach(state, &attach_id))
        .on_long_press(move |state: &mut SessionsState| {
            pages::open_manage_sheet(state, &manage_id)
        });

    if live {
        any(pressable)
    } else {
        // Frust has no *static* opacity wrapper; `AnimatedOpacity` settles at
        // its initial target with no animate-in, so a pinned constant costs
        // one layer push and no extra frames. A card that later stops
        // running fades, which is the right behaviour anyway.
        any(AnimatedOpacity(IDLE_OPACITY, pressable))
    }
}

/// The hue a status chip prints in — `success` for the live one, a neutral
/// wash for everything else. Never amber: a chip is not a selection.
fn status_variant(status: SessionStatus) -> BadgeVariant {
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
        if !children.is_empty() {
            children.push(h_gap(PILL_GAP));
        }
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
    row(children)
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
