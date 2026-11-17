//! The `/settings` route: the connected server's own settings. Its own gestures take
//! `&mut SettingsState`; a dialog or sheet it pushes is a page on the app's overlay host and sees
//! `&mut AppState`, so [`Actions`] is a clonable bundle serving both sides.
use std::rc::Rc;
use std::sync::Arc;

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::{
    AnyView, Component, Either, Get, GetUntracked, NavigatorController, View, WindowMetrics, any,
    component, spawn_local, use_context,
};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::{
    KeyringHandle, expect_credential_service, expect_keyring, expect_saved_servers,
};
use crate::features::pro::di::expect_pro_controller;
use crate::features::pro::domain::entities::ServerLimitVerdict;
use crate::features::pro::presentation::controller::ProController;
use crate::features::pro::presentation::{UpgradeContext, show_upgrade_sheet};
use crate::features::settings::di::expect_auth_tokens_repository;
use crate::responsive::Responsive;
use crate::routes::{APP_SETTINGS_ROUTE, Nav, add_server_location, app_nav};
use crate::ui::dialogs::confirm_dialog;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::sheet::show_sheet;
use crate::ui::toasts::app_toasts;

use super::controller::{SettingsController, saved_server_rows};
use super::scramble::{RevokeAnimations, RevokeScramble, ScramblePose};
use super::widgets::{self, DISCONNECT_COPY, DISCONNECT_LABEL, RowAction, RowHost};

const CONNECT_ROUTE: &str = "/";

const READ_ONLY_TOKEN_TOAST: &str =
    "Read-only session \u{2014} reconnect with a read-write token to change this";

pub fn settings_page(overlays: NavigatorController<AppState>) -> impl View<AppState> {
    component(SettingsScreen { overlays })
}

pub struct SettingsScreen {
    overlays: NavigatorController<AppState>,
}

pub struct SettingsState {
    pub actions: Actions,
}

#[derive(Clone)]
pub struct Actions {
    pub overlays: NavigatorController<AppState>,
    pub app_nav: Nav,
    pub controller: Arc<SettingsController>,
    pub connection: Arc<ConnectionController>,
    pub keyring: KeyringHandle,
    pub pro: Arc<ProController>,
    pub(super) scrambles: RevokeAnimations,
}

impl Component for SettingsScreen {
    type State = SettingsState;

    fn init(&self) -> SettingsState {
        let keyring = expect_keyring();
        let connection = expect_connection_controller();
        let controller = use_controller::<SettingsController, AppFailure>({
            let keyring = keyring.clone();
            let connection = Arc::clone(&connection);
            move || {
                SettingsController::new(
                    expect_auth_tokens_repository(),
                    expect_saved_servers(),
                    expect_credential_service(),
                    keyring,
                    connection,
                )
            }
        });

        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.refresh().await });

        // Refreshed on every mount, so a saved-row switch and `connect`'s gate never disagree.
        let pro = expect_pro_controller();
        let pro_refresh = Arc::clone(&pro);
        spawn_local(async move { pro_refresh.refresh().await });

        SettingsState {
            actions: Actions {
                overlays: self.overlays.clone(),
                app_nav: app_nav(),
                controller,
                connection,
                keyring,
                pro,
                scrambles: RevokeAnimations::new(),
            },
        }
    }

    fn build(&self, state: &mut SettingsState) -> impl View<SettingsState> {
        let info = state.actions.connection.server_info.get();
        let read_only = state.actions.connection.is_read_only.get();
        let tokens = state.actions.controller.tokens.get();
        let saved = state.actions.controller.saved.get();

        let theme = active_theme();
        let colors = Palette::from_theme(&theme);

        let responsive = Responsive::of(use_context::<WindowMetrics>());

        let mut groups: Vec<(&'static str, Vec<AnyView<SettingsState>>)> = Vec::new();
        groups.push((
            "connection",
            widgets::connection_rows(info.as_ref(), &colors),
        ));

        // Tracked: a start or an RPC answer wakes this rebuild; the row's ticker drives the frames.
        let reduce_motion = theme.motion.reduce_motion;
        let scrambles = state.actions.scrambles;
        let timelines = scrambles.live();
        let showing: Vec<(RevokeScramble, ScramblePose)> = timelines
            .iter()
            .map(|entry| (entry.clone(), entry.pose(reduce_motion)))
            .filter(|(entry, pose)| pose.running || !entry.answered)
            .collect();
        if showing.len() != timelines.len() {
            // Swept off this build rather than from inside it.
            spawn_local(async move { scrambles.sweep(reduce_motion) });
        }

        let mut token_group = widgets::token_rows(&tokens, &showing, read_only, &colors);
        token_group.push(any(widgets::create_token_row(&colors, !read_only))); // erasure: keep accumulator into Vec<AnyView>
        groups.push(("auth tokens", token_group));

        let active = info.as_ref().map(|info| info.endpoint.as_str());
        let mut saved_group = Vec::new(); // erasure: keep accumulator into Vec<AnyView>
        if saved.is_empty() {
            saved_group.push(any(widgets::no_saved_servers_row(&colors)));
        }
        saved_group.extend(
            saved_server_rows(&saved, active)
                .iter()
                .map(|row| any(widgets::saved_server_item(row, &colors))),
        );
        groups.push(("saved servers", saved_group));
        groups.push(("app", widgets::app_settings_rows(&colors)));

        if responsive.is_tablet() {
            return widgets::page_tone(
                Either::Left(widgets::tablet_scroller(
                    groups,
                    responsive.is_full(),
                    &colors,
                )),
                &colors,
            );
        }

        let mut body = Vec::new(); // erasure: keep accumulator into Vec<AnyView>
        for (label, rows) in groups {
            body.push(any(widgets::group_label(label, &colors)));
            body.extend(rows);
        }
        body.push(any(widgets::disconnect_button()));

        widgets::page_tone(
            Either::Right(widgets::phone_scroller(body, responsive.wide_clamp)),
            &colors,
        )
    }
}

impl Actions {
    fn tokens_blocked(&self) -> bool {
        if self.connection.is_read_only.get_untracked() {
            app_toasts().show(READ_ONLY_TOKEN_TOAST);
            true
        } else {
            false
        }
    }

    /// The confirmation toasts only on a landed mint; a failure already toasts through the bridge.
    pub fn open_create_token_dialog(&self) {
        if self.tokens_blocked() {
            return;
        }
        let controller = Arc::clone(&self.controller);
        let overlays = self.overlays.clone();
        widgets::open_create_token_dialog(
            &self.overlays,
            move |_app: &mut AppState, name: String, read_only: bool| {
                let handle = Arc::clone(&controller);
                let overlays = overlays.clone();
                let confirmation = format!("Created {name}");
                spawn_local(async move {
                    if handle.create_token(name, read_only).await {
                        app_toasts().show(confirmation);
                        open_secret_reveal(&overlays, &handle);
                    }
                });
            },
        );
    }

    pub fn open_revoke_dialog(&self, name: &str) {
        if self.tokens_blocked() {
            return;
        }
        let actions = self.clone();
        let name = name.to_string();
        confirm_dialog(
            &self.overlays,
            "Revoke token?",
            "Any client still using this token is disconnected immediately.",
            "Revoke",
            move |_app: &mut AppState| actions.revoke(&name),
        );
    }

    /// The scramble and the RPC start on the same tap, so a fast revoke still shows the row go;
    /// the snapshot precedes the refresh that removes the token mid-animation.
    fn revoke(&self, name: &str) {
        let controller = Arc::clone(&self.controller);
        let scrambles = self.scrambles;
        if let Some((index, token)) = controller
            .tokens
            .get_untracked()
            .into_iter()
            .enumerate()
            .find(|(_, token)| token.name == name)
        {
            scrambles.begin(&token, index);
        }
        let name = name.to_string();
        let confirmation = format!("Revoked {name}");
        spawn_local(async move {
            if controller.revoke_token(name.clone()).await {
                app_toasts().show(confirmation);
            }
            // A refusal leaves the token in `tokens`, so dropping the timeline restores the row.
            scrambles.answered(&name);
        });
    }

    /// No read-only guard: deleting a device-local credential is not a server mutation.
    pub fn open_delete_credential_dialog(&self, endpoint: &str) {
        let controller = Arc::clone(&self.controller);
        let endpoint = endpoint.to_string();
        confirm_dialog(
            &self.overlays,
            "Delete credential?",
            format!(
                "The credential saved for {endpoint} is removed from this device and its \
                 token revoked on the server. The saved server itself is kept."
            ),
            "Delete",
            move |_app: &mut AppState| {
                let handle = Arc::clone(&controller);
                let endpoint = endpoint.clone();
                let confirmation = format!("Deleted the credential for {endpoint}");
                spawn_local(async move {
                    if handle.delete_credential(endpoint).await {
                        app_toasts().show(confirmation);
                    }
                });
            },
        );
    }

    pub fn open_forget_server_dialog(&self, endpoint: &str) {
        let controller = Arc::clone(&self.controller);
        let endpoint = endpoint.to_string();
        let has_credential = controller
            .saved
            .get_untracked()
            .iter()
            .any(|server| server.endpoint() == endpoint && server.has_credential);
        let credential_clause = if has_credential {
            ", along with its saved credential"
        } else {
            ""
        };
        confirm_dialog(
            &self.overlays,
            "Forget server?",
            format!(
                "The saved profile for {endpoint} is deleted{credential_clause}. You will \
                 need its address and a token to connect again."
            ),
            "Forget",
            move |_app: &mut AppState| {
                let handle = Arc::clone(&controller);
                let endpoint = endpoint.clone();
                let confirmation = format!("Forgot {endpoint}");
                spawn_local(async move {
                    if handle.forget_server(endpoint).await {
                        app_toasts().show(confirmation);
                    }
                });
            },
        );
    }

    /// The router's session guard runs only at navigation time, so confirming both clears the
    /// connection and navigates; `go`, since a disconnected session is a new stack.
    pub fn open_disconnect_dialog(&self) {
        let connection = Arc::clone(&self.connection);
        let keyring = self.keyring.clone();
        let app_nav = self.app_nav.clone();
        confirm_dialog(
            &self.overlays,
            DISCONNECT_LABEL,
            DISCONNECT_COPY,
            DISCONNECT_LABEL,
            move |_app: &mut AppState| {
                connection.lock_and_disconnect(&keyring);
                app_nav.go(CONNECT_ROUTE);
            },
        );
    }

    /// The verdict comes first, so a blocked row raises the paywall and no unlock prompt. The
    /// settle-time pop is marked, so it never fires once the user has left Settings.
    pub fn switch_server(&self, endpoint: &str) {
        let active = self
            .connection
            .server_info
            .get_untracked()
            .is_some_and(|info| info.endpoint == endpoint);
        if active
            || self.controller.switching.get_untracked()
            || self.connection.connecting.get_untracked()
        {
            return;
        }
        let saved = self.controller.saved.get_untracked();
        let Some(server) = saved.iter().find(|server| server.endpoint() == endpoint) else {
            return;
        };
        if !server.has_credential {
            self.open_add_server(Some(endpoint));
            return;
        }
        if let ServerLimitVerdict::Blocked { .. } = self.pro.verdict_for(&saved, endpoint) {
            let retry = self.clone();
            let endpoint = endpoint.to_string();
            show_upgrade_sheet(
                &self.overlays,
                Arc::clone(&self.pro),
                UpgradeContext {
                    on_unlocked: Rc::new(move || retry.switch_server(&endpoint)),
                    server_limit: None,
                },
            );
            return;
        }
        let nav = self.app_nav.clone();
        let mark = nav.mark_top();
        self.controller.switch_server(
            endpoint.to_string(),
            // Settings is only pushed from Sessions, so a pop returns there.
            Box::new(move || nav.pop_marked(&mark)),
        );
    }

    pub fn open_add_server(&self, endpoint: Option<&str>) {
        self.app_nav.push(&add_server_location(endpoint));
    }

    pub fn open_app_settings(&self) {
        self.app_nav.push(APP_SETTINGS_ROUTE);
    }
}

fn open_secret_reveal(
    overlays: &NavigatorController<AppState>,
    controller: &Arc<SettingsController>,
) {
    let controller = Arc::clone(controller);
    show_sheet(overlays, move |sheet| {
        let theme = active_theme();
        let colors = Palette::from_theme(&theme);
        // Tracked, so the body re-resolves on every navigator rebuild.
        let minted = controller.pending_token_secret.get();
        let sheet = sheet.clone();
        let controller = Arc::clone(&controller);
        widgets::secret_reveal_sheet(minted.as_ref(), &colors, move |_app: &mut AppState| {
            sheet.close();
            controller.clear_pending_secret();
        })
    });
}

impl RowHost for SettingsState {
    fn row_tapped(&mut self, action: RowAction) {
        match action {
            RowAction::AppSettings => self.actions.open_app_settings(),
            // Unreachable: this screen renders no such row, and an inert row carries no gesture.
            RowAction::Static
            | RowAction::Picker(_)
            | RowAction::UpgradeSheet
            | RowAction::GettingStarted => {}
        }
    }
}

pub(super) fn open_create_token_dialog(state: &mut SettingsState) {
    state.actions.open_create_token_dialog();
}

pub(super) fn open_revoke_dialog(state: &mut SettingsState, name: &str) {
    state.actions.open_revoke_dialog(name);
}

pub(super) fn open_delete_credential_dialog(state: &mut SettingsState, endpoint: &str) {
    state.actions.open_delete_credential_dialog(endpoint);
}

pub(super) fn open_forget_server_dialog(state: &mut SettingsState, endpoint: &str) {
    state.actions.open_forget_server_dialog(endpoint);
}

pub(super) fn switch_server(state: &mut SettingsState, endpoint: &str) {
    state.actions.switch_server(endpoint);
}

pub(super) fn open_add_server(state: &mut SettingsState) {
    state.actions.open_add_server(None);
}

pub(super) fn open_disconnect_dialog(state: &mut SettingsState) {
    state.actions.open_disconnect_dialog();
}
