//! The per-session management sheet a long-press opens: Attach / Rename… /
//! Kill…, over `AppState` like the dialogs (a sheet is a page of the root
//! overlay host, not this screen's own component tree).
//!
//! Rename/Kill close through [`SheetHandle::close_then`] rather than a bare
//! `close`: both push a dialog onto the sheet's own overlay host, and an
//! unchained push would land several frames *ahead* of the sheet's own
//! staged exit, so the eventual pop would take the dialog it opened instead
//! of the sheet. `close_then` chains the push to the exit's completion frame
//! instead. Attach carries no such hazard — it pushes the `/terminal`
//! **route**, a different navigator than this sheet's overlay host — and
//! stays on a plain `close`.
//!
//! `disabled` is always `false` for [`ManageAction::Attach`] — only
//! create/rename/kill are read-only-gated (see
//! [`super::super::pages::Actions::open_manage_sheet`]).

use frust::{
    Axis, ButtonStyle, CrossAxisAlignment, FlexView, SizedBox, View, button, inflexible, text,
};

use crate::AppState;
use crate::ui::palette::{Palette, inked};
use crate::ui::sheet::SheetHandle;
use crate::ui::spacing;

use super::super::pages::Actions;
use super::active_theme;

/// The gap between the sheet's heading and its rows, and between the rows
/// themselves.
const SHEET_GAP: f64 = spacing::LG;
const SHEET_ROW_GAP: f64 = spacing::TIGHT;

/// The whole sheet body: heading plus the three action rows. `id`/`name`
/// name the long-pressed session; `read_only` gates Rename/Kill, never
/// Attach.
pub(in crate::features::sessions::presentation) fn sheet_body(
    actions: &Actions,
    sheet: &SheetHandle,
    id: &str,
    name: String,
    read_only: bool,
) -> impl View<AppState> {
    let heading = sheet_heading(name);
    let rows = [
        sheet_action(actions, sheet, id, ManageAction::Attach, false),
        sheet_action(actions, sheet, id, ManageAction::Rename, read_only),
        sheet_action(actions, sheet, id, ManageAction::Kill, read_only),
    ];
    let mut children = vec![
        inflexible(heading),
        inflexible(SizedBox::<AppState>(None, Some(SHEET_GAP))),
    ];
    for (index, row) in rows.into_iter().enumerate() {
        if index > 0 {
            children.push(inflexible(SizedBox::<AppState>(None, Some(SHEET_ROW_GAP))));
        }
        children.push(inflexible(row));
    }
    FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch)
}

/// The sheet's header: the session's own name, so a sheet opened from a
/// long-press says *which* session it is about.
fn sheet_heading(title: impl Into<String>) -> impl View<AppState> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    text(title.into()).style(inked(&theme.type_scale.label_large, colors.fg))
}

/// One row: a full-width, labelled action. Ordinary rows are `Secondary`;
/// `destructive` swaps in the danger style; `disabled` overrides either with
/// `Ghost` — the row stays on screen, a tap on it does nothing.
fn sheet_row(
    label: impl Into<String>,
    destructive: bool,
    disabled: bool,
    on_press: impl Fn(&mut AppState) + 'static,
) -> impl View<AppState> {
    let style = if disabled {
        ButtonStyle::Ghost
    } else if destructive {
        ButtonStyle::Danger
    } else {
        ButtonStyle::Secondary
    };
    button(label.into(), move |state: &mut AppState| {
        if !disabled {
            on_press(state);
        }
    })
    .style(style)
}

/// The three things the management sheet can do to a session.
#[derive(Clone, Copy)]
enum ManageAction {
    Attach,
    Rename,
    Kill,
}

impl ManageAction {
    /// The row's label — the two that push a dialog carry the platform
    /// ellipsis convention; Attach acts immediately.
    fn label(self) -> &'static str {
        match self {
            ManageAction::Attach => "Attach",
            ManageAction::Rename => "Rename\u{2026}",
            ManageAction::Kill => "Kill\u{2026}",
        }
    }

    fn destructive(self) -> bool {
        matches!(self, ManageAction::Kill)
    }
}

/// One row, wired to `action` for session `id`. See the [module docs](self)
/// for why Rename/Kill chain through [`SheetHandle::close_then`] while
/// Attach stays a plain `close`.
fn sheet_action(
    actions: &Actions,
    sheet: &SheetHandle,
    id: &str,
    action: ManageAction,
    disabled: bool,
) -> impl View<AppState> {
    let actions = actions.clone();
    let sheet = sheet.clone();
    let id = id.to_string();
    sheet_row(
        action.label(),
        action.destructive(),
        disabled,
        move |_app: &mut AppState| match action {
            ManageAction::Attach => {
                // Through the sheet's own handle rather than a bare `pop`, so
                // a second row tap inside the one-rebuild window a queued pop
                // leaves open cannot pop whatever is under it.
                sheet.close();
                actions.attach(&id);
            }
            ManageAction::Rename => {
                let actions = actions.clone();
                let id = id.clone();
                sheet.close_then(move || actions.open_rename_dialog(&id));
            }
            ManageAction::Kill => {
                let actions = actions.clone();
                let id = id.clone();
                sheet.close_then(move || actions.open_kill_dialog(&id));
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_management_sheet_offers_exactly_three_rows_one_of_them_destructive() {
        let labels: Vec<&str> = [
            ManageAction::Attach,
            ManageAction::Rename,
            ManageAction::Kill,
        ]
        .into_iter()
        .map(ManageAction::label)
        .collect();
        assert_eq!(labels, ["Attach", "Rename\u{2026}", "Kill\u{2026}"]);
        assert!(!ManageAction::Attach.destructive());
        assert!(!ManageAction::Rename.destructive());
        assert!(ManageAction::Kill.destructive());
    }
}
