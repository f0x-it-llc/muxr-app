//! The Connect screen and its add-server mode (`/?mode=add`). The layout half is shared
//! with the cold-start splash, so the splash's last frame is the route's first.

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

pub const ADD_SERVER_TITLE: &str = "add server";
pub const ADD_SERVER_HEADING: &str = "Connect to another server";
pub const ADD_SERVER_SUBTITLE: &str = "It joins the keyring and becomes the active server.";
/// The bar icon's 40px hit target, balancing the back chevron so the title centres.
const ADD_BAR_SLOT: f64 = 40.0;

pub fn connect_page(overlays: frust::NavigatorController<AppState>) -> impl View<AppState> {
    component(ConnectScreen {
        overlays,
        mode: ConnectMode::Home,
        prefill: None,
    })
}

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

pub type SplashLayout<S> = Box<dyn FnOnce(Vec<AnyView<S>>) -> AnyView<S>>;

/// Reads the size class once per build, exactly as [`ConnectScreen`]'s `build` does, so
/// the overlay's last frame is the route's first on every class.
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

struct ConnectScreen {
    overlays: frust::NavigatorController<AppState>,
    mode: ConnectMode,
    prefill: Option<String>,
}

/// Raised on the first build, seen open, then seen closed: a still-refused install then
/// pops the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenGate {
    Clear,
    Pending,
    Raised(usize),
    Showing(usize),
}

pub struct ConnectState {
    controller: Arc<ConnectController>,
    app_nav: Nav,
    overlays: frust::NavigatorController<AppState>,
    pro: Arc<ProController>,
    /// Held so `build` can watch `connected`'s true-to-false edge and scrub the token.
    connection: Arc<ConnectionController>,
    /// Seeded at mount, so an already-connected screen does not clear on its first build.
    was_connected: bool,
    /// `Rc<Cell<bool>>`: the permission check's completion runs off this handler's borrow.
    qr_permission_pending: Rc<Cell<bool>>,
    prefill: Option<String>,
    open_gate: OpenGate,
}

impl ConnectState {
    /// A prefilled row is judged as itself, a blank form as a server not yet named.
    fn add_blocked(&self) -> bool {
        let saved = self.controller.saved.get_untracked();
        let verdict = match &self.prefill {
            Some(endpoint) => self.pro.verdict_for(&saved, endpoint),
            None => self.pro.new_server_verdict(&saved),
        };
        matches!(verdict, ServerLimitVerdict::Blocked { .. })
    }

    fn step_open_gate(&mut self, depth: usize) {
        self.open_gate = match self.open_gate {
            OpenGate::Pending => {
                show_upgrade_sheet(
                    &self.overlays,
                    Arc::clone(&self.pro),
                    UpgradeContext {
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
        // Resolved here, where a reactive owner is live; a press handler cannot.
        let repository = expect_saved_servers();
        let credentials = expect_credential_service();
        let tokens = expect_auth_tokens_repository();
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
        // `/` only: the add-server page saves through itself, leaving `/`'s saver installed.
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

        // A Disconnect can flip `connected` while this screen stays mounted underneath. The
        // add page keeps its form: its own switch drops the old connection mid-dial.
        let connected = state.connection.connected.get();
        if mode == ConnectMode::Home && state.was_connected && !connected {
            state.controller.clear_token();
            // A stashed pairing's pin belonged to the session that just ended.
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

        // The one tracked read of the list, so the splash can hand the builder its own snapshot.
        let saved = state.controller.saved.get();
        if mode == ConnectMode::Add {
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
