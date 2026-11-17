//! The `/settings/app` route: device-wide settings that open with or without a server. Like
//! `pages`, its gestures take `&mut AppSettingsState` and the sheets it pushes see
//! `&mut AppState`, so [`AppActions`] and [`ServerActions`] are clonable bundles serving both.
use std::rc::Rc;
use std::sync::Arc;

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::{
    AnyView, Component, Either, Get, GetUntracked, NavigatorController, Set, View, WindowMetrics,
    any, component, spawn_local, use_context,
};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::{
    expect_credential_service, expect_keyring, expect_saved_servers,
};
use crate::features::connect::domain::entities::SavedServer;
use crate::features::connect::presentation::controller::SESSIONS_LOCATION;
use crate::features::connect::presentation::widgets::getting_started::open_getting_started;
use crate::features::pro::di::expect_pro_controller;
use crate::features::pro::domain::entities::ServerLimitVerdict;
use crate::features::pro::presentation::controller::{ProController, RestoreOutcome};
use crate::features::pro::presentation::{
    NOTHING_TO_RESTORE_TOAST, PENDING_TOAST, UNAVAILABLE_COPY, UpgradeContext, show_upgrade_sheet,
};
use crate::features::settings::di::{expect_auth_tokens_repository, expect_preferences_repository};
use crate::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle, PollPrefs, font_size_label, refresh_label,
};
use crate::responsive::Responsive;
use crate::routes::{
    CONNECT_LOCATION, Nav, PageMark, SETTINGS_ROUTE, add_server_location, app_nav,
};
use crate::ui::dialogs::confirm_dialog;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::sheet::{SheetHandle, show_sheet};
use crate::ui::toasts::app_toasts;

use super::controller::{AppSettingsController, SavedServersController, saved_server_rows};
use super::widgets::{
    self, CardColumn, FontPicker, FontRow, Picker, PickerKind, RowAction, RowHost,
    ServerSheetActions,
};

const STORE_BUSY_TOAST: &str = "A store operation is already running";

/// An estimate of everything outside the scroll band, not imported from `ui::sheet`'s private
/// geometry; overestimating only shortens the band.
const SHEET_SCROLL_RESERVE_PX: f64 = 220.0;
const SHEET_SCROLL_FLOOR_PX: f64 = 240.0;
/// The Glyph list-row estimate. The band is a snapshot of `WindowMetrics` when the sheet opens,
/// so a resize while it is open does not re-size it.
const PICK_ROW_ESTIMATE_PX: f64 = 48.0;

fn sheet_band_height(metrics: &WindowMetrics, rows_height: f64) -> f64 {
    (metrics.size.height - SHEET_SCROLL_RESERVE_PX)
        .max(SHEET_SCROLL_FLOOR_PX)
        .min(rows_height)
}

type Group = (CardColumn, &'static str, Vec<AnyView<AppSettingsState>>);

pub fn app_settings_page(overlays: NavigatorController<AppState>) -> impl View<AppState> {
    component(AppSettingsScreen { overlays })
}

pub struct AppSettingsScreen {
    overlays: NavigatorController<AppState>,
}

/// The navigation and connection the saved list was last read under; a change re-reads it.
type ServersSeen = (PageMark, Option<String>);

pub struct AppSettingsState {
    pub actions: AppActions,
    servers_seen: Option<ServersSeen>,
}

#[derive(Clone)]
pub struct AppActions {
    pub overlays: NavigatorController<AppState>,
    pub controller: Arc<AppSettingsController>,
    pub pro: Arc<ProController>,
    pub servers: ServerActions,
}

/// Saved-server actions, needing the device's store and keyring but no connection.
#[derive(Clone)]
pub struct ServerActions {
    pub overlays: NavigatorController<AppState>,
    pub app_nav: Nav,
    pub controller: Arc<SavedServersController>,
    pub connection: Arc<ConnectionController>,
    pub pro: Arc<ProController>,
}

impl Component for AppSettingsScreen {
    type State = AppSettingsState;

    fn init(&self) -> AppSettingsState {
        let controller = use_controller::<AppSettingsController, AppFailure>(|| {
            AppSettingsController::new(expect_preferences_repository())
        });

        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.refresh().await });

        // Refreshed on every mount, so the plan row and `connect`'s gate never disagree.
        let pro = expect_pro_controller();
        let pro_refresh = Arc::clone(&pro);
        spawn_local(async move { pro_refresh.refresh().await });

        let connection = expect_connection_controller();
        let servers = use_controller::<SavedServersController, AppFailure>({
            let connection = Arc::clone(&connection);
            move || {
                SavedServersController::new(
                    expect_auth_tokens_repository(),
                    expect_saved_servers(),
                    expect_credential_service(),
                    expect_keyring(),
                    connection,
                )
            }
        });
        use_failure_listener(servers.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });
        servers.refresh_saved_servers();

        AppSettingsState {
            actions: AppActions {
                overlays: self.overlays.clone(),
                controller,
                pro: Arc::clone(&pro),
                servers: ServerActions {
                    overlays: self.overlays.clone(),
                    app_nav: app_nav(),
                    controller: servers,
                    connection,
                    pro,
                },
            },
            servers_seen: None,
        }
    }

    fn build(&self, state: &mut AppSettingsState) -> impl View<AppSettingsState> {
        let appearance = state.actions.controller.appearance.get();
        let poll = state.actions.controller.poll.get();
        let restoring = state.actions.controller.restoring.get();
        let entitlement = state.actions.pro.entitlement.get();
        let saved = state.actions.servers.controller.saved.get();
        let connected = state.actions.servers.connected_endpoint();
        state.reread_servers_on_change(connected.clone());

        let theme = active_theme();
        let colors = Palette::from_theme(&theme);
        let responsive = Responsive::of(use_context::<WindowMetrics>());

        let mut groups: Vec<Group> = vec![
            (
                CardColumn::Left,
                "terminal",
                widgets::appearance_rows(&appearance, &colors),
            ),
            (
                CardColumn::Left,
                "behaviour",
                widgets::poll_rows(&poll, &colors),
            ),
            (
                CardColumn::Right,
                widgets::PRO_GROUP_LABEL,
                widgets::pro_rows(entitlement, restoring, &colors),
            ),
        ];
        groups.push(servers_group(&saved, connected.as_deref(), &colors));
        groups.push((CardColumn::Left, "about", widgets::about_rows(&colors)));

        if responsive.is_tablet() {
            return widgets::page_tone(
                Either::Left(widgets::card_grid(
                    groups,
                    responsive.is_full(),
                    None,
                    &colors,
                )),
                &colors,
            );
        }

        let mut body = Vec::new(); // erasure: keep accumulator into Vec<AnyView>
        for (_, label, rows) in groups {
            body.push(any(widgets::group_label(label, &colors)));
            body.extend(rows);
        }
        widgets::page_tone(
            Either::Right(widgets::phone_scroller(body, responsive.wide_clamp)),
            &colors,
        )
    }
}

/// Between `muxr pro` and `about`, under `muxr pro` in a Full tablet's right column.
fn servers_group(saved: &[SavedServer], connected: Option<&str>, colors: &Palette) -> Group {
    let mut rows = Vec::new(); // erasure: keep accumulator into Vec<AnyView>
    if saved.is_empty() {
        rows.push(widgets::no_saved_servers_row(colors));
    }
    rows.extend(
        saved_server_rows(saved, connected)
            .iter()
            .map(|row| widgets::saved_server_item(row, colors)),
    );
    (CardColumn::Right, widgets::SERVERS_GROUP_LABEL, rows)
}

impl AppSettingsState {
    /// A return to this page (a pop above it, a pushed add page closing) or a server switch
    /// re-reads the store, which another screen may have written; read off this build.
    fn reread_servers_on_change(&mut self, connected: Option<String>) {
        let seen = (self.actions.servers.app_nav.mark_top(), connected);
        if self.servers_seen.as_ref() == Some(&seen) {
            return;
        }
        self.servers_seen = Some(seen);
        let servers = Arc::clone(&self.actions.servers.controller);
        spawn_local(async move { servers.refresh_saved_servers() });
    }
}

impl RowHost for AppSettingsState {
    fn row_tapped(&mut self, action: RowAction) {
        match action {
            RowAction::Picker(kind) => self.actions.open_picker(kind),
            RowAction::UpgradeSheet => self.actions.open_upgrade_sheet(),
            RowAction::GettingStarted => open_getting_started(&self.actions.overlays),
            // Unreachable: this screen renders no such row, and an inert row carries no gesture.
            RowAction::Static | RowAction::AppSettings => {}
        }
    }
}

impl AppActions {
    /// The body re-runs on every navigator rebuild, so the check mark tracks the stored value.
    fn open_picker(&self, kind: PickerKind) {
        if kind == PickerKind::FontFamily {
            self.open_font_picker();
            return;
        }
        let actions = self.clone();
        show_sheet(&self.overlays, move |sheet| {
            let theme = active_theme();
            let colors = Palette::from_theme(&theme);
            let picker = actions.picker(kind);
            let picking = actions.clone();
            let sheet = sheet.clone();
            widgets::picker_sheet(&picker, &colors, move |_app: &mut AppState, index| {
                // Close first: a sheet pop lands at the next rebuild, so a second tap inside
                // that frame would pop the route out from under the picker.
                sheet.close();
                picking.pick(kind, index);
            })
        });
    }

    fn picker(&self, kind: PickerKind) -> Picker {
        let appearance = self.controller.appearance_untracked();
        let poll = self.controller.poll_untracked();
        let (options, selected): (Vec<String>, Option<usize>) = match kind {
            PickerKind::FontFamily => (Vec::new(), None),
            PickerKind::FontSize => (
                AppearancePrefs::FONT_SIZES_PX
                    .iter()
                    .map(|px| font_size_label(*px))
                    .collect(),
                AppearancePrefs::FONT_SIZES_PX
                    .iter()
                    .position(|px| *px == appearance.font_size_px),
            ),
            PickerKind::CursorStyle => (
                CursorStyle::ALL
                    .iter()
                    .map(|style| style.option_label().to_string())
                    .collect(),
                CursorStyle::ALL
                    .iter()
                    .position(|style| *style == appearance.cursor_style),
            ),
            PickerKind::LayoutRefresh => (
                PollPrefs::REFRESH_CHOICES_SECS
                    .iter()
                    .map(|secs| refresh_label(*secs))
                    .collect(),
                PollPrefs::REFRESH_CHOICES_SECS
                    .iter()
                    .position(|secs| *secs == poll.layout_refresh_secs),
            ),
        };
        let rows_height = options.len() as f64 * PICK_ROW_ESTIMATE_PX;
        let max_height =
            use_context::<WindowMetrics>().map(|metrics| sheet_band_height(&metrics, rows_height));
        Picker {
            title: kind.title(),
            options,
            selected,
            max_height,
        }
    }

    /// An index outside the current list is dropped, never clamped, since a guess is unchosen.
    fn pick(&self, kind: PickerKind, index: usize) {
        let controller = Arc::clone(&self.controller);
        match kind {
            // Unreachable: the font family persists through `pick_font_family`.
            PickerKind::FontFamily => {}
            PickerKind::FontSize => {
                let Some(px) = AppearancePrefs::FONT_SIZES_PX.get(index) else {
                    return;
                };
                let next = AppearancePrefs {
                    font_size_px: *px,
                    ..controller.appearance_untracked()
                };
                spawn_local(async move { controller.set_appearance(next).await });
            }
            PickerKind::CursorStyle => {
                let Some(style) = CursorStyle::ALL.get(index) else {
                    return;
                };
                let next = AppearancePrefs {
                    cursor_style: *style,
                    ..controller.appearance_untracked()
                };
                spawn_local(async move { controller.set_appearance(next).await });
            }
            PickerKind::LayoutRefresh => {
                let Some(secs) = PollPrefs::REFRESH_CHOICES_SECS.get(index) else {
                    return;
                };
                let next = PollPrefs {
                    layout_refresh_secs: *secs,
                    ..controller.poll_untracked()
                };
                spawn_local(async move { controller.set_poll_prefs(next).await });
            }
        }
    }

    /// The catalog loads here, never at startup; `load` is single-flight.
    fn open_font_picker(&self) {
        let catalog = crate::fonts::catalog::font_catalog();
        spawn_local(async move { catalog.load().await });

        let actions = self.clone();
        show_sheet(&self.overlays, move |sheet| {
            let theme = active_theme();
            let colors = Palette::from_theme(&theme);
            let picker = actions.font_picker();
            let sheet = sheet.clone();
            let selecting = actions.clone();
            let downloading = actions.clone();
            widgets::font_picker_sheet(
                &picker,
                &colors,
                move |_app: &mut AppState, family: String| {
                    sheet.close();
                    selecting.pick_font_family(family);
                },
                move |_app: &mut AppState, family: String| downloading.download_font(&family),
            )
        });
    }

    /// Every read is tracked: a catalog landing or a download state change re-renders the rows.
    fn font_picker(&self) -> FontPicker {
        let catalog = crate::fonts::catalog::font_catalog();
        let status = catalog.status();
        let stale = catalog.is_stale();
        let entries = catalog.entries();
        let selected = self.controller.appearance.get().font_family;

        let installed: Vec<FontRow> = crate::fonts::pickable_terminal_families()
            .into_iter()
            .map(|family| {
                let display = crate::fonts::display_name(&family);
                // Provenance, not name: a downloaded face whose display name matches its
                // family would otherwise read as "built-in".
                let meta = if crate::fonts::store::is_downloaded(&family) {
                    widgets::FONT_META_DOWNLOADED
                } else {
                    widgets::FONT_META_BUILT_IN
                };
                FontRow {
                    family,
                    display,
                    meta: meta.to_string(),
                    state: crate::fonts::catalog::FontState::Installed,
                    download_progress: None,
                }
            })
            .collect();

        let available: Vec<FontRow> = entries
            .iter()
            .filter_map(|entry| {
                let state = catalog.state_of(entry);
                if state == crate::fonts::catalog::FontState::Installed {
                    return None;
                }
                let download_progress = if state == crate::fonts::catalog::FontState::Downloading
                    && entry.total_bytes() > 0
                {
                    Some(catalog.progress_of_entry(entry).unwrap_or(0.0))
                } else {
                    None
                };
                Some(FontRow {
                    family: entry.family.clone(),
                    display: entry.display_name.clone(),
                    meta: format!("{} \u{b7} {}", entry.license, entry.size_label()),
                    state,
                    download_progress,
                })
            })
            .collect();

        let max_height = use_context::<WindowMetrics>()
            .map(|metrics| sheet_band_height(&metrics, f64::INFINITY));

        FontPicker {
            installed,
            available,
            selected,
            status,
            stale,
            max_height,
        }
    }

    fn pick_font_family(&self, family: String) {
        let controller = Arc::clone(&self.controller);
        let next = AppearancePrefs {
            font_family: family,
            ..controller.appearance_untracked()
        };
        spawn_local(async move { controller.set_appearance(next).await });
    }

    fn download_font(&self, family: &str) {
        let catalog = crate::fonts::catalog::font_catalog();
        let Some(entry) = catalog
            .entries_untracked()
            .iter()
            .find(|entry| entry.family == family)
            .cloned()
        else {
            return;
        };
        spawn_local(async move {
            if let Err(failure) = catalog.install(entry).await {
                app_toasts().raise(&failure);
            }
        });
    }

    pub fn toggle_bell(&self) {
        let controller = Arc::clone(&self.controller);
        let current = controller.poll_untracked();
        let next = PollPrefs {
            bell_notifications: !current.bell_notifications,
            ..current
        };
        spawn_local(async move { controller.set_poll_prefs(next).await });
    }

    pub fn restore_purchases(&self) {
        let pro = Arc::clone(&self.pro);
        let restoring = self.controller.restoring;
        spawn_local(async move {
            // Only a restore the controller accepts owns the flag, so a refused tap never clears
            // the one in flight. This check and `restore`'s own guard run in the same poll.
            let accepted = !pro.busy.get_untracked();
            if accepted {
                restoring.set(true);
            }
            let outcome = pro.restore().await;
            if accepted {
                restoring.set(false);
            }
            match outcome {
                RestoreOutcome::Pending => app_toasts().show(PENDING_TOAST),
                RestoreOutcome::NothingToRestore => app_toasts().show(NOTHING_TO_RESTORE_TOAST),
                RestoreOutcome::Unavailable => app_toasts().show(UNAVAILABLE_COPY),
                // The app already bridges the pro controller's failure sink to a toast.
                RestoreOutcome::Failed(_) => {}
                RestoreOutcome::Busy => app_toasts().show(STORE_BUSY_TOAST),
                RestoreOutcome::Restored => {}
            }
        });
    }

    /// `on_unlocked` is a no-op: this screen has no blocked dial to resume.
    pub fn open_upgrade_sheet(&self) {
        show_upgrade_sheet(
            &self.overlays,
            Arc::clone(&self.pro),
            UpgradeContext {
                on_unlocked: Rc::new(|| {}),
                server_limit: None,
            },
        );
    }
}

pub(super) fn restore_purchases(state: &mut AppSettingsState) {
    state.actions.restore_purchases();
}

pub(super) fn open_saved_server(state: &mut AppSettingsState, endpoint: &str) {
    state.actions.servers.open_server(endpoint);
}

pub(super) fn open_add_server(state: &mut AppSettingsState) {
    state.actions.servers.open_add_server(None);
}

/// Where a settled switch lands, read when it starts: the Sessions screen of the new server. The
/// mark keeps a settle that arrives after the user left this page from navigating at all.
#[derive(Clone)]
struct SwitchLanding {
    nav: Nav,
    mark: PageMark,
    /// The shell's landing route; `None` with no shell placed, where there is no Sessions to show.
    shell_root: Option<String>,
}

impl SwitchLanding {
    fn take(nav: &Nav) -> Self {
        SwitchLanding {
            nav: nav.clone(),
            mark: nav.mark_top(),
            shell_root: nav.landing_route().map(|location| location.path),
        }
    }

    /// Over Sessions the stack is Sessions, server settings, app settings (only server settings
    /// links here inside a Sessions shell), so both pages come off. Opened from Connect, the
    /// shell holds app settings alone: it is replaced by a Sessions shell placed as a connect
    /// places it, so Back there asks to disconnect.
    fn land(&self) {
        if self.nav.mark_top() != self.mark {
            return;
        }
        match self.shell_root.as_deref() {
            Some(SESSIONS_LOCATION) => {
                self.nav.pop_marked(&self.mark);
                self.nav.pop();
            }
            Some(_) => {
                self.nav.go(CONNECT_LOCATION);
                self.nav.push(SESSIONS_LOCATION);
            }
            None => {}
        }
    }
}

impl ServerActions {
    /// Tracked: a connect, switch or teardown re-renders the rows.
    pub fn connected_endpoint(&self) -> Option<String> {
        if !self.connection.connected.get() {
            return None;
        }
        self.connection.server_info.get().map(|info| info.endpoint)
    }

    pub fn open_server(&self, endpoint: &str) {
        if self.controller.is_connected_endpoint(endpoint) {
            self.app_nav.push(SETTINGS_ROUTE);
        } else {
            self.open_sheet(endpoint);
        }
    }

    pub fn open_add_server(&self, endpoint: Option<&str>) {
        self.app_nav.push(&add_server_location(endpoint));
    }

    /// The body re-reads the store's row on every navigator rebuild.
    fn open_sheet(&self, endpoint: &str) {
        let actions = self.clone();
        let endpoint = endpoint.to_string();
        show_sheet(&self.overlays, move |sheet| {
            let theme = active_theme();
            let colors = Palette::from_theme(&theme);
            let saved = actions.controller.saved.get();
            let server = saved.iter().find(|server| server.endpoint() == endpoint);
            let on = |run: fn(&ServerActions, &str, &SheetHandle)| {
                let actions = actions.clone();
                let endpoint = endpoint.clone();
                let sheet = sheet.clone();
                Rc::new(move |_app: &mut AppState| run(&actions, &endpoint, &sheet))
                    as widgets::SheetAction
            };
            let sheet_actions = ServerSheetActions {
                on_switch: on(ServerActions::switch_server),
                on_delete_credential: on(ServerActions::open_delete_credential_dialog),
                on_forget: on(ServerActions::open_forget_server_dialog),
            };
            widgets::server_sheet(server, &colors, &sheet_actions)
        });
    }

    /// The verdict comes first, so a blocked row raises the paywall and no unlock prompt; a row
    /// with no credential closes the sheet onto the add page, prefilled.
    pub fn switch_server(&self, endpoint: &str, sheet: &SheetHandle) {
        if self.controller.is_connected_endpoint(endpoint)
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
            let actions = self.clone();
            let endpoint = endpoint.to_string();
            sheet.close_then(move || actions.open_add_server(Some(&endpoint)));
            return;
        }
        if let ServerLimitVerdict::Blocked { .. } = self.pro.verdict_for(&saved, endpoint) {
            let retry = self.clone();
            let endpoint = endpoint.to_string();
            let sheet = sheet.clone();
            show_upgrade_sheet(
                &self.overlays,
                Arc::clone(&self.pro),
                UpgradeContext {
                    on_unlocked: Rc::new(move || retry.switch_server(&endpoint, &sheet)),
                    server_limit: None,
                },
            );
            return;
        }
        let controller = Arc::clone(&self.controller);
        let sheet = sheet.clone();
        let landing = SwitchLanding::take(&self.app_nav);
        self.controller.switch_server(
            endpoint.to_string(),
            Box::new(move || {
                controller.refresh_saved_servers();
                // After the exit: the sheet's body reads this page's signals, which a pop disposes.
                let later = landing.clone();
                if !sheet.close_then(move || later.land()) {
                    landing.land();
                }
            }),
        );
    }

    /// No read-only guard: deleting a device-local credential is not a server mutation.
    pub fn open_delete_credential_dialog(&self, endpoint: &str, sheet: &SheetHandle) {
        let revokes = self.controller.is_connected_endpoint(endpoint);
        let token_clause = if revokes {
            " and its token revoked on the server"
        } else {
            ". Its token stays valid on that server until revoked there"
        };
        let message = format!(
            "The credential saved for {endpoint} is removed from this device{token_clause}. \
             The saved server itself is kept."
        );
        let controller = Arc::clone(&self.controller);
        let endpoint = endpoint.to_string();
        let sheet = sheet.clone();
        confirm_dialog(
            &self.overlays,
            "Delete credential?",
            message,
            "Delete",
            move |_app: &mut AppState| {
                let handle = Arc::clone(&controller);
                let endpoint = endpoint.clone();
                let sheet = sheet.clone();
                let confirmation = format!("Deleted the credential for {endpoint}");
                spawn_local(async move {
                    if handle.delete_credential(endpoint).await {
                        sheet.close();
                        app_toasts().show(confirmation);
                    }
                });
            },
        );
    }

    pub fn open_forget_server_dialog(&self, endpoint: &str, sheet: &SheetHandle) {
        let has_credential = self
            .controller
            .saved
            .get_untracked()
            .iter()
            .any(|server| server.endpoint() == endpoint && server.has_credential);
        let credential_clause = match (
            has_credential,
            self.controller.is_connected_endpoint(endpoint),
        ) {
            (false, _) => "",
            (true, true) => ", along with its saved credential",
            (true, false) => {
                ", along with its saved credential. Its token stays valid on that server until \
                 revoked there"
            }
        };
        let message = format!(
            "The saved profile for {endpoint} is deleted{credential_clause}. You will need its \
             address and a token to connect again."
        );
        let controller = Arc::clone(&self.controller);
        let endpoint = endpoint.to_string();
        let sheet = sheet.clone();
        confirm_dialog(
            &self.overlays,
            "Forget server?",
            message,
            "Forget",
            move |_app: &mut AppState| {
                let handle = Arc::clone(&controller);
                let endpoint = endpoint.clone();
                let sheet = sheet.clone();
                let confirmation = format!("Forgot {endpoint}");
                spawn_local(async move {
                    if handle.forget_server(endpoint).await {
                        sheet.close();
                        app_toasts().show(confirmation);
                    }
                });
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::authoring::WindowInsets;
    use kurbo::Size;

    fn window(height: f64) -> WindowMetrics {
        WindowMetrics::new(Size::new(800.0, height), 1.0, WindowInsets::default())
    }

    #[test]
    fn a_short_window_holds_the_band_at_its_floor() {
        let band = sheet_band_height(&window(390.0), f64::INFINITY);
        assert_eq!(band, SHEET_SCROLL_FLOOR_PX);
    }

    #[test]
    fn a_tall_window_leaves_the_reserve_and_never_outgrows_its_rows() {
        let metrics = window(1200.0);
        assert_eq!(
            sheet_band_height(&metrics, f64::INFINITY),
            1200.0 - SHEET_SCROLL_RESERVE_PX
        );
        assert_eq!(
            sheet_band_height(&metrics, 4.0 * PICK_ROW_ESTIMATE_PX),
            192.0
        );
    }
}
