//! The `/sessions` screen's widget clusters, transcribed from
//! `docs/redesign.html`'s **sessions** block and `docs/tablet.html`'s frame
//! 02 — one submodule per visual cluster: [`server_strip`] (`.conn-row`),
//! [`session_card`] (`.sess-card`), [`list`] (new-session button, empty and
//! failed states, the Tablet list column beside the detail), [`detail`] (the
//! Tablet detail pane), [`manage_sheet`] (long-press sheet rows) and
//! [`new_session_dialog`] (name + backend choice). Page-coupled by design
//! (`APP_FEATURES_ARCHITECTURE.md`'s widgets/ paragraph): each reads the
//! page's state type or calls its `pages::*` action shims directly.
//!
//! Amber is brand/CTA and the Tablet selection wash, never a health signal —
//! liveness is always `success`; a running session must never read as
//! "selected". Every element with a Glyph catalog counterpart uses it; the
//! rest (card accent, pane pills, `.conn-row` rule, dashed outline) composes
//! `frust::container`/`colored_box`/`divider` directly, each in the widget
//! module that needs it — the feature-local `chrome` primitives that used to
//! cover the gap are gone now that frust's own baseline carries them.
//! `frust_glyph::baseline()` wins where it and the HTML disagree.

mod detail;
mod list;
mod manage_sheet;
mod new_session_dialog;
mod server_strip;
mod session_card;

use frust::AnyView;
use frust::{Axis, CrossAxisAlignment, FlexChild, FlexView, SizedBox, any, container, inflexible};

use crate::ui::palette::Palette;

use super::pages::SessionsState;

pub(crate) use server_strip::host_of;

pub(in crate::features::sessions::presentation) use crate::ui::palette::active_theme;
pub(in crate::features::sessions::presentation) use detail::detail_pane;
pub(in crate::features::sessions::presentation) use list::{
    empty_view, loading_view, master_detail, new_session_row, tablet_list, tablet_list_pieces,
};
pub(in crate::features::sessions::presentation) use manage_sheet::sheet_body;
pub(in crate::features::sessions::presentation) use new_session_dialog::open_new_session_dialog;
pub(in crate::features::sessions::presentation) use server_strip::{DOT, server_strip};
pub(in crate::features::sessions::presentation) use session_card::{CardMode, session_card};

/// Where the list's load stands: in flight, failed before any list landed,
/// or loaded (possibly empty).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::features::sessions::presentation) enum LoadPhase {
    Loading,
    Failed,
    Loaded,
}

impl LoadPhase {
    /// The phase the controller's `loaded`/`first_load_settled` pair names.
    pub(in crate::features::sessions::presentation) fn of(loaded: bool, settled: bool) -> Self {
        match (loaded, settled) {
            (true, _) => LoadPhase::Loaded,
            (false, true) => LoadPhase::Failed,
            (false, false) => LoadPhase::Loading,
        }
    }
}

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

/// `views` stacked and stretched, with a `gap`-tall spacer between neighbours.
pub(super) fn column(views: Vec<AnyView<SessionsState>>, gap: f64) -> FlexView<SessionsState> {
    let mut children = Vec::with_capacity(views.len() * 2);
    for (index, view) in views.into_iter().enumerate() {
        if index > 0 {
            children.push(inflexible(SizedBox::<SessionsState>(None, Some(gap))));
        }
        children.push(inflexible(view));
    }
    FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch)
}
