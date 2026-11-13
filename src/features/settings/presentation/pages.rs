//! The `/settings` route. Its own gestures take `&mut SettingsState`; a dialog or sheet it
//! pushes is a page on the app's overlay host and sees `&mut AppState`, so [`Actions`] is a
//! clonable bundle serving both sides.
use std::rc::Rc;
use std::sync::Arc;

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::{
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, Either, FlexView,
    Get, GetUntracked, NavigatorController, Padding, Set, SizedBox, View, WindowMetrics, any,
    component, inflexible, safe_area, scroll_view, spawn_local, use_context,
};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::{
    KeyringHandle, expect_credential_service, expect_keyring, expect_saved_servers,
};
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
use crate::routes::{Nav, add_server_location, app_nav};
use crate::ui::dialogs::confirm_dialog;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::sheet::show_sheet;
use crate::ui::toasts::app_toasts;

use super::controller::{SettingsController, saved_server_rows};
use super::scramble::{RevokeAnimations, RevokeScramble, ScramblePose};
use super::widgets::{
    self, DISCONNECT_COPY, DISCONNECT_LABEL, FontPicker, FontRow, Picker, PickerKind, RowAction,
};

const CONNECT_ROUTE: &str = "/";

const READ_ONLY_TOKEN_TOAST: &str =
    "Read-only session \u{2014} reconnect with a read-write token to change this";

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

const SETTINGS_CLAMP_WIDTH: f64 = 640.0;

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
                    expect_preferences_repository(),
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

        // Refreshed on every mount, so this row and `connect`'s gate never disagree.
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
        let appearance = state.actions.controller.appearance.get();
        let poll = state.actions.controller.poll.get();
        let entitlement = state.actions.pro.entitlement.get();

        let theme = active_theme();
        let colors = Palette::from_theme(&theme);

        let responsive = Responsive::of(use_context::<WindowMetrics>());
        let restoring = state.actions.controller.restoring.get();

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

        groups.push((
            widgets::PRO_GROUP_LABEL,
            widgets::pro_rows(entitlement, restoring, &colors),
        ));
        groups.push((
            "terminal appearance",
            widgets::appearance_rows(&appearance, &colors),
        ));
        groups.push(("session poll", widgets::poll_rows(&poll, &colors)));

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

        // The shell's bar consumes the top inset, so the body's safe area skips its top edge.
        let scroller = safe_area(scroll_view(Padding(
            EdgeInsets {
                left: 0.0,
                top: widgets::SCROLL_PAD_TOP,
                right: 0.0,
                bottom: widgets::SCROLL_PAD_BOTTOM,
            },
            FlexView::new(Axis::Vertical, body.into_iter().map(inflexible).collect())
                .cross_axis(CrossAxisAlignment::Stretch),
        )))
        .top(false);
        let scroller = if responsive.wide_clamp {
            Either::Left(Align(
                Alignment::CENTER,
                SizedBox::<SettingsState>(Some(SETTINGS_CLAMP_WIDTH), None).child(scroller),
            ))
        } else {
            Either::Right(scroller)
        };
        widgets::page_tone(Either::Right(scroller), &colors)
    }
}

impl Actions {
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
                // that frame would pop `/settings` out from under the picker.
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

    /// `on_unlocked` is a no-op: Settings has no blocked dial to resume.
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

pub(super) fn row_tapped(state: &mut SettingsState, action: RowAction) {
    match action {
        // Unreachable, as inert rows carry no gesture; matched so a new `RowAction` never no-ops.
        RowAction::Static => {}
        RowAction::Picker(kind) => state.actions.open_picker(kind),
        RowAction::UpgradeSheet => state.actions.open_upgrade_sheet(),
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

pub(super) fn restore_purchases(state: &mut SettingsState) {
    state.actions.restore_purchases();
}

pub(super) fn open_disconnect_dialog(state: &mut SettingsState) {
    state.actions.open_disconnect_dialog();
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
