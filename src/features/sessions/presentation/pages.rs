//! The `/sessions` route: muxr's session list, plus every action a card or
//! its management sheet can take.
//!
//! [`sessions_page`] is what `crate::routes` mounts; [`SessionsScreen`]
//! follows the canonical screen shape (`docs/ARCHITECTURE.md`'s "Building a
//! Screen"). Observed state lives in the hosted [`SessionsController`];
//! app-scoped state (connection, read-only posture) comes from
//! [`crate::connection`]. Every push goes through [`Actions`], which pauses
//! the controller's mock poll first.
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
//! - **Size class**: [`Responsive`] is read once per `build`; a Tablet window
//!   gets the list column beside the detail pane (`widgets::master_detail`),
//!   where a card tap selects rather than attaches.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::{
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, Either, EitherOf4,
    FlexView, Get, GetUntracked, NavigatorController, Padding, RwSignal, Set, SizedBox, Stack,
    View, WindowMetrics, any, component, flexible, inflexible, on_cleanup, safe_area, scroll_view,
    spawn_local, use_context,
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use tokio::sync::{Notify, broadcast};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::{KeyringHandle, expect_keyring};
use crate::features::servers::di::{stage_settled, use_servers_controller};
use crate::features::servers::domain::entities::PageSnapshot;
use crate::features::servers::presentation::controller::ServersSwitcherController;
use crate::features::sessions::di::expect_sessions_repository;
use crate::features::sessions::domain::entities::{Session, SessionAgent, SessionBackend};
use crate::features::settings::di::{SharedPreferencesRepository, expect_preferences_repository};
use crate::net::wake::WakeReason;
use crate::responsive::Responsive;
use crate::routes::{
    Nav, TERMINAL_PANE_QUERY_KEY, TERMINAL_SESSION_QUERY_KEY, TERMINAL_SPACE_QUERY_KEY,
    TERMINAL_TAB_QUERY_KEY, app_nav, sessions_back_signal,
};
use crate::ui::dialogs::{confirm_dialog, name_input_dialog};
use crate::ui::palette::Palette;
use crate::ui::sheet::show_sheet;
use crate::ui::spacing;
use crate::ui::splash::loading::loading_sequence;
use crate::ui::toasts::app_toasts;

use super::controllers::{ConnectionTrouble, POLL_PERIOD_SECS, SessionsController, each_wake};
use super::heartbeat::{self, HeartbeatState};
use super::widgets::{self, CardMode, LoadPhase};

/// The route this screen attaches into. The session id rides as a query
/// param (`Location::parse` splits it off before matching, so the flat
/// `/terminal` route and `crate::routes`' own session-guard `loc.path` check
/// are both unaffected).
const TERMINAL_ROUTE: &str = "/terminal";
/// What the detail header's rename/kill say under a read-only credential.
const READ_ONLY_TOAST: &str =
    "Read-only session \u{2014} reconnect with a read-write token to change this";

const DISCONNECT_TITLE: &str = "Disconnect from server?";
const DISCONNECT_CONFIRM_LABEL: &str = "Disconnect";
/// Fallback subject when the connection has published no endpoint to name
/// (unreachable behind the session guard, but the sentence needs one).
const DISCONNECT_UNKNOWN_HOST: &str = "this server";

/// `.sessions-list{padding:14px 16px}`.
const LIST_PAD_X: f64 = spacing::XL;
const LIST_PAD_Y: f64 = spacing::LG;
/// `.sessions-list{gap:12px}` — between consecutive list children.
const LIST_GAP: f64 = spacing::BASE;

/// The scrolling list column's max width past
/// [`WIDE_BREAKPOINT_PX`](crate::responsive::WIDE_BREAKPOINT_PX) — a
/// tablet/landscape/desktop window centers the card list rather than
/// stretching it edge to edge.
///
/// The conn-row strip stays full-bleed on purpose (screen chrome, not a
/// list item); this is a plain scroller clamp, not a layout switch.
const SESSIONS_CLAMP_WIDTH: f64 = 640.0;

/// Fixes the loading rain's whole field, so a replay of the band's sequence
/// paints the identical drops frame for frame.
const LOADING_RAIN_SEED: u64 = 0x6D75_7872;

/// The list band's loading rain, on by default (see [`set_loading_rain_for_tests`]).
static LOADING_RAIN: AtomicBool = AtomicBool::new(true);

/// How often a deferred rain arm re-checks that the switcher stage has settled.
const RAIN_SETTLE_POLL: Duration = Duration::from_millis(50);

/// How many deferred rain arms have fired in this process (a test seam).
static RAIN_ARMS: AtomicUsize = AtomicUsize::new(0);

/// The number of deferred rain arms fired so far; tests compare deltas.
pub fn deferred_rain_arms_for_tests() -> usize {
    RAIN_ARMS.load(Ordering::SeqCst)
}

/// How many builds have carried the rain overlay in this process (a test seam).
static RAIN_BUILDS: AtomicUsize = AtomicUsize::new(0);

/// The number of builds that carried the rain overlay so far; tests compare deltas.
pub fn rain_overlay_builds_for_tests() -> usize {
    RAIN_BUILDS.load(Ordering::SeqCst)
}

/// Opts the process out of the loading rain for its remaining life — every
/// page-level suite taps under a pinned frame clock an overlay never advances.
/// Same process-static shape as [`crate::features::sessions::di::use_mock_sessions_for_tests`].
pub fn set_loading_rain_for_tests(enabled: bool) {
    LOADING_RAIN.store(enabled, Ordering::Relaxed);
}

/// The Sessions screen's route entry point: a `Component` under the outer
/// [`AppState`], so its `init` runs under a real reactive `Owner` that
/// disposes the hosted controller on unmount.
pub fn sessions_page(
    controller: NavigatorController<AppState>,
    overlays: NavigatorController<AppState>,
) -> impl View<AppState> {
    component(SessionsScreen {
        nav: controller,
        overlays,
    })
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
    /// The server switcher, when the shell provided one: the poll pauses while
    /// its stage is open (the stage covers this screen without moving a
    /// navigator, so the depth mark alone cannot see it).
    switcher: Option<Arc<ServersSwitcherController>>,
    /// The `.conn-row` strip's heartbeat clocks — written by the ping
    /// interval `init` starts, read by every rebuild that renders the strip
    /// (see [`super::heartbeat`]). Screen-internal, unlike the two handles
    /// above.
    pub(super) heartbeat: HeartbeatState,
    /// The back press's observer — see [`BackConfirm`]; screen-internal
    /// like the heartbeat.
    pub(super) back: BackConfirm,
    /// Whether the list band's loading rain is over — seeded `true` when the
    /// rain is off, and set once by the overlay itself when its sequence ends.
    pub(super) loading_done: RwSignal<bool>,
    /// The overlay's own picture, fixed the moment `build` first sees the
    /// release settled — a later change to the session set reaches only the
    /// live band (the read-only posture and colours stay live on both).
    loading_snapshot: Option<(Vec<Session>, bool)>,
    /// Whether the rain is enabled for this mount (read once in `init`), so a
    /// connection change can re-arm `loading_done` the way `init` seeded it.
    rain: bool,
    /// The `connection_epoch` this screen last reacted to; a change while
    /// connected restarts the controller (see `build`).
    seen_epoch: u64,
    /// Sticky: whether this mount has ever seen the connection up. Told apart
    /// from a first connect, which the load `init` already issued serves.
    has_connected: bool,
    /// The endpoint of the cached snapshot this mount was seeded with and has
    /// not yet seen land (a connection landing there refreshes in place).
    seeded: Option<String>,
    /// The latest [`ReconnectSeed`] generation this mount applied.
    seeded_gen: u64,
    /// The snapshot behind [`Self::seeded`]: what the header facts read while
    /// the connection is down.
    seeded_page: Option<Arc<PageSnapshot>>,
    /// Generation of the deferred rain arm: every restart bumps it, so a stale
    /// arm task finds itself superseded and does nothing.
    rain_generation: Arc<AtomicU64>,
    /// Cleared when the screen unmounts, ending any pending arm task.
    alive: Arc<AtomicBool>,
    /// Bumped when the close tail ends, so a build resumes the poll the tail
    /// held paused (nothing else rebuilds at the tail's end).
    tail_wake: RwSignal<u64>,
    /// Whether a task is already waiting out a close tail.
    tail_watch: Arc<AtomicBool>,
}

/// What a back press on `/sessions` means to this screen: the dismiss cell,
/// the generation already answered, and the depth mark of a still-open
/// confirmation.
///
/// Plain state, not signals: read and written from `Component::build`,
/// where a tracked write would re-dirty the very frame that made it (same
/// reason as [`SessionsController`]'s poll gate).
pub(super) struct BackConfirm {
    /// The generation counter the navigator bumps in place of popping this
    /// page — resolved once, in `init` (`crate::routes::sessions_back_signal`).
    signal: Rc<Cell<u64>>,
    /// Generation already answered; seeded from the cell (not zero) so a
    /// mount over an already-used cell answers only its own presses.
    answered: u64,
    /// Stack depth this screen's confirmation was raised at, or `None` while
    /// none is up — the same mark-and-resume arithmetic
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
    /// `depth` — asked once per `build`, the cell's only reader.
    ///
    /// Answers `true` at most once per bump: `build` reruns several times
    /// per frame (each queued op `RenderRoot::rebuild` flushes reruns it,
    /// including the push this answer makes), so the generation is banked
    /// *before* the answer and the next pass sees nothing owed.
    ///
    /// A bump while a confirmation is still up is banked and swallowed, not
    /// queued: with an overlay open, the root host claims the back press
    /// ahead of the route navigator (**R44-back**), so a second press
    /// dismisses the confirmation instead of reaching this screen.
    fn take_request(&mut self, depth: usize) -> bool {
        if let Some(mark) = self.confirming_at
            && depth <= mark
        {
            // Resolved (confirmed/cancelled/dismissed) — stack is back
            // where it opened.
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
/// Cloneable: a card's gesture reaches these through `&mut SessionsState`,
/// while a sheet row (a page on the app's navigator, which only sees `&mut
/// AppState`) captures a clone and calls the same method — one body, both
/// sides of the component boundary.
#[derive(Clone)]
pub struct Actions {
    /// The app's route navigator — held only for its depth (see
    /// [`Actions::stack_depth`]); pushes go through [`Nav`]/[`Self::overlays`],
    /// never this.
    pub nav: NavigatorController<AppState>,
    /// The root overlay host's navigator — every page builder this screen
    /// pushes (dialogs, management sheet) goes through it, landing above the
    /// shell's floating bar.
    pub overlays: NavigatorController<AppState>,
    /// The app's one path-pushing handle — how this screen reaches
    /// `/terminal` and `/settings` (see [`push_terminal`]).
    pub app_nav: Nav,
    /// The hosted view model (created in `init`, disposed on unmount).
    pub controller: Arc<SessionsController>,
    /// `connect`'s app-scoped keyring, which the back press's confirmed
    /// disconnect locks.
    pub keyring: KeyringHandle,
}

/// Resolves the live session-poll cadence — `settings`' `layout_refresh_secs`
/// pref — as a [`Duration`], through the one `preferences` handle
/// [`Component::init`](SessionsScreen::init) resolves (never a fresh
/// repository call). Falls back to [`POLL_PERIOD_SECS`] on a read failure.
/// The one seam both [`spawn_cadence_loop`] instances call before every
/// sleep; `tests/sessions.rs` drives it directly to pin the live-re-read
/// contract.
pub async fn resolve_poll_interval(preferences: &SharedPreferencesRepository) -> Duration {
    let secs = preferences
        .load_preferences()
        .await
        .map(|prefs| prefs.poll.layout_refresh_secs)
        .unwrap_or(POLL_PERIOD_SECS);
    Duration::from_secs(u64::from(secs))
}

/// Arms the loading rain once the switcher stage has settled, provided the
/// restarted first load has not settled by then and no later restart or unmount
/// superseded this `generation`. Handles are resolved here, not in the task.
fn spawn_deferred_rain(state: &SessionsState, generation: u64) {
    let current = Arc::clone(&state.rain_generation);
    let alive = Arc::clone(&state.alive);
    let switcher = state.switcher.clone();
    let first_load_settled = state.actions.controller.first_load_settled;
    let loading_done = state.loading_done;
    let fenced =
        move || !alive.load(Ordering::SeqCst) || current.load(Ordering::SeqCst) != generation;
    spawn_local(async move {
        loop {
            clean_signals::time::sleep(RAIN_SETTLE_POLL).await;
            if fenced() {
                return;
            }
            let open = switcher.as_ref().is_some_and(|s| s.open.get_untracked());
            if !open && stage_settled() {
                break;
            }
        }
        if first_load_settled.get_untracked() {
            return;
        }
        RAIN_ARMS.fetch_add(1, Ordering::SeqCst);
        loading_done.set(false);
    });
}

/// Waits out the stage's close tail, then wakes a rebuild so the poll resumes.
/// One task at a time; a reopen ends it (the next close spawns another).
fn spawn_tail_watch(state: &SessionsState) {
    if state.tail_watch.swap(true, Ordering::SeqCst) {
        return;
    }
    let watching = Arc::clone(&state.tail_watch);
    let alive = Arc::clone(&state.alive);
    let switcher = state.switcher.clone();
    let wake = state.tail_wake;
    spawn_local(async move {
        loop {
            clean_signals::time::sleep(RAIN_SETTLE_POLL).await;
            let reopened = switcher.as_ref().is_some_and(|s| s.open.get_untracked());
            if !alive.load(Ordering::SeqCst) || reopened {
                watching.store(false, Ordering::SeqCst);
                return;
            }
            if stage_settled() {
                watching.store(false, Ordering::SeqCst);
                wake.set(wake.get_untracked() + 1);
                return;
            }
        }
    });
}

/// Starts a self-rescheduling loop running `tick` forever, re-resolving
/// [`resolve_poll_interval`] before every sleep rather than baking a period
/// in at mount time — the live-pref read is why this hand-rolls the loop
/// instead of wrapping `clean_signals_frust::use_interval`'s fixed-`Duration`
/// shape. Must run where a reactive `Owner` is ambient (i.e. from
/// [`Component::init`](SessionsScreen::init)) — `on_cleanup` silently
/// no-ops outside one, so a call from elsewhere never stops.
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

/// One heartbeat round trip, shared by the cadence tick and a wake. A no-op
/// while the poll is covered, or with nothing connected to time; the
/// connection's own single-flight `ping` keeps overlapping beats from stacking.
fn beat(
    polling: &SessionsController,
    connection: &Arc<ConnectionController>,
    heartbeat: HeartbeatState,
) {
    if !polling.is_polling() {
        return;
    }
    // Untracked: a callback isn't a render. `None` means nothing connected to
    // time.
    let Some(from) = connection
        .server_info
        .get_untracked()
        .map(|info| info.latency_ms)
    else {
        return;
    };
    heartbeat.start_ping();
    let connection = Arc::clone(connection);
    spawn_local(async move {
        // A ping that measured nothing rolls nothing — the list poll owns
        // failure surfacing.
        if let Some(measured) = connection.ping().await {
            heartbeat.settle(from, measured);
        }
    });
}

/// How many channel rebuilds a failure streak has asked for in this process (a test seam).
static TROUBLE_RECONNECTS: AtomicUsize = AtomicUsize::new(0);

/// The number of channel rebuilds failure streaks asked for so far; tests compare deltas.
pub fn trouble_reconnects_for_tests() -> usize {
    TROUBLE_RECONNECTS.load(Ordering::SeqCst)
}

/// Reports the failure streak's latest edge: trouble rebuilds the channel
/// (single-flight, the session kept) and goes under the servers controller's
/// banner; recovery flashes it away. Never tears the connection down. Without a
/// servers controller (no shell) only the channel rebuild runs.
fn report_streak(
    controller: &SessionsController,
    connection: &Arc<ConnectionController>,
    servers: Option<&Arc<ServersSwitcherController>>,
) {
    let Some(edge) = controller.take_trouble() else {
        return;
    };
    if edge == ConnectionTrouble::Recovered {
        if let Some(servers) = servers {
            servers.report_recovered();
        }
        return;
    }
    TROUBLE_RECONNECTS.fetch_add(1, Ordering::SeqCst);
    let rebuilding = Arc::clone(connection);
    spawn_local(async move {
        if let Err(failure) = rebuilding.reconnect().await {
            log::debug!("sessions: reconnect on a failure streak failed: {failure:?}");
        }
    });
    if let Some(servers) = servers {
        servers.report_trouble(edge == ConnectionTrouble::Persistent);
    }
}

/// Runs a wake-driven refresh and heartbeat ping for every wake on `wakes`
/// while the screen is mounted, and ends the loop at unmount. `wakes` is
/// injected: production passes [`crate::net::wake::subscribe`], tests a
/// receiver from a local bus. The refresh and the ping go through the same
/// single-flight paths the poll uses, so a wake never starts a second one in
/// flight.
///
/// Every wake, a [`WakeReason::NetworkChanged`] one and a
/// [`WakeReason::Foregrounded`] one alike, first rebuilds the shared client's
/// channel (single-flight with the terminal feed's own), so the refresh and
/// the ping do not ride a half-open one (a suspended app's channel is dead on
/// return) until keepalive notices; a failed rebuild is logged and the refresh
/// still runs, whose own error handling then applies. A wake while the poll is
/// covered does nothing more. With nothing connected a wake first redials a
/// failed background dial's pending reconnect (the servers controller's
/// `retry_pending`), covered or not.
///
/// Must run where a reactive `Owner` is ambient, as `on_cleanup` no-ops
/// otherwise.
fn spawn_wake_loop(
    wakes: broadcast::Receiver<WakeReason>,
    controller: Arc<SessionsController>,
    connection: Arc<ConnectionController>,
    heartbeat: HeartbeatState,
) {
    let servers = use_servers_controller();
    let cancel = Arc::new(Notify::new());
    let stop = Arc::clone(&cancel);
    on_cleanup(move || stop.notify_one());

    spawn_local(async move {
        each_wake(wakes, &cancel, move |_reason| {
            if !connection.connected.get_untracked()
                && let Some(servers) = &servers
            {
                servers.end_trouble();
                servers.retry_pending();
            }
            if !controller.is_polling() {
                return;
            }
            let controller = Arc::clone(&controller);
            let connection = Arc::clone(&connection);
            let servers = servers.clone();
            spawn_local(async move {
                if let Err(failure) = connection.reconnect().await {
                    log::debug!("sessions: reconnect on a wake failed: {failure:?}");
                }
                let refresh = Arc::clone(&controller);
                let reporting = Arc::clone(&connection);
                spawn_local(async move {
                    refresh.refresh_on_wake().await;
                    report_streak(&refresh, &reporting, servers.as_ref());
                });
                beat(&controller, &connection, heartbeat);
            });
        })
        .await;
    });
}

impl Component for SessionsScreen {
    type State = SessionsState;

    fn init(&self) -> SessionsState {
        // Auto-disposed on unmount via `on_cleanup`.
        let controller = use_controller::<SessionsController, AppFailure>(|| {
            SessionsController::new(expect_sessions_repository())
        });

        // UI-thread `app_toasts` is right here since every controller call
        // is `spawn_local`ed onto the same queue.
        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.refresh_background().await });

        // Resolved synchronously in `init`, never inside a spawned future's
        // body: a future's first poll has no ambient owner, so a
        // `use_context` lookup there would panic. Only the resolved `Arc`
        // crosses into the loops below.
        let preferences = expect_preferences_repository();

        let servers = use_servers_controller();
        let connection = expect_connection_controller();
        // A pending reconnect or trouble report is this screen's: it ends with it.
        if let Some(servers) = servers.clone() {
            on_cleanup(move || servers.sessions_left());
        }

        // Session-list poll: re-resolves `layout_refresh_secs` live, not
        // just at mount.
        let polling = Arc::clone(&controller);
        let polled = Arc::clone(&connection);
        spawn_cadence_loop(Arc::clone(&preferences), move || {
            if polling.take_poll_tick() {
                let handle = Arc::clone(&polling);
                let connection = Arc::clone(&polled);
                let servers = servers.clone();
                spawn_local(async move {
                    handle.refresh_from_poll().await;
                    report_streak(&handle, &connection, servers.as_ref());
                });
            } else if !polled.connected.get_untracked()
                && let Some(servers) = &servers
            {
                servers.end_trouble();
            }
        });

        // Connection heartbeat: one real timed `GetVersion`, same cadence as
        // the list poll (the roll `HeartbeatState::settle` animates over is
        // a separate display concern). `is_polling()` is the poll's own
        // covered-screen gate.
        let heartbeat = HeartbeatState::new();
        let beating = Arc::clone(&controller);
        let pinged = Arc::clone(&connection);
        spawn_cadence_loop(preferences, move || {
            beat(&beating, &pinged, heartbeat);
        });

        // A network change or a return to the foreground refreshes the list
        // and re-times the connection now, not at the next tick.
        spawn_wake_loop(
            crate::net::wake::subscribe(),
            Arc::clone(&controller),
            Arc::clone(&connection),
            heartbeat,
        );

        // Read once per mount — a process-wide test opt-out, not observed
        // state, so never from `build` or a spawned block.
        let rain = LOADING_RAIN.load(Ordering::Relaxed);
        let connection_epoch = connection.connection_epoch.get_untracked();
        let seen_connected = connection.connected.get_untracked();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_flag = Arc::clone(&alive);
        on_cleanup(move || alive_flag.store(false, Ordering::SeqCst));

        SessionsState {
            actions: Actions {
                nav: self.nav.clone(),
                overlays: self.overlays.clone(),
                app_nav: app_nav(),
                controller,
                keyring: expect_keyring(),
            },
            connection,
            switcher: use_servers_controller(),
            heartbeat,
            back: BackConfirm::new(),
            loading_done: RwSignal::new(!rain),
            loading_snapshot: None,
            rain,
            seeded: None,
            seeded_gen: use_servers_controller()
                .and_then(|switcher| switcher.seed.get_untracked())
                .map_or(0, |seed| seed.generation),
            seeded_page: None,
            seen_epoch: connection_epoch,
            has_connected: seen_connected,
            rain_generation: Arc::new(AtomicU64::new(0)),
            alive,
            tail_wake: RwSignal::new(0),
            tail_watch: Arc::new(AtomicBool::new(false)),
        }
    }

    fn build(&self, state: &mut SessionsState) -> impl View<SessionsState> {
        // Plain atomic, not a signal — see `BackConfirm`'s doc for why.
        let depth = state.actions.stack_depth();
        state.actions.controller.observe_depth(depth);
        // Tracked, so a close wakes this build and the observe above resumes
        // the poll; while open, re-mark the pause at the current depth right
        // after that observe (nothing polls between the two, same thread).
        // The close tail keeps the pause too: the live page must not change
        // while the neighbours still paint (`stage_settled`).
        state.tail_wake.get();
        if let Some(switcher) = state.switcher.clone() {
            if switcher.open.get() {
                state.actions.pause_for_push();
            } else if !stage_settled() {
                state.actions.pause_for_push();
                spawn_tail_watch(state);
            }
        }

        if state.back.take_request(depth) {
            // The route navigator outranks the switcher's `SwitcherBack`
            // fallback, so a press with the stage up arrives here as this
            // page's dismiss request: it closes the stage, not the session.
            match state.switcher.as_ref().filter(|s| s.open.get_untracked()) {
                Some(switcher) => switcher.close(),
                None => state.actions.open_disconnect_confirm(&state.connection),
            }
        }

        // A cached switch committed: show its snapshot from this very build on
        // (one write, never an empty list) and stay plain, no rain. The
        // connection landing there refreshes in place, below.
        if let Some(seed) = state.switcher.as_ref().and_then(|s| s.seed.get())
            && seed.generation != state.seeded_gen
        {
            state.seeded_gen = seed.generation;
            state.seeded = Some(seed.endpoint.clone());
            state.seeded_page = Some(Arc::clone(&seed.page));
            state.loading_snapshot = None;
            state.rain_generation.fetch_add(1, Ordering::SeqCst);
            state.loading_done.set(true);
            state.actions.controller.seed_from(&seed.page);
        }

        // A new connection (the epoch moved while connected) starts this mount
        // over: the old server's list, selection and rain snapshot go, and the
        // rain replays over the new first load. A move to disconnected is the
        // guard/back flow's business. Untracked: `server_info`, set in the same
        // step, is what wakes this rebuild.
        let epoch = state.connection.connection_epoch.get_untracked();
        let connected = state.connection.connected.get_untracked();
        if epoch != state.seen_epoch {
            // A switch is a screen that has ever seen the connection up (across
            // frames too: a teardown frame, then a dial frame) seeing a new epoch, or a teardown
            // and re-dial inside one frame (two or more moves). A lone move
            // from disconnected is the first connect, which the load `init`
            // issued already serves.
            let switched = state.has_connected || epoch - state.seen_epoch >= 2;
            state.seen_epoch = epoch;
            let landed = state
                .connection
                .server_info
                .get_untracked()
                .map(|info| info.endpoint);
            if connected && switched && landed.is_some() && state.seeded == landed {
                // The endpoint this mount was seeded for: refresh in place.
                state.seeded = None;
                state.seeded_page = None;
                state.actions.controller.refresh_in_place();
            } else if connected && switched {
                state.seeded = None;
                state.seeded_page = None;
                state.loading_snapshot = None;
                let generation = state.rain_generation.fetch_add(1, Ordering::SeqCst) + 1;
                if state.rain && !stage_settled() {
                    // The switcher stage is still animating: the rain's
                    // offscreen layers must not nest inside it. Stay plain and
                    // arm the rain once the stage settles.
                    state.loading_done.set(true);
                    state.actions.controller.restart();
                    spawn_deferred_rain(state, generation);
                } else {
                    state.loading_done.set(!state.rain);
                    state.actions.controller.restart();
                }
            }
        }
        state.has_connected |= connected;

        // Tracked reads: a load, a mutation, or a connection change is what
        // wakes this rebuild.
        let sessions = state.actions.controller.sessions.get();
        let loaded = state.actions.controller.loaded.get();
        let settled = state.actions.controller.first_load_settled.get();
        let loading_done = state.loading_done.get();
        let live_info = state.connection.server_info.get();
        let live_read_only = state.connection.is_read_only.get();
        // A seeded page with no connection behind it keeps showing the
        // snapshot's header facts through the disconnect and the dial, and
        // every action that needs the server is off.
        let offline = state.seeded.is_some() && !state.connection.connected.get_untracked();
        state.actions.controller.set_offline(offline);
        let seeded_page = state.seeded_page.clone().filter(|_| offline);
        let info = match &seeded_page {
            Some(page) if live_info.is_none() => page.info.clone(),
            _ => live_info,
        };
        let strip_read_only = seeded_page.as_ref().map_or(live_read_only, |p| p.read_only);
        // Gates create/rename/kill (never attach) — see
        // `widgets::new_session_row`'s doc for the disabled-state pattern.
        let read_only = live_read_only || offline;

        let theme = widgets::active_theme();
        let colors = Palette::from_theme(&theme);

        // This frame's heartbeat pose — kept fresh by the ticker mounted
        // below while there's still something to animate.
        let beat = state.heartbeat.visual(
            info.as_ref().map_or(0, |info| info.latency_ms),
            theme.motion.reduce_motion,
        );

        // The size-class rule, read once; `None` behaves like a phone.
        let rule = Responsive::of(use_context::<WindowMetrics>());
        let selected = state.actions.controller.selected.get();
        let layout = if rule.is_tablet() {
            ListLayout::Tablet {
                full: rule.is_full(),
                selected: selected.clone(),
            }
        } else {
            ListLayout::Compact {
                wide: rule.wide_clamp,
            }
        };
        // Only the Tablet shows the detail, so only it pays for the agent reads
        // the controller makes on the poll.
        state
            .actions
            .controller
            .set_detail_visible(rule.is_tablet());
        let agents = state.actions.controller.agents.get();
        let tone = if rule.is_tablet() {
            colors.surface
        } else {
            colors.base
        };

        // Both renderings assemble the same pieces through the same band; the
        // live one adds `scroll_view`, a pass-through at offset 0.
        let live_scroller = list_view(
            list_pieces(&sessions, loaded, settled, read_only, &colors, &layout),
            &layout,
            true,
        );
        // Always a `Stack`, overlay or not: an erased child type that flips
        // Some→None tears the pod under it down (`shell::root_view`'s rule).
        let mut children = vec![any(live_scroller)]; // erasure: keep accumulator into Vec<AnyView>
        if !loading_done {
            RAIN_BUILDS.fetch_add(1, Ordering::SeqCst);
            let (overlay_pieces, stale) = if settled {
                let frozen = state
                    .loading_snapshot
                    .get_or_insert_with(|| (sessions.clone(), loaded));
                // The id ladder is the picture: a card that gained a pane or a
                // preview between two polls is the same card in the same slot,
                // and telemetry churn must not hold input on the overlay.
                let frozen_ids = frozen.0.iter().map(|session| &session.id);
                let live_ids = sessions.iter().map(|session| &session.id);
                let stale = frozen.1 != loaded || frozen_ids.ne(live_ids);
                (
                    list_pieces(&frozen.0, frozen.1, settled, read_only, &colors, &layout),
                    stale,
                )
            } else {
                (
                    list_pieces(&sessions, loaded, settled, read_only, &colors, &layout),
                    false,
                )
            };
            let overlay_layout = layout.clone();
            children.push(loading_sequence(
                LOADING_RAIN_SEED,
                tone,
                overlay_pieces,
                move |pieces| list_view(pieces, &overlay_layout, false),
                settled,
                stale,
                state.loading_done,
            ));
        }
        let strip = widgets::server_strip(info.as_ref(), beat, strip_read_only, &colors);
        let list = Stack(children);
        let mut bands = match layout {
            ListLayout::Compact { .. } => vec![inflexible(strip), flexible(1, list)],
            ListLayout::Tablet { full, .. } => {
                let footer = settled.then(|| widgets::new_session_row(&colors, read_only));
                let detail = widgets::detail_pane(
                    &sessions,
                    &agents,
                    selected.as_deref(),
                    LoadPhase::of(loaded, settled),
                    read_only,
                    full,
                    &colors,
                );
                let body = widgets::master_detail(strip, list, footer, detail, full, &colors);
                vec![flexible(1, body)]
            }
        };
        // Mounted only while ring/roll still owe a frame; kept at the
        // list's end so the two real bands keep positional identity.
        if beat.animating() {
            bands.push(heartbeat::frame_ticker());
        }
        // The shell scaffold's app-bar slot already reserves the bar's own
        // band above this screen's body — no clearance to add here (a
        // one-time double-inset bug: the scaffold owns the band, this page
        // used to as well).
        widgets::page_tone(
            FlexView::new(Axis::Vertical, bands).cross_axis(CrossAxisAlignment::Stretch),
            &colors,
        )
    }
}

/// The disconnect confirmation's body, naming the connected `host` — the
/// point of the sentence is *which* connection is ending. The second
/// sentence stops "Disconnect" reading like Settings' "Forget server"
/// (which does delete things).
fn disconnect_message(host: &str) -> String {
    format!(
        "You will be disconnected from {host}. Saved servers and credentials stay on this \
         device."
    )
}

/// Which list this frame lays out: Compact's clamped band or the Tablet column.
#[derive(Clone)]
enum ListLayout {
    Compact {
        wide: bool,
    },
    Tablet {
        full: bool,
        selected: Option<String>,
    },
}

/// The list's children in stagger order: the empty state, or a card per
/// session closed by the dashed create row — closed by [`widgets::loading_view`]
/// instead until the first load settles, failure included (before it, no cards).
/// The Tablet column's own pieces come from [`widgets::tablet_list_pieces`].
fn list_pieces(
    sessions: &[Session],
    loaded: bool,
    settled: bool,
    read_only: bool,
    colors: &Palette,
    layout: &ListLayout,
) -> Vec<AnyView<SessionsState>> // erasure: keep accumulator into Vec<AnyView>
{
    if let ListLayout::Tablet { full, selected } = layout {
        let phase = LoadPhase::of(loaded, settled);
        let selected = selected.as_deref();
        return widgets::tablet_list_pieces(sessions, phase, read_only, selected, *full, colors);
    }
    if loaded && sessions.is_empty() {
        return vec![any(widgets::empty_view(read_only))];
    }
    let mut pieces: Vec<AnyView<SessionsState>> = sessions
        .iter()
        .map(|session| any(widgets::session_card(session, colors, CardMode::Compact)))
        .collect();
    pieces.push(if settled {
        any(widgets::new_session_row(colors, read_only))
    } else {
        any(widgets::loading_view(colors))
    });
    pieces
}

/// `pieces` laid out for `layout`, scrolling when `scroll` (the live band; the
/// loading overlay's picture does not scroll).
fn list_view(
    pieces: Vec<AnyView<SessionsState>>,
    layout: &ListLayout,
    scroll: bool,
) -> impl View<SessionsState> + use<> {
    match (layout, scroll) {
        (ListLayout::Compact { wide }, true) => {
            EitherOf4::A(list_band(scroll_view(padded_list(pieces)), *wide))
        }
        (ListLayout::Compact { wide }, false) => {
            EitherOf4::B(list_band(padded_list(pieces), *wide))
        }
        (ListLayout::Tablet { .. }, true) => {
            EitherOf4::C(scroll_view(widgets::tablet_list(pieces)))
        }
        (ListLayout::Tablet { .. }, false) => EitherOf4::D(widgets::tablet_list(pieces)),
    }
}

/// `pieces` as the screen's padded, [`LIST_GAP`]-spaced, stretched column.
fn padded_list(pieces: Vec<AnyView<SessionsState>>) -> impl View<SessionsState> {
    Padding(
        EdgeInsets::symmetric(LIST_PAD_X, LIST_PAD_Y),
        FlexView::new(Axis::Vertical, pieces.into_iter().map(inflexible).collect())
            .gap(LIST_GAP)
            .cross_axis(CrossAxisAlignment::Stretch),
    )
}

/// The band `content` occupies: safe-area inset with its top edge off (the
/// shell's bar already consumed that window inset), clamped and centered past
/// [`SESSIONS_CLAMP_WIDTH`] while the strip beside it stays full-bleed.
fn list_band(content: impl View<SessionsState>, is_wide: bool) -> impl View<SessionsState> {
    let band = safe_area(content).top(false);
    if is_wide {
        Either::Left(Align(
            Alignment::CENTER,
            SizedBox::<SessionsState>(Some(SESSIONS_CLAMP_WIDTH), None).child(band),
        ))
    } else {
        Either::Right(band)
    }
}

impl Actions {
    /// How deep the app's page stack is, counting both navigators — the
    /// number the mock poll's pause/resume pair keys off. It's a sum
    /// because overlays are pages of the root host, not the route
    /// navigator: route depth alone stops moving when a dialog/sheet opens,
    /// so a pause marked against it would resume on the very next `build`.
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
        if self.controller.is_offline() {
            return;
        }
        self.pause_for_push();
        self.app_nav.push(&terminal_location(id, None));
    }

    /// Attach to `id` carrying a Tablet pane pill's `(tab, pane id)` target.
    /// The terminal honours that target once Phase 4's attach-into-pane card
    /// lands; until then it attaches at the session's server focus.
    pub fn attach_pane(&self, id: &str, tab: u32, pane: u32) {
        if self.controller.is_offline() {
            return;
        }
        self.pause_for_push();
        self.app_nav.push(&terminal_location(id, Some((tab, pane))));
    }

    /// Attach to `id` aimed at an agent row's pane: the same `(tab, pane)`
    /// target as a pill, plus the agent's space when it has one. An agent the
    /// layout could not place has no target, so nothing happens.
    pub fn attach_agent(&self, id: &str, agent: &SessionAgent) {
        let Some(location) = terminal_location_for_agent(id, agent) else {
            return;
        };
        if self.controller.is_offline() {
            return;
        }
        self.pause_for_push();
        self.app_nav.push(&location);
    }

    /// Open the create-session dialog (the dashed row and the empty state's
    /// action both land here).
    pub fn open_create_dialog(&self, connection: &Arc<ConnectionController>) {
        if self.controller.is_offline() {
            return;
        }
        self.pause_for_push();
        let controller = Arc::clone(&self.controller);
        let available = connection
            .server_info
            .get_untracked()
            .map(|info| info.available_backends)
            .unwrap_or_default();
        widgets::open_new_session_dialog(
            &self.overlays,
            &available,
            move |_app: &mut AppState, name: String, backend: SessionBackend| {
                let handle = Arc::clone(&controller);
                spawn_local(async move { handle.create(name, backend).await });
            },
        );
    }

    /// Open the rename dialog for `id`, seeded with that session's *current*
    /// name — re-read from the controller rather than captured, so a sheet
    /// opened several frames ago can't seed a stale name.
    pub fn open_rename_dialog(&self, id: &str) {
        if self.controller.is_offline() {
            return;
        }
        let Some(session) = self.controller.session(id) else {
            // List moved since paint (a poll landed between paint and tap)
            // — nothing to rename.
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
        if self.controller.is_offline() {
            return;
        }
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
    /// controller locks the keyring, closes the connection and clears its
    /// signals (`lock_and_disconnect`, the same leave as Settings' Lock &
    /// disconnect), and the caller navigates — through [`Nav`], so the shell
    /// bar's mirror drops this route's entry on the same frame rather than a
    /// reconcile later.
    /// Every dismissal route does nothing at all: the polls come back
    /// through [`Component::build`]'s own depth observation.
    pub fn open_disconnect_confirm(&self, connection: &Arc<ConnectionController>) {
        self.pause_for_push();
        // Untracked one-shot read for the sentence, same shape as
        // `open_rename_dialog`'s name re-read.
        let host = connection
            .server_info
            .get_untracked()
            .map(|info| widgets::host_of(&info.endpoint).to_string())
            .unwrap_or_else(|| DISCONNECT_UNKNOWN_HOST.to_string());
        let connection = Arc::clone(connection);
        let keyring = self.keyring.clone();
        let app_nav = self.app_nav.clone();
        confirm_dialog(
            &self.overlays,
            DISCONNECT_TITLE,
            disconnect_message(&host),
            DISCONNECT_CONFIRM_LABEL,
            move |_app: &mut AppState| {
                connection.lock_and_disconnect(&keyring);
                app_nav.pop();
            },
        );
    }

    /// Open the per-session management sheet — what a **long-press** on a
    /// card raises, and the only route to rename/kill now that the card
    /// carries no buttons. `read_only` disables the Rename/Kill rows, never
    /// Attach — see [`widgets::sheet_body`] for the sheet's own layout.
    pub fn open_manage_sheet(&self, id: &str, read_only: bool) {
        if self.controller.is_offline() {
            return;
        }
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

pub(super) fn attach(state: &mut SessionsState, id: &str) {
    state.actions.attach(id);
}

/// A Tablet card tap: select `id` for the detail pane.
pub(super) fn select(state: &mut SessionsState, id: &str) {
    state.actions.controller.select_and_refresh_agents(id);
}

pub(super) fn attach_pane(state: &mut SessionsState, id: &str, tab: u32, pane: u32) {
    state.actions.attach_pane(id, tab, pane);
}

/// An agent row's tap: attach into its space, tab and pane.
pub(super) fn attach_agent(state: &mut SessionsState, id: &str, agent: &SessionAgent) {
    state.actions.attach_agent(id, agent);
}

/// The detail header's rename, refused with [`READ_ONLY_TOAST`] when read-only.
pub(super) fn open_rename(state: &mut SessionsState, id: &str) {
    if !refused_read_only(state) {
        state.actions.open_rename_dialog(id);
    }
}

/// The detail header's kill, refused like [`open_rename`].
pub(super) fn open_kill(state: &mut SessionsState, id: &str) {
    if !refused_read_only(state) {
        state.actions.open_kill_dialog(id);
    }
}

fn refused_read_only(state: &SessionsState) -> bool {
    let read_only = state.connection.is_read_only.get_untracked();
    if read_only {
        app_toasts().show(READ_ONLY_TOAST);
    }
    read_only
}

/// The failed first load's Retry.
pub(super) fn retry_first_load(state: &mut SessionsState) {
    let controller = Arc::clone(&state.actions.controller);
    spawn_local(async move { controller.retry_first_load().await });
}

pub(super) fn open_create_dialog(state: &mut SessionsState) {
    state.actions.open_create_dialog(&state.connection);
}

/// Open the management sheet for `id` — a card long-press. Reads the
/// connection's read-only state untracked (a one-shot decision at open
/// time, like `open_rename_dialog`'s name re-read).
pub(super) fn open_manage_sheet(state: &mut SessionsState, id: &str) {
    let read_only = state.connection.is_read_only.get_untracked();
    state.actions.open_manage_sheet(id, read_only);
}

/// The `/terminal` location for `id`, optionally aimed at one
/// `(tab index, pane id)` — the one place it is spelled; pushed through the
/// app's one guarded [`Nav`], so it runs the real route table and session
/// guard like a deep link. The session id is percent-encoded.
fn terminal_location(id: &str, pane: Option<(u32, u32)>) -> String {
    let id = utf8_percent_encode(id, NON_ALPHANUMERIC);
    let mut location = format!("{TERMINAL_ROUTE}?{TERMINAL_SESSION_QUERY_KEY}={id}");
    if let Some((tab, pane)) = pane {
        location.push_str(&format!(
            "&{TERMINAL_TAB_QUERY_KEY}={tab}&{TERMINAL_PANE_QUERY_KEY}={pane}"
        ));
    }
    location
}

/// [`terminal_location`] for an agent row: its tab position and pane id, and
/// `space` (percent-encoded) when the agent reports a space. `None` for an
/// agent with no tab position.
fn terminal_location_for_agent(id: &str, agent: &SessionAgent) -> Option<String> {
    let tab = agent.tab_position?;
    let mut location = terminal_location(id, Some((tab, agent.pane_id)));
    if let Some(space) = &agent.space_id {
        let space = utf8_percent_encode(space, NON_ALPHANUMERIC);
        location.push_str(&format!("&{TERMINAL_SPACE_QUERY_KEY}={space}"));
    }
    Some(location)
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::Set;

    #[test]
    fn the_attach_location_carries_the_session_id_as_a_query_param() {
        // Tripwire: the path stays flat `/terminal` (route table + session
        // guard match on it); the id is only in the query.
        let raw = format!("{TERMINAL_ROUTE}?{TERMINAL_SESSION_QUERY_KEY}=s-1");
        let loc = frust::Location::parse(&raw);

        assert_eq!(loc.path, TERMINAL_ROUTE);
        assert_eq!(
            loc.query
                .get(TERMINAL_SESSION_QUERY_KEY)
                .map(String::as_str),
            Some("s-1")
        );
    }

    #[test]
    fn a_pane_location_adds_the_tab_index_and_the_pane_id_and_encodes_the_session() {
        let raw = terminal_location("zellij:a&b=c d", Some((2, 7)));
        assert!(raw.contains("session=zellij%3Aa%26b%3Dc%20d"), "{raw}");
        let loc = frust::Location::parse(&raw);

        assert_eq!(loc.path, TERMINAL_ROUTE);
        let query = |key: &str| loc.query.get(key).map(String::as_str);
        assert_eq!(query(TERMINAL_SESSION_QUERY_KEY), Some("zellij:a&b=c d"));
        assert_eq!(query(TERMINAL_TAB_QUERY_KEY), Some("2"));
        assert_eq!(query(TERMINAL_PANE_QUERY_KEY), Some("7"));
        assert_eq!(terminal_location("s-1", None), "/terminal?session=s%2D1");
    }

    #[test]
    fn an_agent_location_adds_the_encoded_space_only_when_the_agent_has_one() {
        let mut agent = SessionAgent {
            pane_id: 7,
            tab_position: Some(2),
            space_id: Some("sp a&b".to_string()),
            space_name: None,
            tab_name: "t".to_string(),
            kind: "claude".to_string(),
            name: None,
            display_kind: None,
            status: crate::agents::AgentStatus::Idle,
            title: None,
            summary: None,
            change_seq: 1,
        };
        let loc = frust::Location::parse(&terminal_location_for_agent("herdr:h", &agent).unwrap());
        let query = |key: &str| loc.query.get(key).map(String::as_str);
        assert_eq!(query(TERMINAL_SPACE_QUERY_KEY), Some("sp a&b"));
        assert_eq!(query(TERMINAL_TAB_QUERY_KEY), Some("2"));
        assert_eq!(query(TERMINAL_PANE_QUERY_KEY), Some("7"));

        agent.space_id = None;
        let raw = terminal_location_for_agent("herdr:h", &agent).unwrap();
        assert!(!raw.contains(TERMINAL_SPACE_QUERY_KEY), "{raw}");

        agent.tab_position = None;
        assert_eq!(terminal_location_for_agent("herdr:h", &agent), None);
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
        // Two-navigator sum: 3 at rest (route on top), 4 while confirming.
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
        // `build` reruns within the frame once the queued push lands.
        assert!(
            !back.take_request(CONFIRMING),
            "…exactly one, not one per rebuild"
        );

        // A press reaching this screen while confirming is banked, not
        // queued.
        back_press(&signal);
        assert!(!back.take_request(CONFIRMING));
        assert!(!back.take_request(CONFIRMING));

        // Dismissed (any shape): stack falls back to the mark, seam
        // re-armed.
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
        // A freshly mounted screen must not read a pre-existing generation
        // as a press.
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

    /// What the wake loop asked of its collaborators, in order.
    type Events = Arc<std::sync::Mutex<Vec<&'static str>>>;

    fn note(events: &Events, what: &'static str) {
        events.lock().expect("events").push(what);
    }

    fn noted(events: &Events) -> Vec<&'static str> {
        events.lock().expect("events").clone()
    }

    /// A connection backend that records the reconnects asked of it and every
    /// `client()` lookup (the first thing `ping` does, and `None` ends it).
    struct RecordingBackend(Events);

    impl crate::connection::ConnectionBackend for RecordingBackend {
        fn begin_connect(
            &self,
            _: crate::connection::ConnectTarget,
        ) -> crate::connection::ConnectRun {
            unreachable!("the wake loop never connects")
        }

        fn disconnect(&self) {}

        fn client(&self) -> Option<crate::net::MuxrClient> {
            note(&self.0, "ping");
            None
        }

        fn reconnect(&self) -> crate::connection::ReconnectFuture {
            note(&self.0, "reconnect");
            Box::pin(async { Ok(()) })
        }
    }

    /// The seeded mock repository, recording each `ListSessions`.
    struct RecordingRepository {
        events: Events,
        inner: crate::features::sessions::data::repositories::MockSessionsRepository,
    }

    #[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
    #[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
    impl crate::features::sessions::domain::repositories::SessionsRepository for RecordingRepository {
        async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
            note(&self.events, "refresh");
            self.inner.list_sessions().await
        }

        async fn create_session(
            &self,
            params: crate::features::sessions::domain::entities::CreateSessionParams,
        ) -> Result<Session, AppFailure> {
            self.inner.create_session(params).await
        }

        async fn rename_session(
            &self,
            request: crate::features::sessions::domain::entities::RenameRequest,
        ) -> Result<Session, AppFailure> {
            self.inner.rename_session(request).await
        }

        async fn kill_session(&self, id: String) -> Result<(), AppFailure> {
            self.inner.kill_session(id).await
        }
    }

    struct WakeRig {
        runtime: &'static frust_reactive::ReactiveRuntime,
        owner: Option<frust_reactive::Owner>,
        bus: crate::net::wake::WakeBus,
        events: Events,
        controller: Arc<SessionsController>,
    }

    impl WakeRig {
        /// A loaded sessions controller and a connected-looking connection,
        /// with `spawn_wake_loop` running under its own owner. The recorded
        /// events are cleared once the first load has settled.
        fn mount() -> Self {
            let runtime = frust_reactive::ReactiveRuntime::init(Arc::new(|| {}));
            let events: Events = Events::default();
            let controller = Arc::new(SessionsController::new(Arc::new(RecordingRepository {
                events: Arc::clone(&events),
                inner:
                    crate::features::sessions::data::repositories::MockSessionsRepository::seeded(),
            })));
            let connection = Arc::new(ConnectionController::with_backend(Arc::new(
                RecordingBackend(Arc::clone(&events)),
            )));
            connection
                .server_info
                .set(Some(crate::connection::ServerInfo {
                    endpoint: "host:1".to_string(),
                    trust_mode: crate::connection::TrustMode::System,
                    tls_label: String::new(),
                    version: String::new(),
                    latency_ms: 10,
                    backend_versions: Vec::new(),
                    available_backends: Vec::new(),
                    capabilities: Default::default(),
                    is_read_only: false,
                }));
            let bus = crate::net::wake::WakeBus::new();
            let owner = frust_reactive::Owner::new();
            owner.with(|| {
                spawn_wake_loop(
                    bus.subscribe(),
                    Arc::clone(&controller),
                    connection,
                    HeartbeatState::new(),
                )
            });
            let first = Arc::clone(&controller);
            spawn_local(async move { first.refresh().await });
            let rig = Self {
                runtime,
                owner: Some(owner),
                bus,
                events,
                controller,
            };
            rig.pump();
            assert!(rig.controller.loaded.get_untracked());
            rig.events.lock().expect("events").clear();
            rig
        }

        /// Drains the local queue for a short window: controller calls hop to
        /// the background runtime and back, so one drain is not enough.
        fn pump(&self) {
            for _ in 0..60 {
                self.runtime.pump_local();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }

    #[test]
    fn a_network_change_wake_reconnects_then_refreshes_once_and_pings_once() {
        let rig = WakeRig::mount();

        rig.bus.publish(WakeReason::NetworkChanged);
        rig.pump();

        let events = noted(&rig.events);
        assert_eq!(events.first(), Some(&"reconnect"), "{events:?}");
        assert_eq!(
            events.iter().filter(|e| **e == "refresh").count(),
            1,
            "{events:?}"
        );
        assert_eq!(
            events.iter().filter(|e| **e == "ping").count(),
            1,
            "{events:?}"
        );
        assert_eq!(events.len(), 3, "{events:?}");
    }

    #[test]
    fn a_foregrounded_wake_redials_then_refreshes_once_and_pings_once() {
        let rig = WakeRig::mount();

        rig.bus.publish(WakeReason::Foregrounded);
        rig.pump();

        let events = noted(&rig.events);
        assert_eq!(events.first(), Some(&"reconnect"), "{events:?}");
        assert_eq!(
            events.iter().filter(|e| **e == "reconnect").count(),
            1,
            "{events:?}"
        );
        assert_eq!(
            events.iter().filter(|e| **e == "refresh").count(),
            1,
            "{events:?}"
        );
        assert_eq!(
            events.iter().filter(|e| **e == "ping").count(),
            1,
            "{events:?}"
        );
    }

    #[test]
    fn a_wake_while_the_poll_is_covered_does_nothing() {
        let rig = WakeRig::mount();
        rig.controller.pause_until_depth(0);

        rig.bus.publish(WakeReason::NetworkChanged);
        rig.pump();

        assert_eq!(noted(&rig.events), Vec::<&str>::new());
    }

    #[test]
    fn unmounting_ends_the_wake_loop_and_releases_what_it_held() {
        let mut rig = WakeRig::mount();
        assert!(
            Arc::strong_count(&rig.controller) > 1,
            "the running loop holds the controller"
        );

        drop(rig.owner.take());
        rig.pump();

        assert_eq!(
            Arc::strong_count(&rig.controller),
            1,
            "cleanup ended the loop with no wake published"
        );
        rig.bus.publish(WakeReason::NetworkChanged);
        rig.pump();
        assert_eq!(
            noted(&rig.events),
            Vec::<&str>::new(),
            "a late wake reaches nobody"
        );
    }
}
