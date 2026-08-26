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
//! element with a Glyph catalog counterpart uses it; the rest (card accent,
//! pane pills, `.conn-row` rule, dashed outline) composes `frust::container`/
//! `colored_box`/`divider` directly, each in the widget module that needs it
//! — the feature-local `chrome` primitives that used to cover the gap are
//! gone now that frust's own baseline carries them. `frust_glyph::baseline()`
//! wins where it and the HTML disagree.

mod list;
mod manage_sheet;
mod server_strip;
mod session_card;

use frust::AnyView;
use frust::{Axis, CrossAxisAlignment, FlexChild, FlexView, SizedBox, any, container, inflexible};

use crate::ui::palette::Palette;

use super::pages::SessionsState;

pub(crate) use server_strip::host_of;

pub(in crate::features::sessions::presentation) use crate::ui::palette::active_theme;
pub(in crate::features::sessions::presentation) use list::{
    empty_view, loading_view, new_session_row,
};
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
    any(container(body).fill(colors.base))
}

pub(super) fn h_gap(gap: f64) -> FlexChild<SessionsState> {
    inflexible(SizedBox::<SessionsState>(Some(gap), None))
}

pub(super) fn row(children: Vec<FlexChild<SessionsState>>) -> FlexView<SessionsState> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}
