//! The screen's credential dialogs: the no-verification confirm gate (for a
//! row tap and for the unlock dock), the saved-row delete confirmation, the
//! failed dock's reset confirmation, and the save-credential offer. Each is
//! raised on the overlay host's navigator (never the route one), so it
//! renders above every piece of app chrome.
//!
//! The no-verification confirm gate: a saved row stored under
//! [`TrustMode::NoVerification`](crate::connection::TrustMode) carries a real
//! credential that [`ConnectController::recover_and_connect`] would
//! otherwise replay silently over an unauthenticated connection.
//! [`handle_saved_row_tap`] gates exactly that combination, before the
//! unlock rather than after (a "no" then costs nothing more than the tap
//! just made). The manual form carries no such gate because picking "Trust
//! anyway" beside its own warning chip is the consent.
//!
//! Removing a saved server: [`handle_saved_row_delete`] confirms, then drops
//! this device's copy — strictly local, no call to the server.
//!
//! The save-credential offer: [`credential_offer_prompt`] is the
//! `offer_prompt` lent to [`ConnectController::connect`], and
//! [`offer_credential_save`] the dialog it raises over a still-held
//! redirect, so the answer is what navigates. Composed from the glyph
//! primitives directly rather than through [`confirm_dialog`]: that helper
//! calls back only on confirm, and every way out of this one (decline,
//! scrim, `Escape`, Android back) is an answer the held redirect awaits.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use frust::{ButtonStyle, PopResult, button};
use frust_glyph::{glyph_dialog, show_glyph_dialog};

use super::super::controller::{ConfirmPrompt, ConnectController, CredentialOffer, OfferPrompt};
use crate::AppState;
use crate::routes::Nav;
use crate::ui::dialogs::confirm_dialog;

/// Also how `tests/connect.rs` tells which pushed page is up.
const OFFER_TITLE: &str = "Save this server?";
fn offer_body(label: &str) -> String {
    format!("Keep {label}'s credential so the next connect is one tap.")
}
/// Title when a credential is already stored: accepting mints a fresh token,
/// refreshing one whose server-side token was revoked.
const REPLACE_TITLE: &str = "Replace saved credential?";
fn replace_body(label: &str) -> String {
    format!("Replace the saved credential for {label} with a fresh one for this device?")
}
const OFFER_ACCEPT_LABEL: &str = "Save";
/// Also what every dismissal route resolves as.
const OFFER_DECLINE_LABEL: &str = "Not now";
const REMOVE_TITLE: &str = "Remove server?";
/// States the scope honestly: this device only.
fn remove_body(label: &str) -> String {
    format!(
        "{label} and its saved credential are removed from this device only. \
         Nothing is deleted on the server."
    )
}
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
        UNVERIFIED_TITLE,
        UNVERIFIED_BODY,
        UNVERIFIED_CONFIRM_LABEL,
        move |_app: &mut AppState| {
            controller.recover_and_connect(server.clone(), &nav);
        },
    );
}

const UNVERIFIED_TITLE: &str = "Connect without verification?";
const UNVERIFIED_BODY: &str = "This connection can't verify the server, and the saved token will \
                               be sent over an unauthenticated channel.";
const UNVERIFIED_CONFIRM_LABEL: &str = "Connect anyway";

/// The same gate for the unlock dock, asked once the keyring is open: the
/// dock lends it, and `on_confirm` dials. Built per press.
pub(crate) fn unverified_confirm_prompt(
    overlays: &frust::NavigatorController<AppState>,
) -> ConfirmPrompt {
    let overlays = overlays.clone();
    Rc::new(move |_server, on_confirm: Box<dyn FnOnce()>| {
        let on_confirm = Cell::new(Some(on_confirm));
        confirm_dialog(
            &overlays,
            UNVERIFIED_TITLE,
            UNVERIFIED_BODY,
            UNVERIFIED_CONFIRM_LABEL,
            move |_app: &mut AppState| {
                if let Some(dial) = on_confirm.take() {
                    dial();
                }
            },
        );
    })
}

const RESET_TITLE: &str = "Reset saved credentials?";
const RESET_BODY: &str = "Remove every saved credential on this device? Saved servers stay; you \
                          will paste or scan a token again.";
const RESET_CONFIRM_LABEL: &str = "Reset";

/// The failed dock's reset, behind its confirm — `Failed` also covers a
/// failed authentication, so it never resets unasked.
pub(crate) fn confirm_credential_reset(
    controller: &Arc<ConnectController>,
    overlays: &frust::NavigatorController<AppState>,
) {
    let controller = Arc::clone(controller);
    confirm_dialog(
        overlays,
        RESET_TITLE,
        RESET_BODY,
        RESET_CONFIRM_LABEL,
        move |_app: &mut AppState| controller.reset_saved_credentials(),
    );
}

/// A saved row's delete affordance at `index` was pressed: confirm, then
/// drop this device's copy. A row that has gone away between paint and
/// press does nothing.
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

/// Built per press, since nothing retains it past the connect it was made for.
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

/// Asks whether to save the just-connected server's credential, and resolves
/// the held redirect with the answer: accept navigates via
/// [`ConnectController::accept_offer_then_navigate`], everything else
/// (including a dialog popping empty) via
/// [`ConnectController::decline_offer_then_navigate`] — treating an empty
/// pop as anything but "not now" would park the user on a screen whose
/// connection is already open.
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
        // Re-invoked on every navigator rebuild, so captured values are cloned per call.
        move || {
            let decline_nav = page_nav.clone();
            let accept_nav = page_nav.clone();
            glyph_dialog()
                .title(title)
                .body(body.clone())
                .action(
                    button(OFFER_DECLINE_LABEL, move |_state: &mut AppState| {
                        decline_nav.pop_with_result(PopResult::of(false));
                    })
                    .style(ButtonStyle::Ghost)
                    .small(),
                )
                .action(
                    button(OFFER_ACCEPT_LABEL, move |_state: &mut AppState| {
                        accept_nav.pop_with_result(PopResult::of(true));
                    })
                    .style(ButtonStyle::Primary)
                    .small(),
                )
        },
        // Only the accept action pops `true`; decline and every dismissal
        // route pop `false`/empty, the same answer. Both branches navigate —
        // this is the one place the held redirect resolves from.
        move |_state: &mut AppState, result: PopResult| {
            if result.take::<bool>().unwrap_or(false) {
                controller.accept_offer_then_navigate(&nav);
            } else {
                controller.decline_offer_then_navigate(&nav);
            }
        },
    );
}
