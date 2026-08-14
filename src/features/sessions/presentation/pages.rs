//! The `/sessions` route: muxr's session list, plus every action a card or
//! its management sheet can take.
//!
//! [`sessions_page`] is what `crate::routes` mounts; [`SessionsScreen`]
//! follows the canonical screen shape `docs/ARCHITECTURE.md`'s "Building a
//! Screen" section. Observed state (the session list) lives in the hosted
//! [`SessionsController`]; app-scoped state (the connection, its read-only
//! posture) is read from [`crate::connection`]. Every push this screen makes
//! goes through [`Actions`], which pauses the controller's mock poll first.
//!
//! - **Back press**: `/sessions` is pushed with `BackPolicy::DismissAnimated`,
//!   so a back press bumps a generation cell instead of popping;
//!   [`BackConfirm`] answers it at most once per bump and raises the
//!   disconnect confirmation.
//! - **Poll cadence**: both cadence loops below re-resolve `settings`' live
//!   `layout_refresh_secs` pref before every sleep (see
//!   [`resolve_poll_interval`]) rather than snapshotting it once at mount.
//! - **Read-only gating**: a read-only credential disables create/Rename/Kill
//!   — never Attach (see [`Actions::open_manage_sheet`]).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::{
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexChild,
    FlexView, Get, GetUntracked, NavigatorController, Padding, SizedBox, WindowMetrics, any,
    component, flexible, inflexible, on_cleanup, safe_area, scroll_view, spawn_local, use_context,
};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::sessions::di::expect_sessions_repository;
use crate::features::settings::di::{SharedPreferencesRepository, expect_preferences_repository};
use crate::responsive::WIDE_BREAKPOINT_PX;
use crate::routes::{Nav, app_nav, sessions_back_signal};
use crate::ui::dialogs::{confirm_dialog, name_input_dialog};
use crate::ui::palette::Palette;
use crate::ui::sheet::show_sheet;
use crate::ui::spacing;
use crate::ui::toasts::app_toasts;
use crate::ui::topbar::topbar_clearance;

use super::controllers::{POLL_PERIOD_SECS, SessionsController};
use super::heartbeat::{self, HeartbeatState};
use super::widgets;

/// The route this screen attaches into. The session id rides as a query
/// param (`Location::parse` splits it off before matching, so the flat
/// `/terminal` route and `crate::routes`' own session-guard `loc.path` check
/// are both unaffected).
const TERMINAL_ROUTE: &str = "/terminal";
/// The query key [`TERMINAL_ROUTE`] carries the session id under.
const SESSION_QUERY_KEY: &str = "session";

/// The back-press confirmation, as the user reads it: its title…
const DISCONNECT_TITLE: &str = "Disconnect from server?";
/// …its destructive action…
const DISCONNECT_CONFIRM_LABEL: &str = "Disconnect";
/// …and what its body names when the connection has published no endpoint to
/// name (unreachable behind the session guard, but the sentence still needs
/// a subject).
const DISCONNECT_UNKNOWN_HOST: &str = "this server";

/// `.sessions-list{padding:14px 16px}`.
const LIST_PAD_X: f64 = spacing::XL;
const LIST_PAD_Y: f64 = spacing::LG;
/// `.sessions-list{gap:12px}` — between consecutive list children.
const LIST_GAP: f64 = spacing::BASE;

/// The scrolling list column's max width past [`WIDE_BREAKPOINT_PX`] — a
/// tablet/landscape/desktop window centers the card list rather than
/// stretching it edge to edge.
///
/// **The conn-row strip (`server_strip`) stays full-bleed on purpose**: it
/// is the screen's own persistent chrome, not a list item, so it reads more
/// like a header bar than a content card. No design mock exists for a
/// structural two-pane Sessions, so this is a plain clamp on the scroller
/// alone, not a layout switch.
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

/// The Sessions screen's retained state: the action bundle, the app-scoped
/// connection handle and the strip's heartbeat, and nothing else.
pub struct SessionsState {
    /// Everything this screen can *do*, and the handles it needs to do it —
    /// see [`Actions`].
    pub actions: Actions,
    /// The app-scoped connection handle, for the strip, the top bar's title
    /// and the read-only posture that gates this screen's mutations.
    pub connection: Arc<ConnectionController>,
    /// The `.conn-row` strip's heartbeat clocks — written by the ping
    /// interval `init` starts, read by every rebuild that renders the strip
    /// (see [`super::heartbeat`]). Screen-internal, unlike the two handles
    /// above.
    pub(super) heartbeat: HeartbeatState,
    /// The Android back press's observer — see [`BackConfirm`].
    /// Screen-internal like the heartbeat.
    pub(super) back: BackConfirm,
}

/// What a back press on `/sessions` means to this screen: the route page's
/// dismiss-signal cell, the generation already answered, and the depth mark
/// that says the confirmation it raised is still up.
///
/// **Plain state, not signals**: read *and written* from `Component::build`,
/// where a tracked write would re-dirty the very frame that made it (the
/// same reason [`SessionsController`]'s poll gate is).
pub(super) struct BackConfirm {
    /// The generation counter the navigator bumps in place of popping this
    /// page — resolved once, in `init` (`crate::routes::sessions_back_signal`).
    signal: Rc<Cell<u64>>,
    /// The generation this screen has already answered. Seeded from the cell
    /// rather than zero, so a screen that resolved an already-used cell
    /// answers only what happens from its own mount onwards.
    answered: u64,
    /// The stack depth this screen's confirmation was raised at, or `None`
    /// while none is up — the same mark-and-resume arithmetic
    /// [`Actions::pause_for_push`] uses for the polls.
    confirming_at: Option<usize>,
}

impl BackConfirm {
    /// Resolve this mount's dismiss cell.
    fn new() -> Self {
        let signal = sessions_back_signal();
        let answered = signal.get();
        BackConfirm {
            signal,
            answered,
            confirming_at: None,
        }
    }

    /// Whether a back press has landed since the last call, given the live
    /// `depth` — asked once per `build`, and the only reader of the cell.
    ///
    /// Answers `true` **at most once per bump**, because `build` re-runs
    /// several times per frame (`RenderRoot::rebuild` re-runs the whole app
    /// logic after each queued op it flushes, and the push this answer makes
    /// is itself such an op) — so the generation is banked *before* the
    /// answer, and the pass that follows sees nothing owed.
    ///
    /// A bump arriving while a confirmation is still up is banked and
    /// swallowed rather than queueing a second dialog: with an overlay open
    /// the root host claims the back press ahead of the route navigator
    /// (**R44-back**), so a second press dismisses the confirmation instead
    /// of reaching this screen at all.
    fn take_request(&mut self, depth: usize) -> bool {
        if let Some(mark) = self.confirming_at
            && depth <= mark
        {
            // The confirmation resolved — confirmed, cancelled or dismissed,
            // all of which leave the stack back where it was when it opened.
            self.confirming_at = None;
        }
        let generation = self.signal.get();
        if generation == self.answered {
            return false;
        }
        self.answered = generation;
        if self.confirming_at.is_some() {
            return false;
        }
        self.confirming_at = Some(depth);
        true
    }
}

/// Every navigation this screen performs, plus the three handles they need.
///
/// Cloneable on purpose: a card's gesture reaches these through
/// `&mut SessionsState`, while a *sheet row* — a page on the app's
/// navigator, which only ever sees `&mut AppState` — captures a clone and
/// calls the very same method. One body, both sides of the component
/// boundary.
#[derive(Clone)]
pub struct Actions {
    /// The app's **route** navigator. Nothing is pushed through it — a route
    /// goes through [`Nav`] and an overlay through [`Self::overlays`] — it is
    /// held for its depth alone (see [`Actions::stack_depth`]).
    pub nav: NavigatorController<AppState>,
    /// The root overlay host's navigator — every page *builder* this screen
    /// pushes (the two dialogs, the management sheet) goes through it, so each
    /// lands **above** the shell's floating bar rather than under it.
    pub overlays: NavigatorController<AppState>,
    /// The app's one path-pushing handle — how this screen reaches
    /// `/terminal` and `/settings` (see [`push_terminal`]).
    pub app_nav: Nav,
    /// The hosted view model (created in `init`, disposed on unmount).
    pub controller: Arc<SessionsController>,
}

// ---------------------------------------------------------------------------
// The session-poll cadence — one shared loop, live-resolved every tick
// ---------------------------------------------------------------------------

/// Resolve the live session-poll cadence — `settings`' own stored
/// `layout_refresh_secs` preference — as a [`Duration`], reading through
/// `preferences` (the one handle [`Component::init`](SessionsScreen::init)
/// resolves; never a fresh repository call). Falls back to
/// [`POLL_PERIOD_SECS`] on a read failure, which never actually happens in
/// practice. The **one** seam both [`spawn_cadence_loop`] instances this
/// screen starts call before every sleep, and the same seam
/// `tests/sessions.rs` drives directly to pin the live-re-read contract.
pub async fn resolve_poll_interval(preferences: &SharedPreferencesRepository) -> Duration {
    let secs = preferences
        .load_preferences()
        .await
        .map(|prefs| prefs.poll.layout_refresh_secs)
        .unwrap_or(POLL_PERIOD_SECS);
    Duration::from_secs(u64::from(secs))
}

/// Start a self-rescheduling loop that runs `tick` forever, re-resolving
/// [`resolve_poll_interval`] before every sleep rather than baking a period
/// in at mount time. Mirrors `clean_signals_frust::use_interval`'s
/// spawn/sleep/alive-flag shape, but a fixed `Duration` argument won't do
/// here: the whole point is reading the *live* pref on every iteration, so
/// this hand-rolls the loop instead of wrapping that helper. Must run where
/// a reactive `Owner` is ambient — i.e. from
/// [`Component::init`](SessionsScreen::init) — `on_cleanup` silently no-ops
/// outside one, so a call from anywhere else never stops.
fn spawn_cadence_loop(
    preferences: SharedPreferencesRepository,
    tick: impl Fn() + Send + Sync + 'static,
) {
    let alive = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&alive);
    on_cleanup(move || flag.store(false, Ordering::SeqCst));

    spawn_local(async move {
        loop {
            let period = resolve_poll_interval(&preferences).await;
            clean_signals::time::sleep(period).await;
            if !alive.load(Ordering::SeqCst) {
                break;
            }
            tick();
        }
    });
}

impl Component for SessionsScreen {
    type State = SessionsState;

    fn init(&self) -> SessionsState {
        // The screen-scoped view model, over the app-scoped repository `di`
        // published. Auto-disposed on unmount via `on_cleanup`.
        let controller = use_controller::<SessionsController, AppFailure>(|| {
            SessionsController::new(expect_sessions_repository())
        });

        // Failure → toast, for this screen's lifetime — the UI-thread
        // `app_toasts` handle is the right one since every controller call
        // is `spawn_local`ed onto the same queue.
        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.refresh().await });

        // Resolved *synchronously, in `init`*, never inside a spawned
        // future's body: the `frust::spawn_local` poller that later polls a
        // loop's future installs no ambient owner, so a `use_context`
        // lookup from inside a future's first poll would panic. Only the
        // resolved `Arc` crosses into either cadence loop below.
        let preferences = expect_preferences_repository();

        // The session-list poll: `spawn_cadence_loop` re-resolves
        // `layout_refresh_secs` before every sleep, so a pref change applies
        // live rather than only from the *next* mount.
        let polling = Arc::clone(&controller);
        spawn_cadence_loop(Arc::clone(&preferences), move || {
            if polling.take_poll_tick() {
                let handle = Arc::clone(&polling);
                spawn_local(async move { handle.refresh_from_poll().await });
            }
        });

        let connection = expect_connection_controller();

        // The strip's connection heartbeat: one real timed `GetVersion`, on
        // the same live-resolved cadence as the list poll above (the 300ms
        // roll `heartbeat::HeartbeatState::settle` animates over is a
        // separate display concern). `is_polling()` is the same
        // covered-screen gate the list poll uses.
        let heartbeat = HeartbeatState::new();
        let beating = Arc::clone(&controller);
        let pinged = Arc::clone(&connection);
        spawn_cadence_loop(preferences, move || {
            if !beating.is_polling() {
                return;
            }
            // Untracked: a callback is not a render. `None` while nothing is
            // connected gates there being anything to time at all.
            let Some(from) = pinged
                .server_info
                .get_untracked()
                .map(|info| info.latency_ms)
            else {
                return;
            };
            heartbeat.start_ping();
            let connection = Arc::clone(&pinged);
            spawn_local(async move {
                // A ping that measured nothing rolls nothing — the list poll
                // owns failure surfacing.
                if let Some(measured) = connection.ping().await {
                    heartbeat.settle(from, measured);
                }
            });
        });

        SessionsState {
            actions: Actions {
                nav: self.nav.clone(),
                overlays: self.overlays.clone(),
                app_nav: app_nav(),
                controller,
            },
            connection,
            heartbeat,
            // The cell published now is this mount's own.
            back: BackConfirm::new(),
        }
    }

    fn build(&self, state: &mut SessionsState) -> AnyView<SessionsState> {
        // Page-driven poll resume. A plain atomic, deliberately not a
        // signal: a tracked write here would re-dirty the very frame that
        // made it — same for the back-press answer just below.
        let depth = state.actions.stack_depth();
        state.actions.controller.observe_depth(depth);

        if state.back.take_request(depth) {
            state.actions.open_disconnect_confirm(&state.connection);
        }

        // Tracked reads: a load, a mutation, or a connection change is what
        // wakes this rebuild.
        let sessions = state.actions.controller.sessions.get();
        let loaded = state.actions.controller.loaded.get();
        let info = state.connection.server_info.get();
        // Gates create/rename/kill (never attach) — see
        // `widgets::new_session_row`'s doc for the disabled-state pattern.
        let read_only = state.connection.is_read_only.get();

        let theme = widgets::active_theme();
        let colors = Palette::from_theme(&theme);

        // The heartbeat's pose for *this* frame — a wall-clock read kept
        // coming by the ticker mounted below while there is still something
        // to animate (see `super::heartbeat`).
        let beat = state.heartbeat.visual(
            info.as_ref().map_or(0, |info| info.latency_ms),
            theme.motion.reduce_motion,
        );

        // Responsive width-clamp pass: read metrics once, branch on one page
        // constant, never touch layout again (`examples/glyph-catalog/src/
        // pages/responsive.rs`'s reference idiom). `None` takes the same
        // branch as a genuinely narrow window.
        let metrics = use_context::<WindowMetrics>();
        let is_wide = metrics
            .map(|m| m.size.width >= WIDE_BREAKPOINT_PX)
            .unwrap_or(false);

        // Either the empty state, or every card plus the dashed create row.
        let mut list = Vec::new();
        if loaded && sessions.is_empty() {
            list.push(widgets::empty_view(read_only));
        } else {
            list.extend(
                sessions
                    .iter()
                    .map(|session| widgets::session_card(session, &colors)),
            );
            list.push(widgets::new_session_row(&colors, read_only));
        }

        // The shell's bar consumes the top window inset, so the body's safe
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
        let mut bands = vec![
            inflexible(widgets::server_strip(info.as_ref(), beat, &colors)),
            flexible(1, scroller),
        ];
        // Mounted only while the ping ring or latency roll still owes a
        // frame, at the end of the list so the two real bands keep their
        // positional identity as it comes and goes.
        if beat.animating() {
            bands.push(heartbeat::frame_ticker());
        }
        // Outside the clearance so it fills the whole surface, bar band
        // included — the bar slides in over it.
        any(widgets::page_tone(
            topbar_clearance(
                FlexView::new(Axis::Vertical, bands).cross_axis(CrossAxisAlignment::Stretch),
            ),
            &colors,
        ))
    }
}

/// The disconnect confirmation's body, naming the connected `host` — the
/// whole point of the sentence is *which* connection a back press is about
/// to end. The second sentence is the reassurance that stops "Disconnect"
/// reading like Settings' own "Forget server" (which does delete things).
fn disconnect_message(host: &str) -> String {
    format!(
        "You will be disconnected from {host}. Saved servers and credentials stay on this \
         device."
    )
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
    /// It has to be the sum since overlays are pages of the root host rather
    /// than of the route navigator: the route depth alone no longer moves
    /// when a dialog or the management sheet opens, so a pause marked
    /// against it would resume on the very next `build`.
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
            // The card was built from a list that no longer holds this
            // session (a poll landed between paint and tap). Nothing to
            // rename.
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

    /// Open the back press's disconnect confirmation — the one action this
    /// screen takes that no gesture on it can reach.
    ///
    /// Confirming does both halves the connection contract splits: the
    /// controller closes the connection and clears its signals, and the
    /// caller navigates — through [`Nav`], so the shell bar's mirror drops
    /// this route's entry on the same frame rather than a reconcile later.
    /// Every dismissal route does nothing at all: the polls come back
    /// through [`Component::build`]'s own depth observation.
    pub fn open_disconnect_confirm(&self, connection: &Arc<ConnectionController>) {
        self.pause_for_push();
        // Untracked: a one-shot read for a sentence, at the moment the
        // dialog opens — the same shape `open_rename_dialog` re-reads a name
        // with.
        let host = connection
            .server_info
            .get_untracked()
            .map(|info| widgets::host_of(&info.endpoint).to_string())
            .unwrap_or_else(|| DISCONNECT_UNKNOWN_HOST.to_string());
        let connection = Arc::clone(connection);
        let app_nav = self.app_nav.clone();
        confirm_dialog(
            &self.overlays,
            DISCONNECT_TITLE,
            disconnect_message(&host),
            DISCONNECT_CONFIRM_LABEL,
            move |_app: &mut AppState| {
                connection.disconnect();
                app_nav.pop();
            },
        );
    }

    /// Open the per-session management sheet — what a **long-press** on a
    /// card raises, and the only route to rename/kill now that the card
    /// carries no buttons. `read_only` disables the Rename/Kill rows, never
    /// Attach — see [`widgets::sheet_body`] for the sheet's own layout.
    pub fn open_manage_sheet(&self, id: &str, read_only: bool) {
        let Some(session) = self.controller.session(id) else {
            return;
        };
        self.pause_for_push();
        let actions = self.clone();
        let name = session.name;
        let id = id.to_string();
        show_sheet(&self.overlays, move |sheet| {
            widgets::sheet_body(&actions, sheet, &id, name.clone(), read_only)
        });
    }
}

// ---------------------------------------------------------------------------
// The screen-state shims the widgets' own callbacks call
// ---------------------------------------------------------------------------

/// Attach to `id` — a card tap.
pub(super) fn attach(state: &mut SessionsState, id: &str) {
    state.actions.attach(id);
}

/// Open the create-session dialog — the dashed row and the empty state.
pub(super) fn open_create_dialog(state: &mut SessionsState) {
    state.actions.open_create_dialog();
}

/// Open the management sheet for `id` — a card long-press. Reads the
/// connection's current read-only state (untracked: a one-shot decision at
/// the moment the sheet opens, like `open_rename_dialog`'s own re-read of
/// the current name).
pub(super) fn open_manage_sheet(state: &mut SessionsState, id: &str) {
    let read_only = state.connection.is_read_only.get_untracked();
    state.actions.open_manage_sheet(id, read_only);
}

/// Push the `/terminal` route for session `id` onto the app's page stack,
/// through the app's one guarded [`Nav`] handle. The one place this
/// location is spelled — both attach paths (a card tap and the management
/// sheet's Attach row) come through here — so the push runs the app's real
/// route table with its real session guard applied, same as a deep link.
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

    /// An observer over `signal`, mounted (as `init` mounts one) at whatever
    /// generation the cell already carries.
    fn observer(signal: &Rc<Cell<u64>>) -> BackConfirm {
        BackConfirm {
            signal: Rc::clone(signal),
            answered: signal.get(),
            confirming_at: None,
        }
    }

    /// One back press against `signal` — what the navigator does to a
    /// `DismissAnimated` page instead of popping it.
    fn back_press(signal: &Rc<Cell<u64>>) {
        signal.set(signal.get().wrapping_add(1));
    }

    #[test]
    fn one_back_press_raises_one_confirmation_however_often_build_reruns() {
        // The depths are the screen's own two-navigator sum: 3 while the route
        // is on top with nothing over it, 4 while the confirmation is up.
        const AT_REST: usize = 3;
        const CONFIRMING: usize = 4;

        let signal = Rc::new(Cell::new(0));
        let mut back = observer(&signal);
        assert!(!back.take_request(AT_REST), "nothing owed at rest");

        back_press(&signal);
        assert!(
            back.take_request(AT_REST),
            "the press raises a confirmation"
        );
        // `build` re-runs within the same frame once the queued push lands —
        // the pass that opens the dialog is never the last one.
        assert!(
            !back.take_request(CONFIRMING),
            "…exactly one, not one per rebuild"
        );

        // A press that somehow reaches this screen while the confirmation is
        // up is banked and swallowed rather than queueing a second dialog.
        back_press(&signal);
        assert!(!back.take_request(CONFIRMING));
        assert!(!back.take_request(CONFIRMING));

        // Dismissed (confirmed, cancelled, scrim-tapped — all the same shape):
        // the stack falls back to the mark, and the seam is armed again.
        assert!(
            !back.take_request(AT_REST),
            "the swallowed press stays spent"
        );
        back_press(&signal);
        assert!(
            back.take_request(AT_REST),
            "the next press raises a fresh confirmation"
        );
    }

    #[test]
    fn a_screen_that_mounted_over_a_used_cell_answers_only_its_own_presses() {
        // The seed cell a `sessions_page` mounted outside `Nav::push` resolves
        // may carry any generation; a mount must not read that as a press.
        let signal = Rc::new(Cell::new(7));
        let mut back = observer(&signal);
        assert!(
            !back.take_request(3),
            "the mount answers nothing on its own"
        );
        back_press(&signal);
        assert!(back.take_request(3));
    }

    #[test]
    fn the_disconnect_wording_names_the_host_and_promises_the_saved_rows_stay() {
        // The whole point of the sentence is *which* connection is ending, and
        // the reassurance that this is not Settings' "Forget server" (which
        // does delete things).
        let message = disconnect_message("100.71.31.57");
        assert!(message.contains("100.71.31.57"), "{message}");
        assert!(message.contains("Saved servers"), "{message}");
        assert!(
            DISCONNECT_TITLE.ends_with('?'),
            "it asks: {DISCONNECT_TITLE}"
        );
        assert!(
            disconnect_message(DISCONNECT_UNKNOWN_HOST).contains(DISCONNECT_UNKNOWN_HOST),
            "…and still reads as a sentence with no endpoint published"
        );
    }
}
