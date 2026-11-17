//! The band between the status row and the body that says a live session is unhealthy, and the
//! body that stands in for the grid while no pane is open.

use frust::{Align, Alignment, ButtonStyle, View, button};
use frust_glyph::empty_state;

use crate::features::terminal::domain::feed::LinkStatus;
use crate::ui::palette::{Palette, active_theme, inked};

use super::super::chrome::status_strip;
use super::super::pages::TerminalState;
use super::card::CardInk;
use super::status::PANES_LABEL;

const LINK_RECONNECTING: &str = "reconnecting\u{2026}";
const LINK_DISCONNECTED: &str = "disconnected";
const LINK_CLOSED: &str = "session ended";
const LINK_RETRY: &str = "Retry";
const LINK_LEAVE: &str = "Back to sessions";

const NO_PANE_TITLE: &str = "No pane open";
const NO_PANE_LINE: &str = "Pick a pane to continue.";

/// Reconnecting offers no button, since one would invite interrupting it; disconnected offers the
/// retry, and a server-ended session the way back to the list. A gone pane is no link problem.
pub(crate) fn link_strip(
    link: Option<LinkStatus>,
    colors: &Palette,
    ink: &CardInk,
) -> Option<impl View<TerminalState>> {
    let (message, action) = match link? {
        LinkStatus::Idle | LinkStatus::Connected | LinkStatus::PaneGone => return None,
        LinkStatus::Reconnecting => (LINK_RECONNECTING, None),
        LinkStatus::Disconnected => (
            LINK_DISCONNECTED,
            Some(button(LINK_RETRY, |state: &mut TerminalState| {
                if let Some(session) = state.live.as_ref() {
                    session.retry();
                }
            })),
        ),
        LinkStatus::Closed => (
            LINK_CLOSED,
            Some(button(LINK_LEAVE, |state: &mut TerminalState| {
                state.actions.app_nav.pop();
            })),
        ),
    };
    let style = inked(&active_theme().type_scale.label_medium, colors.fg_muted);
    Some(status_strip(message, style, ink.head, action))
}

/// The phone's only way to the tree is the sheet, so it gets the button; a docked rail is the tree.
pub(crate) fn no_pane_body(open_tree: bool) -> impl View<TerminalState> {
    let mut state = empty_state(NO_PANE_TITLE, NO_PANE_LINE);
    if open_tree {
        state = state.action(
            button(PANES_LABEL, |state: &mut TerminalState| {
                state.actions.open_sheet();
            })
            .style(ButtonStyle::Primary),
        );
    }
    Align(Alignment::CENTER, state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_no_pane_state_reads_exactly_as_specified() {
        assert_eq!(NO_PANE_TITLE, "No pane open");
        assert_eq!(NO_PANE_LINE, "Pick a pane to continue.");
        assert_eq!(PANES_LABEL, "Panes & tabs");
    }
}
