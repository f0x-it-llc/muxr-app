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
//! │ ┃ D  dev      running zellij│  the scrolling list: the cards and the
//! │ ┃ ▣ 9 panes ◧ 4 tabs ◎ 0 …  │  dashed new-session row
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
//!   controller's signals; **app-scoped** state (the connection's server info,
//!   its read-only posture) is [`crate::connection`]'s controller, resolved
//!   from context. [`SessionsState`] itself holds neither — only [`Actions`]'
//!   four handles plus the connection. The credential offer that same
//!   controller carries is **not** read here: it is answered on Connect, before
//!   the redirect that lands on this screen (see [`crate::connection`]'s
//!   "Credential offer placement").
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
//! # The Android back press asks before it disconnects
//!
//! A back press on this route would pop straight to Connect and abandon the
//! live connection silently, so `/sessions` is the one route pushed with
//! `BackPolicy::DismissAnimated` (`crate::routes::Nav::push`): the navigator
//! **bumps a shared generation cell** instead of popping, and this screen owns
//! what that means. [`BackConfirm`] is the observer — the cell resolved in
//! `init` (`crate::routes::sessions_back_signal`), the generation already
//! answered, and the depth mark that says the confirmation it raised is still
//! up — and `Component::build` is where it is read, beside the poll's own
//! depth observation and under the same rule: plain state, never a tracked
//! write from `build`. Answering opens [`Actions::open_disconnect_confirm`] as
//! an ordinary queued push on the overlay host, exactly like every other
//! overlay this screen raises.
//!
//! Two properties fall out of the arrangement rather than out of code here: a
//! second back press while the confirmation is up is claimed by the overlay
//! host (**R44-back**), so it dismisses the dialog — a cancel — instead of
//! reaching this navigator at all; and a dismissal of any kind resumes the
//! polls through the existing `observe_depth` machinery, since the mark this
//! screen took when it opened the dialog is the one it resumes at.
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

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use clean_signals_frust::{use_controller, use_failure_listener, use_interval};
use frust::{
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexChild,
    FlexView, Get, GetUntracked, NavigatorController, Padding, Set, SizedBox, WindowMetrics, any,
    component, flexible, inflexible, safe_area, scroll_view, spawn_local, use_context,
};

use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::sessions::di::expect_sessions_repository;
use crate::features::settings::di::expect_preferences_repository;
use crate::responsive::WIDE_BREAKPOINT_PX;
use crate::routes::{Nav, app_nav, sessions_back_signal};
use crate::ui::dialogs::{confirm_dialog, name_input_dialog};
use crate::ui::sheet::{SheetHandle, show_sheet};
use crate::ui::toasts::app_toasts;
use crate::ui::topbar::topbar_clearance;

use super::controllers::{POLL_PERIOD_SECS, SessionsController};
use super::heartbeat::{self, HeartbeatState, PING_PERIOD};
use super::pieces;

/// The route this screen attaches into. The session id rides as a query param
/// (`Location::parse` splits it off before matching, so the flat `/terminal`
/// route and `crate::routes`' own session-guard `loc.path` check are both
/// unaffected) — the terminal screen ignores it today and will read it from
/// `RouteParams` when it has a session to attach to.
const TERMINAL_ROUTE: &str = "/terminal";
/// The query key [`TERMINAL_ROUTE`] carries the session id under.
const SESSION_QUERY_KEY: &str = "session";

/// The back-press confirmation, as the user reads it: its title…
const DISCONNECT_TITLE: &str = "Disconnect from server?";
/// …its destructive action…
const DISCONNECT_CONFIRM_LABEL: &str = "Disconnect";
/// …and what its body names when the connection has published no endpoint to
/// name (unreachable behind the session guard, but the sentence still needs a
/// subject).
const DISCONNECT_UNKNOWN_HOST: &str = "this server";

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

/// The Sessions screen's retained state: the action bundle, the app-scoped
/// connection handle and the strip's heartbeat, and nothing else.
pub struct SessionsState {
    /// Everything this screen can *do*, and the handles it needs to do it —
    /// see [`Actions`].
    pub actions: Actions,
    /// The app-scoped connection handle, for the strip, the top bar's title
    /// and the read-only posture that gates this screen's mutations (see
    /// [`crate::connection`]).
    pub connection: Arc<ConnectionController>,
    /// The `.conn-row` strip's heartbeat clocks — written by the ping
    /// interval `init` starts, read by every rebuild that renders the strip
    /// (see [`super::heartbeat`]). Screen-internal, unlike the two handles
    /// above: nothing outside this feature's presentation layer drives it.
    pub(super) heartbeat: HeartbeatState,
    /// The Android back press's observer — see [`BackConfirm`] and the module
    /// docs' back section. Screen-internal like the heartbeat.
    pub(super) back: BackConfirm,
}

/// What a back press on `/sessions` means to this screen: the route page's
/// dismiss-signal cell, the generation already answered, and the depth mark
/// that says the confirmation it raised is still up.
///
/// **Plain state, not signals**, for exactly the reason
/// [`SessionsController`]'s poll gate is (see that module's docs): it is read
/// *and written* from `Component::build`, where a tracked write would re-dirty
/// the very frame that made it.
pub(super) struct BackConfirm {
    /// The generation counter the navigator bumps in place of popping this
    /// page — resolved once, in `init`, like every other app-scoped handle
    /// (`crate::routes::sessions_back_signal`).
    signal: Rc<Cell<u64>>,
    /// The generation this screen has already answered. Seeded from the cell
    /// rather than from zero, so a screen that somehow resolved a used cell
    /// answers only what happens from its own mount onwards.
    answered: u64,
    /// The stack depth this screen's confirmation was raised at, or `None`
    /// while none is up — the same mark-and-resume arithmetic
    /// [`Actions::pause_for_push`] uses for the polls, against the same
    /// two-navigator depth.
    confirming_at: Option<usize>,
}

impl BackConfirm {
    /// Resolve this mount's dismiss cell. Called from `Component::init`, where
    /// the published cell is exactly the one the push that mounted this screen
    /// carried (see `crate::routes::sessions_back_signal`'s contract).
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
    /// several times per frame: `RenderRoot::rebuild` re-runs the whole app
    /// logic after each queued op it flushes, and the push this answer makes
    /// is itself such an op. So the generation is banked *before* the answer,
    /// and the pass that follows sees nothing owed.
    ///
    /// A bump arriving while a confirmation is still up is banked and
    /// swallowed rather than queueing a second dialog. That is belt to the
    /// arrangement's braces: with an overlay open the root host claims the
    /// back press ahead of the route navigator (**R44-back**), so a second
    /// press dismisses the confirmation instead of reaching this screen at
    /// all.
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

        // The mock poll's cadence: resolved once from `settings`' own stored
        // `layout_refresh_secs` preference (this mount only — see
        // `SessionsController`'s "Poll cadence" module docs for why a pref
        // changed while this screen is already mounted applies only from the
        // *next* mount). `settings::di::expect_preferences_repository()` is
        // resolved directly here, the same di.rs-is-composition-root-wiring
        // widening `terminal`'s page already uses for this exact repository
        // (`docs/ARCHITECTURE.md`'s Feature Slices section) — and resolved
        // *synchronously, right here in `init`*, never inside the spawned
        // future body below: `Component::init` runs under this component's
        // real `Owner`, but the `frust::spawn_local` poller that later polls
        // this future installs no ambient owner, so a `use_context`-backed
        // lookup taken from inside the future's first poll would panic on
        // the first real `/sessions` mount (a headless suite masks this
        // because its harness keeps an ambient owner live throughout).
        // Only the resolved `Arc` crosses into the spawned block.
        // `Component::build` starts `use_interval` itself, the moment
        // `poll_period` resolves.
        let poll_loader = Arc::clone(&controller);
        let preferences = expect_preferences_repository();
        spawn_local(async move {
            let secs = preferences
                .load_preferences()
                .await
                .map(|prefs| prefs.poll.layout_refresh_secs)
                .unwrap_or(POLL_PERIOD_SECS);
            poll_loader.poll_period.set(Some(secs));
        });

        // The app-scoped connection handle, resolved here with every other
        // app-scoped dependency — never from a press handler, and never from
        // inside the spawned body below.
        let connection = expect_connection_controller();

        // The strip's connection heartbeat: one real timed `GetVersion` every
        // `PING_PERIOD`, whose measurement rolls the strip's latency readout
        // (see `super::heartbeat`). Unlike the list poll above, this cadence
        // is a constant, so the interval starts right here — `init` runs under
        // the component `Owner` `use_interval` needs, and the interval is
        // disposed with it on unmount. `is_polling()` is the same
        // covered-screen gate the list poll uses, so a dialog, the management
        // sheet or the terminal route pauses both; nothing new to pause.
        let heartbeat = HeartbeatState::new();
        let beating = Arc::clone(&controller);
        let pinged = Arc::clone(&connection);
        use_interval(PING_PERIOD, move || {
            if !beating.is_polling() {
                return;
            }
            // The reading the roll will start from, taken before the round
            // trip that replaces it — and, being `None` while nothing is
            // connected, the gate on there being anything to time at all.
            // Untracked: a callback is not a render.
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
                // A ping that measured nothing (disconnected, or refused)
                // rolls nothing — the list poll owns failure surfacing.
                if let Some(measured) = connection.ping().await {
                    heartbeat.settle(from, measured);
                }
            });
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
            connection,
            heartbeat,
            // Resolved here, with every other app-scoped handle — and here
            // *only*: the cell published now is this mount's own (see
            // `crate::routes::sessions_back_signal`).
            back: BackConfirm::new(),
        }
    }

    fn build(&self, state: &mut SessionsState) -> AnyView<SessionsState> {
        // Page-driven poll resume (see `SessionsController`'s module docs).
        // A plain atomic, deliberately not a signal: a tracked write here
        // would re-dirty the very frame that made it.
        let depth = state.actions.stack_depth();
        state.actions.controller.observe_depth(depth);

        // The Android back press, answered here rather than obeyed by the
        // navigator (see the module docs' back section). Both halves of this
        // are the same kind of write the line above makes — plain state and a
        // queued navigator op, never a tracked signal — which is what makes
        // `build` a sound place to observe from.
        if state.back.take_request(depth) {
            state.actions.open_disconnect_confirm(&state.connection);
        }

        // Start the mock poll's interval the moment its cadence resolves
        // (tracked read: `init`'s async load writing `poll_period` is what
        // wakes this rebuild). `claim_poll_interval_start` makes the actual
        // `use_interval` call run exactly once even though `build` re-runs
        // on every later tracked write — see `SessionsController`'s "Poll
        // cadence" module docs for why this lives in `build` rather than
        // `init` (both run under the same component `Owner`, so
        // `use_interval` is exactly as safe to start from here).
        if let Some(secs) = state.actions.controller.poll_period.get()
            && state.actions.controller.claim_poll_interval_start()
        {
            let polling = Arc::clone(&state.actions.controller);
            use_interval(Duration::from_secs(u64::from(secs)), move || {
                if polling.take_poll_tick() {
                    let handle = Arc::clone(&polling);
                    spawn_local(async move { handle.refresh_from_poll().await });
                }
            });
        }

        // Tracked reads: a load, a mutation, or a connection change is what
        // wakes this rebuild.
        let sessions = state.actions.controller.sessions.get();
        let loaded = state.actions.controller.loaded.get();
        let info = state.connection.server_info.get();
        // Gates create/rename/kill (never attach) — see `pieces::new_session_row`'s
        // doc for the disabled-state pattern this and `open_manage_sheet` share.
        let read_only = state.connection.is_read_only.get();

        let theme = pieces::active_theme();
        let colors = pieces::palette(&theme);

        // The heartbeat's pose for *this* frame. The interval writes only the
        // two instants it clocks from; every frame in between is a wall-clock
        // read taken here, kept coming by the ticker mounted below while
        // there is still something to animate (see `super::heartbeat`).
        let beat = state.heartbeat.visual(
            info.as_ref().map_or(0, |info| info.latency_ms),
            theme.motion.reduce_motion,
        );

        // Responsive width-clamp pass (FINDINGS #21): read metrics once,
        // branch on one page constant, never touch layout again — the
        // reference idiom `examples/glyph-catalog/src/pages/responsive.rs`
        // documents. `None` (no shell has published yet, or a bare test
        // harness) takes the same branch as a genuinely narrow window.
        let metrics = use_context::<WindowMetrics>();
        let is_wide = metrics
            .map(|m| m.size.width >= WIDE_BREAKPOINT_PX)
            .unwrap_or(false);

        // The scrolling half: either the empty state, or every card plus the
        // dashed create row.
        let mut list = Vec::new();
        if loaded && sessions.is_empty() {
            list.push(pieces::empty_view(read_only));
        } else {
            list.extend(
                sessions
                    .iter()
                    .map(|session| pieces::session_card(session, &colors)),
            );
            list.push(pieces::new_session_row(&colors, read_only));
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
        let mut bands = vec![
            inflexible(pieces::server_strip(info.as_ref(), beat, &colors)),
            flexible(1, scroller),
        ];
        // Per-frame rebuilds last exactly as long as the animation does: the
        // ticker is mounted only while the ping ring or the latency roll still
        // owes a frame, so a settled strip — and every reduced-motion one —
        // asks for none. It rides at the end of the band list so the two real
        // bands keep their positional identity as it comes and goes.
        if beat.animating() {
            bands.push(heartbeat::frame_ticker());
        }
        // The page tone stays **outside** the clearance so it fills the whole
        // surface, bar band included: the bar slides in over it (see
        // `crate::shell::root_view`), and a page that stopped at the clearance
        // would flash the shell's own clear colour through the gap for the
        // length of that entrance.
        any(pieces::page_tone(
            topbar_clearance(
                FlexView::new(Axis::Vertical, bands).cross_axis(CrossAxisAlignment::Stretch),
            ),
            &colors,
        ))
    }
}

/// The disconnect confirmation's body, naming the connected `host`.
///
/// A function rather than a fourth constant because the host is the whole
/// point of the sentence: it is what tells the user *which* connection a back
/// press is about to end. The second sentence is the reassurance that stops
/// "Disconnect" reading like "Forget server" (which is Settings' own, and does
/// delete things).
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

    /// Open the back press's disconnect confirmation — the one action this
    /// screen takes that no gesture on it can reach (see the module docs' back
    /// section).
    ///
    /// Confirming does both halves the connection contract splits: the
    /// controller closes the connection and clears its signals, and the caller
    /// navigates — through [`Nav`], so the shell bar's mirror drops this
    /// route's entry on the same frame rather than a reconcile later. Every
    /// dismissal route does nothing at all: the polls come back through
    /// [`Component::build`]'s own depth observation.
    ///
    /// `connection` is passed in rather than held on [`Actions`] because this
    /// is the only action that needs it, and the screen already keeps the
    /// app-scoped handle beside the bundle.
    pub fn open_disconnect_confirm(&self, connection: &Arc<ConnectionController>) {
        self.pause_for_push();
        // Untracked: a one-shot read for a sentence, at the moment the dialog
        // opens — the same shape `open_rename_dialog` re-reads a name with.
        let host = connection
            .server_info
            .get_untracked()
            .map(|info| pieces::host_of(&info.endpoint).to_string())
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

    /// Open the per-session management sheet — what a **long-press** on a card
    /// raises, and the only route to rename/kill now that the card carries no
    /// buttons.
    ///
    /// Each row pops the sheet first and then does its work, so the stack goes
    /// sheet → dialog (or sheet → `/terminal`) in one rebuild: both ops land in
    /// the same drain, and the poll stays paused throughout, since the mark
    /// taken when the sheet opened is shallower than either depth.
    ///
    /// `read_only` disables the Rename/Kill rows (never Attach) — see
    /// `pieces::sheet_row`'s doc for the disabled-state pattern.
    pub fn open_manage_sheet(&self, id: &str, read_only: bool) {
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
                sheet_action(&actions, sheet, &id, ManageAction::Attach, false),
                sheet_action(&actions, sheet, &id, ManageAction::Rename, read_only),
                sheet_action(&actions, sheet, &id, ManageAction::Kill, read_only),
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

/// One management-sheet row, wired to `action` for session `id`. `disabled`
/// is always `false` for [`ManageAction::Attach`] — only
/// create/rename/kill are read-only-gated (see [`Actions::open_manage_sheet`]).
fn sheet_action(
    actions: &Actions,
    sheet: &SheetHandle,
    id: &str,
    action: ManageAction,
    disabled: bool,
) -> AnyView<AppState> {
    let actions = actions.clone();
    let sheet = sheet.clone();
    let id = id.to_string();
    pieces::sheet_row(
        action.label(),
        action.destructive(),
        disabled,
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

/// Open the management sheet for `id` — a card long-press. Reads the
/// connection's current read-only state (untracked: a one-shot decision at
/// the moment the sheet opens, like `open_rename_dialog`'s own re-read of the
/// current name).
pub(super) fn open_manage_sheet(state: &mut SessionsState, id: &str) {
    let read_only = state.connection.is_read_only.get_untracked();
    state.actions.open_manage_sheet(id, read_only);
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
