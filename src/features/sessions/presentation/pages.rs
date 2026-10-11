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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clean_signals_frust::{use_controller, use_failure_listener};
use frust::{
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexChild,
    FlexView, Get, GetUntracked, NavigatorController, Padding, RwSignal, SizedBox, Stack, View,
    WindowMetrics, any, component, flexible, inflexible, on_cleanup, safe_area, scroll_view,
    spawn_local, use_context,
};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::sessions::di::expect_sessions_repository;
use crate::features::sessions::domain::entities::{Session, SessionBackend};
use crate::features::settings::di::{SharedPreferencesRepository, expect_preferences_repository};
use crate::responsive::Responsive;
use crate::routes::{
    Nav, TERMINAL_PANE_QUERY_KEY, TERMINAL_SESSION_QUERY_KEY, TERMINAL_TAB_QUERY_KEY, app_nav,
    sessions_back_signal,
};
use crate::ui::dialogs::{confirm_dialog, name_input_dialog};
use crate::ui::palette::Palette;
use crate::ui::sheet::show_sheet;
use crate::ui::spacing;
use crate::ui::splash::loading::loading_sequence;
use crate::ui::toasts::app_toasts;

use super::controllers::{POLL_PERIOD_SECS, SessionsController};
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
        spawn_local(async move { first_load.refresh().await });

        // Resolved synchronously in `init`, never inside a spawned future's
        // body: a future's first poll has no ambient owner, so a
        // `use_context` lookup there would panic. Only the resolved `Arc`
        // crosses into the loops below.
        let preferences = expect_preferences_repository();

        // Session-list poll: re-resolves `layout_refresh_secs` live, not
        // just at mount.
        let polling = Arc::clone(&controller);
        spawn_cadence_loop(Arc::clone(&preferences), move || {
            if polling.take_poll_tick() {
                let handle = Arc::clone(&polling);
                spawn_local(async move { handle.refresh_from_poll().await });
            }
        });

        let connection = expect_connection_controller();

        // Connection heartbeat: one real timed `GetVersion`, same cadence as
        // the list poll (the roll `HeartbeatState::settle` animates over is
        // a separate display concern). `is_polling()` is the poll's own
        // covered-screen gate.
        let heartbeat = HeartbeatState::new();
        let beating = Arc::clone(&controller);
        let pinged = Arc::clone(&connection);
        spawn_cadence_loop(preferences, move || {
            if !beating.is_polling() {
                return;
            }
            // Untracked: a callback isn't a render. `None` means nothing
            // connected to time.
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
                // A ping that measured nothing rolls nothing — the list
                // poll owns failure surfacing.
                if let Some(measured) = connection.ping().await {
                    heartbeat.settle(from, measured);
                }
            });
        });

        // Read once per mount — a process-wide test opt-out, not observed
        // state, so never from `build` or a spawned block.
        let rain = LOADING_RAIN.load(Ordering::Relaxed);

        SessionsState {
            actions: Actions {
                nav: self.nav.clone(),
                overlays: self.overlays.clone(),
                app_nav: app_nav(),
                controller,
            },
            connection,
            heartbeat,
            back: BackConfirm::new(),
            loading_done: RwSignal::new(!rain),
            loading_snapshot: None,
        }
    }

    fn build(&self, state: &mut SessionsState) -> impl View<SessionsState> {
        // Plain atomic, not a signal — see `BackConfirm`'s doc for why.
        let depth = state.actions.stack_depth();
        state.actions.controller.observe_depth(depth);

        if state.back.take_request(depth) {
            state.actions.open_disconnect_confirm(&state.connection);
        }

        // Tracked reads: a load, a mutation, or a connection change is what
        // wakes this rebuild.
        let sessions = state.actions.controller.sessions.get();
        let loaded = state.actions.controller.loaded.get();
        let settled = state.actions.controller.first_load_settled.get();
        let loading_done = state.loading_done.get();
        let info = state.connection.server_info.get();
        // Gates create/rename/kill (never attach) — see
        // `widgets::new_session_row`'s doc for the disabled-state pattern.
        let read_only = state.connection.is_read_only.get();

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
        let tone = if rule.is_tablet() {
            colors.surface
        } else {
            colors.base
        };

        // Both renderings assemble the same pieces through the same band; the
        // live one adds `scroll_view`, a pass-through at offset 0.
        let live_scroller = arranged_list(
            list_pieces(&sessions, loaded, settled, read_only, &colors, &layout),
            &layout,
            true,
        );
        // Always a `Stack`, overlay or not: an erased child type that flips
        // Some→None tears the pod under it down (`shell::root_view`'s rule).
        let mut children = vec![live_scroller];
        if !loading_done {
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
                move |pieces| arranged_list(pieces, &overlay_layout, false),
                settled,
                stale,
                state.loading_done,
            ));
        }
        let strip = widgets::server_strip(info.as_ref(), beat, read_only, &colors);
        let list = any(Stack(children));
        let mut bands = match layout {
            ListLayout::Compact { .. } => vec![inflexible(strip), flexible(1, list)],
            ListLayout::Tablet { full, .. } => {
                let footer = settled.then(|| widgets::new_session_row(&colors, read_only));
                let detail = widgets::detail_pane(
                    &sessions,
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
        any(widgets::page_tone(
            FlexView::new(Axis::Vertical, bands).cross_axis(CrossAxisAlignment::Stretch),
            &colors,
        ))
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
) -> Vec<AnyView<SessionsState>> {
    if let ListLayout::Tablet { full, selected } = layout {
        let phase = LoadPhase::of(loaded, settled);
        let selected = selected.as_deref();
        return widgets::tablet_list_pieces(sessions, phase, read_only, selected, *full, colors);
    }
    if loaded && sessions.is_empty() {
        return vec![widgets::empty_view(read_only)];
    }
    let mut pieces: Vec<AnyView<SessionsState>> = sessions
        .iter()
        .map(|session| widgets::session_card(session, colors, CardMode::Compact))
        .collect();
    pieces.push(if settled {
        widgets::new_session_row(colors, read_only)
    } else {
        widgets::loading_view(colors)
    });
    pieces
}

/// `pieces` laid out for `layout`, scrolling when `scroll` (the live band; the
/// loading overlay's picture does not scroll).
fn arranged_list(
    pieces: Vec<AnyView<SessionsState>>,
    layout: &ListLayout,
    scroll: bool,
) -> AnyView<SessionsState> {
    let list = match layout {
        ListLayout::Compact { .. } => padded_list(pieces),
        ListLayout::Tablet { .. } => widgets::tablet_list(pieces),
    };
    let list = if scroll { any(scroll_view(list)) } else { list };
    match layout {
        ListLayout::Compact { wide } => list_band(list, *wide),
        ListLayout::Tablet { .. } => list,
    }
}

/// `pieces` as the screen's padded, [`LIST_GAP`]-spaced, stretched column.
fn padded_list(pieces: Vec<AnyView<SessionsState>>) -> AnyView<SessionsState> {
    any(Padding(
        EdgeInsets::symmetric(LIST_PAD_X, LIST_PAD_Y),
        FlexView::new(Axis::Vertical, gapped(pieces)).cross_axis(CrossAxisAlignment::Stretch),
    ))
}

/// The band `content` occupies: safe-area inset with its top edge off (the
/// shell's bar already consumed that window inset), clamped and centered past
/// [`SESSIONS_CLAMP_WIDTH`] while the strip beside it stays full-bleed.
fn list_band(content: AnyView<SessionsState>, is_wide: bool) -> AnyView<SessionsState> {
    let band = any(safe_area(content).top(false));
    if !is_wide {
        return band;
    }
    any(Align(
        Alignment::CENTER,
        SizedBox::<SessionsState>(Some(SESSIONS_CLAMP_WIDTH), None).child(band),
    ))
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
        self.pause_for_push();
        self.app_nav.push(&terminal_location(id, None));
    }

    /// Attach to `id` carrying a Tablet pane pill's `(tab, pane id)` target.
    /// The terminal honours that target once Phase 4's attach-into-pane card
    /// lands; until then it attaches at the session's server focus.
    pub fn attach_pane(&self, id: &str, tab: u32, pane: u32) {
        self.pause_for_push();
        self.app_nav.push(&terminal_location(id, Some((tab, pane))));
    }

    /// Open the create-session dialog (the dashed row and the empty state's
    /// action both land here).
    pub fn open_create_dialog(&self, connection: &Arc<ConnectionController>) {
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
        // Untracked one-shot read for the sentence, same shape as
        // `open_rename_dialog`'s name re-read.
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

pub(super) fn attach(state: &mut SessionsState, id: &str) {
    state.actions.attach(id);
}

/// A Tablet card tap: select `id` for the detail pane.
pub(super) fn select(state: &mut SessionsState, id: &str) {
    state.actions.controller.select(id);
}

pub(super) fn attach_pane(state: &mut SessionsState, id: &str, tab: u32, pane: u32) {
    state.actions.attach_pane(id, tab, pane);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
