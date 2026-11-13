//! The `/sessions` route: the session list and every action a card or its management sheet can take.
//! Every push goes through [`Actions`], which pauses the controller's mock poll first; both cadence
//! loops re-resolve `layout_refresh_secs` before every sleep.

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

/// The route this screen attaches into; the session id rides as a query param.
const TERMINAL_ROUTE: &str = "/terminal";
const READ_ONLY_TOAST: &str =
    "Read-only session \u{2014} reconnect with a read-write token to change this";

const DISCONNECT_TITLE: &str = "Disconnect from server?";
const DISCONNECT_CONFIRM_LABEL: &str = "Disconnect";
const DISCONNECT_UNKNOWN_HOST: &str = "this server";

const LIST_PAD_X: f64 = spacing::XL;
const LIST_PAD_Y: f64 = spacing::LG;
const LIST_GAP: f64 = spacing::BASE;

/// The scrolling list column's max width past [`WIDE_BREAKPOINT_PX`](crate::responsive::WIDE_BREAKPOINT_PX).
/// The conn-row strip stays full-bleed.
const SESSIONS_CLAMP_WIDTH: f64 = 640.0;

/// Fixes the loading rain's whole field, so a replay paints the identical drops frame for frame.
const LOADING_RAIN_SEED: u64 = 0x6D75_7872;

static LOADING_RAIN: AtomicBool = AtomicBool::new(true);

/// How often a deferred rain arm re-checks that the switcher stage has settled.
const RAIN_SETTLE_POLL: Duration = Duration::from_millis(50);

static RAIN_ARMS: AtomicUsize = AtomicUsize::new(0);

pub fn deferred_rain_arms_for_tests() -> usize {
    RAIN_ARMS.load(Ordering::SeqCst)
}

static RAIN_BUILDS: AtomicUsize = AtomicUsize::new(0);

pub fn rain_overlay_builds_for_tests() -> usize {
    RAIN_BUILDS.load(Ordering::SeqCst)
}

/// Opts the process out of the loading rain: page-level suites tap under a pinned frame clock an overlay never advances.
pub fn set_loading_rain_for_tests(enabled: bool) {
    LOADING_RAIN.store(enabled, Ordering::Relaxed);
}

pub fn sessions_page(
    controller: NavigatorController<AppState>,
    overlays: NavigatorController<AppState>,
) -> impl View<AppState> {
    component(SessionsScreen {
        nav: controller,
        overlays,
    })
}

pub struct SessionsScreen {
    nav: NavigatorController<AppState>,
    overlays: NavigatorController<AppState>,
}

pub struct SessionsState {
    pub actions: Actions,
    pub connection: Arc<ConnectionController>,
    /// The server switcher, when the shell provided one: the poll pauses while its stage is open (the
    /// stage covers this screen without moving a navigator).
    switcher: Option<Arc<ServersSwitcherController>>,
    pub(super) heartbeat: HeartbeatState,
    pub(super) back: BackConfirm,
    pub(super) loading_done: RwSignal<bool>,
    /// The overlay's picture, fixed when `build` first sees the release settled; the live band stays live.
    loading_snapshot: Option<(Vec<Session>, bool)>,
    rain: bool,
    seen_epoch: u64,
    /// Sticky: whether this mount has ever seen the connection up, as opposed to a first connect.
    has_connected: bool,
    seeded: Option<String>,
    seeded_gen: u64,
    seeded_page: Option<Arc<PageSnapshot>>,
    /// Generation of the deferred rain arm: every restart bumps it, so a stale arm task does nothing.
    rain_generation: Arc<AtomicU64>,
    alive: Arc<AtomicBool>,
    /// Bumped when the close tail ends, so a build resumes the poll the tail held paused.
    tail_wake: RwSignal<u64>,
    tail_watch: Arc<AtomicBool>,
}

/// What a back press on `/sessions` means to this screen.
///
/// Plain state, not signals: read and written from `Component::build`, where a tracked write
/// would re-dirty the frame that made it.
pub(super) struct BackConfirm {
    signal: Rc<Cell<u64>>,
    /// Generation already answered; seeded from the cell so a mount answers only its own presses.
    answered: u64,
    confirming_at: Option<usize>,
}

impl BackConfirm {
    fn new() -> Self {
        let signal = sessions_back_signal();
        let answered = signal.get();
        BackConfirm {
            signal,
            answered,
            confirming_at: None,
        }
    }

    /// Whether a back press has landed since the last call; asked once per `build`.
    ///
    /// Answers `true` at most once per bump: `build` reruns several times per frame, so the generation
    /// is banked before the answer. A bump while a confirmation is up is banked and swallowed: the root
    /// host claims the back press ahead of the route navigator.
    fn take_request(&mut self, depth: usize) -> bool {
        if let Some(mark) = self.confirming_at
            && depth <= mark
        {
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

/// Cloneable: a card's gesture reaches these through `&mut SessionsState`, while a sheet row
/// (a page on the app's navigator) captures a clone and calls the same method.
#[derive(Clone)]
pub struct Actions {
    pub nav: NavigatorController<AppState>,
    pub overlays: NavigatorController<AppState>,
    pub app_nav: Nav,
    pub controller: Arc<SessionsController>,
    pub keyring: KeyringHandle,
}

/// Resolves the live session-poll cadence (`layout_refresh_secs`) through the one `preferences`
/// handle `init` resolves; falls back to [`POLL_PERIOD_SECS`] on a read failure.
pub async fn resolve_poll_interval(preferences: &SharedPreferencesRepository) -> Duration {
    let secs = preferences
        .load_preferences()
        .await
        .map(|prefs| prefs.poll.layout_refresh_secs)
        .unwrap_or(POLL_PERIOD_SECS);
    Duration::from_secs(u64::from(secs))
}

/// Arms the loading rain once the switcher stage has settled, provided the restarted first load has
/// not settled and no later restart or unmount superseded this `generation`.
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

/// Waits out the stage's close tail, then wakes a rebuild so the poll resumes. One task at a time.
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

/// Starts a self-rescheduling loop running `tick`, re-resolving [`resolve_poll_interval`] before every
/// sleep. Must run where a reactive `Owner` is ambient: `on_cleanup` no-ops outside one.
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

/// One heartbeat round trip, shared by the cadence tick and a wake. A no-op while the poll is
/// covered or nothing is connected; the connection's single-flight `ping` stops beats stacking.
fn beat(
    polling: &SessionsController,
    connection: &Arc<ConnectionController>,
    heartbeat: HeartbeatState,
) {
    if !polling.is_polling() {
        return;
    }
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
        // A ping that measured nothing rolls nothing; the list poll owns failure surfacing.
        if let Some(measured) = connection.ping().await {
            heartbeat.settle(from, measured);
        }
    });
}

static TROUBLE_RECONNECTS: AtomicUsize = AtomicUsize::new(0);

pub fn trouble_reconnects_for_tests() -> usize {
    TROUBLE_RECONNECTS.load(Ordering::SeqCst)
}

/// Reports the failure streak's latest edge: trouble rebuilds the channel (single-flight) and goes
/// under the servers controller's banner; recovery flashes it away. Never tears the connection down.
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

/// Runs a wake-driven refresh and heartbeat ping for every wake on `wakes` while the screen is
/// mounted. Both go through the poll's single-flight paths, so a wake never starts a second one.
///
/// Every wake first rebuilds the shared client's channel (a suspended app's channel is dead on
/// return); a failed rebuild is logged and the refresh still runs. A wake while the poll is covered
/// does nothing more. With nothing connected a wake redials the pending reconnect, covered or not.
///
/// Must run where a reactive `Owner` is ambient.
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
        let controller = use_controller::<SessionsController, AppFailure>(|| {
            SessionsController::new(expect_sessions_repository())
        });

        use_failure_listener(controller.core().failures(), |failure: AppFailure| {
            app_toasts().raise(&failure);
        });

        let first_load = Arc::clone(&controller);
        spawn_local(async move { first_load.refresh_background().await });

        // Resolved synchronously in `init`: a future's first poll has no ambient owner, so a
        // `use_context` lookup there would panic.
        let preferences = expect_preferences_repository();

        let servers = use_servers_controller();
        let connection = expect_connection_controller();
        if let Some(servers) = servers.clone() {
            on_cleanup(move || servers.sessions_left());
        }

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

        let heartbeat = HeartbeatState::new();
        let beating = Arc::clone(&controller);
        let pinged = Arc::clone(&connection);
        spawn_cadence_loop(preferences, move || {
            beat(&beating, &pinged, heartbeat);
        });

        spawn_wake_loop(
            crate::net::wake::subscribe(),
            Arc::clone(&controller),
            Arc::clone(&connection),
            heartbeat,
        );

        // Read once per mount: a process-wide test opt-out, never from `build` or a spawned block.
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
        // Plain atomic, not a signal (see `BackConfirm`).
        let depth = state.actions.stack_depth();
        state.actions.controller.observe_depth(depth);
        // Tracked, so a close wakes this build and the observe above resumes the poll; while open, the
        // pause is re-marked at the current depth right after that observe. The close tail keeps the
        // pause: the live page must not change while the neighbours still paint.
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
            // The route navigator outranks the switcher's `SwitcherBack` fallback: a press with the stage up
            // closes the stage, not the session.
            match state.switcher.as_ref().filter(|s| s.open.get_untracked()) {
                Some(switcher) => switcher.close(),
                None => state.actions.open_disconnect_confirm(&state.connection),
            }
        }

        // A cached switch committed: show its snapshot from this build on (one write, never an empty
        // list) and stay plain, no rain.
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

        // A new connection (the epoch moved while connected) starts this mount over and the rain replays.
        // A move to disconnected is the guard/back flow's business. Untracked: `server_info`, set in the
        // same step, wakes this rebuild.
        let epoch = state.connection.connection_epoch.get_untracked();
        let connected = state.connection.connected.get_untracked();
        if epoch != state.seen_epoch {
            // A switch is an ever-connected screen seeing a new epoch, or a teardown and re-dial inside one
            // frame. A lone move from disconnected is the first connect, which `init`'s load serves.
            let switched = state.has_connected || epoch - state.seen_epoch >= 2;
            state.seen_epoch = epoch;
            let landed = state
                .connection
                .server_info
                .get_untracked()
                .map(|info| info.endpoint);
            if connected && switched && landed.is_some() && state.seeded == landed {
                state.seeded = None;
                state.seeded_page = None;
                state.actions.controller.refresh_in_place();
            } else if connected && switched {
                state.seeded = None;
                state.seeded_page = None;
                state.loading_snapshot = None;
                let generation = state.rain_generation.fetch_add(1, Ordering::SeqCst) + 1;
                if state.rain && !stage_settled() {
                    // The switcher stage is still animating: the rain's offscreen layers must not nest inside it.
                    // Stay plain and arm the rain once the stage settles.
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

        let sessions = state.actions.controller.sessions.get();
        let loaded = state.actions.controller.loaded.get();
        let settled = state.actions.controller.first_load_settled.get();
        let loading_done = state.loading_done.get();
        let live_info = state.connection.server_info.get();
        let live_read_only = state.connection.is_read_only.get();
        // A seeded page with no connection keeps the snapshot's header facts through the disconnect and
        // the dial, with every action that needs the server off.
        let offline = state.seeded.is_some() && !state.connection.connected.get_untracked();
        state.actions.controller.set_offline(offline);
        let seeded_page = state.seeded_page.clone().filter(|_| offline);
        let info = match &seeded_page {
            Some(page) if live_info.is_none() => page.info.clone(),
            _ => live_info,
        };
        let strip_read_only = seeded_page.as_ref().map_or(live_read_only, |p| p.read_only);
        let read_only = live_read_only || offline;

        let theme = widgets::active_theme();
        let colors = Palette::from_theme(&theme);

        let beat = state.heartbeat.visual(
            info.as_ref().map_or(0, |info| info.latency_ms),
            theme.motion.reduce_motion,
        );

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

        let live_scroller = list_view(
            list_pieces(&sessions, loaded, settled, read_only, &colors, &layout),
            &layout,
            true,
        );
        // Always a `Stack`: an erased child type flipping Some to None tears the pod under it down.
        let mut children = vec![any(live_scroller)]; // erasure: keep accumulator into Vec<AnyView>
        if !loading_done {
            RAIN_BUILDS.fetch_add(1, Ordering::SeqCst);
            let (overlay_pieces, stale) = if settled {
                let frozen = state
                    .loading_snapshot
                    .get_or_insert_with(|| (sessions.clone(), loaded));
                // The id ladder is the picture: a card that gained a pane or preview between polls stays the same
                // card in the same slot, and telemetry churn must not hold input on the overlay.
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
        // Mounted only while ring/roll still owe a frame; kept at the list's end so the two real bands
        // keep positional identity.
        if beat.animating() {
            bands.push(heartbeat::frame_ticker());
        }
        // The shell scaffold's app-bar slot already reserves the bar's band; no clearance to add.
        widgets::page_tone(
            FlexView::new(Axis::Vertical, bands).cross_axis(CrossAxisAlignment::Stretch),
            &colors,
        )
    }
}

/// The disconnect confirmation's body, naming the connected `host`. The second sentence keeps
/// "Disconnect" from reading like Settings' "Forget server".
fn disconnect_message(host: &str) -> String {
    format!(
        "You will be disconnected from {host}. Saved servers and credentials stay on this \
         device."
    )
}

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

/// The list's children in stagger order: the empty state, or a card per session closed by the dashed
/// create row; [`widgets::loading_view`] instead until the first load settles, failure included.
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

fn padded_list(pieces: Vec<AnyView<SessionsState>>) -> impl View<SessionsState> {
    Padding(
        EdgeInsets::symmetric(LIST_PAD_X, LIST_PAD_Y),
        FlexView::new(Axis::Vertical, pieces.into_iter().map(inflexible).collect())
            .gap(LIST_GAP)
            .cross_axis(CrossAxisAlignment::Stretch),
    )
}

/// The band `content` occupies: safe-area inset with its top edge off, clamped and centered past
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
    /// How deep the app's page stack is, counting both navigators: overlays are pages of the root host,
    /// so route depth alone stops moving when a dialog or sheet opens.
    fn stack_depth(&self) -> usize {
        self.nav.depth() + self.overlays.depth()
    }

    fn pause_for_push(&self) {
        self.controller.pause_until_depth(self.stack_depth());
    }

    pub fn attach(&self, id: &str) {
        if self.controller.is_offline() {
            return;
        }
        self.pause_for_push();
        self.app_nav.push(&terminal_location(id, None));
    }

    /// Attach to `id` carrying a Tablet pane pill's `(tab, pane id)` target; until attach-into-pane
    /// lands the terminal attaches at the session's server focus.
    pub fn attach_pane(&self, id: &str, tab: u32, pane: u32) {
        if self.controller.is_offline() {
            return;
        }
        self.pause_for_push();
        self.app_nav.push(&terminal_location(id, Some((tab, pane))));
    }

    /// Attach to `id` aimed at an agent row's pane; an agent the layout could not place has no target,
    /// so nothing happens.
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

    /// Seeded with the session's current name, re-read from the controller so an old sheet cannot seed a stale one.
    pub fn open_rename_dialog(&self, id: &str) {
        if self.controller.is_offline() {
            return;
        }
        let Some(session) = self.controller.session(id) else {
            // List moved since paint (a poll landed between paint and tap): nothing to rename.
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

    /// Open the back press's disconnect confirmation. Confirming locks the keyring, closes the
    /// connection and clears its signals, then navigates through [`Nav`] so the shell bar's mirror drops
    /// this route's entry on the same frame. Every dismissal does nothing: the polls come back through
    /// [`Component::build`]'s depth observation.
    pub fn open_disconnect_confirm(&self, connection: &Arc<ConnectionController>) {
        self.pause_for_push();
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

    /// Open the per-session management sheet (long-press on a card). `read_only` disables the
    /// Rename/Kill rows, never Attach.
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

pub(super) fn select(state: &mut SessionsState, id: &str) {
    state.actions.controller.select_and_refresh_agents(id);
}

pub(super) fn attach_pane(state: &mut SessionsState, id: &str, tab: u32, pane: u32) {
    state.actions.attach_pane(id, tab, pane);
}

pub(super) fn attach_agent(state: &mut SessionsState, id: &str, agent: &SessionAgent) {
    state.actions.attach_agent(id, agent);
}

pub(super) fn open_rename(state: &mut SessionsState, id: &str) {
    if !refused_read_only(state) {
        state.actions.open_rename_dialog(id);
    }
}

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

pub(super) fn retry_first_load(state: &mut SessionsState) {
    let controller = Arc::clone(&state.actions.controller);
    spawn_local(async move { controller.retry_first_load().await });
}

pub(super) fn open_create_dialog(state: &mut SessionsState) {
    state.actions.open_create_dialog(&state.connection);
}

/// Open the management sheet for `id`. Reads the read-only state untracked: a one-shot decision at open time.
pub(super) fn open_manage_sheet(state: &mut SessionsState, id: &str) {
    let read_only = state.connection.is_read_only.get_untracked();
    state.actions.open_manage_sheet(id, read_only);
}

/// The `/terminal` location for `id`, optionally aimed at one `(tab index, pane id)`; the session
/// id is percent-encoded. Pushed through the guarded [`Nav`] like a deep link.
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

/// [`terminal_location`] for an agent row: its tab position and pane id, and `space`
/// (percent-encoded) when reported. `None` for an agent with no tab position.
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
        // The path stays flat `/terminal` (route table and session guard match on it); the id is in the query.
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

    fn observer(signal: &Rc<Cell<u64>>) -> BackConfirm {
        BackConfirm {
            signal: Rc::clone(signal),
            answered: signal.get(),
            confirming_at: None,
        }
    }

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
        assert!(
            !back.take_request(CONFIRMING),
            "…exactly one, not one per rebuild"
        );

        // A press reaching this screen while confirming is banked, not queued.
        back_press(&signal);
        assert!(!back.take_request(CONFIRMING));
        assert!(!back.take_request(CONFIRMING));

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
        let message = disconnect_message("100.64.0.7");
        assert!(message.contains("100.64.0.7"), "{message}");
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

    type Events = Arc<std::sync::Mutex<Vec<&'static str>>>;

    fn note(events: &Events, what: &'static str) {
        events.lock().expect("events").push(what);
    }

    fn noted(events: &Events) -> Vec<&'static str> {
        events.lock().expect("events").clone()
    }

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

        /// Controller calls hop to the background runtime and back, so one drain is not enough.
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
