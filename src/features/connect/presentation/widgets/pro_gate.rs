//! The free-tier server gate: the paywall a blocked dial raises, and the retry once the
//! block is gone. Both ways out re-run the same connect from its entry point; a saved-row
//! retry re-enters by row, since a forget moves every later index.

use std::rc::Rc;
use std::sync::Arc;

use frust::{GetUntracked, NavigatorController};

use super::super::controller::{ConnectController, ConnectRetry, GatePrompt, ServerLimitBlock};
use super::credential_dialogs::credential_offer_prompt;
use crate::AppState;
use crate::features::connect::domain::entities::SavedServer;
use crate::features::pro::domain::entities::{
    Entitlement, ServerLimitVerdict, row_is_beyond_free_tier,
};
use crate::features::pro::presentation::controller::ProController;
use crate::features::pro::presentation::{ServerLimitContext, UpgradeContext, show_upgrade_sheet};
use crate::routes::Nav;

pub(crate) fn server_limit_prompt(
    controller: &Arc<ConnectController>,
    overlays: &NavigatorController<AppState>,
    nav: &Nav,
    pro: &Arc<ProController>,
) -> GatePrompt {
    let controller = Arc::clone(controller);
    let overlays = overlays.clone();
    let nav = nav.clone();
    let pro = Arc::clone(pro);
    Rc::new(move |block: ServerLimitBlock| {
        let ServerLimitBlock { to_forget, retry } = block;
        let retry = retry_connect(&controller, &overlays, &nav, &pro, retry);
        let on_forget_and_connect = forget_then_retry(&controller, &to_forget, &retry);
        show_upgrade_sheet(
            &overlays,
            Arc::clone(&pro),
            UpgradeContext {
                on_unlocked: Rc::clone(&retry),
                server_limit: Some(ServerLimitContext {
                    to_forget,
                    on_forget_and_connect,
                }),
            },
        );
    })
}

/// Busy comes first: a paywall raised for a dial `recover_and_connect` then refuses would
/// drop rows for a connect that never runs.
pub(crate) fn gated_saved_row_tap(
    controller: &Arc<ConnectController>,
    overlays: &NavigatorController<AppState>,
    nav: &Nav,
    pro: &Arc<ProController>,
    index: usize,
) {
    if controller.busy_untracked() {
        return;
    }
    let Some(server) = controller.saved_row(index) else {
        return;
    };
    let gate = server_limit_prompt(controller, overlays, nav, pro);
    if controller.saved_row_blocked(&server, &gate) {
        return;
    }
    controller.saved_row_tapped(index, nav);
}

/// The verdict is taken inside the controller, ahead of the prompt.
pub(crate) fn gated_keyring_unlock(
    controller: &Arc<ConnectController>,
    overlays: &NavigatorController<AppState>,
    nav: &Nav,
    pro: &Arc<ProController>,
) {
    let gate = server_limit_prompt(controller, overlays, nav, pro);
    controller.unlock_keyring_and_connect(nav, gate);
}

/// The pro tag marks a row beyond the free tier, not a gated tap: a credential-less row
/// only refills the form, and the paywall fires at Connect.
pub(crate) fn row_is_locked(
    server: &SavedServer,
    saved: &[SavedServer],
    entitlement: Entitlement,
) -> bool {
    row_is_beyond_free_tier(entitlement, saved, &server.endpoint())
}

/// Forget first: the verdict the retry re-takes reads the same list.
fn forget_then_retry(
    controller: &Arc<ConnectController>,
    to_forget: &[SavedServer],
    retry: &Rc<dyn Fn()>,
) -> Rc<dyn Fn()> {
    let controller = Arc::clone(controller);
    let rows = to_forget.to_vec();
    let retry = Rc::clone(retry);
    Rc::new(move || {
        let retry = Rc::clone(&retry);
        controller.forget_saved_servers_then(rows.clone(), Box::new(move || retry()));
    })
}

/// Prompts are rebuilt: each is lent for one press.
fn retry_connect(
    controller: &Arc<ConnectController>,
    overlays: &NavigatorController<AppState>,
    nav: &Nav,
    pro: &Arc<ProController>,
    retry: ConnectRetry,
) -> Rc<dyn Fn()> {
    let controller = Arc::clone(controller);
    let overlays = overlays.clone();
    let nav = nav.clone();
    let pro = Arc::clone(pro);
    Rc::new(move || {
        let gate = server_limit_prompt(&controller, &overlays, &nav, &pro);
        match &retry {
            ConnectRetry::Manual => {
                let offer = credential_offer_prompt(&controller, &overlays, &nav);
                controller.connect(&nav, offer, gate);
            }
            ConnectRetry::Pairing => controller.connect_from_pairing(&nav, gate),
            ConnectRetry::Saved(server) => {
                if let Some(index) = controller.saved_row_index(&server.endpoint()) {
                    gated_saved_row_tap(&controller, &overlays, &nav, &pro, index);
                }
            }
            ConnectRetry::Keyring => gated_keyring_unlock(&controller, &overlays, &nav, &pro),
        }
    })
}

/// Taken at the tap: the endpoint is unknown until the scan completes.
pub(crate) fn scan_blocked(
    controller: &Arc<ConnectController>,
    overlays: &NavigatorController<AppState>,
    pro: &Arc<ProController>,
    on_unlocked: Rc<dyn Fn()>,
) -> bool {
    let saved = controller.saved.get_untracked();
    match pro.new_server_verdict(&saved) {
        ServerLimitVerdict::Allowed => false,
        ServerLimitVerdict::Blocked { .. } => {
            show_upgrade_sheet(
                overlays,
                Arc::clone(pro),
                UpgradeContext {
                    on_unlocked,
                    server_limit: None,
                },
            );
            true
        }
    }
}
