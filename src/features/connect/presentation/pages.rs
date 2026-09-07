//! The `/` route's page: the Connect screen scaffold.
//!
//! [`connect_page`] is the route seam; [`connect_layout`] is the arrangement
//! half, split out because the cold-start splash lays the same pieces out
//! through the same function, so the frame it finishes on and the frame the
//! route paints are one picture.
//!
//! The screen's three layers are backdrop, scrolled layout, QR affordance. The
//! scroll and the FAB both belong to the screen, not the layout — the splash
//! assembles into a fixed layer with nothing to scroll.
//!
//! Handles are resolved once in [`Component::init`], never in a press handler.
//! The credential dialogs live in [`super::widgets::credential_dialogs`], the
//! QR gate and camera lifecycle in
//! [`super::widgets::qr_affordance`]/[`super::widgets::qr_sheet`], and the
//! free-tier server gate's paywall in [`super::widgets::pro_gate`].

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use clean_signals_frust::use_controller;
use frust::{
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexView, Get,
    GetUntracked, Padding, SizedBox, Stack, WindowMetrics, any, component, inflexible, safe_area,
    scroll_view, use_context,
};

use super::controller::ConnectController;
use super::widgets::credential_dialogs::{credential_offer_prompt, handle_saved_row_delete};
use super::widgets::pro_gate::{gated_saved_row_tap, server_limit_prompt};
use super::widgets::qr_affordance::qr_affordance;
use super::widgets::{Live, connect_pieces_with};
use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::{expect_credential_service, expect_saved_servers};
use crate::features::pro::di::expect_pro_controller;
use crate::features::pro::presentation::controller::ProController;
use crate::features::settings::di::expect_auth_tokens_repository;
use crate::responsive::WIDE_BREAKPOINT_PX;
use crate::routes::{Nav, app_nav};
use crate::ui::backdrop::page_backdrop;
use crate::ui::palette::active_theme;
use crate::ui::spacing;

/// The content column's max width past [`WIDE_BREAKPOINT_PX`]: a wide window
/// centers the form rather than stretching it edge to edge. A plain clamp, not
/// a layout switch (see `crate::responsive`).
const CONNECT_CLAMP_WIDTH: f64 = 520.0;
/// Where the clamped column sits in a wide window: centered horizontally,
/// top-anchored vertically. Not [`Alignment::CENTER`] — the splash assembles
/// into a bounded `Stack` layer, and `CENTER` there would trade this clamp's
/// horizontal jump for a vertical one.
const CLAMP_ALIGNMENT: Alignment = Alignment { x: 0.0, y: -1.0 };

/// Arranges `pieces` into a safe-area-inset, padded, top-anchored column with
/// [`spacing::SHEET`] between them, each stretched to the layer width — and,
/// past [`WIDE_BREAKPOINT_PX`], clamped to [`CONNECT_CLAMP_WIDTH`] and
/// centered.
///
/// Takes the pieces already built, so the splash can pass its wrapped copies
/// and the screen its live ones. The clamp lives here rather than in the
/// screen's own `build`, so the splash and the route agree on layout without
/// a jump between them.
pub fn connect_layout<S: 'static>(pieces: Vec<AnyView<S>>) -> AnyView<S> {
    let mut children = Vec::with_capacity(pieces.len() * 2);
    for (index, piece) in pieces.into_iter().enumerate() {
        if index > 0 {
            children.push(inflexible(any(SizedBox::<S>(None, Some(spacing::SHEET)))));
        }
        children.push(inflexible(piece));
    }

    let layer = any(safe_area(Padding(
        EdgeInsets::symmetric(spacing::SHEET, spacing::HERO),
        FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch),
    )));

    // `None` (no shell published metrics yet, or a bare test harness) takes the
    // same branch as a genuinely narrow window.
    let is_wide = use_context::<WindowMetrics>()
        .map(|metrics| metrics.size.width >= WIDE_BREAKPOINT_PX)
        .unwrap_or(false);
    if !is_wide {
        return layer;
    }
    any(Align(
        CLAMP_ALIGNMENT,
        SizedBox::<S>(Some(CONNECT_CLAMP_WIDTH), None).child(layer),
    ))
}

/// The Connect screen: the `/` route's page. See the [module docs](self).
pub fn connect_page(overlays: frust::NavigatorController<AppState>) -> AnyView<AppState> {
    any(component(ConnectScreen { overlays }))
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
    /// The FAB's single-flight latch (see [`super::widgets::qr_affordance`]).
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
        let connection = expect_connection_controller();
        let was_connected = connection.connected.get_untracked();
        let controller = use_controller::<ConnectController, AppFailure>({
            let connection = Arc::clone(&connection);
            let pro = Arc::clone(&pro);
            move || ConnectController::new(repository, credentials, tokens, connection, pro)
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

    fn build(&self, state: &mut ConnectState) -> AnyView<ConnectState> {
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
        };

        // The one tracked read of the list — lets the splash hand the same
        // builder its own snapshot (the widgets' parity contract).
        let saved = state.controller.saved.get();
        let content = connect_layout(connect_pieces_with(&theme, Some(&live), &saved));

        any(Stack(vec![
            any(page_backdrop::<ConnectState>()),
            any(scroll_view(content)),
            qr_affordance(
                &state.overlays,
                &state.controller,
                &state.qr_permission_pending,
                &state.pro,
            ),
        ]))
    }
}
