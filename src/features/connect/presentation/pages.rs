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
//! [`add_server_page`] is the same screen in add-server mode (`/?mode=add`,
//! pushed over the placed shell): its own back bar, a heading of its own and
//! the form with a primary CTA and the QR square — no mark, badge, saved list
//! or dock. Its controller runs in [`ConnectMode::Add`]; a free install the
//! free tier refuses a new server meets the paywall first, and dismissing it
//! without unlocking pops the page.
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
use frust::authoring::text::TextAlign;
use frust::{
    Align, Alignment, AnyView, Component, CrossAxisAlignment, EitherOf3, Get, GetUntracked,
    SizedBox, Theme, View, WindowMetrics, any, column, component, icons, row, scroll_view, stack,
    text, use_context,
};

use super::controller::{ConnectController, ConnectMode};
use super::widgets::credential_dialogs::{
    confirm_credential_reset, credential_offer_prompt, handle_saved_row_delete,
};
use super::widgets::cta::{Prominence, connect_button};
use super::widgets::fields::{server_url_field, token_field};
use super::widgets::getting_started::info_affordance;
use super::widgets::layout::clamped_layout;
pub use super::widgets::layout::connect_layout;
use super::widgets::pro_gate::{gated_keyring_unlock, gated_saved_row_tap, server_limit_prompt};
use super::widgets::qr_affordance::{qr_button, qr_scan_action};
use super::widgets::tablet::{tablet_screen, tablet_splash_layout, tablet_splash_pieces};
use super::widgets::trust::remember_row;
use super::widgets::{Live, connect_pieces, connect_pieces_with};
use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::{
    expect_credential_service, expect_keyring, expect_saved_servers,
};
use crate::features::connect::domain::entities::SavedServer;
use crate::features::pro::di::expect_pro_controller;
use crate::features::pro::domain::entities::ServerLimitVerdict;
use crate::features::pro::presentation::controller::ProController;
use crate::features::pro::presentation::{UpgradeContext, show_upgrade_sheet};
use crate::features::settings::di::expect_auth_tokens_repository;
use crate::responsive::Responsive;
use crate::routes::{Nav, app_nav};
use crate::ui::backdrop::page_backdrop;
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;
use crate::ui::topbar::{BACK_LABEL, bar_icon};

/// The add-server page's bar title.
pub const ADD_SERVER_TITLE: &str = "add server";
/// The add-server page's heading and the line under it.
pub const ADD_SERVER_HEADING: &str = "Connect to another server";
pub const ADD_SERVER_SUBTITLE: &str = "It joins the keyring and becomes the active server.";
/// The trailing slot that balances the back chevron so the title centres on
/// the page — the bar icon's own 40px hit target.
const ADD_BAR_SLOT: f64 = 40.0;

/// The Connect screen: the `/` route's page. See the [module docs](self).
pub fn connect_page(overlays: frust::NavigatorController<AppState>) -> impl View<AppState> {
    component(ConnectScreen {
        overlays,
        mode: ConnectMode::Home,
        prefill: None,
    })
}

/// The add-server page (`/?mode=add`) — see the [module docs](self).
/// `prefill` names a saved endpoint whose row seeds the form.
pub fn add_server_page(
    overlays: frust::NavigatorController<AppState>,
    prefill: Option<String>,
) -> impl View<AppState> {
    component(ConnectScreen {
        overlays,
        mode: ConnectMode::Add,
        prefill,
    })
}

/// A class's layout function, boxed so [`splash_assembly`] can hand back
/// either class's.
pub type SplashLayout<S> = Box<dyn FnOnce(Vec<AnyView<S>>) -> AnyView<S>>;

/// The splash's copy of this screen: its pieces in stagger order and the
/// layout they assemble through, for the window's size class — read here,
/// once per build, exactly as [`ConnectScreen`]'s own `build` reads it, so
/// the overlay's last frame is the route's first on every class. `saved` is
/// piece 8's rows (the parity contract).
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
    mode: ConnectMode,
    prefill: Option<String>,
}

/// The add-server page's open-time paywall, tracked against the overlay
/// host's depth: raised on the first build, seen open, then seen closed — at
/// which point a still-refused install pops the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenGate {
    Clear,
    Pending,
    /// Raised at this overlay depth; not yet seen above it.
    Raised(usize),
    /// Seen open above this depth.
    Showing(usize),
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
    /// The saved endpoint the add-server form was opened for, if any.
    prefill: Option<String>,
    open_gate: OpenGate,
}

impl ConnectState {
    /// The add-server page's free-tier verdict: a prefilled saved row is
    /// judged as itself, a blank form as a server not yet named.
    fn add_blocked(&self) -> bool {
        let saved = self.controller.saved.get_untracked();
        let verdict = match &self.prefill {
            Some(endpoint) => self.pro.verdict_for(&saved, endpoint),
            None => self.pro.new_server_verdict(&saved),
        };
        matches!(verdict, ServerLimitVerdict::Blocked { .. })
    }

    /// Advance [`OpenGate`] by one build at overlay `depth`.
    fn step_open_gate(&mut self, depth: usize) {
        self.open_gate = match self.open_gate {
            OpenGate::Pending => {
                show_upgrade_sheet(
                    &self.overlays,
                    Arc::clone(&self.pro),
                    UpgradeContext {
                        // Unlocked: the form stays, and the next build's
                        // verdict lets it keep the page.
                        on_unlocked: Rc::new(|| {}),
                        server_limit: None,
                    },
                );
                OpenGate::Raised(depth)
            }
            OpenGate::Raised(mark) if depth > mark => OpenGate::Showing(mark),
            OpenGate::Showing(mark) if depth <= mark => {
                if self.add_blocked() {
                    self.app_nav.pop();
                }
                OpenGate::Clear
            }
            other => other,
        };
    }
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
        let mode = self.mode;
        let controller = use_controller::<ConnectController, AppFailure>({
            let connection = Arc::clone(&connection);
            let pro = Arc::clone(&pro);
            move || {
                let controller = ConnectController::new(
                    repository,
                    credentials,
                    tokens,
                    connection,
                    pro,
                    keyring,
                );
                match mode {
                    ConnectMode::Home => controller,
                    ConnectMode::Add => controller.in_add_mode(),
                }
            }
        });
        // Installed here, where the flow's dependencies are already resolved.
        // `/` only: the add-server page saves through itself (see
        // `ConnectController::accept_offer_then_navigate`), leaving `/`'s saver.
        if mode == ConnectMode::Home {
            connection.set_credential_saver(controller.credential_saver());
        }
        if let Some(endpoint) = &self.prefill {
            let row = controller
                .saved_row_index(endpoint)
                .and_then(|index| controller.saved_row(index));
            match row {
                Some(server) => controller.prefill(&server),
                None => controller.set_url(endpoint.clone()),
            }
        }

        let mut state = ConnectState {
            controller,
            app_nav: app_nav(),
            overlays: self.overlays.clone(),
            pro,
            connection,
            was_connected,
            qr_permission_pending: Rc::new(Cell::new(false)),
            prefill: self.prefill.clone(),
            open_gate: OpenGate::Clear,
        };
        if mode == ConnectMode::Add && state.add_blocked() {
            state.open_gate = OpenGate::Pending;
        }
        state
    }

    fn build(&self, state: &mut ConnectState) -> impl View<ConnectState> {
        let rule = Responsive::of(use_context::<WindowMetrics>());
        let theme = active_theme();
        let mode = state.controller.mode();

        if state.open_gate != OpenGate::Clear {
            let depth = state.overlays.depth();
            state.step_open_gate(depth);
        }

        // Tracked read: a Disconnect can flip `connected` back to `false` while
        // this screen stays mounted underneath whatever covers it. The
        // add-server page keeps its form: its own switch drops the old
        // connection mid-dial, and a refused one leaves the form for a retry.
        let connected = state.connection.connected.get();
        if mode == ConnectMode::Home && state.was_connected && !connected {
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
        if mode == ConnectMode::Add {
            // One column on every class, clamped wherever the window is wide.
            let content = clamped_layout(
                add_server_pieces(&theme, &live),
                rule.wide_clamp || rule.is_tablet(),
            );
            return EitherOf3::A(
                stack()
                    .child(page_backdrop::<ConnectState>())
                    .child(scroll_view(content)),
            );
        }
        if rule.is_tablet() {
            let qr = qr_button(
                &state.overlays,
                &state.controller,
                &state.qr_permission_pending,
                &state.pro,
            );
            return EitherOf3::B(tablet_screen(
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

        EitherOf3::C(
            stack()
                .child(page_backdrop::<ConnectState>())
                .child(scroll_view(content))
                .child(info_affordance(&state.overlays)),
        )
    }
}

/// The add-server page's pieces, top to bottom: its back bar, its heading,
/// the form, and the CTA row (always primary — there is no dock here — with
/// the QR square beside it).
fn add_server_pieces(theme: &Theme, live: &Live) -> Vec<AnyView<ConnectState>> // erasure: keep accumulator into Vec<AnyView>
{
    let colors = Palette::from_theme(theme);
    vec![
        any(add_server_bar(theme, &colors)),
        any(add_server_heading(theme, &colors)),
        any(server_url_field(theme, &colors, Some(live))),
        any(token_field(theme, &colors, Some(live))),
        any(remember_row(theme, &colors, Some(live))),
        any(connect_button(
            theme,
            &colors,
            Some(live),
            Prominence::Primary,
        )),
    ]
}

/// `/` carries no app bar, so the add-server page composes its own from the
/// bar furniture: the back chevron (popping this page off the shell) and the
/// centred title, balanced by an empty slot the chevron's width.
fn add_server_bar(theme: &Theme, colors: &Palette) -> impl View<ConnectState> + use<> {
    row()
        .child(bar_icon(
            icons::CHEVRON_LEFT,
            BACK_LABEL,
            |state: &mut ConnectState| state.app_nav.pop(),
        ))
        .flex(
            1,
            Align(
                Alignment::CENTER,
                text(ADD_SERVER_TITLE).style(inked(&theme.type_scale.title_small, colors.fg)),
            ),
        )
        .child(SizedBox::<ConnectState>(Some(ADD_BAR_SLOT), None))
        .cross_axis(CrossAxisAlignment::Center)
}

/// The add-server heading: `badge::heading`'s shape over this page's copy.
fn add_server_heading(theme: &Theme, colors: &Palette) -> impl View<ConnectState> + use<> {
    column()
        .child(Align(
            Alignment::CENTER,
            text(ADD_SERVER_HEADING).style(inked(&theme.type_scale.title_medium, colors.fg)),
        ))
        .child(SizedBox::<ConnectState>(None, Some(spacing::XS)))
        .child(
            text(ADD_SERVER_SUBTITLE)
                .style(inked(&theme.type_scale.body_small, colors.fg_muted))
                .align(TextAlign::Center),
        )
        .cross_axis(CrossAxisAlignment::Stretch)
}
