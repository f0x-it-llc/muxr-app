//! The card's link strip — the one band between the status row and the body
//! that says a live `muxrd` session is not currently healthy.

use frust::{AnyView, any, button};

use crate::features::terminal::data::feeds::grpc_feed::FeedStatus;
use crate::ui::palette::{Palette, active_theme, inked};

use super::super::chrome::status_strip;
use super::super::pages::TerminalState;
use super::card::CardInk;

/// The link strip's three messages, and the two affordances beside them.
const LINK_RECONNECTING: &str = "reconnecting\u{2026}";
const LINK_DISCONNECTED: &str = "disconnected";
const LINK_CLOSED: &str = "session ended";
const LINK_RETRY: &str = "Retry";
const LINK_LEAVE: &str = "Back to sessions";

/// The card's link strip for `link`, or `None` when there is nothing to say.
///
/// Four of the feed's five states are silent: `Idle` and `Connected` are a
/// session behaving, and a screen with no live `muxrd` session behind it
/// (`None` — the mock transcript, the demo replay) has no link at all. The
/// three that show speak in the same order of severity the affordance beside
/// them does: reconnecting says only that (the session is coming back on its
/// own, and a button would invite interrupting it), a disconnected one offers
/// the retry that re-arms it, and a session the server *ended* offers the only
/// thing left — the way back to the list.
///
/// Every callback here takes `&mut TerminalState`, which is exactly why the
/// strip belongs to the card rather than to the sheet: `retry` needs the live
/// session, and the live session is screen state.
pub(crate) fn link_strip(
    link: Option<FeedStatus>,
    colors: &Palette,
    ink: &CardInk,
) -> Option<AnyView<TerminalState>> {
    let (message, action): (&str, Option<AnyView<TerminalState>>) = match link? {
        FeedStatus::Idle | FeedStatus::Connected => return None,
        FeedStatus::Reconnecting => (LINK_RECONNECTING, None),
        FeedStatus::Disconnected => (
            LINK_DISCONNECTED,
            Some(any(button(LINK_RETRY, |state: &mut TerminalState| {
                if let Some(session) = state.live.as_ref() {
                    session.retry();
                }
            }))),
        ),
        FeedStatus::Closed => (
            LINK_CLOSED,
            Some(any(button(LINK_LEAVE, |state: &mut TerminalState| {
                state.actions.app_nav.pop();
            }))),
        ),
    };
    // Resolved only once there is a strip to build — a `build` that has
    // nothing to say must stay as cheap as it was before this existed.
    let style = inked(&active_theme().type_scale.label_medium, colors.fg_muted);
    Some(status_strip(message, style, ink.head, action))
}
