//! Raised on the overlay host's navigator, never the route one, so each renders above all
//! app chrome. No confirm guards a saved no-verification row: choosing "Trust anyway" when
//! it was added is the consent.

use std::rc::Rc;
use std::sync::Arc;

use frust::{ButtonStyle, PopResult, button};
use frust_glyph::{glyph_dialog, show_glyph_dialog};

use super::super::controller::{ConnectController, CredentialOffer, OfferPrompt};
use crate::AppState;
use crate::routes::Nav;
use crate::ui::dialogs::confirm_dialog;

/// Also how `tests/connect.rs` tells which pushed page is up.
const OFFER_TITLE: &str = "Save this server?";
fn offer_body(label: &str) -> String {
    format!("Keep {label}'s credential so the next connect is one tap.")
}
/// Accepting mints a fresh token, replacing one whose server-side token was revoked.
const REPLACE_TITLE: &str = "Replace saved credential?";
fn replace_body(label: &str) -> String {
    format!("Replace the saved credential for {label} with a fresh one for this device?")
}
const OFFER_ACCEPT_LABEL: &str = "Save";
/// Also what every dismissal route resolves as.
const OFFER_DECLINE_LABEL: &str = "Not now";
const REMOVE_TITLE: &str = "Remove server?";
fn remove_body(label: &str) -> String {
    format!(
        "{label} and its saved credential are removed from this device only. \
         Nothing is deleted on the server."
    )
}
const REMOVE_CONFIRM_LABEL: &str = "Remove";

const RESET_TITLE: &str = "Reset saved credentials?";
const RESET_BODY: &str = "Remove every saved credential on this device? Saved servers stay; you \
                          will paste or scan a token again.";
const RESET_CONFIRM_LABEL: &str = "Reset";

/// `Failed` also covers a failed authentication, so it never resets unasked.
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

/// A row gone between paint and press does nothing.
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

/// Built from glyph primitives, not `confirm_dialog`: every way out is an answer the held
/// redirect awaits. Anything but accept, an empty pop included, declines; anything else
/// would park the user on a screen whose connection is already open.
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
        // Both branches navigate: this is the one place the held redirect resolves.
        move |_state: &mut AppState, result: PopResult| {
            if result.take::<bool>().unwrap_or(false) {
                controller.accept_offer_then_navigate(&nav);
            } else {
                controller.decline_offer_then_navigate(&nav);
            }
        },
    );
}
