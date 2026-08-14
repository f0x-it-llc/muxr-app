//! The screen's three credential dialogs: the no-verification confirm gate, the
//! saved-row delete confirmation, and the save-credential offer. Each is raised on
//! the **overlay host's** navigator (never the route one), so it renders above
//! every piece of app chrome.
//!
//! **The no-verification confirm gate.** A saved row stored under
//! [`TrustMode::NoVerification`](crate::connection::TrustMode) carries a real
//! credential that [`ConnectController::recover_and_connect`] would otherwise
//! replay, silently, over a connection that authenticates nothing.
//! [`handle_saved_row_tap`] gates exactly that combination and leaves every other
//! row untouched. **Before the unlock, not after**: a "no" then costs the user
//! nothing more than the tap they just made, where gating afterwards would have
//! spent a system prompt — and, on some platforms, a lockout counter — on a
//! connect they were about to decline. The manual form carries no such gate
//! because picking "Trust anyway" beside its own warning chip *is* the consent.
//!
//! **Removing a saved server.** [`handle_saved_row_delete`] confirms, then drops
//! **this device's** copy — strictly local, which is what the confirmation says in
//! as many words; no call is made to the server.
//!
//! **The save-credential offer.** [`credential_offer_prompt`] is the `offer_prompt`
//! the screen lends [`ConnectController::connect`], and [`offer_credential_save`]
//! the dialog it raises over a still-held redirect, so the answer is what
//! navigates. Composed from the glyph primitives directly rather than through
//! [`confirm_dialog`]: that helper calls back **only** on confirm, and every way
//! out of this one — decline, scrim, `Escape`, Android back — is an answer the held
//! redirect is waiting for.

use std::rc::Rc;
use std::sync::Arc;

use frust::glyph::{glyph_dialog, show_glyph_dialog};
use frust::{ButtonStyle, PopResult, any, button};

use super::super::controller::{ConnectController, CredentialOffer, OfferPrompt};
use crate::AppState;
use crate::routes::Nav;
use crate::ui::dialogs::confirm_dialog;

/// The offer's title for a server this device holds no credential for yet. Also
/// how `tests/connect.rs` tells *which* pushed page is up.
const OFFER_TITLE: &str = "Save this server?";
/// …and its body, with the server's own label.
fn offer_body(label: &str) -> String {
    format!("Keep {label}'s credential so the next connect is one tap.")
}
/// …and its title when one is **already** stored: accepting mints a fresh token
/// and overwrites the secret, which is how a credential whose token was revoked
/// server-side gets refreshed.
const REPLACE_TITLE: &str = "Replace saved credential?";
/// …and its body. It says *fresh* because the accept mints this device's own
/// token rather than storing the one just typed.
fn replace_body(label: &str) -> String {
    format!("Replace the saved credential for {label} with a fresh one for this device?")
}
/// The offer's accept action — what raises the biometric prompt.
const OFFER_ACCEPT_LABEL: &str = "Save";
/// …and its decline, which is also what every dismissal route resolves as.
const OFFER_DECLINE_LABEL: &str = "Not now";
/// The saved-row delete confirmation's title.
const REMOVE_TITLE: &str = "Remove server?";
/// …its body, which states the scope honestly: this device only.
fn remove_body(label: &str) -> String {
    format!(
        "{label} and its saved credential are removed from this device only. \
         Nothing is deleted on the server."
    )
}
/// …and its confirm action.
const REMOVE_CONFIRM_LABEL: &str = "Remove";

/// A saved row at `index` was tapped: raise the confirm gate for a row
/// [`ConnectController::saved_row_needs_confirmation`] flags, and run
/// [`ConnectController::saved_row_tapped`] immediately for every other.
pub(crate) fn handle_saved_row_tap(
    controller: &Arc<ConnectController>,
    overlays: &frust::NavigatorController<AppState>,
    nav: &Nav,
    index: usize,
) {
    let Some(server) = controller.saved_row_needs_confirmation(index) else {
        controller.saved_row_tapped(index, nav);
        return;
    };
    let controller = Arc::clone(controller);
    let nav = nav.clone();
    confirm_dialog(
        overlays,
        "Connect without verification?",
        "This connection can't verify the server, and the saved token will be \
         sent over an unauthenticated channel.",
        "Connect anyway",
        move |_app: &mut AppState| {
            controller.recover_and_connect(server.clone(), &nav);
        },
    );
}

/// A saved row's delete affordance at `index` was pressed: confirm, then drop this
/// device's copy. The body states the scope, because "remove" over a server row is
/// exactly the word a user could read as "revoke this on the server". A row that
/// has gone away between paint and press does nothing.
pub(crate) fn handle_saved_row_delete(
    controller: &Arc<ConnectController>,
    overlays: &frust::NavigatorController<AppState>,
    index: usize,
) {
    let Some(server) = controller.saved_row(index) else {
        return;
    };
    let controller = Arc::clone(controller);
    confirm_dialog(
        overlays,
        REMOVE_TITLE,
        remove_body(&server.label),
        REMOVE_CONFIRM_LABEL,
        move |_app: &mut AppState| {
            controller.forget_saved_server(server.clone());
        },
    );
}

/// The `offer_prompt` this screen lends [`ConnectController::connect`] — built per
/// press, since nothing retains it past the connect it was made for.
pub(crate) fn credential_offer_prompt(
    controller: &Arc<ConnectController>,
    overlays: &frust::NavigatorController<AppState>,
    nav: &Nav,
) -> OfferPrompt {
    let controller = Arc::clone(controller);
    let overlays = overlays.clone();
    let nav = nav.clone();
    Rc::new(move |offer: CredentialOffer| {
        offer_credential_save(&controller, &overlays, &nav, &offer);
    })
}

/// Ask whether to save the just-connected server's credential, and resolve the held
/// redirect with the answer: [`ConnectController::accept_offer_then_navigate`] for
/// the accept, [`ConnectController::decline_offer_then_navigate`] for everything
/// else.
///
/// "Everything else" is the load-bearing half — a dialog that fell off the screen
/// without an answer pops empty, and treating that as anything but "not now" would
/// park the user on a Connect screen whose connection is already open. The question
/// comes in two wordings, one per [`CredentialOffer::replacing`].
fn offer_credential_save(
    controller: &Arc<ConnectController>,
    overlays: &frust::NavigatorController<AppState>,
    nav: &Nav,
    offer: &CredentialOffer,
) {
    let (title, body) = if offer.replacing {
        (REPLACE_TITLE, replace_body(&offer.label))
    } else {
        (OFFER_TITLE, offer_body(&offer.label))
    };
    let page_nav = overlays.clone();
    let controller = Arc::clone(controller);
    let nav = nav.clone();
    show_glyph_dialog(
        overlays,
        // Re-invoked on every navigator rebuild to diff the page's content, so
        // every captured value is cloned per call rather than moved.
        move || {
            let decline_nav = page_nav.clone();
            let accept_nav = page_nav.clone();
            glyph_dialog()
                .title(title)
                .body(body.clone())
                .action(any(button(
                    OFFER_DECLINE_LABEL,
                    move |_state: &mut AppState| {
                        decline_nav.pop_with_result(PopResult::of(false));
                    },
                )
                .style(ButtonStyle::Ghost)
                .small()))
                .action(any(button(
                    OFFER_ACCEPT_LABEL,
                    move |_state: &mut AppState| {
                        accept_nav.pop_with_result(PopResult::of(true));
                    },
                )
                .style(ButtonStyle::Primary)
                .small()))
        },
        // Only the accept action pops with `true`; the decline pops `false` and
        // every dismissal route pops *empty*, which reads as the same answer.
        // Both branches navigate — this callback is the one place the held
        // redirect can be resolved from, so it has no third outcome.
        move |_state: &mut AppState, result: PopResult| {
            if result.take::<bool>().unwrap_or(false) {
                controller.accept_offer_then_navigate(&nav);
            } else {
                controller.decline_offer_then_navigate(&nav);
            }
        },
    );
}
