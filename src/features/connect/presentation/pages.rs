//! The `/` route's page: the Connect screen scaffold.
//!
//! [`connect_page`] is the route seam; [`connect_layout`] (re-exported from
//! [`super::widgets::layout`]) is the arrangement
//! half, split out because the cold-start splash lays the same pieces out
//! through the same function, so the frame it finishes on and the frame the
//! route paints are one picture.
//!
//! The screen's three layers are backdrop, scrolled layout (the QR square
//! and the unlock dock ride in its pieces), and the getting-started
//! affordance. The scroll and the affordance belong to the screen, not the
//! layout — the splash assembles into a fixed layer with nothing to scroll.
//! That is the Compact tree; a
//! Tablet window ([`Responsive`], read once per build) gets
//! [`tablet_screen`] instead, assembled in [`super::widgets::tablet`], which
//! the splash assembles on a Tablet window too, through that module's own
//! seam.
//!
//! Handles are resolved once in [`Component::init`], never in a press handler.
//! The credential dialogs live in [`super::widgets::credential_dialogs`], the
//! QR gate and camera lifecycle in
//! [`super::widgets::qr_affordance`]/[`super::widgets::qr_sheet`], the
//! free-tier server gate's paywall in [`super::widgets::pro_gate`], and the
//! orientation dialog in [`super::widgets::getting_started`].

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use clean_signals_frust::use_controller;
use frust::{
    AnyView, Component, Either, Get, GetUntracked, View, WindowMetrics, component, scroll_view,
    stack, use_context,
};

use super::controller::ConnectController;
use super::widgets::credential_dialogs::{
    confirm_credential_reset, credential_offer_prompt, handle_saved_row_delete,
};
use super::widgets::getting_started::info_affordance;
use super::widgets::layout::clamped_layout;
pub use super::widgets::layout::connect_layout;
use super::widgets::pro_gate::{gated_keyring_unlock, gated_saved_row_tap, server_limit_prompt};
use super::widgets::qr_affordance::{qr_button, qr_scan_action};
use super::widgets::tablet::{tablet_screen, tablet_splash_layout, tablet_splash_pieces};
use super::widgets::{Live, connect_pieces, connect_pieces_with};
use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::{
    expect_credential_service, expect_keyring, expect_saved_servers,
};
use crate::features::connect::domain::entities::SavedServer;
use crate::features::pro::di::expect_pro_controller;
use crate::features::pro::presentation::controller::ProController;
use crate::features::settings::di::expect_auth_tokens_repository;
use crate::responsive::Responsive;
use crate::routes::{Nav, app_nav};
use crate::ui::backdrop::page_backdrop;
use crate::ui::palette::active_theme;

/// The Connect screen: the `/` route's page. See the [module docs](self).
pub fn connect_page(overlays: frust::NavigatorController<AppState>) -> impl View<AppState> {
    component(ConnectScreen { overlays })
}

/// A class's layout function, boxed so [`splash_assembly`] can hand back
/// either class's.
pub type SplashLayout<S> = Box<dyn FnOnce(Vec<AnyView<S>>) -> AnyView<S>>;

/// The splash's copy of this screen: its pieces in stagger order and the
/// layout they assemble through, for the window's size class — read here,
/// once per build, exactly as [`ConnectScreen`]'s own `build` reads it, so
/// the overlay's last frame is the route's first on every class. `saved` is
/// piece 7's rows (the parity contract).
pub fn splash_assembly<S: 'static>(
    theme: &frust::Theme,
    saved: &[SavedServer],
) -> (Vec<AnyView<S>>, SplashLayout<S>) {
    let rule = Responsive::of(use_context::<WindowMetrics>());
    if rule.is_tablet() {
        let full = rule.is_full();
        let assemble_theme = theme.clone();
        return (
            tablet_splash_pieces(theme, saved, full),
            Box::new(move |pieces| tablet_splash_layout(&assemble_theme, full, pieces)),
        );
    }
    (connect_pieces(theme, saved), Box::new(connect_layout::<S>))
}

/// Stateless configuration; everything retained lives in [`ConnectState`].
struct ConnectScreen {
    overlays: frust::NavigatorController<AppState>,
}

/// The Connect screen's retained state: the non-reactive half only. Every
/// observed value lives on [`ConnectController`]'s signals instead.
pub struct ConnectState {
    /// Disposed with this component (`use_controller`).
    controller: Arc<ConnectController>,
    app_nav: Nav,
    /// Every sheet and dialog this screen raises.
    overlays: frust::NavigatorController<AppState>,
    /// The app-scoped entitlement handle, held so a press handler can hand it
    /// to the paywall it raises (the controller keeps its own, for the verdict).
    pro: Arc<ProController>,
    /// Held only so `build` can watch `connected`'s true→false edge (a
    /// Disconnect) and scrub the token field.
    connection: Arc<ConnectionController>,
    /// Whether `connected` read `true` on the last `build`. Seeded at mount, so
    /// an already-connected screen doesn't misfire a clear on its first build.
    was_connected: bool,
    /// The QR affordance's single-flight latch (see [`super::widgets::qr_affordance`]).
    /// `Rc<Cell<bool>>` because the permission check's completion runs off
    /// this handler's borrow.
    qr_permission_pending: Rc<Cell<bool>>,
}

impl Component for ConnectScreen {
    type State = ConnectState;

    fn init(&self) -> ConnectState {
        // Resolved here, where a reactive owner is live — a press handler cannot.
        let repository = expect_saved_servers();
        let credentials = expect_credential_service();
        // Cross-feature resolve: a saved credential is a server-side token, and
        // `settings` owns that store.
        let tokens = expect_auth_tokens_repository();
        // Cross-feature resolve too: the free-tier server rule is `pro`'s, and
        // this screen is one of the two that gate on it.
        let pro = expect_pro_controller();
        let keyring = expect_keyring();
        let connection = expect_connection_controller();
        let was_connected = connection.connected.get_untracked();
        let controller = use_controller::<ConnectController, AppFailure>({
            let connection = Arc::clone(&connection);
            let pro = Arc::clone(&pro);
            move || {
                ConnectController::new(repository, credentials, tokens, connection, pro, keyring)
            }
        });
        // Installed here, where the flow's dependencies are already resolved.
        connection.set_credential_saver(controller.credential_saver());

        ConnectState {
            controller,
            app_nav: app_nav(),
            overlays: self.overlays.clone(),
            pro,
            connection,
            was_connected,
            qr_permission_pending: Rc::new(Cell::new(false)),
        }
    }

    fn build(&self, state: &mut ConnectState) -> impl View<ConnectState> {
        let rule = Responsive::of(use_context::<WindowMetrics>());
        let theme = active_theme();

        // Tracked read: a Disconnect can flip `connected` back to `false` while
        // this screen stays mounted underneath whatever covers it.
        let connected = state.connection.connected.get();
        if state.was_connected && !connected {
            state.controller.clear_token();
            // A stashed pairing goes out on the same edge — its pin belonged to
            // the session that just ended.
            state.controller.clear_pending_pairing();
        }
        state.was_connected = connected;

        let live = Live {
            controller: Arc::clone(&state.controller),
            on_connect: {
                let controller = Arc::clone(&state.controller);
                let overlays = state.overlays.clone();
                let nav = state.app_nav.clone();
                let pro = Arc::clone(&state.pro);
                Rc::new(move || {
                    let prompt = credential_offer_prompt(&controller, &overlays, &nav);
                    let gate = server_limit_prompt(&controller, &overlays, &nav, &pro);
                    controller.connect(&nav, prompt, gate);
                })
            },
            on_connect_pinned: {
                let controller = Arc::clone(&state.controller);
                let overlays = state.overlays.clone();
                let nav = state.app_nav.clone();
                let pro = Arc::clone(&state.pro);
                Rc::new(move || {
                    let gate = server_limit_prompt(&controller, &overlays, &nav, &pro);
                    controller.connect_from_pairing(&nav, gate);
                })
            },
            on_saved_tap: {
                let controller = Arc::clone(&state.controller);
                let overlays = state.overlays.clone();
                let nav = state.app_nav.clone();
                let pro = Arc::clone(&state.pro);
                Rc::new(move |index| gated_saved_row_tap(&controller, &overlays, &nav, &pro, index))
            },
            on_saved_delete: {
                let controller = Arc::clone(&state.controller);
                let overlays = state.overlays.clone();
                Rc::new(move |index| handle_saved_row_delete(&controller, &overlays, index))
            },
            on_unlock: {
                let controller = Arc::clone(&state.controller);
                let overlays = state.overlays.clone();
                let nav = state.app_nav.clone();
                let pro = Arc::clone(&state.pro);
                Rc::new(move || gated_keyring_unlock(&controller, &overlays, &nav, &pro))
            },
            on_reset_credentials: {
                let controller = Arc::clone(&state.controller);
                let overlays = state.overlays.clone();
                Rc::new(move || confirm_credential_reset(&controller, &overlays))
            },
            on_scan: qr_scan_action(
                &state.overlays,
                &state.controller,
                &state.qr_permission_pending,
                &state.pro,
            ),
        };

        // The one tracked read of the list — lets the splash hand the same
        // builder its own snapshot (the widgets' parity contract).
        let saved = state.controller.saved.get();
        if rule.is_tablet() {
            let qr = qr_button(
                &state.overlays,
                &state.controller,
                &state.qr_permission_pending,
                &state.pro,
            );
            return Either::Left(tablet_screen(
                &theme,
                &live,
                &saved,
                rule.is_full(),
                qr,
                &state.overlays,
            ));
        }
        let content = clamped_layout(
            connect_pieces_with(&theme, Some(&live), &saved),
            rule.wide_clamp,
        );

        Either::Right(
            stack()
                .child(page_backdrop::<ConnectState>())
                .child(scroll_view(content))
                .child(info_affordance(&state.overlays)),
        )
    }
}
