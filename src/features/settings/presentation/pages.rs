//! The `/settings` route: muxr's server-settings screen — five groups
//! (`connection`, `auth tokens`, `saved servers`, `terminal appearance`,
//! `session poll`) plus Disconnect, following `docs/ARCHITECTURE.md`'s
//! canonical screen shape (a per-screen [`Component`] hosting
//! [`SettingsController`] via `use_controller`, `build` a pure arrangement
//! over [`widgets`]).
//!
//! **Component boundary.** This screen's own gestures take `&mut
//! SettingsState`; a dialog/picker sheet it pushes is a page on the *app's*
//! root overlay host instead, so its callback sees `&mut AppState`, never
//! `SettingsState`. [`Actions`] is the clonable bundle that lets one method
//! body serve both sides of that boundary — every mutation and every opened
//! overlay lives on it.
//!
//! **Minting a token**: [`Actions::open_create_token_dialog`] is gated by
//! [`Actions::tokens_blocked`] (a read-only connected session cannot mint or
//! revoke — `ConnectionController::is_read_only`'s primary defence); on a
//! landed mint it opens [`open_secret_reveal`], the one-time secret sheet —
//! shown exactly once, then gone for good, per
//! [`SettingsController::pending_token_secret`]'s own contract.
//!
//! **Revoking**: a confirmed revoke starts the row's scramble-then-collapse
//! ([`super::scramble`]) and fires the RPC on the same tap
//! ([`Actions::revoke`]); [`widgets::token_rows`] renders a group whose
//! animating rows may no longer be in `tokens` at all.
//!
//! **Disconnecting** clears [`ConnectionController`] then `go`es to `/` —
//! both halves deliberate, since the router's session guard only fires at
//! *navigation* time, never reactively.

use std::sync::Arc;

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::{
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexView, Get,
    GetUntracked, NavigatorController, Padding, SizedBox, WindowMetrics, any, component,
    inflexible, safe_area, scroll_view, spawn_local, use_context,
};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::{expect_credential_service, expect_saved_servers};
use crate::features::settings::di::{expect_auth_tokens_repository, expect_preferences_repository};
use crate::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle, PollPrefs, font_size_label, refresh_label,
};
use crate::responsive::WIDE_BREAKPOINT_PX;
use crate::routes::{Nav, app_nav};
use crate::ui::dialogs::confirm_dialog;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::sheet::show_sheet;
use crate::ui::toasts::app_toasts;
use crate::ui::topbar::topbar_clearance;

use super::controller::SettingsController;
use super::scramble::{RevokeAnimations, RevokeScramble, ScramblePose};
use super::widgets::{self, FontPicker, FontRow, Picker, PickerKind, RowAction};

/// Where Disconnect lands — the same location `crate::routes`' guard bounces
/// an unauthenticated push to.
const CONNECT_ROUTE: &str = "/";

/// What tapping a mutating token control says while the connected session is
/// itself read-only.
const READ_ONLY_TOKEN_TOAST: &str =
    "Read-only session \u{2014} reconnect with a read-write token to change this";

/// The font sheet's scroll-region reserve: the window's own height minus this
/// estimate of everything outside the scrollable band. Generous and
/// community-approximate rather than imported from `ui/sheet.rs`'s private
/// geometry (`features::terminal`'s own "panes & tabs" sheet makes the same
/// trade) — the cost of overestimating is a slightly shorter scroll band, not
/// a broken one.
const SHEET_SCROLL_RESERVE_PX: f64 = 220.0;
const SHEET_SCROLL_FLOOR_PX: f64 = 240.0;

/// The groups column's max width past [`WIDE_BREAKPOINT_PX`] — a
/// tablet/landscape/desktop window centers the whole scrolling group list
/// rather than stretching it edge to edge. A plain width clamp, not a
/// structural layout switch (no design mock exists for one here).
const SETTINGS_CLAMP_WIDTH: f64 = 640.0;

/// The Settings screen's route entry point.
pub fn settings_page(overlays: NavigatorController<AppState>) -> AnyView<AppState> {
    any(component(SettingsScreen { overlays }))
}

/// The Settings screen `Component`. Stateless configuration — everything
/// mutable lives in [`SettingsState`] and the controller it hosts.
pub struct SettingsScreen {
    overlays: NavigatorController<AppState>,
}

/// The Settings screen's retained state: the action bundle, and nothing else.
pub struct SettingsState {
    pub actions: Actions,
}

/// Every overlay this screen opens and every mutation it performs, plus the
/// handles they need. Cloneable: a row's gesture reaches these through `&mut
/// SettingsState`, while a picker row or dialog action — a page on the app's
/// navigator, seeing only `&mut AppState` — captures a clone and calls the
/// very same method.
#[derive(Clone)]
pub struct Actions {
    /// The root overlay host's navigator — every dialog/sheet this screen
    /// pushes goes through it, landing above the shell's floating bar.
    pub overlays: NavigatorController<AppState>,
    /// The app's one path-pushing handle — how Disconnect reaches `/`.
    pub app_nav: Nav,
    /// The hosted view model (created in `init`, disposed on unmount).
    pub controller: Arc<SettingsController>,
    /// The app-scoped connection handle: the `connection` group reads it,
    /// and Disconnect clears it.
    pub connection: Arc<ConnectionController>,
    /// The revoke animations in flight — screen-scoped presentation state,
    /// not the controller's (see [`RevokeAnimations`]).
    pub(super) scrambles: RevokeAnimations,
}

impl Component for SettingsScreen {
    type State = SettingsState;

    fn init(&self) -> SettingsState {
        let controller = use_controller::<SettingsController, AppFailure>(|| {
            SettingsController::new(
                expect_auth_tokens_repository(),
                expect_preferences_repository(),
                // `connect`'s two stores — the cross-feature reach the
                // terminal screen already makes (see `SettingsController`'s
                // module docs for why the credential rows live here).
                expect_saved_servers(),
                expect_credential_service(),
            )
        });

        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.refresh().await });

        SettingsState {
            actions: Actions {
                overlays: self.overlays.clone(),
                app_nav: app_nav(),
                controller,
                connection: expect_connection_controller(),
                scrambles: RevokeAnimations::new(),
            },
        }
    }

    fn build(&self, state: &mut SettingsState) -> AnyView<SettingsState> {
        let info = state.actions.connection.server_info.get();
        let read_only = state.actions.connection.is_read_only.get();
        let tokens = state.actions.controller.tokens.get();
        let saved = state.actions.controller.saved.get();
        let appearance = state.actions.controller.appearance.get();
        let poll = state.actions.controller.poll.get();

        let theme = active_theme();
        let colors = Palette::from_theme(&theme);

        // Responsive width-clamp pass: read metrics once, branch on one page
        // constant, never touch layout again. `None` (no shell has
        // published metrics yet, or a bare test harness) takes the same
        // branch as a genuinely narrow window.
        let metrics = use_context::<WindowMetrics>();
        let is_wide = metrics
            .map(|m| m.size.width >= WIDE_BREAKPOINT_PX)
            .unwrap_or(false);

        let mut body = vec![widgets::group_label("connection", &colors)];
        body.extend(widgets::connection_rows(info.as_ref(), &colors));

        // The revoke timelines in flight, and what each renders this frame.
        // Tracked: starting one — and its RPC answering — is what wakes this
        // rebuild; every frame in between comes from the ticker the row
        // mounts.
        let reduce_motion = theme.motion.reduce_motion;
        let scrambles = state.actions.scrambles;
        let timelines = scrambles.live();
        let showing: Vec<(RevokeScramble, ScramblePose)> = timelines
            .iter()
            .map(|entry| (entry.clone(), entry.pose(reduce_motion)))
            .filter(|(entry, pose)| pose.running || !entry.answered)
            .collect();
        if showing.len() != timelines.len() {
            // Something finished: drop it off the build that noticed rather
            // than out of the middle of it (`RevokeAnimations::sweep`).
            spawn_local(async move { scrambles.sweep(reduce_motion) });
        }

        body.push(widgets::group_label("auth tokens", &colors));
        body.extend(widgets::token_rows(&tokens, &showing, read_only, &colors));
        body.push(widgets::create_token_row(&colors, !read_only));

        body.push(widgets::group_label("saved servers", &colors));
        if saved.is_empty() {
            body.push(widgets::no_saved_servers_row(&colors));
        } else {
            body.extend(
                saved
                    .iter()
                    .map(|server| widgets::saved_server_item(server, &colors)),
            );
        }

        body.push(widgets::group_label("terminal appearance", &colors));
        body.extend(widgets::appearance_rows(&appearance, &colors));

        body.push(widgets::group_label("session poll", &colors));
        body.extend(widgets::poll_rows(&poll, &colors));

        body.push(widgets::disconnect_button());

        // The *shell's* bar consumes the top window inset on this screen's
        // behalf, so the body's safe area disables its top edge to avoid
        // double-padding it.
        let scroller: AnyView<SettingsState> = any(safe_area(scroll_view(Padding(
            EdgeInsets {
                left: 0.0,
                top: widgets::SCROLL_PAD_TOP,
                right: 0.0,
                bottom: widgets::SCROLL_PAD_BOTTOM,
            },
            FlexView::new(Axis::Vertical, body.into_iter().map(inflexible).collect())
                .cross_axis(CrossAxisAlignment::Stretch),
        )))
        .top(false));
        let scroller = if is_wide {
            any(Align(
                Alignment::CENTER,
                SizedBox::<SettingsState>(Some(SETTINGS_CLAMP_WIDTH), None).child(scroller),
            ))
        } else {
            scroller
        };
        // The page tone stays **outside** the clearance so it fills the
        // whole surface, bar band included: the bar slides in over it, and a
        // page that stopped at the clearance would flash the shell's own
        // clear colour through the gap for the length of that entrance.
        widgets::page_tone(topbar_clearance(scroller), &colors)
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

impl Actions {
    /// Open `kind`'s picker sheet. The body closure re-runs on every
    /// navigator rebuild, so it re-resolves the picker each time and the
    /// check mark tracks the stored value rather than a snapshot taken when
    /// the sheet opened. The font family has no list here — it has its own
    /// sheet ([`open_font_picker`](Self::open_font_picker)), since its
    /// choices are the faces this device has installed plus a downloadable
    /// catalog, not a domain-declared list.
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
                // Close first, then persist — a sheet pop only *lands* at
                // the next rebuild, so a second row tap inside that one
                // frame would otherwise pop `/settings` out from under the
                // picker (`crate::ui::sheet`'s "One sheet, one dismissal").
                sheet.close();
                picking.pick(kind, index);
            })
        });
    }

    /// Resolve `kind` against the controller's current values — the option
    /// list the domain declares, and which of them is stored.
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
        Picker {
            title: kind.title(),
            options,
            selected,
        }
    }

    /// Persist option `index` of `kind`'s list. An index the domain's own
    /// list no longer holds is dropped rather than clamped — it can only
    /// come from a sheet built against a different list, and guessing would
    /// write a value the user never chose.
    fn pick(&self, kind: PickerKind, index: usize) {
        let controller = Arc::clone(&self.controller);
        match kind {
            // Unreachable: the font family persists through
            // `pick_font_family` rather than by option index.
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

    /// Open the font-family sheet: the installed faces, then the
    /// downloadable catalog. The catalog is fetched **here**, when the sheet
    /// opens — never at app startup, which has no use for one (`load` is
    /// single-flight and a no-op once a catalog has landed).
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
                // A download leaves the sheet up: it makes the row
                // selectable, it does not select it.
                move |_app: &mut AppState, family: String| downloading.download_font(&family),
            )
        });
    }

    /// Assemble the font sheet's two sections from the installed faces, the
    /// catalog's signals and the stored preference. Every read here is
    /// **tracked** — this runs inside the sheet's body builder, so a catalog
    /// landing or a download flipping state is what re-renders the rows.
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
                // Where the face came from, not what it is called: a
                // downloaded family whose display name already matches its
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

        // `None` in every headless suite, which leaves the region unbounded.
        let max_height = use_context::<WindowMetrics>().map(|metrics| {
            (metrics.size.height - SHEET_SCROLL_RESERVE_PX).max(SHEET_SCROLL_FLOOR_PX)
        });

        FontPicker {
            installed,
            available,
            selected,
            status,
            stale,
            max_height,
        }
    }

    /// Persist `family` as the terminal's face — the installed-row tap.
    fn pick_font_family(&self, family: String) {
        let controller = Arc::clone(&self.controller);
        let next = AppearancePrefs {
            font_family: family,
            ..controller.appearance_untracked()
        };
        spawn_local(async move { controller.set_appearance(next).await });
    }

    /// Download the catalog entry whose family is `family`. Single-flight
    /// and state-tracking live in the catalog itself; this owns only the
    /// presentation half: a failed install raises the app's failure toast.
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

    /// Flip the bell preference — a tap anywhere on that row.
    pub fn toggle_bell(&self) {
        let controller = Arc::clone(&self.controller);
        let current = controller.poll_untracked();
        let next = PollPrefs {
            bell_notifications: !current.bell_notifications,
            ..current
        };
        spawn_local(async move { controller.set_poll_prefs(next).await });
    }

    /// Whether a mutating token control is blocked because the connected
    /// session is itself read-only. Raises [`READ_ONLY_TOKEN_TOAST`] when it
    /// is — the one guard both token mutations open with.
    fn tokens_blocked(&self) -> bool {
        if self.connection.is_read_only.get_untracked() {
            app_toasts().show(READ_ONLY_TOKEN_TOAST);
            true
        } else {
            false
        }
    }

    /// Open the mint-a-token dialog (the dashed row): a name and its
    /// read-only toggle, then — on a landed mint — the one-time secret
    /// reveal. The confirmation toast is raised **only if the mint landed**:
    /// a failure already surfaces through the screen's own failure→toast
    /// bridge, and a "Created …" beside it would contradict it.
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

    /// Open the revoke confirmation for the token called `name`.
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

    /// Revoke the token called `name`: the row's scramble-then-collapse and
    /// the RPC start on the same tap — concurrent on purpose, since the
    /// animation is not a pre-roll and a revoke that lands in 20ms must not
    /// make the row vanish before the eye has seen it go. The row's snapshot
    /// is taken here, ahead of the refresh that call ends with, because that
    /// refresh is exactly what takes the token out of `tokens` mid-animation.
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
            // Either way the row may now finish: a refusal leaves the token
            // in `tokens`, so dropping the timeline restores the row it was
            // collapsing, and the failure toast the controller emitted
            // stands.
            scrambles.answered(&name);
        });
    }

    /// Open the delete-credential confirmation for the saved server at
    /// `endpoint`. **No read-only guard**, unlike the two token controls:
    /// deleting a device-local credential is not a server mutation, and the
    /// token revoke that rides along is best-effort by design (see
    /// `SettingsController::delete_credential`).
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

    /// Open the forget-server confirmation for the saved server at
    /// `endpoint`. The message names the stored credential only when there
    /// is one to lose.
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

    /// Open the Disconnect confirmation. Confirming both clears the
    /// connection *and* navigates: the router's session guard only runs at
    /// navigation time, so it is the backstop that keeps a later deep link
    /// out — not the mechanism that gets the user to Connect. `go` rather
    /// than `push`, since a disconnected session is a new stack, not a
    /// deeper one.
    pub fn open_disconnect_dialog(&self) {
        let connection = Arc::clone(&self.connection);
        let app_nav = self.app_nav.clone();
        confirm_dialog(
            &self.overlays,
            "Disconnect",
            "You will need the server's token to attach again.",
            "Disconnect",
            move |_app: &mut AppState| {
                connection.disconnect();
                app_nav.go(CONNECT_ROUTE);
            },
        );
    }
}

/// Open the one-time secret reveal for `controller`'s currently pending
/// secret — [`Actions::open_create_token_dialog`]'s own follow-up once a
/// mint lands.
fn open_secret_reveal(
    overlays: &NavigatorController<AppState>,
    controller: &Arc<SettingsController>,
) {
    let controller = Arc::clone(controller);
    show_sheet(overlays, move |sheet| {
        let theme = active_theme();
        let colors = Palette::from_theme(&theme);
        // Tracked: re-resolved on every navigator rebuild, like the picker
        // sheet's own body.
        let minted = controller.pending_token_secret.get();
        let sheet = sheet.clone();
        let controller = Arc::clone(&controller);
        widgets::secret_reveal_sheet(minted.as_ref(), &colors, move |_app: &mut AppState| {
            sheet.close();
            controller.clear_pending_secret();
        })
    });
}

// ---------------------------------------------------------------------------
// The screen-state shims `widgets`' own callbacks call back into
// ---------------------------------------------------------------------------

/// A `.set-row` was tapped — dispatch on what that row is for.
pub(super) fn row_tapped(state: &mut SettingsState, action: RowAction) {
    match action {
        // Inert rows carry no gesture at all, so this arm is unreachable;
        // matching it exhaustively is what keeps a new `RowAction` from
        // silently doing nothing.
        RowAction::Static => {}
        RowAction::Picker(kind) => state.actions.open_picker(kind),
    }
}

/// The dashed `+ create new token` row.
pub(super) fn open_create_token_dialog(state: &mut SettingsState) {
    state.actions.open_create_token_dialog();
}

/// A token's `revoke` button.
pub(super) fn open_revoke_dialog(state: &mut SettingsState, name: &str) {
    state.actions.open_revoke_dialog(name);
}

/// A saved server's `delete credential` button.
pub(super) fn open_delete_credential_dialog(state: &mut SettingsState, endpoint: &str) {
    state.actions.open_delete_credential_dialog(endpoint);
}

/// A saved server's `forget` button.
pub(super) fn open_forget_server_dialog(state: &mut SettingsState, endpoint: &str) {
    state.actions.open_forget_server_dialog(endpoint);
}

/// The `Disconnect` button.
pub(super) fn open_disconnect_dialog(state: &mut SettingsState) {
    state.actions.open_disconnect_dialog();
}
