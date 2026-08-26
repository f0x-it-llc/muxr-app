//! `.new-sess-btn` and the empty state — the screen's only two create entry
//! points now that the stock app bar's own `+` is gone.

use frust::authoring::DashPattern;
use frust::motion::AnimatedOpacity;
use frust::{
    Align, Alignment, AnyView, BorderStyle, ButtonStyle, EdgeInsets, GestureDetector, Padding, any,
    button, container, inflexible, text,
};
use frust_glyph::empty_state;

use crate::ui::paint::{DASH_OFF, DASH_ON, HAIRLINE};
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

use super::super::pages::{self, SessionsState};
use super::session_card::IDLE_OPACITY;
use super::{active_theme, h_gap, row};

/// `.new-sess-btn{padding:13px}` — no on-scale `ui::spacing` match.
const NEW_ROW_PAD: f64 = 13.0;
/// Gap between the amber `+` and the "new session" label — the source's
/// `<b>+</b> new session` is one text node whose space is the gap; composed
/// as two runs, it needs a real one.
const NEW_ROW_GAP: f64 = spacing::XS;
/// The amber call-to-action mark on the new-session row (`.new-sess-btn b`).
const NEW_SESSION_PLUS: &str = "+";
/// The new-session row's label — one spelling, shared by the row and the
/// empty state's own action.
const NEW_SESSION_LABEL: &str = "new session";
/// What the list says while its first load is still in flight.
const LOADING_LABEL: &str = "loading sessions\u{2026}";

/// The dashed, centre-aligned row that closes the list. The `+` is amber
/// because it is the call to action; the label beside it is muted.
///
/// `read_only` "disables" it the way `ui::dialogs`' submit button does
/// (frust's `Button` has no disabled variant): a no-op `on_tap` is the
/// behavioral half, [`IDLE_OPACITY`]'s whole-element dim the visual half —
/// one "this is inert right now" language for the screen.
pub(in crate::features::sessions::presentation) fn new_session_row(
    colors: &Palette,
    read_only: bool,
) -> AnyView<SessionsState> {
    let theme = active_theme();
    let label = row(vec![
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
        any(AnimatedOpacity(IDLE_OPACITY, dashed))
    } else {
        any(dashed)
    }
}

/// What closes the list while the first load is still in flight: one muted
/// line, padded to the band the dashed row takes once it settles. No border
/// and no action — nothing here is pressable yet.
pub(in crate::features::sessions::presentation) fn loading_view(
    colors: &Palette,
) -> AnyView<SessionsState> {
    let theme = active_theme();
    any(Padding(
        EdgeInsets::all(NEW_ROW_PAD),
        Align(
            Alignment::CENTER,
            text(LOADING_LABEL).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        ),
    ))
}

/// What the list shows once every session has been killed — Glyph's own
/// [`empty_state`] panel with the same create action the dashed row carries.
/// `read_only` disables the action the same way [`new_session_row`] does.
pub(in crate::features::sessions::presentation) fn empty_view(
    read_only: bool,
) -> AnyView<SessionsState> {
    let style = if read_only {
        ButtonStyle::Ghost
    } else {
        ButtonStyle::Primary
    };
    any(
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
            ),
    )
}
