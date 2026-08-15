//! The `/sessions` screen's individually-composable pieces, transcribed from
//! `docs/redesign.html`'s **sessions** block (CSS lines 92-113, markup lines
//! 227-257) — one submodule per visual cluster, so
//! [`pages`](super::pages)'s `Component::build` reads as an arrangement
//! rather than a wall of nesting (the `features::connect` screen precedent):
//!
//! - [`server_strip`] — `.conn-row`: the heartbeat dot, version meta,
//!   latency, the `insecure` chip.
//! - [`session_card`] — `.sess-card`: accent, avatar, name, chips, meta row,
//!   pane pills.
//! - [`list`] — `.new-sess-btn` and the empty state, the screen's two create
//!   entry points.
//! - [`manage_sheet`] — the long-press management sheet's rows.
//!
//! # Colour roles: amber is the brand, green is health
//!
//! Amber appears only as brand/call-to-action (the top bar's mark, the `+`
//! on the new-session row) and never as a health or selection signal —
//! liveness is `success` (the card's left accent, its avatar wash, the
//! strip's dot). A running session must never read as "selected".
//!
//! # Real components over hand-composed chrome
//!
//! Every element with a Glyph catalog counterpart uses it — `badge` for
//! chips, `avatar` for `.sess-avatar`, `empty_state` for the empty list,
//! `button` for the sheet's rows. [`super::chrome`]'s panel/swatch/dashed-box
//! cover what the catalog has no counterpart for: the card's 2px left
//! accent, the pane pills, the `.conn-row` rule, the dashed new-session
//! outline.
//!
//! `Theme::glyph_baseline()` is canonical where it and the HTML disagree.

mod list;
mod manage_sheet;
mod server_strip;
mod session_card;

use frust::AnyView;
use frust::{Axis, CrossAxisAlignment, FlexChild, FlexView, SizedBox, any, inflexible};

use crate::ui::palette::Palette;

use super::chrome::panel;
use super::pages::SessionsState;

pub(crate) use server_strip::host_of;

pub(in crate::features::sessions::presentation) use crate::ui::palette::active_theme;
pub(in crate::features::sessions::presentation) use list::{empty_view, new_session_row};
pub(in crate::features::sessions::presentation) use manage_sheet::sheet_body;
pub(in crate::features::sessions::presentation) use server_strip::{DOT, server_strip};
pub(in crate::features::sessions::presentation) use session_card::session_card;

/// The screen's page tone behind `body` — `--bg-base`, which the shell's own
/// clear colour (`--bg-surface`, the *card* tone) is not, so a screen that
/// painted nothing would show cards with no visible edge at all.
pub(in crate::features::sessions::presentation) fn page_tone(
    body: impl frust::View<SessionsState>,
    colors: &Palette,
) -> AnyView<SessionsState> {
    any(panel(body, colors.base, 0.0))
}

/// A `gap`-wide horizontal spacer, shared by every row-shaped piece.
pub(super) fn h_gap(gap: f64) -> FlexChild<SessionsState> {
    inflexible(SizedBox::<SessionsState>(Some(gap), None))
}

/// A vertically-centered row, shared by every row-shaped piece.
pub(super) fn row(children: Vec<FlexChild<SessionsState>>) -> FlexView<SessionsState> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}
