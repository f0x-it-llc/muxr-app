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
//! │ z zellij version  0.44.3    │  static, one row per reported backend
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
//!   `server_info`, app-scoped because routing itself reads it. Its version
//!   row(s) are one per [`ServerInfo::backend_versions`] entry (falling back
//!   to the legacy single version string for a server that reported none —
//!   see [`connection_rows`]). **No latency**: the sessions strip owns that
//!   number, next to the list a round trip actually gates; repeating it here
//!   would be a second, differently-stale copy of the same fact.
//! - **auth tokens**, **saved servers**, **terminal appearance**, **session
//!   poll** — this feature's own [`SettingsController`], over the two
//!   repositories `di` published plus the two `connect`'s own `di` publishes
//!   (the saved-server list and its credential store — see that controller's
//!   module docs for why the credential rows live on this screen).
//!
//! # Minting a token
//!
//! [`Actions::open_create_token_dialog`] opens
//! [`pieces::open_create_token_dialog`] (name + read-only toggle); on a
//! landed mint it opens [`open_secret_reveal`], the one-time secret sheet
//! reading [`SettingsController::pending_token_secret`] — see that field's own
//! doc for the whole flow. Both mutating token actions
//! ([`Actions::open_create_token_dialog`]/[`Actions::open_revoke_dialog`])
//! open with the same [`Actions::tokens_blocked`] guard: a read-only
//! connected session (`ConnectionController::is_read_only`) cannot mint or
//! revoke, and the guard raises [`READ_ONLY_TOKEN_TOAST`] rather than
//! silently doing nothing.
//!
//! # Revoking a token
//!
//! A confirmed revoke starts the row's scramble-then-collapse animation and
//! fires the RPC at the same moment ([`Actions::revoke`]); [`token_rows`] is
//! what renders a group whose animating rows may no longer be in `tokens` at
//! all. [`super::scramble`] owns the timeline, its clock and its widgets.
//!
//! # The component state boundary, and what it means for callbacks
//!
//! A `Component`'s subtree is bound to its own `State`, so this screen's own
//! gestures take `&mut SettingsState`. The **overlays** it pushes are a
//! different story: a dialog or picker sheet is a page on the *app's* root
//! overlay host (`crate::AppState::overlays`, above every piece of chrome),
//! so its callbacks receive `&mut AppState` and can't see
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
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexView, Get,
    GetUntracked, NavigatorController, Padding, SizedBox, WindowMetrics, any, component,
    inflexible, safe_area, scroll_view, spawn_local, use_context,
};

use crate::AppState;
use crate::connection::{
    ConnectionController, ServerBackend, ServerInfo, TrustMode, expect_connection_controller,
};
use crate::failure::AppFailure;
use crate::features::connect::di::{expect_credential_service, expect_saved_servers};
use crate::features::settings::di::{expect_auth_tokens_repository, expect_preferences_repository};
use crate::features::settings::domain::entities::{
    AppearancePrefs, AuthToken, CursorStyle, PollPrefs, font_size_label, refresh_label,
};
use crate::responsive::WIDE_BREAKPOINT_PX;
use crate::routes::{Nav, app_nav};
use crate::ui::dialogs::confirm_dialog;
use crate::ui::sheet::show_sheet;
use crate::ui::toasts::app_toasts;
use crate::ui::topbar::topbar_clearance;

use super::controller::SettingsController;
use super::pieces::{self, Palette, Picker, PickerKind, RowAction, ValueTone};
use super::scramble::{RevokeAnimations, RevokeScramble, ScramblePose};

/// Where Disconnect lands. Spelled once, and the same location
/// `crate::routes`' guard bounces an unauthenticated push to.
const CONNECT_ROUTE: &str = "/";

/// The `connection` group's value while nothing is connected. `/settings` is
/// deliberately **not** guarded (`crate::routes`), so this is reachable — via a
/// deep link, or a disconnect racing a rebuild — and a blank row would be the
/// worse failure.
const DISCONNECTED_VALUE: &str = "not connected";

/// What tapping a mutating token control says while the connected session is
/// itself read-only — `ConnectionController::is_read_only`'s own "primary
/// defence" note (`crate::connection`'s module docs): a read-only credential
/// cannot mint or revoke, so both controls stay reachable but honest — raising
/// this toast — rather than silently doing nothing.
const READ_ONLY_TOKEN_TOAST: &str =
    "Read-only session \u{2014} reconnect with a read-write token to change this";

/// The groups column's max width past [`WIDE_BREAKPOINT_PX`] (FINDINGS #21's
/// conservative width-clamp pass) — a tablet/landscape/desktop window centers
/// the whole scrolling group list rather than stretching it edge to edge.
/// There is only one scrolling column on this screen (unlike Sessions' strip
/// and list split), so the whole body clamps together. No design mock exists
/// for a structural two-column Settings, so this is a plain clamp, not a
/// layout switch.
const SETTINGS_CLAMP_WIDTH: f64 = 640.0;

/// The Settings screen's route entry point: a `Component` under the outer
/// [`AppState`], so its `init` runs under a real reactive `Owner` that disposes
/// the hosted controller on unmount.
pub fn settings_page(overlays: NavigatorController<AppState>) -> AnyView<AppState> {
    any(component(SettingsScreen { overlays }))
}

/// The Settings screen `Component`. Stateless configuration — everything
/// mutable lives in [`SettingsState`] and the controller it hosts.
pub struct SettingsScreen {
    /// The root overlay host's navigator, handed in by the route builder.
    overlays: NavigatorController<AppState>,
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
    /// The root overlay host's navigator — every page *builder* this screen
    /// pushes (the two dialogs, the four picker sheets) goes through it, so
    /// each lands above the shell's floating bar rather than under it
    /// (`crate::AppState::overlays`).
    pub overlays: NavigatorController<AppState>,
    /// The app's one path-pushing handle — how Disconnect reaches `/`.
    pub app_nav: Nav,
    /// The hosted view model (created in `init`, disposed on unmount).
    pub controller: Arc<SettingsController>,
    /// The app-scoped connection handle: the `connection` group reads it, and
    /// Disconnect clears it.
    pub connection: Arc<ConnectionController>,
    /// The revoke animations in flight — screen-scoped presentation state, not
    /// the controller's, since nothing in it is repository data (see
    /// [`RevokeAnimations`]). `Copy`, so a dialog action on the *app's*
    /// navigator and the future it spawns both carry the same handle.
    ///
    /// `pub(super)`, unlike this bundle's four handles: the timeline vocabulary
    /// is `presentation`-internal, and nothing outside this module arranges a
    /// row with it.
    pub(super) scrambles: RevokeAnimations,
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
                // `connect`'s two stores, resolved through its own `di` — the
                // cross-feature reach the terminal screen already makes, and
                // which `SettingsController`'s module docs justify for these.
                expect_saved_servers(),
                expect_credential_service(),
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
                overlays: self.overlays.clone(),
                // Resolved here, with every other app-scoped dependency —
                // never from inside a press handler (the desktop shell
                // dispatches events with no ambient reactive owner).
                app_nav: app_nav(),
                controller,
                connection: expect_connection_controller(),
                scrambles: RevokeAnimations::new(),
            },
        }
    }

    fn build(&self, state: &mut SettingsState) -> AnyView<SettingsState> {
        // Tracked reads: a load, a mutation, or a connection change is what
        // wakes this rebuild.
        let info = state.actions.connection.server_info.get();
        let read_only = state.actions.connection.is_read_only.get();
        let tokens = state.actions.controller.tokens.get();
        let saved = state.actions.controller.saved.get();
        let appearance = state.actions.controller.appearance.get();
        let poll = state.actions.controller.poll.get();

        let theme = pieces::active_theme();
        let colors = pieces::palette(&theme);

        // Responsive width-clamp pass (FINDINGS #21): read metrics once,
        // branch on one page constant, never touch layout again — the
        // reference idiom `examples/glyph-catalog/src/pages/responsive.rs`
        // documents. `None` (no shell has published yet, or a bare test
        // harness) takes the same branch as a genuinely narrow window.
        let metrics = use_context::<WindowMetrics>();
        let is_wide = metrics
            .map(|m| m.size.width >= WIDE_BREAKPOINT_PX)
            .unwrap_or(false);

        let mut body = vec![pieces::group_label("connection", &colors)];
        body.extend(connection_rows(info.as_ref(), &colors));

        // The revoke timelines in flight, and what each renders this frame.
        // Tracked: starting one — and its RPC answering — is what wakes this
        // rebuild; every frame in between comes from the ticker the row mounts.
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

        body.push(pieces::group_label("auth tokens", &colors));
        body.extend(token_rows(&tokens, &showing, read_only, &colors));
        body.push(pieces::create_token_row(&colors, !read_only));

        body.push(pieces::group_label("saved servers", &colors));
        if saved.is_empty() {
            body.push(pieces::no_saved_servers_row(&colors));
        } else {
            body.extend(
                saved
                    .iter()
                    .map(|server| pieces::saved_server_item(server, &colors)),
            );
        }

        body.push(pieces::group_label("terminal appearance", &colors));
        body.extend(appearance_rows(&appearance, &colors));

        body.push(pieces::group_label("session poll", &colors));
        body.extend(poll_rows(&poll, &colors));

        body.push(pieces::disconnect_button());

        // The *shell's* bar consumes the top window inset on this screen's
        // behalf (`crate::ui::topbar`'s own module docs), so the body's safe
        // area disables its top edge to avoid double-padding it.
        let scroller: AnyView<SettingsState> = any(safe_area(scroll_view(Padding(
            EdgeInsets {
                left: 0.0,
                top: pieces::SCROLL_PAD_TOP,
                right: 0.0,
                bottom: pieces::SCROLL_PAD_BOTTOM,
            },
            FlexView::new(Axis::Vertical, body.into_iter().map(inflexible).collect())
                .cross_axis(CrossAxisAlignment::Stretch),
        )))
        .top(false));
        // Clamp/center the one scrolling column — see `SETTINGS_CLAMP_WIDTH`'s
        // doc.
        let scroller = if is_wide {
            any(Align(
                Alignment::CENTER,
                SizedBox::<SettingsState>(Some(SETTINGS_CLAMP_WIDTH), None).child(scroller),
            ))
        } else {
            scroller
        };
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

/// The `auth tokens` group's rows.
///
/// One `token_item` per token with no revoke in flight; this frame's scramble
/// view for every token that has one; and — for a revoke whose own refresh has
/// already taken its token out of `tokens` — the same scramble view re-inserted
/// at the position that row is vanishing from (clamped, since the list it was
/// snapshotted against may have shrunk), so nothing under it jumps while the
/// collapse plays out. Orphans go back in ascending order, so two at once keep
/// their original order between them.
fn token_rows(
    tokens: &[AuthToken],
    showing: &[(RevokeScramble, ScramblePose)],
    read_only: bool,
    colors: &Palette,
) -> Vec<AnyView<SettingsState>> {
    let animating = |token: &AuthToken| {
        showing
            .iter()
            .find(|(entry, _)| entry.token.name == token.name)
    };
    let mut rows: Vec<AnyView<SettingsState>> = tokens
        .iter()
        .map(|token| match animating(token) {
            Some((entry, pose)) => pieces::scrambling_token_item(&entry.token, pose, colors),
            None => pieces::token_item(token, colors, !read_only),
        })
        .collect();

    let mut orphans: Vec<&(RevokeScramble, ScramblePose)> = showing
        .iter()
        .filter(|(entry, _)| !tokens.iter().any(|token| token.name == entry.token.name))
        .collect();
    orphans.sort_by_key(|(entry, _)| entry.index);
    for (entry, pose) in orphans {
        let at = entry.index.min(rows.len());
        rows.insert(
            at,
            pieces::scrambling_token_item(&entry.token, pose, colors),
        );
    }
    rows
}

/// The `connection` group: the endpoint, the TLS posture, and one version row
/// per backend the server reports. Every value is read off `info`; `None`
/// prints [`DISCONNECTED_VALUE`] rather than nothing.
fn connection_rows(info: Option<&ServerInfo>, colors: &Palette) -> Vec<AnyView<SettingsState>> {
    let (endpoint, endpoint_tone) = match info {
        Some(info) => (info.endpoint.clone(), ValueTone::Ok),
        None => (DISCONNECTED_VALUE.to_string(), ValueTone::Plain),
    };
    let (tls_value, tls_tone) = match info {
        Some(info) => tls_posture(info.trust_mode),
        None => (DISCONNECTED_VALUE, ValueTone::Plain),
    };
    let mut rows = vec![
        pieces::settings_row(
            pieces::MARK_SERVER_URL,
            "server url",
            endpoint,
            endpoint_tone,
            RowAction::Static,
            colors,
        ),
        pieces::settings_row(
            pieces::MARK_TLS,
            "tls certificate",
            tls_value,
            tls_tone,
            RowAction::Static,
            colors,
        ),
    ];
    rows.extend(backend_version_rows(info, colors));
    rows
}

/// One row per backend `info` actually reported
/// ([`ServerInfo::backend_versions`]), or the legacy single-version fallback
/// for an older server that reported none — always at least one row, so this
/// group never goes silent about the multiplexer it's attached to (or, while
/// disconnected, the one it *was*).
fn backend_version_rows(
    info: Option<&ServerInfo>,
    colors: &Palette,
) -> Vec<AnyView<SettingsState>> {
    match info {
        Some(info) if !info.backend_versions.is_empty() => info
            .backend_versions
            .iter()
            .map(|backend_version| {
                pieces::settings_row(
                    backend_mark(backend_version.backend),
                    backend_version_label(backend_version.backend),
                    backend_version.version.clone(),
                    ValueTone::Plain,
                    RowAction::Static,
                    colors,
                )
            })
            .collect(),
        Some(info) => vec![pieces::settings_row(
            pieces::MARK_ZELLIJ,
            "zellij version",
            version_number(&info.version).to_string(),
            ValueTone::Plain,
            RowAction::Static,
            colors,
        )],
        None => vec![pieces::settings_row(
            pieces::MARK_ZELLIJ,
            "zellij version",
            DISCONNECTED_VALUE,
            ValueTone::Plain,
            RowAction::Static,
            colors,
        )],
    }
}

/// The `.set-ic` mark for `backend`'s version row — the backend's own initial
/// (see [`pieces::MARK_ZELLIJ`]/[`pieces::MARK_HERDR`]'s own doc for why).
fn backend_mark(backend: ServerBackend) -> pieces::RowMark {
    match backend {
        ServerBackend::Zellij => pieces::MARK_ZELLIJ,
        ServerBackend::Herdr => pieces::MARK_HERDR,
    }
}

/// …and its row label.
fn backend_version_label(backend: ServerBackend) -> &'static str {
    match backend {
        ServerBackend::Zellij => "zellij version",
        ServerBackend::Herdr => "herdr version",
    }
}

/// What the `tls certificate` row prints for a posture, and in which hue.
///
/// Branches on the [`TrustMode`] **variant**, never on `ServerInfo::tls_label`
/// (`crate::connection`'s own rule: that field is display text). The words are
/// the redesign's rather than that label's — its "No verification" names the
/// *posture*, where screen 5 names the *consequence*, which is the whole point
/// of the note under that mock.
fn tls_posture(mode: TrustMode) -> (&'static str, ValueTone) {
    match mode {
        TrustMode::NoVerification => ("insecure (plaintext)", ValueTone::Warn),
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
        show_sheet(&self.overlays, move |sheet| {
            let theme = pieces::active_theme();
            let colors = pieces::palette(&theme);
            let picker = actions.picker(kind);
            let picking = actions.clone();
            let sheet = sheet.clone();
            pieces::picker_sheet(&picker, &colors, move |_app: &mut AppState, index| {
                // Close first, then persist: the sheet's own exit staging and a
                // repository round trip should not overlap (the same ordering
                // the sessions management sheet's rows use).
                //
                // Through the sheet's own gate, which is what makes the close
                // single-shot: a pop only lands at the *next* rebuild, so a
                // second row tap inside that one-frame window would otherwise
                // pop the `/settings` route out from under the picker
                // (`crate::ui::sheet`'s "One sheet, one dismissal").
                sheet.close();
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

    /// Whether a mutating token control is blocked because the connected
    /// session is itself read-only — see [`READ_ONLY_TOKEN_TOAST`]'s own doc.
    /// Raises that toast when it is; the one guard both
    /// [`open_create_token_dialog`](Self::open_create_token_dialog) and
    /// [`open_revoke_dialog`](Self::open_revoke_dialog) open with.
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
    /// reveal (see [`open_secret_reveal`]).
    ///
    /// The confirmation toast is raised **only if the mint landed**: a failure
    /// already surfaces through the screen's own failure→toast bridge, and a
    /// "Created …" beside it would contradict it.
    pub fn open_create_token_dialog(&self) {
        if self.tokens_blocked() {
            return;
        }
        let controller = Arc::clone(&self.controller);
        let overlays = self.overlays.clone();
        pieces::open_create_token_dialog(
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

    /// Revoke the token called `name`: the row's scramble-then-collapse and the
    /// RPC start on the same tap.
    ///
    /// Concurrent on purpose — the animation is not a pre-roll waiting on the
    /// network, and a revoke that lands in 20ms must not make the row vanish
    /// before the eye has seen it go. The row's snapshot is taken here, ahead of
    /// the refresh that call ends with, because that refresh is exactly what
    /// takes the token out of `tokens` mid-animation (see [`token_rows`]).
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
            // Confirm only on success — see `open_create_token_dialog`.
            if controller.revoke_token(name.clone()).await {
                app_toasts().show(confirmation);
            }
            // Either way the row may now finish: a refusal leaves the token in
            // `tokens`, so dropping the timeline restores the row it was
            // collapsing, and the failure toast the controller emitted stands.
            scrambles.answered(&name);
        });
    }

    /// Open the delete-credential confirmation for the saved server at
    /// `endpoint`.
    ///
    /// **No read-only guard**, unlike the two token controls: deleting a
    /// device-local credential is not a server mutation, and the token revoke
    /// that rides along is best-effort by design (see
    /// [`SettingsController::delete_credential`]). A read-only session must
    /// still be able to forget its own stored secret.
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
                    // Confirm only on success — the same rule the token
                    // controls follow.
                    if handle.delete_credential(endpoint).await {
                        app_toasts().show(confirmation);
                    }
                });
            },
        );
    }

    /// Open the forget-server confirmation for the saved server at `endpoint`.
    /// The message names the stored credential only when there is one to lose.
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

    /// Open the Disconnect confirmation. See the [module docs](self) for why
    /// confirming both clears the connection *and* navigates.
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
/// secret — [`Actions::open_create_token_dialog`]'s own follow-up once a mint
/// lands. Mirrors [`Actions::open_picker`]'s split: the `show_sheet` call and
/// the controller read live here, [`pieces::secret_reveal_sheet`] is the pure
/// render.
fn open_secret_reveal(
    overlays: &NavigatorController<AppState>,
    controller: &Arc<SettingsController>,
) {
    let controller = Arc::clone(controller);
    show_sheet(overlays, move |sheet| {
        let theme = pieces::active_theme();
        let colors = pieces::palette(&theme);
        // Tracked: re-resolved on every navigator rebuild, exactly like the
        // picker sheet's own body (`Actions::open_picker`'s own doc).
        let minted = controller.pending_token_secret.get();
        let sheet = sheet.clone();
        let controller = Arc::clone(&controller);
        pieces::secret_reveal_sheet(minted.as_ref(), &colors, move |_app: &mut AppState| {
            // Close first, then clear — the picker sheet's own ordering (the
            // exit staging and a state write should not overlap).
            sheet.close();
            controller.clear_pending_secret();
        })
    });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_validation_skipping_posture_warns() {
        // The redesign's own correction, as a tripwire: `insecure (plaintext)`
        // must never read as a success state, and a validated posture must
        // never read as a warning.
        assert!(matches!(
            tls_posture(TrustMode::NoVerification),
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
