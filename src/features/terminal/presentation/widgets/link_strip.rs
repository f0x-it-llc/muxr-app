//! The band between the status row and the body that says a live session is unhealthy.

use frust::{View, button};

use crate::features::terminal::domain::feed::LinkStatus;
use crate::ui::palette::{Palette, active_theme, inked};

use super::super::chrome::status_strip;
use super::super::pages::TerminalState;
use super::card::CardInk;

const LINK_RECONNECTING: &str = "reconnecting\u{2026}";
const LINK_DISCONNECTED: &str = "disconnected";
const LINK_CLOSED: &str = "session ended";
const LINK_RETRY: &str = "Retry";
const LINK_LEAVE: &str = "Back to sessions";

/// Reconnecting offers no button, since one would invite interrupting it; disconnected offers the
/// retry, and a server-ended session the way back to the list.
pub(crate) fn link_strip(
    link: Option<LinkStatus>,
    colors: &Palette,
    ink: &CardInk,
) -> Option<impl View<TerminalState>> {
    let (message, action) = match link? {
        LinkStatus::Idle | LinkStatus::Connected => return None,
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
