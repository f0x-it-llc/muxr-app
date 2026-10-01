//! The Tablet's action glue, kept out of `pages.rs` so that file stays scaffold-only:
//! [`rail_view`] (the docked or mini rail beside the card), [`AfterCommit`] (what a
//! landed tree commit does next), and the rail rows' overflow-menu dispatch
//! ([`Actions::open_row_menu`]). Everything here runs over [`pages`](super::pages)'
//! [`Actions`] bundle; nothing holds state of its own.

use frust::authoring::{Rect, WindowInsets};
use frust::{AnyView, Get, PopResult, use_context};
use frust_glyph::show_glyph_menu;

use crate::AppState;
use crate::features::terminal::domain::feed::LinkStatus;
use crate::ui::sheet::SheetHandle;

use super::controller::TerminalController;
use super::pages::{Actions, SHEET_CLOSE_DELAY, TerminalState, tree_view};
use super::widgets::rail::{MiniData, RailHead, docked_rail, mini_rail, rail_summary};
use super::widgets::side_sheet_tree::{
    RowMenuChoice, RowTarget, TreeMode, row_menu, row_menu_choice,
};

impl Actions {
    /// Open the docked rail's overflow menu for `target`, anchored at `anchor`,
    /// and run whichever [`RowMenuChoice`] it answers with.
    pub(super) fn open_row_menu(&self, anchor: Rect, target: &RowTarget) {
        let (entries, target) = (target.clone(), target.clone());
        let actions = self.clone();
        show_glyph_menu(
            &self.overlays,
            anchor,
            move || row_menu(&entries),
            move |_app: &mut AppState, result: PopResult| {
                let choice = result
                    .take::<usize>()
                    .and_then(|index| row_menu_choice(&target, index));
                match (choice, &target) {
                    (Some(RowMenuChoice::Rename), RowTarget::Space { id, name }) => {
                        actions.open_rename_space_dialog(id, name);
                    }
                    (Some(RowMenuChoice::Rename), RowTarget::Tab { tab_id, label, .. }) => {
                        actions.open_rename_tab_dialog(*tab_id, label);
                    }
                    (
                        Some(RowMenuChoice::Rename),
                        RowTarget::Pane {
                            id,
                            is_plugin,
                            title,
                        },
                    ) => {
                        actions.open_rename_pane_dialog(*id, *is_plugin, title);
                    }
                    (Some(RowMenuChoice::NewHere), RowTarget::Space { id, .. }) => {
                        actions.open_new_tab_dialog_in(Some(id));
                    }
                    (Some(RowMenuChoice::NewHere), RowTarget::Tab { path, .. }) => {
                        actions.new_tree_pane(path);
                    }
                    _ => {}
                }
            },
        );
    }
}

/// The Tablet's rail beside the card: the full rail, or the mini rail in the
/// collapsed band and in immersive (`mini`), so the tree is never out of reach.
pub(super) fn rail_view(
    actions: &Actions,
    mini: bool,
    fullscreen: bool,
    read_only: bool,
    link: Option<LinkStatus>,
) -> AnyView<TerminalState> {
    let controller = &actions.controller;
    let top = match use_context::<WindowInsets>() {
        Some(insets) if fullscreen => insets.padding().top,
        _ => 0.0,
    };
    let tabs = controller.tabs.get();
    if mini {
        let (spaces, active_tab) = (controller.spaces.get(), controller.active_tab.get());
        let data = MiniData {
            spaces: &spaces,
            tabs: &tabs,
            active_tab: &active_tab,
            fullscreen,
            top,
        };
        return mini_rail(&data, &actions.mini_actions());
    }
    let spaces = controller
        .has_spaces()
        .then(|| controller.spaces.get().len());
    let name = controller
        .session_name
        .get()
        .or_else(|| controller.session_id())
        .unwrap_or_default();
    let head = RailHead {
        name: &name,
        summary: rail_summary(spaces, &tabs),
        link,
        read_only,
    };
    let detach = actions.clone();
    docked_rail(
        &head,
        tree_view(actions, TreeMode::Docked, AfterCommit::Rest),
        top,
        move |_state: &mut TerminalState| detach.detach(),
    )
}

/// What a landed tree commit does next: the sheet closes a beat later, the
/// docked rail stays and re-rests on where the session now is.
#[derive(Clone)]
pub(super) enum AfterCommit {
    Close(SheetHandle),
    Rest,
}

impl AfterCommit {
    pub(super) async fn land(&self, controller: &TerminalController) {
        match self {
            AfterCommit::Close(sheet) => sheet.close_after(SHEET_CLOSE_DELAY).await,
            AfterCommit::Rest => controller.settle_rest(),
        }
    }
}
