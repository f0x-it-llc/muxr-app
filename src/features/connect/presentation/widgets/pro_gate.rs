//! The screen's free-tier server gate: the paywall a blocked dial raises
//! instead of connecting, and the retry that runs once the block is gone.
//!
//! [`server_limit_prompt`] is the `gate_prompt` lent to
//! [`ConnectController::connect`] and
//! [`connect_from_pairing`](ConnectController::connect_from_pairing), built per
//! press like [`credential_offer_prompt`] and never stored (it wraps `Rc`,
//! while the controller is `Send + Sync`). A block raises `features::pro`'s one
//! sheet over the overlay host, whose two ways out — unlock Pro, or forget the
//! rows the free tier cannot keep — both end in the *same* connect the user
//! asked for, re-run from the entry point it came from.
//!
//! The saved-row tap is gated one level up, in [`gated_saved_row_tap`], rather
//! than inside `ConnectController::recover_and_connect`: the row's own
//! no-verification confirmation lives in
//! [`credential_dialogs::handle_saved_row_tap`](super::credential_dialogs::handle_saved_row_tap)
//! and dials through that ungated method, so the verdict has to be taken before
//! the dialog is raised. The retry re-enters the same handler by *row* — a
//! forget moves every later index — so the confirmation is still asked once,
//! and only for a connect that will actually happen.
//!
//! [`scan_blocked`] is the fourth gated entry point, taken at the tap since
//! the endpoint is unknown until the scan completes; it raises the same
//! sheet with no forget-and-connect action, and leaves the pairing dial a
//! scan ends in to take its own verdict, the way [`server_limit_prompt`] does.

use std::rc::Rc;
use std::sync::Arc;

use frust::{GetUntracked, NavigatorController};

use super::super::controller::{ConnectController, ConnectRetry, GatePrompt, ServerLimitBlock};
use super::credential_dialogs::{credential_offer_prompt, handle_saved_row_tap};
use crate::AppState;
use crate::features::connect::domain::entities::SavedServer;
use crate::features::pro::domain::entities::{
    Entitlement, ServerLimitVerdict, row_is_beyond_free_tier,
};
use crate::features::pro::presentation::controller::ProController;
use crate::features::pro::presentation::{ServerLimitContext, UpgradeContext, show_upgrade_sheet};
use crate::routes::Nav;

/// Built per press, since nothing retains it past the connect it was made for.
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

/// A saved row tapped, with the free-tier verdict taken first: a blocked row
/// raises the paywall instead of unlocking anything, and everything past the
/// verdict — the no-verification confirmation, the unlock, the dial — stays
/// [`handle_saved_row_tap`]'s. See the [module docs](self) for why the verdict
/// is taken here rather than inside the connect it guards. Busy comes first,
/// the order the other two entry points take: a paywall raised for a dial
/// `recover_and_connect` then refuses mid-flight would have dropped rows (its
/// forget-and-connect) for a connect that never runs.
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
    handle_saved_row_tap(controller, overlays, nav, index);
}

/// Whether connecting to `server`'s endpoint raises the paywall while `saved`
/// is remembered ([`row_is_beyond_free_tier`]). The pro tag means that, not
/// "this tap is gated": a credential-less row is tagged too, though its tap
/// only refills the form and the paywall fires at Connect.
pub(crate) fn row_is_locked(
    server: &SavedServer,
    saved: &[SavedServer],
    entitlement: Entitlement,
) -> bool {
    row_is_beyond_free_tier(entitlement, saved, &server.endpoint())
}

/// Drop the rows the free tier cannot keep, then retry — in that order, since
/// the verdict the retry re-takes reads the same list.
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

/// The connect to run once the block is gone: the entry point it came from,
/// with its prompts freshly built (each is lent for one press).
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
        }
    })
}

/// The QR affordance's own gate — see the [module docs](self). `Blocked`
/// raises the paywall with no forget-and-connect action and returns `true`;
/// `Allowed` returns `false` and the affordance proceeds to its permission gate.
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
