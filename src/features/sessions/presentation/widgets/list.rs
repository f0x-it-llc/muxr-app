//! The create entry points (`.new-sess-btn`, the empty state) and the Tablet list column with its
//! failed-load Retry and the [`master_detail`] row.

use frust::authoring::DashPattern;
use frust::motion::AnimatedOpacity;
use frust::{
    Align, Alignment, AnyView, Axis, BorderStyle, ButtonStyle, CrossAxisAlignment, EdgeInsets,
    Either, FlexView, GestureDetector, Padding, SizedBox, View, any, button, colored_box,
    container, divider, flexible, inflexible, row, safe_area, text,
};
use frust_glyph::empty_state;

use crate::features::sessions::domain::entities::Session;
use crate::ui::paint::{DASH_OFF, DASH_ON, HAIRLINE};
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

use super::super::pages::{self, SessionsState};
use super::session_card::{CardMode, IDLE_OPACITY, session_card};
use super::{LoadPhase, active_theme, centered_row, gapped_column, h_gap};

const NEW_ROW_PAD: f64 = 13.0;
const NEW_ROW_GAP: f64 = spacing::XS;
const NEW_SESSION_PLUS: &str = "+";
const NEW_SESSION_LABEL: &str = "new session";
const LOADING_LABEL: &str = "loading sessions\u{2026}";
const COLUMN_WIDTH_FULL: f64 = 300.0;
const COLUMN_WIDTH_COLLAPSED: f64 = 240.0;
const COLUMN_PAD: f64 = spacing::MD;
const COLUMN_GAP: f64 = spacing::SM;

/// `read_only` disables the row with a no-op `on_tap` plus the [`IDLE_OPACITY`] dim: one "inert" language.
pub(in crate::features::sessions::presentation) fn new_session_row(
    colors: &Palette,
    read_only: bool,
) -> impl View<SessionsState> {
    let theme = active_theme();
    let label = centered_row(vec![
        inflexible(text(NEW_SESSION_PLUS).style(inked(&theme.type_scale.body_small, colors.amber))),
        h_gap(NEW_ROW_GAP),
        inflexible(
            text(NEW_SESSION_LABEL).style(inked(&theme.type_scale.body_small, colors.fg_muted)),
        ),
    ]);
    let dashed = GestureDetector(
        container(Padding(
            EdgeInsets::all(NEW_ROW_PAD),
            Align(Alignment::CENTER, label),
        ))
        .border(colors.outline, HAIRLINE)
        .border_style(BorderStyle::Dashed(DashPattern::new(DASH_ON, DASH_OFF)))
        .radius(theme.shape.medium),
    )
    .on_tap(move |state: &mut SessionsState| {
        if !read_only {
            pages::open_create_dialog(state);
        }
    });
    if read_only {
        Either::Left(AnimatedOpacity(IDLE_OPACITY, dashed))
    } else {
        Either::Right(dashed)
    }
}

/// One muted line padded to the band the dashed row takes once it settles; nothing here is pressable.
pub(in crate::features::sessions::presentation) fn loading_view(
    colors: &Palette,
) -> impl View<SessionsState> {
    let theme = active_theme();
    Padding(
        EdgeInsets::all(NEW_ROW_PAD),
        Align(
            Alignment::CENTER,
            text(LOADING_LABEL).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        ),
    )
}

pub(in crate::features::sessions::presentation) fn tablet_list_pieces(
    sessions: &[Session],
    phase: LoadPhase,
    read_only: bool,
    selected: Option<&str>,
    previews: bool,
    colors: &Palette,
) -> Vec<AnyView<SessionsState>> // erasure: keep accumulator into Vec<AnyView>
{
    match phase {
        LoadPhase::Failed => return vec![any(failed_view())],
        LoadPhase::Loaded if sessions.is_empty() => return vec![any(empty_view(read_only))],
        LoadPhase::Loaded | LoadPhase::Loading => {}
    }
    let mut pieces: Vec<AnyView<SessionsState>> = sessions
        .iter()
        .map(|session| {
            let selected = selected == Some(session.id.as_str());
            any(session_card(
                session,
                colors,
                CardMode::Tablet { selected, previews },
            ))
        })
        .collect();
    if phase == LoadPhase::Loading {
        pieces.push(any(loading_view(colors)));
    }
    pieces
}

pub(in crate::features::sessions::presentation) fn tablet_list(
    pieces: Vec<AnyView<SessionsState>>,
) -> impl View<SessionsState> {
    Padding(
        EdgeInsets::all(COLUMN_PAD),
        gapped_column(pieces, COLUMN_GAP),
    )
}

pub(in crate::features::sessions::presentation) fn master_detail(
    strip: impl View<SessionsState>,
    list: impl View<SessionsState>,
    footer: Option<impl View<SessionsState>>,
    detail: impl View<SessionsState>,
    full: bool,
    colors: &Palette,
) -> impl View<SessionsState> {
    let mut side = vec![inflexible(strip), flexible(1, list)];
    if let Some(footer) = footer {
        side.push(inflexible(divider(colors.rule)));
        side.push(inflexible(Padding(EdgeInsets::all(COLUMN_PAD), footer)));
    }
    let side =
        container(FlexView::new(Axis::Vertical, side).cross_axis(CrossAxisAlignment::Stretch))
            .fill(colors.surface);
    let width = if full {
        COLUMN_WIDTH_FULL
    } else {
        COLUMN_WIDTH_COLLAPSED
    };
    let edge = colored_box::<SessionsState>()
        .size(HAIRLINE, 0.0)
        .fill(colors.rule);
    let body = row()
        .child(SizedBox::<SessionsState>(Some(width), None).child(side))
        .child(edge)
        .flex(1, detail)
        .cross_axis(CrossAxisAlignment::Stretch);
    safe_area(body).top(false)
}

fn failed_view() -> impl View<SessionsState> {
    empty_state(
        "Couldn't load sessions",
        "Connection problem \u{2014} please try again.",
    )
    .glyph("\u{25a3}")
    .action(
        button("Retry", |state: &mut SessionsState| {
            pages::retry_first_load(state)
        })
        .style(ButtonStyle::Secondary),
    )
}

/// `read_only` disables the action the same way [`new_session_row`] does.
pub(in crate::features::sessions::presentation) fn empty_view(
    read_only: bool,
) -> impl View<SessionsState> {
    let style = if read_only {
        ButtonStyle::Ghost
    } else {
        ButtonStyle::Primary
    };
    empty_state("No sessions", "Create one to start working on this server.")
        .glyph("\u{25a3}")
        .action(
            button(
                format!("{NEW_SESSION_PLUS} {NEW_SESSION_LABEL}"),
                move |state: &mut SessionsState| {
                    if !read_only {
                        pages::open_create_dialog(state);
                    }
                },
            )
            .style(style),
        )
}
