//! The `/sessions` route's page: muxr's session list.
//!
//! [`sessions_page`] is what `crate::routes` mounts; everything below it is
//! the canonical screen shape `docs/ARCHITECTURE.md`'s **Building a Screen**
//! section mandates — a per-screen [`Component`] ([`SessionsScreen`]) whose
//! `init` hosts a [`SessionsController`] through
//! [`use_controller`](clean_signals_frust::use_controller), so the controller
//! is created on route mount and disposed on unmount (`initState`/`dispose`
//! parity), and whose `build` is pure arrangement over
//! [`super::pieces`]' elements.
//!
//! # The screen's two bands (the third is the shell's)
//!
//! ```text
//! ┌─────────────────────────────┐
//! │ ▪▪ muxr   100.71.31.57   ⚙  │  crate::shell's persistent bar — NOT this
//! ├─────────────────────────────┤  screen's; it floats over this page
//! │ ● 100.71.31.57:50051  …     │  .conn-row — FIXED, outside the scroll
//! ├─────────────────────────────┤
//! │ ┃ D  dev      running zellij│  the scrolling list: cards, the offer
//! │ ┃ ▣ 9 panes ◧ 4 tabs ◎ 0 …  │  banner, and the dashed new-session row
//! │ + new session (dashed)      │
//! └─────────────────────────────┘
//! ```
//!
//! The bar above the strip belongs to [`crate::shell`], not here: it is
//! persistent chrome whose title crossfades across route changes, so mounting a
//! second copy per screen would fight it. It *floats* over this page rather
//! than displacing it (`crate::shell::root_view`'s reflow note), so the page
//! runs the full surface and reserves the band the bar covers with
//! [`topbar_clearance`] — under which the strip sits, exactly where it did when
//! the shell still handed the page a shortened box. The body keeps
//! `safe_area(..).top(false)` because that clearance already contributes the
//! status-bar inset the bar consumes (`crate::ui::topbar`'s own module docs).
//! The `⚙` that reaches `/settings` moved with the bar — it is `/sessions`'
//! route metadata now (`crate::routes::BarAction`).
//!
//! The strip is deliberately **not** a list item: it describes the connection
//! the list belongs to, so scrolling the list must not scroll it away
//! (`docs/redesign.html` puts it outside `.scroll` for the same reason). The
//! page tone behind both is `--bg-base`, painted by the screen: the shell
//! clears to `--bg-surface`, which is the *card* tone, so a screen that painted
//! no background would render cards with no visible edge at all.
//!
//! # Interaction model
//!
//! There are no per-card buttons. **Tap a card to attach** (`/terminal`);
//! **long-press it for the management sheet** (Attach / Rename… / Kill…), which
//! is where the two dialogs are reached from. Create has exactly two entry
//! points — the dashed row at the end of the list, and the empty state — since
//! the stock app bar (and the `+` and `Switch` actions it carried) is gone.
//!
//! # What lives where
//!
//! - **Observed state** (the session list, its loaded flag) is the hosted
//!   controller's signals; **app-scoped** state (the connection's server info
//!   and credential offer) is [`crate::connection`]'s controller, resolved
//!   from context. [`SessionsState`] itself holds neither — only [`Actions`]'
//!   four handles plus the connection.
//! - **Every push this screen makes** — the terminal route, the two dialogs,
//!   the management sheet — goes through [`Actions`], and pauses the
//!   controller's mock poll first (see [`SessionsController`]'s module docs
//!   for why that pause has to be explicit).
//!
//! # The component state boundary, and what it means for callbacks
//!
//! A `Component`'s subtree is bound to its own `State`, so this screen's own
//! gestures take `&mut SessionsState`. The **overlays** it pushes are a
//! different story: a dialog/sheet is a page on the *app's* root overlay host
//! (`crate::AppState::overlays`, above every piece of chrome), so its
//! callbacks receive `&mut AppState` and can't see [`SessionsState`] at all.
//! That is exactly why every action lives on [`Actions`] — a small clonable
//! bundle of handles that a sheet row can capture and act through, with the
//! same method body serving both sides of the boundary.
//!
//! # Navigating to `/terminal`
//!
//! A `NavigatorController` pushes page *builders*; only a `Router` resolves
//! *paths* (and runs the guard attached to it), and the app's one `Router`
//! lives in `AppState` — on the far side of the component boundary above. So
//! the composition root publishes it as [`crate::routes::Nav`], which this
//! screen resolves once in [`Component::init`] and keeps in [`Actions`];
//! [`push_terminal`] is the one place that spells the location. Every push
//! therefore runs the app's real table and its real (capture-based, owner-free)
//! session guard.

use std::sync::Arc;
use std::time::Duration;

use clean_signals_frust::{use_controller, use_failure_listener, use_interval};
use frust::{
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexChild,
    FlexView, Get, NavigatorController, Padding, SizedBox, WindowMetrics, any, component, flexible,
    inflexible, safe_area, scroll_view, spawn_local, use_context,
};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::sessions::di::expect_sessions_repository;
use crate::responsive::WIDE_BREAKPOINT_PX;
use crate::routes::{Nav, app_nav};
use crate::ui::dialogs::{confirm_dialog, name_input_dialog};
use crate::ui::sheet::{SheetHandle, show_sheet};
use crate::ui::toasts::app_toasts;
use crate::ui::topbar::topbar_clearance;

use super::controllers::{POLL_PERIOD_SECS, SessionsController};
use super::pieces;

/// The route this screen attaches into. The session id rides as a query param
/// (`Location::parse` splits it off before matching, so the flat `/terminal`
/// route and `crate::routes`' own session-guard `loc.path` check are both
/// unaffected) — the terminal screen ignores it today and will read it from
/// `RouteParams` when it has a session to attach to.
const TERMINAL_ROUTE: &str = "/terminal";
/// The query key [`TERMINAL_ROUTE`] carries the session id under.
const SESSION_QUERY_KEY: &str = "session";

/// `.sessions-list{padding:14px 16px}`.
const LIST_PAD_X: f64 = 16.0;
const LIST_PAD_Y: f64 = 14.0;
/// `.sessions-list{gap:12px}` — between consecutive list children.
const LIST_GAP: f64 = 12.0;
/// The gap between the management sheet's heading and its rows, and between
/// the rows themselves.
const SHEET_GAP: f64 = 14.0;
const SHEET_ROW_GAP: f64 = 4.0;

/// The scrolling list column's max width past [`WIDE_BREAKPOINT_PX`]
/// (FINDINGS #21's conservative width-clamp pass) — a tablet/landscape/desktop
/// window centers the card list rather than stretching it edge to edge.
///
/// **The conn-row strip (`server_strip`) stays full-bleed on purpose.** It is
/// the screen's own persistent chrome, not a list item (see the module docs'
/// "two bands" note) — the strip describing the connection reads more like a
/// header bar than a content card, so it is left spanning the whole window
/// while only the scrolling list clamps and centers under it. No design mock
/// exists for a structural two-pane Sessions, so this is a plain clamp on the
/// scroller alone, not a layout switch.
const SESSIONS_CLAMP_WIDTH: f64 = 640.0;

/// The Sessions screen's route entry point: a `Component` under the outer
/// [`AppState`], so its `init` runs under a real reactive `Owner` that
/// disposes the hosted controller on unmount.
pub fn sessions_page(
    controller: NavigatorController<AppState>,
    overlays: NavigatorController<AppState>,
) -> AnyView<AppState> {
    any(component(SessionsScreen {
        nav: controller,
        overlays,
    }))
}

/// The Sessions screen `Component`. Stateless configuration — everything
/// mutable lives in [`SessionsState`] and the controller it hosts.
pub struct SessionsScreen {
    /// The app's route navigator, handed in by the route builder — read for
    /// its depth alone (see [`Actions::stack_depth`]).
    nav: NavigatorController<AppState>,
    /// The root overlay host's navigator: every dialog and sheet this screen
    /// opens is a page here (`crate::AppState::overlays`).
    overlays: NavigatorController<AppState>,
}

/// The Sessions screen's retained state: the action bundle and the app-scoped
/// connection handle, and nothing else.
pub struct SessionsState {
    /// Everything this screen can *do*, and the handles it needs to do it —
    /// see [`Actions`].
    pub actions: Actions,
    /// The app-scoped connection handle, for the strip, the top bar's title
    /// and the credential offer (see [`crate::connection`]).
    pub connection: Arc<ConnectionController>,
}

/// Every navigation this screen performs, plus the three handles they need.
///
/// Cloneable on purpose: a card's gesture reaches these through
/// `&mut SessionsState`, while a *sheet row* — a page on the app's navigator,
/// which only ever sees `&mut AppState` — captures a clone and calls the very
/// same method. One body, both sides of the component boundary.
#[derive(Clone)]
pub struct Actions {
    /// The app's **route** navigator. Nothing is pushed through it — a route
    /// goes through [`Nav`] and an overlay through [`Self::overlays`] — it is
    /// held for its depth alone (see [`Actions::stack_depth`]).
    pub nav: NavigatorController<AppState>,
    /// The root overlay host's navigator — every page *builder* this screen
    /// pushes (the two dialogs, the management sheet) goes through it, so each
    /// lands **above** the shell's floating bar rather than under it
    /// (`crate::AppState::overlays`; FINDINGS #44).
    pub overlays: NavigatorController<AppState>,
    /// The app's one path-pushing handle — how this screen reaches `/terminal`
    /// and `/settings` (see the module docs' navigation note).
    pub app_nav: Nav,
    /// The hosted view model (created in `init`, disposed on unmount).
    pub controller: Arc<SessionsController>,
}

impl Component for SessionsScreen {
    type State = SessionsState;

    fn init(&self) -> SessionsState {
        // The screen-scoped view model, over the app-scoped repository `di`
        // published. Auto-disposed on unmount via `on_cleanup`.
        let controller = use_controller::<SessionsController, AppFailure>(|| {
            SessionsController::new(expect_sessions_repository())
        });

        // Failure → toast, for this screen's lifetime. The handler runs on the
        // UI thread (every controller call is `spawn_local`ed onto the
        // UI-thread queue), which is what makes the thread-local `app_toasts`
        // handle the right one — see `ui::toasts`' module docs.
        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        // First load.
        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.refresh().await });

        // The mock poll: a component-scoped interval, so it stops the moment
        // this route unmounts. While it is merely *covered* (`/terminal` on
        // top) the component is still very much alive, which is what
        // `SessionsController`'s pause exists for.
        let polling = Arc::clone(&controller);
        use_interval(Duration::from_secs(POLL_PERIOD_SECS), move || {
            if polling.take_poll_tick() {
                let handle = Arc::clone(&polling);
                spawn_local(async move { handle.refresh().await });
            }
        });

        SessionsState {
            actions: Actions {
                nav: self.nav.clone(),
                overlays: self.overlays.clone(),
                // Resolved here, with every other app-scoped dependency —
                // never from inside a press handler.
                app_nav: app_nav(),
                controller,
            },
            connection: expect_connection_controller(),
        }
    }

    fn build(&self, state: &mut SessionsState) -> AnyView<SessionsState> {
        // Page-driven poll resume (see `SessionsController`'s module docs).
        // A plain atomic, deliberately not a signal: a tracked write here
        // would re-dirty the very frame that made it.
        state
            .actions
            .controller
            .observe_depth(state.actions.stack_depth());

        // Tracked reads: a load, a mutation, or a connection change is what
        // wakes this rebuild.
        let sessions = state.actions.controller.sessions.get();
        let loaded = state.actions.controller.loaded.get();
        let offer = state.connection.credential_offer.get();
        let info = state.connection.server_info.get();

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

        // The scrolling half: the offer banner (when there is one), then either
        // the empty state or every card plus the dashed create row.
        let mut list = Vec::new();
        if let Some(label) = &offer {
            list.push(pieces::credential_banner(label));
        }
        if loaded && sessions.is_empty() {
            list.push(pieces::empty_view());
        } else {
            list.extend(
                sessions
                    .iter()
                    .map(|session| pieces::session_card(session, &colors)),
            );
            list.push(pieces::new_session_row(&colors));
        }

        // The *shell's* bar consumes the top window inset on this screen's
        // behalf (`crate::ui::topbar`'s own module docs), so the body's safe
        // area disables its top edge to avoid double-padding it.
        let scroller = any(safe_area(scroll_view(Padding(
            EdgeInsets::symmetric(LIST_PAD_X, LIST_PAD_Y),
            FlexView::new(Axis::Vertical, gapped(list)).cross_axis(CrossAxisAlignment::Stretch),
        )))
        .top(false));
        // Clamp/center the list column only — the conn-row strip beside it
        // stays full-bleed (see `SESSIONS_CLAMP_WIDTH`'s doc).
        let scroller = if is_wide {
            any(Align(
                Alignment::CENTER,
                SizedBox::<SessionsState>(Some(SESSIONS_CLAMP_WIDTH), None).child(scroller),
            ))
        } else {
            scroller
        };
        // The page tone stays **outside** the clearance so it fills the whole
        // surface, bar band included: the bar slides in over it (see
        // `crate::shell::root_view`), and a page that stopped at the clearance
        // would flash the shell's own clear colour through the gap for the
        // length of that entrance.
        any(pieces::page_tone(
            topbar_clearance(
                FlexView::new(
                    Axis::Vertical,
                    vec![
                        inflexible(pieces::server_strip(info.as_ref(), &colors)),
                        flexible(1, scroller),
                    ],
                )
                .cross_axis(CrossAxisAlignment::Stretch),
            ),
            &colors,
        ))
    }
}

/// The list's children with a [`LIST_GAP`] spacer between consecutive ones.
fn gapped(views: Vec<AnyView<SessionsState>>) -> Vec<FlexChild<SessionsState>> {
    let mut children = Vec::with_capacity(views.len() * 2);
    for (index, view) in views.into_iter().enumerate() {
        if index > 0 {
            children.push(inflexible(SizedBox::<SessionsState>(None, Some(LIST_GAP))));
        }
        children.push(inflexible(view));
    }
    children
}

// ---------------------------------------------------------------------------
// Actions (each one a push, so each one pauses the poll first)
// ---------------------------------------------------------------------------

impl Actions {
    /// How deep the app's page stack is, counting **both** navigators — the
    /// one number the mock poll's pause/resume pair is expressed against.
    ///
    /// It has to be the sum now that overlays are pages of the root host
    /// rather than of the route navigator: the route depth alone no longer
    /// moves when a dialog or the management sheet opens, so a pause marked
    /// against it would resume on the very next `build`. Both terms are read
    /// the same way at both ends ([`pause_for_push`](Self::pause_for_push) and
    /// [`Component::build`]'s `observe_depth`), so the arithmetic is exactly
    /// the one-navigator arithmetic it replaces, plus a constant.
    fn stack_depth(&self) -> usize {
        self.nav.depth() + self.overlays.depth()
    }

    /// Pause the mock poll for whatever is about to be pushed, marking the
    /// depth to resume at. See [`SessionsController`]'s module docs.
    fn pause_for_push(&self) {
        self.controller.pause_until_depth(self.stack_depth());
    }

    /// Attach to `id`: push the `/terminal` route, carrying the session id as
    /// a query param. A card tap and the management sheet's own Attach row
    /// both land here.
    pub fn attach(&self, id: &str) {
        self.pause_for_push();
        push_terminal(&self.app_nav, id);
    }

    /// Open the create-session dialog (the dashed row and the empty state's
    /// action both land here).
    pub fn open_create_dialog(&self) {
        self.pause_for_push();
        let controller = Arc::clone(&self.controller);
        name_input_dialog(
            &self.overlays,
            "New session",
            "",
            move |_app: &mut AppState, name: String| {
                let handle = Arc::clone(&controller);
                spawn_local(async move { handle.create(name).await });
            },
        );
    }

    /// Open the rename dialog for `id`, seeded with that session's *current*
    /// name — re-read from the controller rather than captured, so a sheet
    /// opened several frames ago can't seed a stale name.
    pub fn open_rename_dialog(&self, id: &str) {
        let Some(session) = self.controller.session(id) else {
            // The card was built from a list that no longer holds this session
            // (a poll landed between paint and tap). Nothing to rename.
            return;
        };
        self.pause_for_push();
        let controller = Arc::clone(&self.controller);
        let id = id.to_string();
        name_input_dialog(
            &self.overlays,
            "Rename session",
            session.name,
            move |_app: &mut AppState, name: String| {
                let handle = Arc::clone(&controller);
                let id = id.clone();
                spawn_local(async move { handle.rename(id, name).await });
            },
        );
    }

    /// Open the kill confirmation for `id`.
    pub fn open_kill_dialog(&self, id: &str) {
        self.pause_for_push();
        let controller = Arc::clone(&self.controller);
        let id = id.to_string();
        confirm_dialog(
            &self.overlays,
            "Kill session?",
            "Any attached client is disconnected immediately.",
            "Kill",
            move |_app: &mut AppState| {
                let handle = Arc::clone(&controller);
                let id = id.clone();
                spawn_local(async move { handle.kill(id).await });
            },
        );
    }

    /// Open the per-session management sheet — what a **long-press** on a card
    /// raises, and the only route to rename/kill now that the card carries no
    /// buttons.
    ///
    /// Each row pops the sheet first and then does its work, so the stack goes
    /// sheet → dialog (or sheet → `/terminal`) in one rebuild: both ops land in
    /// the same drain, and the poll stays paused throughout, since the mark
    /// taken when the sheet opened is shallower than either depth.
    pub fn open_manage_sheet(&self, id: &str) {
        let Some(session) = self.controller.session(id) else {
            return;
        };
        self.pause_for_push();
        let actions = self.clone();
        let name = session.name;
        let id = id.to_string();
        show_sheet(&self.overlays, move |sheet| {
            let heading = pieces::sheet_heading(name.clone());
            let rows = [
                sheet_action(&actions, sheet, &id, ManageAction::Attach),
                sheet_action(&actions, sheet, &id, ManageAction::Rename),
                sheet_action(&actions, sheet, &id, ManageAction::Kill),
            ];
            let mut children = vec![
                inflexible(heading),
                inflexible(SizedBox::<AppState>(None, Some(SHEET_GAP))),
            ];
            for (index, row) in rows.into_iter().enumerate() {
                if index > 0 {
                    children.push(inflexible(SizedBox::<AppState>(None, Some(SHEET_ROW_GAP))));
                }
                children.push(inflexible(row));
            }
            any(FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch))
        });
    }
}

/// The three things the management sheet can do to a session.
#[derive(Clone, Copy)]
enum ManageAction {
    Attach,
    Rename,
    Kill,
}

impl ManageAction {
    /// The row's label. The two that push a dialog carry the platform ellipsis
    /// convention ("more input needed"); Attach acts immediately.
    fn label(self) -> &'static str {
        match self {
            ManageAction::Attach => "Attach",
            ManageAction::Rename => "Rename\u{2026}",
            ManageAction::Kill => "Kill\u{2026}",
        }
    }

    /// Whether the row is the destructive one.
    fn destructive(self) -> bool {
        matches!(self, ManageAction::Kill)
    }
}

/// One management-sheet row, wired to `action` for session `id`.
fn sheet_action(
    actions: &Actions,
    sheet: &SheetHandle,
    id: &str,
    action: ManageAction,
) -> AnyView<AppState> {
    let actions = actions.clone();
    let sheet = sheet.clone();
    let id = id.to_string();
    pieces::sheet_row(
        action.label(),
        action.destructive(),
        move |_app: &mut AppState| {
            // Close the sheet first: the dialog (or the terminal route) takes its
            // place in the same rebuild — see `open_manage_sheet`'s docs.
            // Through the sheet's own handle rather than a bare `pop`, so a
            // second row tap inside the rebuild its queued pop leaves open
            // cannot pop whatever is under it (`crate::ui::sheet`'s "One sheet,
            // one dismissal").
            sheet.close();
            match action {
                ManageAction::Attach => actions.attach(&id),
                ManageAction::Rename => actions.open_rename_dialog(&id),
                ManageAction::Kill => actions.open_kill_dialog(&id),
            }
        },
    )
}

// ---------------------------------------------------------------------------
// The screen-state shims the pieces' own callbacks call
// ---------------------------------------------------------------------------

/// Attach to `id` — a card tap.
pub(super) fn attach(state: &mut SessionsState, id: &str) {
    state.actions.attach(id);
}

/// Open the create-session dialog — the dashed row and the empty state.
pub(super) fn open_create_dialog(state: &mut SessionsState) {
    state.actions.open_create_dialog();
}

/// Open the management sheet for `id` — a card long-press.
pub(super) fn open_manage_sheet(state: &mut SessionsState, id: &str) {
    state.actions.open_manage_sheet(id);
}

/// Push the `/terminal` route for session `id` onto the app's page stack,
/// through the app's one guarded [`Nav`] handle.
///
/// The one place this location is spelled — both attach paths (a card tap and
/// the management sheet's Attach row) come through here, so the format and the
/// guarded route they resolve against cannot drift apart. The push runs the
/// app's real route table with its real session guard applied, so an
/// unauthenticated attach bounces to `/` exactly as a deep link to `/terminal`
/// would.
fn push_terminal(nav: &Nav, id: &str) {
    nav.push(&format!("{TERMINAL_ROUTE}?{SESSION_QUERY_KEY}={id}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_attach_location_carries_the_session_id_as_a_query_param() {
        // A tripwire on the format string: the *path* must stay the flat
        // `/terminal` the route table and the session guard both match on,
        // with the id only in the query.
        let raw = format!("{TERMINAL_ROUTE}?{SESSION_QUERY_KEY}=s-1");
        let loc = frust::Location::parse(&raw);

        assert_eq!(loc.path, TERMINAL_ROUTE);
        assert_eq!(
            loc.query.get(SESSION_QUERY_KEY).map(String::as_str),
            Some("s-1")
        );
    }

    #[test]
    fn the_management_sheet_offers_exactly_three_rows_one_of_them_destructive() {
        let labels: Vec<&str> = [
            ManageAction::Attach,
            ManageAction::Rename,
            ManageAction::Kill,
        ]
        .into_iter()
        .map(ManageAction::label)
        .collect();
        assert_eq!(labels, ["Attach", "Rename\u{2026}", "Kill\u{2026}"]);
        assert!(!ManageAction::Attach.destructive());
        assert!(!ManageAction::Rename.destructive());
        assert!(ManageAction::Kill.destructive());
    }
}
