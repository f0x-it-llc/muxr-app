//! The `/settings` route's page: muxr's server-settings screen.
//!
//! [`settings_page`] is what `crate::routes` mounts; everything below it is
//! the canonical screen shape `docs/ARCHITECTURE.md`'s **Building a Screen**
//! section mandates — a per-screen [`Component`] ([`SettingsScreen`]) whose
//! `init` hosts a [`SettingsController`] through
//! [`use_controller`](clean_signals_frust::use_controller), so the controller
//! is created on route mount and disposed on unmount (`initState`/`dispose`
//! parity), and whose `build` is pure arrangement over [`super::pieces`]'
//! elements.
//!
//! # The screen, top to bottom
//!
//! ```text
//! ┌─────────────────────────────┐
//! │ ‹    server settings        │  crate::shell's persistent bar — NOT this
//! ├─────────────────────────────┤  screen's; it floats over this page
//! │ CONNECTION                  │
//! │ ▤ server url    100.71…   › │  green value, chevron
//! │ ▤ tls certificate insecure › │  amber value, chevron
//! │ z zellij version  0.44.3    │  static
//! │ h herdr version   …-v14     │  static
//! │ AUTH TOKENS                 │
//! │ zelli-mobile-…  [read-write]│
//! │ created 2026-07-02  (revoke)│
//! │ + create new token (dashed) │
//! │ TERMINAL APPEARANCE         │  three picker rows
//! │ SESSION POLL                │  one picker row + the bell switch
//! │ [ Disconnect ]              │
//! └─────────────────────────────┘
//! ```
//!
//! The bar above belongs to [`crate::shell`], not here: it is persistent chrome
//! whose title crossfades across route changes, so mounting a second copy per
//! screen would fight it. It *floats* over this page rather than displacing it,
//! so the page runs the full surface and reserves the band the bar covers with
//! [`topbar_clearance`] — under which the first group label sits. The body
//! keeps `safe_area(..).top(false)` because that clearance already contributes
//! the status-bar inset the bar consumes (`crate::ui::topbar`'s module docs).
//! `/settings`' own `‹` is that bar's, from `crate::routes::route_meta`.
//!
//! # Where each group's data comes from
//!
//! - **connection** — [`crate::connection::ConnectionController`]'s
//!   `server_info`, app-scoped because routing itself reads it. **No latency**:
//!   the sessions strip owns that number, next to the list a round trip
//!   actually gates; repeating it here would be a second, differently-stale
//!   copy of the same fact.
//! - **auth tokens**, **terminal appearance**, **session poll** — this
//!   feature's own [`SettingsController`], over the two repositories `di`
//!   published.
//!
//! # The component state boundary, and what it means for callbacks
//!
//! A `Component`'s subtree is bound to its own `State`, so this screen's own
//! gestures take `&mut SettingsState`. The **overlays** it pushes are a
//! different story: a dialog or picker sheet is a page on the *app's*
//! navigator, so its callbacks receive `&mut AppState` and can't see
//! [`SettingsState`] at all. That is why every action lives on [`Actions`] — a
//! small clonable bundle of handles a sheet row can capture and act through,
//! with the same method body serving both sides of the boundary.
//!
//! # Disconnecting
//!
//! The Disconnect confirmation clears
//! [`ConnectionController`](crate::connection::ConnectionController) and then
//! `go`es to `/`. Both halves are deliberate: the router's session guard only
//! runs at *navigation* time (`crate::routes`' own note), so it is the backstop
//! that keeps a later deep link out — not the mechanism that gets the user to
//! Connect. `go` rather than `push`, because a disconnected session is a new
//! stack, not a deeper one.

use std::sync::Arc;

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::{
    AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexView, Get, NavigatorController,
    Padding, any, component, inflexible, safe_area, scroll_view, spawn_local,
};

use crate::AppState;
use crate::connection::{
    ConnectionController, ServerInfo, TrustMode, expect_connection_controller,
};
use crate::failure::AppFailure;
use crate::features::settings::di::{expect_auth_tokens_repository, expect_preferences_repository};
use crate::features::settings::domain::entities::{
    AppearancePrefs, CursorStyle, PollPrefs, font_size_label, refresh_label,
};
use crate::routes::{Nav, app_nav};
use crate::ui::dialogs::{confirm_dialog, name_input_dialog};
use crate::ui::sheet::show_sheet;
use crate::ui::toasts::app_toasts;
use crate::ui::topbar::topbar_clearance;

use super::controller::SettingsController;
use super::pieces::{self, Palette, Picker, PickerKind, RowAction, ValueTone};

/// Where Disconnect lands. Spelled once, and the same location
/// `crate::routes`' guard bounces an unauthenticated push to.
const CONNECT_ROUTE: &str = "/";

/// What the two chevron rows in the `connection` group say when tapped.
/// They print facts about the connection *currently* up; changing either means
/// reconnecting, which is the Connect screen's job — so the row is honest
/// rather than dead.
const UNAVAILABLE_TOAST: &str = "Not yet available \u{2014} reconnect to change this";

/// The `connection` group's value while nothing is connected. `/settings` is
/// deliberately **not** guarded (`crate::routes`), so this is reachable — via a
/// deep link, or a disconnect racing a rebuild — and a blank row would be the
/// worse failure.
const DISCONNECTED_VALUE: &str = "not connected";

/// The `herdr version` row's value.
///
/// **A screen-local constant, and the one value on this screen not read from
/// live state.** [`ServerInfo`] carries a single `version` (the multiplexer's,
/// which the `zellij version` row prints); nothing on it names the herdr wire
/// protocol yet, and `src/connection.rs` is outside this task's write scope.
/// The real `muxrd` handshake reports it — see the completion summary's
/// findings.
const HERDR_WIRE_VERSION: &str = "herdr-wire-v14";

/// The Settings screen's route entry point: a `Component` under the outer
/// [`AppState`], so its `init` runs under a real reactive `Owner` that disposes
/// the hosted controller on unmount.
pub fn settings_page(controller: NavigatorController<AppState>) -> AnyView<AppState> {
    any(component(SettingsScreen { nav: controller }))
}

/// The Settings screen `Component`. Stateless configuration — everything
/// mutable lives in [`SettingsState`] and the controller it hosts.
pub struct SettingsScreen {
    /// The app's one navigator, handed in by the route builder.
    nav: NavigatorController<AppState>,
}

/// The Settings screen's retained state: the action bundle, and nothing else.
pub struct SettingsState {
    /// Everything this screen can *do*, and the handles it needs to do it —
    /// see [`Actions`].
    pub actions: Actions,
}

/// Every overlay this screen opens and every mutation it performs, plus the
/// four handles they need.
///
/// Cloneable on purpose: a row's gesture reaches these through
/// `&mut SettingsState`, while a *picker row* or a dialog action — a page on
/// the app's navigator, which only ever sees `&mut AppState` — captures a clone
/// and calls the very same method. One body, both sides of the component
/// boundary.
#[derive(Clone)]
pub struct Actions {
    /// The app's navigator — every page *builder* this screen pushes (the two
    /// dialogs, the four picker sheets) goes through it.
    pub nav: NavigatorController<AppState>,
    /// The app's one path-pushing handle — how Disconnect reaches `/`.
    pub app_nav: Nav,
    /// The hosted view model (created in `init`, disposed on unmount).
    pub controller: Arc<SettingsController>,
    /// The app-scoped connection handle: the `connection` group reads it, and
    /// Disconnect clears it.
    pub connection: Arc<ConnectionController>,
}

impl Component for SettingsScreen {
    type State = SettingsState;

    fn init(&self) -> SettingsState {
        // The screen-scoped view model, over the app-scoped repositories `di`
        // published. Auto-disposed on unmount via `on_cleanup`.
        let controller = use_controller::<SettingsController, AppFailure>(|| {
            SettingsController::new(
                expect_auth_tokens_repository(),
                expect_preferences_repository(),
            )
        });

        // Failure → toast, for this screen's lifetime. The handler runs on the
        // UI thread (every controller call is `spawn_local`ed onto the
        // UI-thread queue), which is what makes the thread-local `app_toasts`
        // handle the right one — see `ui::toasts`' module docs.
        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        // First load: the token list and both preference groups.
        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.refresh().await });

        SettingsState {
            actions: Actions {
                nav: self.nav.clone(),
                // Resolved here, with every other app-scoped dependency —
                // never from inside a press handler (the desktop shell
                // dispatches events with no ambient reactive owner).
                app_nav: app_nav(),
                controller,
                connection: expect_connection_controller(),
            },
        }
    }

    fn build(&self, state: &mut SettingsState) -> AnyView<SettingsState> {
        // Tracked reads: a load, a mutation, or a connection change is what
        // wakes this rebuild.
        let info = state.actions.connection.server_info.get();
        let tokens = state.actions.controller.tokens.get();
        let appearance = state.actions.controller.appearance.get();
        let poll = state.actions.controller.poll.get();

        let theme = pieces::active_theme();
        let colors = pieces::palette(&theme);

        let mut body = vec![pieces::group_label("connection", &colors)];
        body.extend(connection_rows(info.as_ref(), &colors));

        body.push(pieces::group_label("auth tokens", &colors));
        body.extend(
            tokens
                .iter()
                .map(|token| pieces::token_item(token, &colors)),
        );
        body.push(pieces::create_token_row(&colors));

        body.push(pieces::group_label("terminal appearance", &colors));
        body.extend(appearance_rows(&appearance, &colors));

        body.push(pieces::group_label("session poll", &colors));
        body.extend(poll_rows(&poll, &colors));

        body.push(pieces::disconnect_button());

        // The *shell's* bar consumes the top window inset on this screen's
        // behalf (`crate::ui::topbar`'s own module docs), so the body's safe
        // area disables its top edge to avoid double-padding it.
        let scroller = safe_area(scroll_view(Padding(
            EdgeInsets {
                left: 0.0,
                top: pieces::SCROLL_PAD_TOP,
                right: 0.0,
                bottom: pieces::SCROLL_PAD_BOTTOM,
            },
            FlexView::new(Axis::Vertical, body.into_iter().map(inflexible).collect())
                .cross_axis(CrossAxisAlignment::Stretch),
        )))
        .top(false);
        // The page tone stays **outside** the clearance so it fills the whole
        // surface, bar band included: the bar slides in over it, and a page
        // that stopped at the clearance would flash the shell's own clear
        // colour through the gap for the length of that entrance.
        pieces::page_tone(topbar_clearance(scroller), &colors)
    }
}

// ---------------------------------------------------------------------------
// The four groups' rows
// ---------------------------------------------------------------------------

/// The `connection` group: the endpoint, the TLS posture, and the two version
/// facts. Every value is read off `info`; `None` prints
/// [`DISCONNECTED_VALUE`] rather than nothing.
fn connection_rows(info: Option<&ServerInfo>, colors: &Palette) -> Vec<AnyView<SettingsState>> {
    let (endpoint, endpoint_tone) = match info {
        Some(info) => (info.endpoint.clone(), ValueTone::Ok),
        None => (DISCONNECTED_VALUE.to_string(), ValueTone::Plain),
    };
    let (tls_value, tls_tone) = match info {
        Some(info) => tls_posture(info.trust_mode),
        None => (DISCONNECTED_VALUE, ValueTone::Plain),
    };
    let zellij = info.map_or(DISCONNECTED_VALUE.to_string(), |info| {
        version_number(&info.version).to_string()
    });
    vec![
        pieces::settings_row(
            pieces::MARK_SERVER_URL,
            "server url",
            endpoint,
            endpoint_tone,
            RowAction::Unavailable,
            colors,
        ),
        pieces::settings_row(
            pieces::MARK_TLS,
            "tls certificate",
            tls_value,
            tls_tone,
            RowAction::Unavailable,
            colors,
        ),
        pieces::settings_row(
            pieces::MARK_ZELLIJ,
            "zellij version",
            zellij,
            ValueTone::Plain,
            RowAction::Static,
            colors,
        ),
        pieces::settings_row(
            pieces::MARK_HERDR,
            "herdr version",
            HERDR_WIRE_VERSION,
            ValueTone::Plain,
            RowAction::Static,
            colors,
        ),
    ]
}

/// What the `tls certificate` row prints for a posture, and in which hue.
///
/// Branches on the [`TrustMode`] **variant**, never on `ServerInfo::tls_label`
/// (`crate::connection`'s own rule: that field is display text). The words are
/// the redesign's rather than that label's — its "Trust on first use" describes
/// the *mechanism*, where screen 5 names the *consequence*, which is the whole
/// point of the note under that mock.
fn tls_posture(mode: TrustMode) -> (&'static str, ValueTone) {
    match mode {
        TrustMode::TrustOnFirstUse => ("insecure (plaintext)", ValueTone::Warn),
        TrustMode::System => ("public CA", ValueTone::Ok),
        TrustMode::Pinned => ("pinned certificate", ValueTone::Ok),
    }
}

/// The version number out of a `"<product> <version>"` string —
/// `"zellij 0.44.3"` → `"0.44.3"`, which is what the row prints (its *label*
/// already says `zellij version`). A string with no space is its own version.
fn version_number(version: &str) -> &str {
    version.rsplit_once(' ').map_or(version, |(_, tail)| tail)
}

/// The `terminal appearance` group: three picker rows.
fn appearance_rows(prefs: &AppearancePrefs, colors: &Palette) -> Vec<AnyView<SettingsState>> {
    vec![
        pieces::settings_row(
            pieces::MARK_FONT_FAMILY,
            "font family",
            prefs.font_family.clone(),
            ValueTone::Plain,
            RowAction::Picker(PickerKind::FontFamily),
            colors,
        ),
        pieces::settings_row(
            pieces::MARK_FONT_SIZE,
            "font size",
            prefs.font_size_label(),
            ValueTone::Plain,
            RowAction::Picker(PickerKind::FontSize),
            colors,
        ),
        pieces::settings_row(
            pieces::MARK_CURSOR,
            "cursor style",
            prefs.cursor_style.label(),
            ValueTone::Plain,
            RowAction::Picker(PickerKind::CursorStyle),
            colors,
        ),
    ]
}

/// The `session poll` group: one picker row and the bell switch.
fn poll_rows(prefs: &PollPrefs, colors: &Palette) -> Vec<AnyView<SettingsState>> {
    vec![
        pieces::settings_row(
            pieces::MARK_REFRESH,
            "layout refresh",
            prefs.layout_refresh_label(),
            ValueTone::Plain,
            RowAction::Picker(PickerKind::LayoutRefresh),
            colors,
        ),
        pieces::settings_row(
            pieces::MARK_BELL,
            "bell notifications",
            pieces::BELL_VALUE,
            ValueTone::Plain,
            RowAction::Bell(prefs.bell_notifications),
            colors,
        ),
    ]
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

impl Actions {
    /// Say that a `connection` row's destination isn't built yet. UI-only, on
    /// purpose — see [`UNAVAILABLE_TOAST`].
    pub fn notify_unavailable(&self) {
        app_toasts().show(UNAVAILABLE_TOAST);
    }

    /// Open `kind`'s picker sheet.
    ///
    /// The body closure is re-invoked on every navigator rebuild (the
    /// page-builder contract), so it re-resolves the picker each time and the
    /// check mark tracks the stored value rather than a snapshot taken when the
    /// sheet opened.
    ///
    /// Module-private, with [`picker`](Self::picker)/[`pick`](Self::pick): the
    /// whole trio speaks [`PickerKind`], which is `presentation`-internal
    /// vocabulary, and [`row_tapped`] is its one caller.
    fn open_picker(&self, kind: PickerKind) {
        let actions = self.clone();
        show_sheet(&self.nav, move || {
            let theme = pieces::active_theme();
            let colors = pieces::palette(&theme);
            let picker = actions.picker(kind);
            let picking = actions.clone();
            pieces::picker_sheet(&picker, &colors, move |_app: &mut AppState, index| {
                // Close first, then persist: the sheet's own exit staging and a
                // repository round trip should not overlap (the same ordering
                // the sessions management sheet's rows use).
                picking.nav.pop();
                picking.pick(kind, index);
            })
        });
    }

    /// Resolve `kind` against the controller's current values — the option list
    /// the domain declares, and which of them is stored.
    fn picker(&self, kind: PickerKind) -> Picker {
        let appearance = self.controller.appearance_untracked();
        let poll = self.controller.poll_untracked();
        let (options, selected): (Vec<String>, Option<usize>) = match kind {
            PickerKind::FontFamily => (
                AppearancePrefs::FONT_FAMILIES
                    .iter()
                    .map(|family| (*family).to_string())
                    .collect(),
                AppearancePrefs::FONT_FAMILIES
                    .iter()
                    .position(|family| *family == appearance.font_family),
            ),
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

    /// Persist option `index` of `kind`'s list. An index the domain's own list
    /// no longer holds is dropped rather than clamped — it can only come from a
    /// sheet built against a different list, and guessing would write a value
    /// the user never chose.
    fn pick(&self, kind: PickerKind, index: usize) {
        let controller = Arc::clone(&self.controller);
        match kind {
            PickerKind::FontFamily => {
                let Some(family) = AppearancePrefs::FONT_FAMILIES.get(index) else {
                    return;
                };
                let next = AppearancePrefs {
                    font_family: (*family).to_string(),
                    ..controller.appearance_untracked()
                };
                spawn_local(async move { controller.set_appearance(next).await });
            }
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

    /// Open the mint-a-token dialog (the dashed row).
    ///
    /// The confirmation toast is raised **only if the mint landed**: a failure
    /// already surfaces through the screen's own failure→toast bridge, and a
    /// "Created …" beside it would contradict it.
    pub fn open_create_token_dialog(&self) {
        let controller = Arc::clone(&self.controller);
        name_input_dialog(
            &self.nav,
            "New token",
            "",
            move |_app: &mut AppState, name: String| {
                let handle = Arc::clone(&controller);
                let confirmation = format!("Created {name}");
                spawn_local(async move {
                    if handle.create_token(name).await {
                        app_toasts().show(confirmation);
                    }
                });
            },
        );
    }

    /// Open the revoke confirmation for the token called `name`.
    pub fn open_revoke_dialog(&self, name: &str) {
        let controller = Arc::clone(&self.controller);
        let name = name.to_string();
        confirm_dialog(
            &self.nav,
            "Revoke token?",
            "Any client still using this token is disconnected immediately.",
            "Revoke",
            move |_app: &mut AppState| {
                let handle = Arc::clone(&controller);
                let name = name.clone();
                let confirmation = format!("Revoked {name}");
                spawn_local(async move {
                    // Confirm only on success — see `open_create_token_dialog`.
                    if handle.revoke_token(name).await {
                        app_toasts().show(confirmation);
                    }
                });
            },
        );
    }

    /// Open the Disconnect confirmation. See the [module docs](self) for why
    /// confirming both clears the connection *and* navigates.
    pub fn open_disconnect_dialog(&self) {
        let connection = Arc::clone(&self.connection);
        let app_nav = self.app_nav.clone();
        confirm_dialog(
            &self.nav,
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

// ---------------------------------------------------------------------------
// The screen-state shims the pieces' own callbacks call
// ---------------------------------------------------------------------------

/// A `.set-row` was tapped — dispatch on what that row is for.
pub(super) fn row_tapped(state: &mut SettingsState, action: RowAction) {
    match action {
        // Inert rows carry no gesture at all, so this arm is unreachable;
        // matching it exhaustively is what keeps a new `RowAction` from
        // silently doing nothing.
        RowAction::Static => {}
        RowAction::Unavailable => state.actions.notify_unavailable(),
        RowAction::Picker(kind) => state.actions.open_picker(kind),
        RowAction::Bell(_) => state.actions.toggle_bell(),
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

/// The `Disconnect` button.
pub(super) fn open_disconnect_dialog(state: &mut SettingsState) {
    state.actions.open_disconnect_dialog();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_validation_skipping_posture_warns() {
        // The redesign's own correction, as a tripwire: `insecure (plaintext)`
        // must never read as a success state, and a validated posture must
        // never read as a warning.
        assert!(matches!(
            tls_posture(TrustMode::TrustOnFirstUse),
            ("insecure (plaintext)", ValueTone::Warn)
        ));
        assert!(matches!(
            tls_posture(TrustMode::System),
            ("public CA", ValueTone::Ok)
        ));
        assert!(matches!(
            tls_posture(TrustMode::Pinned),
            ("pinned certificate", ValueTone::Ok)
        ));
    }

    #[test]
    fn the_version_row_prints_the_number_without_the_product_name() {
        // The row's *label* already says `zellij version`; `ServerInfo` carries
        // the joined `"zellij 0.44.3"` because the sessions strip prints it
        // that way.
        assert_eq!(version_number("zellij 0.44.3"), "0.44.3");
        assert_eq!(version_number("herdr 1.2"), "1.2");
        // No product name at all: the whole string is the version.
        assert_eq!(version_number("0.44.3"), "0.44.3");
    }
}
