//! The `/settings` route: the connected server's own settings. Its own gestures take
//! `&mut SettingsState`; a dialog or sheet it pushes is a page on the app's overlay host and sees
//! `&mut AppState`, so [`Actions`] is a clonable bundle serving both sides.
use std::rc::Rc;
use std::sync::Arc;

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::authoring::Rect;
use frust::{
    AnyView, Component, EdgeInsets, Either, Get, GetUntracked, NavigatorController, Padding,
    PopResult, RwSignal, Set, View, WindowMetrics, any, component, spawn_local, use_context,
};
use frust_glyph::show_glyph_menu;

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::{KeyringHandle, expect_keyring};
use crate::features::connect::domain::entities::SavedServer;
use crate::features::settings::di::expect_auth_tokens_repository;
use crate::features::settings::domain::entities::TokenScope;
use crate::responsive::Responsive;
use crate::routes::{APP_SETTINGS_ROUTE, Nav, PageMark, TOKENS_ROUTE, app_nav};
use crate::ui::dialogs::confirm_dialog;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::sheet::show_sheet;
use crate::ui::spacing;
use crate::ui::toasts::app_toasts;

use super::app_pages::ServerActions;
use super::controller::{
    EpochFence, ScopeFilter, SettingsController, TokenListView, token_list_view, token_shown,
};
use super::scramble::{RevokeAnimations, RevokeScramble, ScramblePose};
use super::widgets::{self, DISCONNECT_COPY, DISCONNECT_LABEL, RowAction, RowHost, TokenMenu};

const CONNECT_ROUTE: &str = "/";

const READ_ONLY_TOKEN_TOAST: &str =
    "Read-only session \u{2014} reconnect with a read-write token to change this";

pub fn settings_page(overlays: NavigatorController<AppState>) -> impl View<AppState> {
    component(SettingsScreen { overlays })
}

pub struct SettingsScreen {
    overlays: NavigatorController<AppState>,
}

/// Shared by `/settings` and `/settings/tokens`, both screens of the connected server.
pub struct SettingsState {
    pub actions: Actions,
    /// The connection the saved list was last read under; a change re-reads it.
    servers_seen: Option<Option<String>>,
    /// The connection epoch the token list was last requested for.
    tokens_epoch: u64,
    /// The navigation the token list was last read under; a return to the page re-reads it.
    tokens_mark: Option<PageMark>,
    /// Revokes begun under an earlier connection, whose rows never show under this one.
    stale_scrambles: Vec<String>,
}

/// The token list's filter, kept where a dialog's callback can clear it.
#[derive(Clone, Copy)]
pub struct TokenFilter {
    pub query: RwSignal<String>,
    pub segment: RwSignal<ScopeFilter>,
}

impl TokenFilter {
    fn new() -> Self {
        Self {
            query: RwSignal::new(String::new()),
            segment: RwSignal::new(ScopeFilter::All),
        }
    }

    /// Writes only a changed value; a disposed screen's signals ignore it.
    fn clear(self) {
        if self
            .query
            .try_get_untracked()
            .is_some_and(|q| !q.is_empty())
        {
            self.query.set(String::new());
        }
        if self
            .segment
            .try_get_untracked()
            .is_some_and(|s| s != ScopeFilter::All)
        {
            self.segment.set(ScopeFilter::All);
        }
    }

    fn admits(self, name: &str, scope: TokenScope) -> bool {
        let query = self.query.try_get_untracked().unwrap_or_default();
        let segment = self.segment.try_get_untracked().unwrap_or_default();
        token_shown(name, scope, &query, segment)
    }
}

#[derive(Clone)]
pub struct Actions {
    pub overlays: NavigatorController<AppState>,
    pub app_nav: Nav,
    pub controller: Arc<SettingsController>,
    pub connection: Arc<ConnectionController>,
    pub keyring: KeyringHandle,
    /// The connected server's saved row: its credential delete and its forget.
    pub servers: ServerActions,
    pub filter: TokenFilter,
    pub(super) scrambles: RevokeAnimations,
}

/// One build's view of the token list, shared by the tokens page and the Tablet card.
pub(super) struct TokenFrame {
    pub(super) view: TokenListView,
    pub(super) loaded: bool,
    pub(super) failed: bool,
    pub(super) showing: Vec<(RevokeScramble, ScramblePose)>,
    pub(super) query: String,
    pub(super) segment: ScopeFilter,
    pub(super) device_token: Option<String>,
}

impl Component for SettingsScreen {
    type State = SettingsState;

    fn init(&self) -> SettingsState {
        SettingsState::mount(&self.overlays)
    }

    fn build(&self, state: &mut SettingsState) -> impl View<SettingsState> {
        let info = state.actions.connection.server_info.get();

        let theme = active_theme();
        let colors = Palette::from_theme(&theme);

        let responsive = Responsive::of(use_context::<WindowMetrics>());

        let saved = state.actions.servers.controller.saved.get();
        let connected = state.actions.servers.connected_endpoint();
        state.reread_servers_on_change(connected.clone());
        state.reread_tokens_on_return();
        let saved_row = connected_row(&saved, connected.as_deref());
        let label = info.as_ref().map(|info| {
            saved_row.map_or_else(|| host_of(&info.endpoint), |server| server.label.clone())
        });
        let has_credential = saved_row.is_some_and(|server| server.has_credential);
        let device_token = saved_row.and_then(|server| server.token_name.clone());
        let tokens = state.token_frame(device_token.as_deref(), theme.motion.reduce_motion);

        let identity = widgets::identity_card(info.as_ref(), label.as_deref(), &colors);
        let credential = has_credential.then(|| widgets::saved_credential_row(&colors));
        let forget = saved_row.map(|_| widgets::forget_server_row(&colors));
        let tokens_row = widgets::tokens_row(&tokens.view, tokens.loaded, tokens.failed, &colors);

        if responsive.is_tablet() {
            let full = responsive.is_full();
            let mut access: Vec<AnyView<SettingsState>> = Vec::new(); // erasure: keep accumulator into Vec<AnyView>
            if !full {
                access.push(tokens_row);
            }
            access.extend(credential);
            access.extend(widgets::app_settings_rows(&colors));
            let mut this_server: Vec<AnyView<SettingsState>> = Vec::new(); // erasure: keep accumulator into Vec<AnyView>
            this_server.extend(forget);
            this_server.push(any(widgets::disconnect_button()));
            let read_only = state.actions.connection.is_read_only.get();
            return widgets::page_tone(
                Either::Left(widgets::server_tablet_scroller(
                    widgets::ServerCards {
                        identity: any(identity),
                        access,
                        this_server,
                        tokens: full.then(|| widgets::tokens_card(&tokens, read_only, &colors)),
                    },
                    full,
                    &colors,
                )),
                &colors,
            );
        }

        let mut body = vec![any(Padding(
            EdgeInsets::symmetric(spacing::XL, 0.0),
            identity,
        ))]; // erasure: keep accumulator into Vec<AnyView>
        body.push(any(widgets::group_label("access", &colors)));
        body.push(tokens_row);
        body.extend(credential);
        if let Some(forget) = forget {
            body.push(any(widgets::group_label("this server", &colors)));
            body.push(forget);
        }
        body.push(any(widgets::disconnect_button()));
        body.push(any(widgets::group_label("app", &colors)));
        body.extend(widgets::app_settings_rows(&colors));

        widgets::page_tone(
            Either::Right(widgets::phone_scroller(body, responsive.wide_clamp)),
            &colors,
        )
    }
}

/// The endpoint without its port: the label a row saved nowhere falls back to.
fn host_of(endpoint: &str) -> String {
    endpoint
        .rsplit_once(':')
        .map_or(endpoint, |(host, _)| host)
        .to_string()
}

pub(super) fn connected_row<'a>(
    saved: &'a [SavedServer],
    connected: Option<&str>,
) -> Option<&'a SavedServer> {
    connected.and_then(|endpoint| saved.iter().find(|server| server.endpoint() == endpoint))
}

impl SettingsState {
    /// Mounts the screen's controllers, so it must run in a `Component::init`.
    pub(super) fn mount(overlays: &NavigatorController<AppState>) -> Self {
        let keyring = expect_keyring();
        let connection = expect_connection_controller();
        let controller = use_controller::<SettingsController, AppFailure>(|| {
            SettingsController::new(expect_auth_tokens_repository())
        });

        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        let tokens_epoch = connection.connection_epoch.get_untracked();
        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.load_for(tokens_epoch).await });

        let servers = ServerActions::init(overlays);

        SettingsState {
            actions: Actions {
                overlays: overlays.clone(),
                app_nav: app_nav(),
                controller,
                connection,
                keyring,
                servers,
                filter: TokenFilter::new(),
                scrambles: RevokeAnimations::new(),
            },
            servers_seen: None,
            tokens_epoch,
            tokens_mark: None,
            stale_scrambles: Vec::new(),
        }
    }

    /// A connect, switch or teardown re-reads the store, which `connect` may have written;
    /// read off this build.
    pub(super) fn reread_servers_on_change(&mut self, connected: Option<String>) {
        let seen = Some(connected);
        if self.servers_seen == seen {
            return;
        }
        self.servers_seen = seen;
        let servers = Arc::clone(&self.actions.servers.controller);
        spawn_local(async move { servers.refresh_saved_servers() });
    }

    /// A return to this page (the tokens page popping) re-reads the list its counts come from.
    fn reread_tokens_on_return(&mut self) {
        let mark = self.actions.app_nav.mark_top();
        let previous = self.tokens_mark.replace(mark);
        if previous.is_some_and(|seen| seen != mark) {
            let controller = Arc::clone(&self.actions.controller);
            spawn_local(async move {
                controller.refresh_tokens().await;
            });
        }
    }

    /// Tracked. A new connection reloads the list and clears the filter, and until that list
    /// lands nothing of the previous server's shows.
    pub(super) fn token_frame(&mut self, device_token: Option<&str>, reduce: bool) -> TokenFrame {
        let epoch = self.actions.connection.connection_epoch.get();
        if epoch != self.tokens_epoch {
            self.tokens_epoch = epoch;
            self.stale_scrambles = self
                .actions
                .scrambles
                .live()
                .into_iter()
                .map(|entry| entry.token.name)
                .collect();
            let controller = Arc::clone(&self.actions.controller);
            let filter = self.actions.filter;
            spawn_local(async move {
                filter.clear();
                controller.load_for(epoch).await;
            });
        }
        let snapshot = self.actions.controller.snapshot_for(epoch);
        let query = self.actions.filter.query.get();
        let segment = self.actions.filter.segment.get();

        // Tracked: a start or an RPC answer wakes this rebuild; the row's ticker drives the frames.
        let scrambles = self.actions.scrambles;
        let timelines = scrambles.live();
        self.stale_scrambles
            .retain(|name| timelines.iter().any(|entry| entry.token.name == *name));
        let posed: Vec<(RevokeScramble, ScramblePose)> = timelines
            .iter()
            .map(|entry| (entry.clone(), entry.pose(reduce)))
            .collect();
        if posed
            .iter()
            .any(|(entry, pose)| !pose.running && entry.answered)
        {
            // Swept off this build rather than from inside it.
            spawn_local(async move { scrambles.sweep(reduce) });
        }
        let showing = posed
            .into_iter()
            .filter(|(entry, pose)| pose.running || !entry.answered)
            .filter(|(entry, _)| !self.stale_scrambles.contains(&entry.token.name))
            .collect();

        TokenFrame {
            view: token_list_view(&snapshot.tokens, &query, segment, device_token),
            loaded: snapshot.loaded,
            failed: snapshot.failed,
            showing,
            query,
            segment,
            device_token: device_token.map(str::to_string),
        }
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
    /// A filter that would hide the new token is cleared.
    pub fn open_create_token_dialog(&self) {
        if self.tokens_blocked() {
            return;
        }
        let controller = Arc::clone(&self.controller);
        let overlays = self.overlays.clone();
        let filter = self.filter;
        let fence = EpochFence::capture(self.connection.connection_epoch);
        widgets::open_create_token_dialog(
            &self.overlays,
            move |_app: &mut AppState, name: String, read_only: bool| {
                let handle = Arc::clone(&controller);
                let overlays = overlays.clone();
                let confirmation = format!("Created {name}");
                spawn_local(async move {
                    let scope = if read_only {
                        TokenScope::ReadOnly
                    } else {
                        TokenScope::ReadWrite
                    };
                    if handle.create_token(name.clone(), read_only, fence).await {
                        if !filter.admits(&name, scope) {
                            filter.clear();
                        }
                        app_toasts().show(confirmation);
                        open_secret_reveal(&overlays, &handle);
                    }
                });
            },
        );
    }

    /// The row's ⋯ menu: Copy name always works; Revoke is refused on a read-only session.
    pub fn open_token_menu(&self, name: &str, anchor: Rect) {
        let actions = self.clone();
        let name = name.to_string();
        show_glyph_menu(
            &self.overlays,
            anchor,
            widgets::token_menu,
            move |_app: &mut AppState, result: PopResult| match result
                .take::<usize>()
                .and_then(TokenMenu::from_index)
            {
                Some(TokenMenu::CopyName) => widgets::copy_token_name(&name),
                Some(TokenMenu::Revoke) => actions.open_revoke_dialog(&name),
                None => {}
            },
        );
    }

    pub fn open_revoke_dialog(&self, name: &str) {
        if self.tokens_blocked() {
            return;
        }
        let actions = self.clone();
        let name = name.to_string();
        let fence = EpochFence::capture(self.connection.connection_epoch);
        confirm_dialog(
            &self.overlays,
            "Revoke token?",
            "Any client still using this token is disconnected immediately.",
            "Revoke",
            move |_app: &mut AppState| actions.revoke(&name, fence),
        );
    }

    /// The scramble and the RPC start on the same tap, so a fast revoke still shows the row go;
    /// the snapshot, at its on-screen index, precedes the refresh that removes the token.
    fn revoke(&self, name: &str, fence: EpochFence) {
        let controller = Arc::clone(&self.controller);
        let scrambles = self.scrambles;
        let tokens = controller.tokens.get_untracked();
        let view = token_list_view(
            &tokens,
            &self.filter.query.get_untracked(),
            self.filter.segment.get_untracked(),
            self.device_token_untracked().as_deref(),
        );
        if let Some((index, row)) = view
            .rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.token.name == name)
        {
            scrambles.begin(&row.token, index);
        }
        let name = name.to_string();
        let confirmation = format!("Revoked {name}");
        spawn_local(async move {
            if controller.revoke_token(name.clone(), fence).await {
                app_toasts().show(confirmation);
            }
            // A refusal leaves the token in `tokens`, so dropping the timeline restores the row.
            scrambles.answered(&name);
        });
    }

    fn device_token_untracked(&self) -> Option<String> {
        let connected = self.servers.connected_endpoint_untracked();
        let saved = self.servers.controller.saved.get_untracked();
        connected_row(&saved, connected.as_deref()).and_then(|server| server.token_name.clone())
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

    /// The connected server's own credential: its token is revoked on the server, as the sheet's
    /// delete does for it. Nothing to close afterwards: the row leaves with the claim.
    pub fn open_delete_credential_dialog(&self) {
        if let Some(endpoint) = self.servers.connected_endpoint_untracked() {
            self.servers
                .confirm_delete_credential(&endpoint, Rc::new(|| {}));
        }
    }

    pub fn open_forget_server_dialog(&self) {
        if let Some(endpoint) = self.servers.connected_endpoint_untracked() {
            self.servers
                .confirm_forget_server(&endpoint, Rc::new(|| {}));
        }
    }

    pub fn open_app_settings(&self) {
        self.app_nav.push(APP_SETTINGS_ROUTE);
    }

    pub fn open_tokens(&self) {
        self.app_nav.push(TOKENS_ROUTE);
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
            RowAction::AuthTokens => self.actions.open_tokens(),
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

pub(super) fn open_token_menu(state: &mut SettingsState, name: &str, anchor: Rect) {
    state.actions.open_token_menu(name, anchor);
}

pub(super) fn set_token_query(state: &mut SettingsState, query: String) {
    state.actions.filter.query.set(query);
}

pub(super) fn set_token_segment(state: &mut SettingsState, segment: ScopeFilter) {
    state.actions.filter.segment.set(segment);
}

pub(super) fn open_disconnect_dialog(state: &mut SettingsState) {
    state.actions.open_disconnect_dialog();
}

pub(super) fn open_delete_credential_dialog(state: &mut SettingsState) {
    state.actions.open_delete_credential_dialog();
}

pub(super) fn open_forget_server_dialog(state: &mut SettingsState) {
    state.actions.open_forget_server_dialog();
}
