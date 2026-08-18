//! The `/sessions` screen's widget clusters, transcribed from
//! `docs/redesign.html`'s **sessions** block — one submodule per visual
//! cluster: [`server_strip`] (`.conn-row`), [`session_card`] (`.sess-card`),
//! [`list`] (new-session button + empty state), [`manage_sheet`] (long-press
//! sheet rows). Page-coupled by design (`docs/FEATURES_ARCHITECTURE.md`'s
//! widgets/ paragraph): each reads the page's state type or calls its
//! `pages::*` action shims directly.
//!
//! Amber is brand/CTA only, never a health/selection signal — liveness is
//! always `success`; a running session must never read as "selected". Every
//! element with a Glyph catalog counterpart uses it; [`super::chrome`] covers
//! what the catalog doesn't (card accent, pane pills, `.conn-row` rule,
//! dashed outline). `frust_glyph::baseline()` wins where it and the HTML
//! disagree.

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

/// Page tone behind `body`: `--bg-base`, not the shell's `--bg-surface`
/// clear colour — without it, cards would show no visible edge against an
/// unpainted background.
pub(in crate::features::sessions::presentation) fn page_tone(
    body: impl frust::View<SessionsState>,
    colors: &Palette,
) -> AnyView<SessionsState> {
    any(panel(body, colors.base, 0.0))
}

pub(super) fn h_gap(gap: f64) -> FlexChild<SessionsState> {
    inflexible(SizedBox::<SessionsState>(Some(gap), None))
}

pub(super) fn row(children: Vec<FlexChild<SessionsState>>) -> FlexView<SessionsState> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}
