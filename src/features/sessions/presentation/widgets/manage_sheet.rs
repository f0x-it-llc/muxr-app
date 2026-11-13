//! The per-session management sheet a long-press opens: Attach / Rename… / Kill….
//! Rename/Kill close through [`SheetHandle::close_then`]: an unchained push would land ahead of the
//! sheet's staged exit and the eventual pop would take the dialog. Attach uses a plain `close`.

use frust::{
    Axis, ButtonStyle, CrossAxisAlignment, FlexView, SizedBox, View, button, inflexible, text,
};

use crate::AppState;
use crate::ui::palette::{Palette, inked};
use crate::ui::sheet::SheetHandle;
use crate::ui::spacing;

use super::super::pages::Actions;
use super::active_theme;

const SHEET_GAP: f64 = spacing::LG;
const SHEET_ROW_GAP: f64 = spacing::TIGHT;

pub(in crate::features::sessions::presentation) fn sheet_body(
    actions: &Actions,
    sheet: &SheetHandle,
    id: &str,
    name: String,
    read_only: bool,
) -> impl View<AppState> + use<> {
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

fn sheet_heading(title: impl Into<String>) -> impl View<AppState> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    text(title.into()).style(inked(&theme.type_scale.label_large, colors.fg))
}

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

#[derive(Clone, Copy)]
enum ManageAction {
    Attach,
    Rename,
    Kill,
}

impl ManageAction {
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
                // Through the sheet's handle, so a second tap inside the one-rebuild window of a queued pop
                // cannot pop whatever is under it.
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
