//! The `/sessions` screen's widget clusters, page-coupled by design: each reads the page's state
//! type or calls its `pages::*` action shims.
//! Amber is brand/CTA and the Tablet selection wash, never a health signal: liveness is `success`.

mod detail;
mod list;
mod manage_sheet;
mod new_session_dialog;
mod server_strip;
mod session_card;
mod snapshot;
mod switcher_fab;

use frust::View;
use frust::{Axis, CrossAxisAlignment, FlexChild, FlexView, SizedBox, container, inflexible};

use crate::ui::palette::Palette;

use super::pages::SessionsState;

pub(crate) use server_strip::host_of;
pub(crate) use snapshot::{built as snapshots_built, sessions_snapshot};
pub(crate) use switcher_fab::{shell_responsive, switcher_fab};

pub(in crate::features::sessions::presentation) use crate::ui::palette::active_theme;
pub(in crate::features::sessions::presentation) use detail::detail_pane;
pub(in crate::features::sessions::presentation) use list::{
    empty_view, loading_view, master_detail, new_session_row, tablet_list, tablet_list_pieces,
};
pub(in crate::features::sessions::presentation) use manage_sheet::sheet_body;
pub(in crate::features::sessions::presentation) use new_session_dialog::open_new_session_dialog;
pub(in crate::features::sessions::presentation) use server_strip::{DOT, server_strip};
pub(in crate::features::sessions::presentation) use session_card::{CardMode, session_card};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::features::sessions::presentation) enum LoadPhase {
    Loading,
    Failed,
    Loaded,
}

impl LoadPhase {
    pub(in crate::features::sessions::presentation) fn of(loaded: bool, settled: bool) -> Self {
        match (loaded, settled) {
            (true, _) => LoadPhase::Loaded,
            (false, true) => LoadPhase::Failed,
            (false, false) => LoadPhase::Loading,
        }
    }
}

/// Page tone behind `body`: `--bg-base`, so cards show an edge against the shell's surface clear colour.
pub(in crate::features::sessions::presentation) fn page_tone<V: View<SessionsState>>(
    body: V,
    colors: &Palette,
) -> impl View<SessionsState> + use<V> {
    container(body).fill(colors.base)
}

pub(super) fn h_gap(gap: f64) -> FlexChild<SessionsState> {
    inflexible(SizedBox::<SessionsState>(Some(gap), None))
}

pub(super) fn centered_row(children: Vec<FlexChild<SessionsState>>) -> FlexView<SessionsState> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

pub(super) fn gapped_column<V: View<SessionsState>>(
    views: Vec<V>,
    gap: f64,
) -> FlexView<SessionsState> {
    let children = views.into_iter().map(inflexible).collect();
    FlexView::new(Axis::Vertical, children)
        .gap(gap)
        .cross_axis(CrossAxisAlignment::Stretch)
}
