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
//! # What lives where
//!
//! - **Observed state** (the session list, its loaded flag) is the hosted
//!   controller's signals; **app-scoped** state (the connection's server info
//!   and credential offer) is [`crate::connection`]'s controller, resolved
//!   from context. [`SessionsState`] itself holds neither — only the three
//!   handles, plus the navigator this screen pushes through.
//! - **Every push this screen makes** — the terminal route, the two dialogs,
//!   the picker sheet — goes through the same [`NavigatorController`] the
//!   route builder handed in, and pauses the controller's mock poll first
//!   (see [`SessionsController`]'s module docs for why that pause has to be
//!   explicit).
//!
//! # The component state boundary, and what it means for callbacks
//!
//! A `Component`'s subtree is bound to its own `State`, so this screen's own
//! buttons take `&mut SessionsState`. The **overlays** it pushes are a
//! different story: a dialog/sheet is a page on the *app's* navigator, so its
//! callbacks receive `&mut AppState` and can't see [`SessionsState`] at all.
//! That is exactly why every overlay callback here captures an
//! `Arc<SessionsController>` and acts through it — the controller is the
//! shared object on both sides of the boundary.
//!
//! # Navigating to `/terminal`
//!
//! See [`push_terminal`]. Short version: a `NavigatorController` pushes page
//! *builders*, a `Router` resolves *paths*, and the app's own `Router` lives
//! in `AppState` — on the far side of the component boundary above. So this
//! screen rebuilds a router over `crate::routes`' own table (guard included),
//! attached to the same shared controller.

use std::sync::Arc;
use std::time::Duration;

use clean_signals_frust::{use_controller, use_failure_listener, use_interval};
use frust::glyph::app_bar;
use frust::{
    AnyView, Axis, ButtonStyle, Component, CrossAxisAlignment, EdgeInsets, FlexChild, FlexView,
    Get, NavigatorController, Padding, Router, SizedBox, any, button, component, flexible,
    inflexible, safe_area, scroll_view, spawn_local,
};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::sessions::di::expect_sessions_repository;
use crate::ui::dialogs::{confirm_dialog, name_input_dialog};
use crate::ui::sheet::show_sheet;
use crate::ui::toasts::app_toasts;

use super::controllers::{POLL_PERIOD_SECS, SessionsController};
use super::pieces;

/// The route this screen attaches into. The session id rides as a query param
/// (`Location::parse` splits it off before matching, so the flat `/terminal`
/// route and `crate::routes::session_guard`'s own `loc.path` check are both
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
/// The gap between the picker sheet's heading and its rows.
const SHEET_GAP: f64 = 14.0;

/// The Sessions screen's route entry point: a `Component` under the outer
/// [`AppState`], so its `init` runs under a real reactive `Owner` that
/// disposes the hosted controller on unmount.
pub fn sessions_page(controller: NavigatorController<AppState>) -> AnyView<AppState> {
    any(component(SessionsScreen { nav: controller }))
}

/// The Sessions screen `Component`. Stateless configuration — everything
/// mutable lives in [`SessionsState`] and the controller it hosts.
pub struct SessionsScreen {
    /// The app's one navigator, handed in by the route builder.
    nav: NavigatorController<AppState>,
}

/// The Sessions screen's retained state: three handles and nothing else.
pub struct SessionsState {
    /// The app's navigator — every push this screen makes goes through it.
    pub nav: NavigatorController<AppState>,
    /// The hosted view model (created in `init`, disposed on unmount).
    pub controller: Arc<SessionsController>,
    /// The app-scoped connection handle, for the server strip and the
    /// credential offer (see [`crate::connection`]).
    pub connection: Arc<ConnectionController>,
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
            nav: self.nav.clone(),
            controller,
            connection: expect_connection_controller(),
        }
    }

    fn build(&self, state: &mut SessionsState) -> AnyView<SessionsState> {
        // Page-driven poll resume (see `SessionsController`'s module docs).
        // A plain atomic, deliberately not a signal: a tracked write here
        // would re-dirty the very frame that made it.
        state.controller.observe_depth(state.nav.depth());

        // Tracked reads: a load, a mutation, or a connection change is what
        // wakes this rebuild.
        let sessions = state.controller.sessions.get();
        let loaded = state.controller.loaded.get();
        let offer = state.connection.credential_offer.get();
        let info = state.connection.server_info.get();

        let mut children = vec![pieces::server_strip(info.as_ref())];
        if let Some(label) = &offer {
            children.push(pieces::credential_banner(label));
        }
        if loaded && sessions.is_empty() {
            children.push(pieces::empty_view());
        } else {
            children.extend(sessions.iter().map(pieces::session_card));
            children.push(pieces::new_session_row());
        }

        let bar = app_bar::<SessionsState>("Sessions").actions(vec![
            any(
                button("Switch", |state: &mut SessionsState| open_picker(state))
                    .style(ButtonStyle::Ghost)
                    .small(),
            ),
            any(
                button("+", |state: &mut SessionsState| open_create_dialog(state))
                    .style(ButtonStyle::Primary)
                    .small(),
            ),
        ]);

        // The bar consumes the top window inset itself (`glyph::app_bar`'s own
        // module docs), so the body's safe area disables its top edge to avoid
        // double-padding it — the same composition the placeholder this screen
        // replaced used.
        let body = any(safe_area(scroll_view(Padding(
            EdgeInsets::symmetric(LIST_PAD_X, LIST_PAD_Y),
            FlexView::new(Axis::Vertical, gapped(children)).cross_axis(CrossAxisAlignment::Stretch),
        )))
        .top(false));
        any(FlexView::new(
            Axis::Vertical,
            vec![inflexible(any(bar)), flexible(1, body)],
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

/// Pause the mock poll for whatever this screen is about to push, marking the
/// depth to resume at. See [`SessionsController`]'s module docs.
fn pause_for_push(state: &SessionsState) {
    state.controller.pause_until_depth(state.nav.depth());
}

/// Attach to `id`: push the `/terminal` route, carrying the session id as a
/// query param.
pub(super) fn attach(state: &mut SessionsState, id: &str) {
    pause_for_push(state);
    push_terminal(&state.nav, id);
}

/// Open the create-session dialog (the app bar's `+`, the list's
/// "+ new session" row, and the empty state's action all land here).
pub(super) fn open_create_dialog(state: &mut SessionsState) {
    pause_for_push(state);
    let controller = Arc::clone(&state.controller);
    name_input_dialog(
        &state.nav,
        "New session",
        "",
        move |_app: &mut AppState, name: String| {
            let handle = Arc::clone(&controller);
            spawn_local(async move { handle.create(name).await });
        },
    );
}

/// Open the rename dialog for `id`, seeded with that session's *current*
/// name — re-read from the controller rather than captured, so a card built
/// several frames ago can't seed a stale name.
pub(super) fn open_rename_dialog(state: &mut SessionsState, id: &str) {
    let Some(session) = state.controller.session(id) else {
        // The card was built from a list that no longer holds this session
        // (a poll landed between paint and tap). Nothing to rename.
        return;
    };
    pause_for_push(state);
    let controller = Arc::clone(&state.controller);
    let id = id.to_string();
    name_input_dialog(
        &state.nav,
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
pub(super) fn open_kill_dialog(state: &mut SessionsState, id: &str) {
    pause_for_push(state);
    let controller = Arc::clone(&state.controller);
    let id = id.to_string();
    confirm_dialog(
        &state.nav,
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

/// Open the session-picker sheet: the same list in one line per row, as a
/// modal bottom sheet (`ui::sheet`, itself a documented workaround for the
/// missing Glyph sheet widget). Selecting a row pops the sheet and takes the
/// *same* attach path a card's Attach button does.
pub(super) fn open_picker(state: &mut SessionsState) {
    pause_for_push(state);
    let controller = Arc::clone(&state.controller);
    let nav = state.nav.clone();
    show_sheet(&state.nav, move || {
        // Tracked read inside the sheet's page builder — the navigator re-runs
        // it every rebuild, so a session killed behind the sheet drops out of
        // it on the next frame.
        let sessions = controller.sessions.get();
        let row_nav = nav.clone();
        let rows = pieces::picker_rows(&sessions, move |id| {
            // Pop the sheet, then push the terminal route: both ops land in
            // the same drain, so the stack goes sheet → terminal in one
            // rebuild. The poll stays paused throughout (the mark taken when
            // the sheet opened is shallower than either depth).
            row_nav.pop();
            push_terminal(&row_nav, id);
        });
        any(FlexView::new(
            Axis::Vertical,
            vec![
                inflexible(pieces::picker_heading(sessions.len())),
                inflexible(SizedBox::<AppState>(None, Some(SHEET_GAP))),
                inflexible(rows),
            ],
        )
        .cross_axis(CrossAxisAlignment::Stretch))
    });
}

/// Push the `/terminal` route for session `id` onto the app's page stack.
///
/// # Why this rebuilds a `Router`
///
/// Three facts meet here, and the shape falls out of them:
///
/// 1. A [`NavigatorController`] pushes page **builders**, never paths — only a
///    [`Router`] resolves a path against a route table (and runs the
///    redirects attached to it, `crate::routes::session_guard` among them).
/// 2. The app's own `Router` lives in `AppState` (`crate::AppState::nav`), and
///    this screen is a `Component`: its callbacks see [`SessionsState`], never
///    the outer app state (`frust_core::component`'s state boundary). There is
///    no route from here to that handle.
/// 3. Resolving the route by hand instead would mean calling the *terminal*
///    feature's own page builder — which `docs/ARCHITECTURE.md`'s cross-feature
///    rule forbids outright (a feature never imports a sibling's
///    `presentation`).
///
/// So this rebuilds a router over `crate::routes`' **own** table and **own**
/// guard, attached to the *same* shared [`NavigatorController`] — the push
/// therefore lands on the app's real navigator, through the app's real route
/// table, with the app's real guard applied (an unauthenticated attach bounces
/// to `/`, exactly as a deep link to `/terminal` would).
///
/// It is cheap and side-effect-free: `Router::with_controller` is a plain value
/// construction that never touches the controller, the table is four closures,
/// a resolved page owns its builder outright (so the router can be dropped the
/// moment it has enqueued the push), and this runs per *tap*, never per frame.
///
/// The seam that would remove it entirely — the composition root handing every
/// screen a path-pushing handle (or publishing the `Router` in context) — lives
/// in `src/routes.rs`/`src/lib.rs`, outside this feature.
fn push_terminal(nav: &NavigatorController<AppState>, id: &str) {
    Router::with_controller(nav, crate::routes::build_routes(nav.clone()))
        .redirect(crate::routes::session_guard)
        .push(&format!("{TERMINAL_ROUTE}?{SESSION_QUERY_KEY}={id}"));
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
}
