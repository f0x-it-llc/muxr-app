//! Muxr's full route table, its one guard, and the app-wide navigation handle
//! every screen pushes paths through — a hub file: each page builder lives in
//! its owning feature's `presentation::pages` module (see each feature's own
//! module docs for the `{domain, data, presentation}` layering convention
//! `examples/huddle`'s `src/README-phase-c.md` establishes).
//!
//! Four routes, one per feature, all flat leaves today: `/` (Connect),
//! `/sessions`, `/terminal`, `/settings`. [`session_guard_with`] is the one
//! guard: a top-level [`frust::Router::redirect`] bouncing an unauthenticated
//! push to `/sessions`/`/terminal` back to `/`.
//!
//! # One router, one handle — [`install_app_router`] / [`app_nav`]
//!
//! There is exactly **one** [`Router`] in this app, built here and owned by
//! `crate::AppState::nav`. Screens navigate by *path*, and only a `Router`
//! resolves a path against the route table (and runs the guard attached to it)
//! — a [`NavigatorController`] pushes page *builders*. But a screen cannot be
//! handed the router through the route table it belongs to: [`build_routes`]
//! runs **before** `Router::with_controller`, so a page builder closure has
//! nothing to capture. And it cannot be handed one through its component state
//! either — a `Component`'s callbacks see their own `State`, never the outer
//! [`AppState`] where the router lives (`frust_core::component`'s state
//! boundary).
//!
//! So the router is published *after* construction, as a small clonable
//! [`Nav`], and a screen resolves it in its own `Component::init` (the same
//! discipline `crate::connection::expect_connection_controller` follows) and
//! keeps it in its `State`.
//!
//! ## Why a thread-local and not `provide_context`
//!
//! [`frust::provide_context`] is `reactive_graph`'s, whose signature is
//! `provide_context<T: Send + Sync + 'static>`. [`Router`]/
//! [`NavigatorController`] are `Rc`-backed and so neither `Send` nor `Sync`
//! (which is also why `clean_signals_frust::use_controller`, bounded the same
//! way, can never host one) — a `Nav` simply cannot be put into frust's
//! context. It therefore lives in a process-`thread_local!`, reached by calling
//! [`app_nav`] from anywhere on the UI thread: the same handle-in-a-thread-local
//! idiom `crate::ui::toasts::app_toasts` documents, minus the self-healing
//! (a router cannot be recreated without the navigator it drives, so a missing
//! handle is a loud panic instead). Per-**thread** is exactly right here —
//! frust is single-UI-threaded, and it additionally isolates each headless
//! test, which `cargo test` runs on its own thread.
//!
//! # The route-meta mirror (what the shell's top bar reads)
//!
//! [`crate::shell`] renders **one** persistent top bar above the navigator, so
//! something has to answer "which route is on top, and what does its bar say?".
//! Only the navigator knows its own page stack, and it exposes just a
//! [`NavigatorController::depth`] count — no per-page identity. So [`Nav`],
//! which already fronts *every* path navigation in the app, keeps a small
//! **mirror** beside it: one `(depth_at_push, `[`Option<RouteMeta>`](RouteMeta)`)`
//! entry per path navigation it performed, reconciled against that authoritative
//! depth on every read ([`Nav::bar`]).
//!
//! Three properties fall out of that shape, and they are the whole design:
//!
//! - **A pop that never passed through `Nav` still corrects the bar.** An
//!   Android back gesture (or a bare [`NavigatorController::pop`]) drops the
//!   stack without telling anyone; the next reconcile sees the live depth fall
//!   and drops every entry recorded deeper than it.
//! - **A transparent utility push does not retitle the bar.** A sheet/dialog is
//!   pushed as a *page builder* straight through the [`NavigatorController`],
//!   never through [`Nav`], so it adds depth with **no** mirror entry — the bar
//!   keeps showing the covered route's title, which is exactly right.
//! - **A guarded push retitles honestly.** The mirror records the *resolved*
//!   (post-redirect) path, so an unauthenticated `/sessions` bounced to `/`
//!   records `/`'s (absent) bar, not `/sessions`'.
//!
//! Reconciliation is depth-*fall* triggered, not depth-equality triggered,
//! because [`NavigatorController::depth`] is advisory — published at the last
//! rebuild, so it still reads the pre-push value at the moment a press handler
//! records a push (see [`NavState::reconcile`]).
//!
//! # Overlay accounting — why depth alone cannot dim the bar
//!
//! That advisory depth is one rebuild stale *by construction*:
//! `RenderRoot::rebuild` runs the whole app-logic closure — the shell's bar
//! with it — **before** the navigator drains its queued ops. So the frame that
//! applies a modal's pop still publishes the depth from before it, and since
//! nothing else in a settled app asks for a frame, the loop parks there: bar
//! dimmed, gear and `‹` inert, until unrelated input happens along.
//!
//! [`Nav`] therefore *counts its own overlays* beside the mirror
//! ([`NavState::overlays`]/[`NavState::overlays_closing`]), and every change
//! publishes. Three calls, in the order a modal lives:
//!
//! - [`Nav::begin_overlay`] — a helper pushed one;
//! - [`Nav::dismiss_overlay`] — the app is popping one *now* (the synchronous
//!   half: a tracked write here dirties the shell's scope, which schedules the
//!   frame that paints the bar lit);
//! - [`Nav::end_overlay`] — the navigator delivered that page's pop-completion
//!   callback (the authoritative half: it runs exactly once per pushed page,
//!   whichever way it was dismissed).
//!
//! The depth gap stays in the answer, because an overlay pushed straight
//! through the [`NavigatorController`] — by a test, or by a frust helper the
//! app has not wrapped — has no accounting at all. And
//! [`NavState::reconcile`] clamps the counters flat the moment the live depth
//! falls back to the route stack, so no dismissal, however it happened, can
//! strand a dimmed bar.
//!
//! ## Consult the queued ops *before* mutating
//!
//! That clamp — and the mirror's own retain beside it — are both **erasing**
//! operations gated on a depth that is one rebuild stale. So the two places
//! that *add* coverage the eraser could later wipe have to read the queue
//! first, and each does it in the way its own staleness demands:
//!
//! - [`NavState::record_push_depth`] discounts the pops already queued before
//!   deciding how deep a landing route sits (a handler that closes an overlay
//!   and pushes a route in one rebuild would otherwise record one too deep —
//!   permanently);
//! - [`NavState::begin_overlay`] reconciles *before* it increments, so an
//!   unobserved depth fall is spent on the frame it belongs to rather than on
//!   the next overlay's count.
//!
//! [`Nav::dismiss_overlay`] and [`Nav::end_overlay`] deliberately stay as they
//! are, and the asymmetry is the point: they only ever move counters **toward
//! zero**. A clamp that arrives a beat early can at worst duplicate what they
//! were going to do anyway; it can never undo real state. Only
//! [`Nav::begin_overlay`]'s increment and [`Nav::record_push`]'s stack entry
//! create something the clamp/retain could erase, so only those two consult.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use frust::{
    AnyView, Get, GetUntracked, Location, NavigatorController, Resolution, Route, RouteParams,
    Router, RwSignal, Set, TransitionSpec,
};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::features::connect::presentation::pages::connect_page;
use crate::features::sessions::presentation::pages::sessions_page;
use crate::features::sessions::presentation::pieces::host_of;
use crate::features::settings::presentation::pages::settings_page;
use crate::features::terminal::presentation::pages::terminal_page;

/// Where the session guard bounces an unauthenticated push to.
const CONNECT_LOCATION: &str = "/";

/// The `/` route (and the navigator's initial page): Connect. Takes the
/// [`NavigatorController`] so this function fits both [`build_routes`]'s
/// `"/"` closure and the navigator's `initial` closure identically (see
/// `examples/huddle`'s `home_page` for the same dual-use shape); forwarded
/// straight through to [`connect_page`], which uses it for the QR sheet it
/// pushes (a sheet is a page builder, not a path — see [`Nav`] for the other
/// half).
pub(crate) fn home_page(controller: NavigatorController<AppState>) -> AnyView<AppState> {
    connect_page(controller)
}

// ---------------------------------------------------------------------------
// Route metadata — what the shell's persistent top bar shows per route
// ---------------------------------------------------------------------------

/// Where a route's bar title comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarTitle {
    /// The connected server's host — `/sessions`' title, resolved per frame
    /// from [`ConnectionController::server_info`] (the strip below it prints the
    /// whole `host:port`; the bar prints only the host).
    ConnectedHost,
    /// A literal title (`terminal`, `server settings`).
    Fixed(&'static str),
}

/// What sits in the bar's leading slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarLeading {
    /// The `muxr` brand mark + wordmark — the stack's first titled route.
    Brand,
    /// A `‹` that pops back to the route below.
    ///
    /// **Design deviation, deliberate.** `docs/redesign.html`'s terminal mock
    /// carries the brand + a `⋮` rather than a back affordance (only its
    /// settings screen, line 318, shows the `‹`); the product decision is that
    /// *every pushed route* gets a back button, so `/terminal` takes the `‹`
    /// too and its `⋮` joins the trailing slot in Phase 3.
    Back,
}

/// The bar's optional trailing action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarAction {
    /// `⚙` — pushes [`SETTINGS_ROUTE`]. The app's one entry point to
    /// `/settings`, which is why it lives in route metadata rather than on the
    /// Sessions screen (which no longer owns a bar at all).
    Settings,
    /// `⋮` — opens the terminal screen's overflow menu (Panes & tabs / Rename
    /// session / Detach session).
    ///
    /// `docs/terminal.html` v2 moved that menu off the terminal's own head row
    /// and onto the **app** bar, which is why it is route metadata; the menu
    /// itself still belongs to the feature, so `crate::shell` only forwards the
    /// press to
    /// [`open_terminal_menu`](crate::features::terminal::presentation::pages::open_terminal_menu)
    /// (the same shell-forwards/feature-owns split the `⚙` follows in the
    /// other direction).
    TerminalMenu,
}

/// Everything the shell's persistent top bar needs to render one route.
///
/// `Copy` on purpose: it rides inside an [`RwSignal`], which is `Send + Sync`
/// bounded, and every field is a plain enum over `'static` data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteMeta {
    /// Where the centered title comes from.
    pub title: BarTitle,
    /// What the leading slot shows.
    pub leading: BarLeading,
    /// The trailing icon slot, if this route carries one.
    pub action: Option<BarAction>,
}

impl RouteMeta {
    /// This route's bar title for the current frame — a [`BarTitle::Fixed`]
    /// verbatim, or the connected host read (tracked) off `connection`.
    pub fn resolve_title(&self, connection: &ConnectionController) -> String {
        match self.title {
            BarTitle::Fixed(title) => title.to_string(),
            BarTitle::ConnectedHost => connection
                .server_info
                .get()
                .map(|info| host_of(&info.endpoint).to_string())
                .unwrap_or_else(|| DISCONNECTED_TITLE.to_string()),
        }
    }
}

/// The `/settings` route — the bar's `⚙` destination.
pub const SETTINGS_ROUTE: &str = "/settings";
/// The bar's title while no connection is up: unreachable behind the session
/// guard, but [`BarTitle::ConnectedHost`] always has a slot to fill.
const DISCONNECTED_TITLE: &str = "muxrd";

/// The bar metadata for `path`, or `None` for a route that carries **no** bar.
///
/// `/` is the only such route today: the Connect screen is a full-bleed design
/// with no chrome above it (and the cold-start splash overlays exactly that
/// route), so the shell's bar collapses to zero height there.
pub fn route_meta(path: &str) -> Option<RouteMeta> {
    match path {
        "/sessions" => Some(RouteMeta {
            title: BarTitle::ConnectedHost,
            leading: BarLeading::Brand,
            action: Some(BarAction::Settings),
        }),
        "/terminal" => Some(RouteMeta {
            title: BarTitle::Fixed("terminal"),
            leading: BarLeading::Back,
            action: Some(BarAction::TerminalMenu),
        }),
        SETTINGS_ROUTE => Some(RouteMeta {
            title: BarTitle::Fixed("server settings"),
            leading: BarLeading::Back,
            action: None,
        }),
        _ => None,
    }
}

/// Which way the bar's title/leading crossfade travels on the next change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BarDirection {
    /// Forward navigation: the new title enters from the trailing edge.
    #[default]
    Forward,
    /// Back navigation: the new title enters from the leading edge.
    Back,
}

/// One frame's worth of bar state: the route on top (if it has a bar), the
/// direction the last change travelled, and whether a modal covers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BarState {
    /// The topmost *titled* route's metadata, or `None` for a bar-less route.
    pub meta: Option<RouteMeta>,
    /// The crossfade direction for this change.
    pub direction: BarDirection,
    /// Whether a **transparent overlay** (a sheet, a dialog) sits above the
    /// route the bar is titling — the live page-stack depth is deeper than the
    /// top mirror entry's, or the app's own overlay accounting says one is up
    /// ([`NavState::overlays`]).
    ///
    /// The mirror already had everything needed to answer this: a modal is
    /// pushed as a page *builder* straight through the
    /// [`NavigatorController`], so it adds depth with **no** mirror entry (the
    /// very property that keeps it from retitling the bar). The gap between
    /// the two counts is therefore exactly "how many overlays are up".
    ///
    /// `crate::shell` hands it to [`crate::ui::topbar::TopBarView::overlay`],
    /// which is what closes the modality hole the floating bar opened: the bar
    /// is painted over by the navigator's own scrim nowhere, because it floats
    /// *above* the navigator, so without this it stayed lit and tappable under
    /// an open sheet.
    pub overlay_active: bool,
    /// Whether the route on top has asked the bar to get out of the way
    /// entirely — the terminal screen's **fullscreen** state, published
    /// through [`Nav::set_fullscreen`].
    ///
    /// A *second input to the bar's existing hidden flag*, not a second hide
    /// mechanism: `crate::shell` ORs this with "this route has no bar at all"
    /// and hands the sum to [`crate::ui::topbar::TopBarView::hidden`], so the
    /// bar plays exactly the reveal it already plays when leaving a titled
    /// route for `/`. It rides here rather than in [`RouteMeta`] because it is
    /// per-*screen-state*, not per-route: the same `/terminal` is both
    /// fullscreen and not over its lifetime.
    ///
    /// The screen clears it on unmount (its `Component`'s `on_cleanup`), which
    /// is what stops a back-nav out of fullscreen from stranding a hidden bar
    /// on the route below.
    pub fullscreen: bool,
}

/// The page transition **every** path navigation through [`Nav`] carries: the
/// Glyph screen transition (a 16dp directional slide + cross-fade, 340ms
/// spatial enter / 150ms exit, Glyph `spatial`/`exit` easings), theme-timed so
/// `frust`'s own `resolve_spec` collapses it to a ≤120ms linear cross-fade
/// under `reduce_motion`.
///
/// Wired here, at the one seam every route push funnels through, rather than
/// per-screen — and deliberately **not** as the navigator's default transition,
/// which would also catch the sheet (which owns its `SlideUp`), the dialogs,
/// and the start-location `replace` the splash hands off over.
pub const fn route_transition() -> TransitionSpec {
    TransitionSpec::glyph()
}

// ---------------------------------------------------------------------------
// The app's navigation handle
// ---------------------------------------------------------------------------

/// The route-meta mirror behind [`Nav`] — see the [module docs](self).
struct NavState {
    /// One entry per path navigation, in stack order: the page-stack depth the
    /// navigation lands at, and the bar metadata for the route it landed on
    /// (`None` for a bar-less route like `/`).
    stack: Vec<(usize, Option<RouteMeta>)>,
    /// The last navigator depth [`Self::reconcile`] observed. A *fall* below
    /// this is what identifies a pop that bypassed [`Nav`] — a plain equality
    /// check would misread the frame between a recorded push and the rebuild
    /// that applies it, when `depth()` still reports the pre-push value.
    last_seen_depth: usize,
    /// Which way the last change travelled.
    direction: BarDirection,
    /// Set by [`Nav::pop`]: this handle dropped a route entry, but
    /// [`NavigatorController::depth`] still reports the **pre**-pop value
    /// until the next rebuild applies the queued op.
    ///
    /// [`Self::overlay_active`] is the one reader that cares. Its question —
    /// "is the live depth deeper than the top route entry?" — is answered
    /// `true` by both a real overlay push *and* by that one stale frame, and
    /// the two are indistinguishable from depth alone. So the pop path says
    /// so, and the flag clears itself the moment the live depth catches up.
    /// (The mirror's other stale case is the opposite sign — a fresh
    /// [`Nav::push`] reads a depth that is too *shallow*, which answers
    /// `false` and needs no help.)
    pending_pop: bool,
    /// How many overlays **this app's own helpers** ([`crate::ui::sheet`],
    /// [`crate::ui::dialogs`], the terminal's overflow menu) have pushed and
    /// not yet seen popped — the synchronous half of
    /// [`BarState::overlay_active`], and the reason the bar can no longer stay
    /// dimmed after a modal closes.
    ///
    /// The depth-derived answer alone is one rebuild stale **by
    /// construction**: `RenderRoot::rebuild` runs the whole app-logic closure
    /// (bar included) *before* the navigator drains its queued ops, so the
    /// frame that applies a push or a pop still publishes the previous depth.
    /// A push is harmless (the overlay's own entrance keeps frames coming);
    /// a **pop** is not — the follow-up frame publishes the stale dimmed
    /// state, nothing else asks for a frame, and the desktop loop parks with
    /// the bar dimmed and inert until unrelated input.
    ///
    /// So the app counts its own overlays and each change *publishes*
    /// ([`Nav::publish`]): a tracked write dirties the shell's scope, which is
    /// what schedules the very frame the depth fact could not.
    ///
    /// Incremented by [`Nav::begin_overlay`], decremented by
    /// [`Nav::end_overlay`] (the pusher's pop-completion callback — the one
    /// place that runs exactly once for *every* dismissal path), and reset
    /// wholesale by [`Self::reconcile`]'s backstop so a dismissal that reached
    /// neither can never strand it high.
    overlays: usize,
    /// …of those, how many the app has already told the navigator to pop but
    /// whose pop the navigator has not applied yet.
    ///
    /// [`Nav::dismiss_overlay`]'s half of the pair, and the one that makes the
    /// un-dim **synchronous**: the app-owned dismissal seams (a sheet's
    /// staged exit, a dialog's own action buttons, a sheet row that closes
    /// itself) know a pop is coming a full rebuild before
    /// [`NavigatorController::depth`] reflects it, so both terms of
    /// [`Self::overlay_active`] discount it — the counter directly, and the
    /// depth check by requiring the gap to exceed it.
    ///
    /// Cleared by [`Nav::end_overlay`] (the same completion callback that
    /// drops [`Self::overlays`]) and by the reconcile backstop. A dismissal
    /// path the app cannot observe (`frust::glyph::dialog`'s own scrim/Escape,
    /// a menu, an Android back press) simply never sets it: that costs the
    /// *synchronous* un-dim, never correctness — the completion callback and
    /// the backstop still land it on the next pass.
    overlays_closing: usize,
    /// The query of the most recent path navigation — see [`Nav::param`].
    query: RouteParams,
}

impl NavState {
    /// Drop every entry recorded deeper than `live` — but only once the live
    /// depth has actually *fallen* since the last observation (see
    /// [`Self::last_seen_depth`]). Returns whether anything was dropped.
    fn reconcile(&mut self, live: usize) -> bool {
        let fell = live < self.last_seen_depth;
        self.last_seen_depth = live;
        if self.pending_pop && live <= self.route_depth() {
            // The queued pop has been applied; depth is authoritative again.
            self.pending_pop = false;
        }
        // The overlay accounting's backstop: the live depth has fallen back to
        // (or below) the route stack, so nothing transparent is up — whatever
        // the counters still believe. A dismissal that reached neither
        // `Nav::end_overlay` nor `Nav::dismiss_overlay` (an Android back press
        // straight through `BackPolicy::Pop`, a `Router::go` that replaced the
        // overlay page and dropped its completion callback with it) cannot
        // strand a dimmed, inert bar behind it.
        //
        // Depth-*fall* gated like the retain below, and for the same reason:
        // an un-gated clamp would fire on the frame a sheet is pushed, when
        // `live` is still the pre-push value and the counter is the only thing
        // that knows better.
        if fell && live <= self.route_depth() {
            self.overlays = 0;
            self.overlays_closing = 0;
        }
        if !fell {
            return false;
        }
        let before = self.stack.len();
        self.stack.retain(|(depth, _)| *depth <= live);
        let dropped = self.stack.len() != before;
        if dropped {
            self.direction = BarDirection::Back;
        }
        dropped
    }

    /// The page-stack depth the *route* stack occupies — the deepest recorded
    /// mirror entry, or `0` for an empty mirror.
    fn route_depth(&self) -> usize {
        self.stack.last().map_or(0, |(depth, _)| *depth)
    }

    /// The depth a route landing *now* sits at: the deeper of the mirror's own
    /// stack and the navigator's last-published depth **less the pops already
    /// queued against it** ([`Self::overlays_closing`]/[`Self::pending_pop`]).
    /// [`Nav::record_push`] records one past it.
    ///
    /// # Why the discount, and what it costs to skip
    ///
    /// `live` is advisory — published at the last rebuild — and the app can
    /// have told the navigator to *drop* pages since. The Sessions management
    /// sheet's **Attach** row is exactly that handler: it closes the sheet
    /// ([`crate::ui::sheet::SheetHandle::close`], which enqueues a raw
    /// controller pop and reports [`Nav::dismiss_overlay`]) and then pushes
    /// `/terminal` — both in one rebuild, so `live` still counts the sheet's
    /// page while the push is being recorded. Reading it undiscounted records
    /// the route one too deep, and *permanently*: the mirror entry is never
    /// re-derived. The next depth fall past it — the terminal's own ⋮ menu
    /// opening and closing — makes [`Self::reconcile`]'s retain drop the
    /// route's own entry, and the bar retitles to the route underneath, sticks
    /// modal, and loses its back affordance.
    ///
    /// The raw `live` still feeds [`Self::bar`] at that call site: only the
    /// *recorded* depth is a lasting fact, and
    /// [`overlay_active`](Self::overlay_active) already discounts
    /// [`Self::overlays_closing`] in its own terms.
    ///
    /// # The accepted boundary
    ///
    /// [`Self::pending_pop`] is a **bool**, so two bare [`Nav::pop`]s queued
    /// before any drain discount as one and this answers one too deep again.
    /// No call site in the app produces that shape (a `‹` press and the
    /// terminal's detach each pop once, a rebuild apart), and widening the flag
    /// to a counter would change what
    /// [`overlay_active`](Self::overlay_active) reads — so it is documented
    /// here rather than redesigned.
    fn record_push_depth(&self, live: usize) -> usize {
        let queued_pops = self.overlays_closing + usize::from(self.pending_pop);
        self.route_depth().max(live.saturating_sub(queued_pops))
    }

    /// Count one more app-pushed overlay, draining any unobserved depth fall
    /// **first** — [`Nav::begin_overlay`]'s whole body beyond the publish.
    ///
    /// The order is the fix. [`Self::reconcile`]'s backstop clamps the
    /// counters flat on a depth fall, and a fall is only *spent* once
    /// something reconciles at the fallen depth — so a fall nothing observed
    /// is still sitting in [`Self::last_seen_depth`], waiting to be consumed
    /// by whichever reconcile comes next. Incrementing before that reconcile
    /// hands the fresh overlay's count to it as the thing it clamps.
    ///
    /// The terminal's **menu → sheet** chain is that trace: `frust::glyph`'s
    /// menu pops itself from inside its own paint, the app never sees it, and
    /// the loop can park before any [`Nav::bar`] read observes the drop. The
    /// pop-completion callback then runs [`Nav::end_overlay`] and opens the
    /// sheet in one pass; the sheet's `overlays += 1` lands *ahead* of the
    /// reconcile that spends the menu's fall, and is wiped by it. The sheet
    /// then dims the bar on the depth term alone — until its dismissal, where
    /// [`Nav::dismiss_overlay`]'s `overlays_closing < overlays` guard finds
    /// nothing to count, the depth term keeps answering `true`, and the loop
    /// parks on a dimmed, inert bar. (A dialog whose `on_confirm` opens
    /// another overlay carries the identical exposure.)
    ///
    /// Reconciling here is safe precisely because it is the *pre*-increment
    /// state it reconciles: the clamp can only wipe counts that a fall has
    /// already invalidated, and the retain only drops mirror entries deeper
    /// than the live depth.
    fn begin_overlay(&mut self, live: usize) {
        self.reconcile(live);
        self.overlays += 1;
    }

    /// How many overlays the app believes are up right now: the ones it
    /// pushed, less the ones it has already told the navigator to pop (see
    /// [`Self::overlays`]/[`Self::overlays_closing`]).
    fn open_overlays(&self) -> usize {
        self.overlays.saturating_sub(self.overlays_closing)
    }

    /// Whether a transparent overlay sits above the top route — see
    /// [`BarState::overlay_active`]. `live` is the navigator's own depth.
    ///
    /// Two independent answers, either of which dims the bar:
    ///
    /// - the app's **own** accounting ([`Self::open_overlays`]), which is
    ///   exact from the moment a helper pushes to the moment the app closes
    ///   it, staleness and all;
    /// - the **depth** gap, which still covers every overlay pushed straight
    ///   through the [`NavigatorController`] without passing a helper.
    ///
    /// The depth gap is measured past [`Self::overlays_closing`] rather than
    /// past zero: an overlay whose pop is queued is still counted by `live`
    /// for one more rebuild, and reading that as "a modal is up" is exactly
    /// the staleness this pair exists to erase.
    fn overlay_active(&self, live: usize) -> bool {
        self.open_overlays() > 0
            || (!self.pending_pop && live > self.route_depth() + self.overlays_closing)
    }

    /// The current bar state: the deepest recorded entry's metadata, whether
    /// `live` shows an overlay above it, and the topmost screen's
    /// `fullscreen` request (which lives outside this mirror — see
    /// [`Nav::fullscreen`]).
    fn bar(&self, live: usize, fullscreen: bool) -> BarState {
        BarState {
            meta: self.stack.last().and_then(|(_, meta)| *meta),
            direction: self.direction,
            overlay_active: self.overlay_active(live),
            fullscreen,
        }
    }
}

/// A cheap, clonable path-pushing handle over the app's one [`Router`] — what
/// a screen navigates with. See the [module docs](self) for why this exists
/// (and why it is not in frust's context).
///
/// Cloning is an `Rc` bump, so a press closure can hold its own copy; every
/// clone drives the *same* router — hence the same route table, the same
/// [`session_guard_with`] guard, and the same route-meta mirror the shell's bar
/// reads. A screen resolves one with [`app_nav`] in its `Component::init` and
/// keeps it in its own `State` — never resolves one from inside a press
/// handler (the discipline every other app-scoped dependency follows).
#[derive(Clone)]
pub struct Nav {
    router: Rc<Router<AppState>>,
    /// The route-meta mirror, shared by every clone.
    state: Rc<RefCell<NavState>>,
    /// The mirror's published, trackable face — what wakes the shell's rebuild
    /// when a navigation retitles the bar.
    current: RwSignal<BarState>,
    /// The topmost screen's fullscreen request — see [`BarFullscreen`]. A
    /// signal of its own rather than a [`NavState`] field precisely because it
    /// is the one piece of bar state a screen must be able to clear from a
    /// `Send + Sync` context (see that type's docs).
    fullscreen: RwSignal<bool>,
}

/// A `Send + Sync` handle to just the bar's fullscreen flag — see
/// [`BarState::fullscreen`].
///
/// [`Nav`] itself is `Rc`-backed and so can be captured by neither
/// [`frust::on_cleanup`] nor anything else `Send + Sync`-bounded, and clearing
/// this flag on unmount is exactly a cleanup's job (a screen that left
/// fullscreen set would strand a hidden bar on the route it popped back to).
/// An [`RwSignal`] *is* `Send + Sync + Copy`, so the flag lives in one and this
/// hands out a named, single-purpose face of it rather than the raw signal.
///
/// Writing it wakes the shell's rebuild on its own: [`Nav::bar`] reads it
/// **tracked** every frame, which is the whole subscription.
#[derive(Clone, Copy)]
pub struct BarFullscreen(RwSignal<bool>);

impl BarFullscreen {
    /// Ask the bar to animate away (`true`) or come back (`false`).
    pub fn set(self, fullscreen: bool) {
        if self.0.get_untracked() != fullscreen {
            self.0.set(fullscreen);
        }
    }
}

impl Nav {
    /// Push `location`'s page onto the stack, retaining the page below —
    /// guarded, like every other resolution through this router, and carrying
    /// [`route_transition`]'s Glyph slide.
    ///
    /// Reimplements [`Router::push`]'s two lines rather than calling it,
    /// because `Router` has no push-with-a-transition entry point: the leaf of
    /// the resolved chain is pushed through
    /// [`NavigatorController::push_with`] so *this* push carries the spec (and
    /// a later pop reverses it) without making the Glyph slide the navigator's
    /// blanket default. An unmatched location falls back to the router's own
    /// error page, transition-less.
    pub fn push(&self, location: &str) {
        match self.router.resolve(location) {
            Resolution::Matched {
                location, pages, ..
            } => {
                let Some(leaf) = pages.into_iter().next_back() else {
                    return;
                };
                self.router
                    .controller()
                    .push_with(move || leaf.build(), route_transition());
                self.record_push(&location);
            }
            Resolution::Error { .. } => self.router.push(location),
        }
    }

    /// Reset the stack to `location`'s matched chain (replace semantics), and
    /// with it the bar mirror — a `go` is a new stack, not a deeper one.
    ///
    /// The mirror is reseeded at depth 1 because muxr's table is entirely
    /// **flat**: a matched chain is one page. A nested route would land its leaf
    /// deeper than that and want the whole chain mirrored — reseeding is
    /// correct-by-table today, not correct-in-general, and the tripwire is that
    /// adding a child route to [`build_routes`] makes it wrong.
    ///
    /// Unlike [`push`](Self::push) this carries **no** transition: a `go`
    /// replaces the root, and the one caller of that today is the launch
    /// `handle_location` the splash hands off over — which must not animate.
    ///
    /// # What it leaves behind, knowingly
    ///
    /// The reset clears the queued-pop flag and the overlay counters (below)
    /// but **not** [`NavState::last_seen_depth`], so a `go` off a deeper stack
    /// leaves that reading high — an unobserved fall the next
    /// [`reconcile`](NavState::reconcile) spends. Left as it is rather than
    /// fixed: the app's one `go` is the launch `handle_location`, off a
    /// one-page stack with no fall to leave behind, so it is latent rather
    /// than live, and resetting it here would put a second owner on the flag
    /// whose whole invariant is that `reconcile` alone moves it.
    pub fn go(&self, location: &str) {
        self.router.go(location);
        let (meta, query) = match self.router.resolve(location) {
            Resolution::Matched { location, .. } => {
                (route_meta(&location.path), location.query.clone())
            }
            Resolution::Error { .. } => (None, RouteParams::new()),
        };
        let live = self.router.controller().depth();
        let mut state = self.state.borrow_mut();
        state.stack = vec![(1, meta)];
        state.query = query;
        state.direction = BarDirection::Forward;
        // A `go` resets the whole stack, so nothing survives above the route
        // it lands on — including the staleness a queued pop would leave, and
        // including the overlay accounting. The latter is not merely tidy:
        // `Router::go` *replaces* the top page, and a replaced page's
        // pop-completion callback is dropped rather than delivered, so an
        // overlay that was on top when this ran would otherwise leave its
        // count behind with no callback left to clear it.
        state.pending_pop = false;
        state.overlays = 0;
        state.overlays_closing = 0;
        let bar = state.bar(live.min(1), self.fullscreen.get_untracked());
        drop(state);
        self.publish(bar);
    }

    /// Pop the top page — the shell bar's `‹`.
    ///
    /// The mirror entry is dropped here rather than left to the next
    /// [`reconcile`](NavState::reconcile), so the bar retitles on the very
    /// frame the pop is recorded instead of a frame later; the reconcile is
    /// still what covers a pop that never came through here (an Android back
    /// gesture).
    pub fn pop(&self) {
        self.router.controller().pop();
        let mut state = self.state.borrow_mut();
        state.stack.pop();
        state.direction = BarDirection::Back;
        // `depth()` still reports the pre-pop value until the next rebuild
        // drains the queued op; without this the frame in between would read
        // as "an overlay is up" and dim the bar it just retitled (see
        // `NavState::pending_pop`).
        state.pending_pop = true;
        let live = state.route_depth();
        let bar = state.bar(live, self.fullscreen.get_untracked());
        drop(state);
        self.publish(bar);
    }

    /// The shell bar's state for this frame: the mirror reconciled against the
    /// navigator's authoritative depth, read **tracked** so a later navigation
    /// wakes the shell's rebuild.
    ///
    /// The reconcile may publish (a pop that bypassed [`Nav`] is only visible
    /// once the navigator has applied it, i.e. one rebuild later) — that write
    /// is guarded by an inequality, so it happens at most once per real change
    /// and the following rebuild is clean.
    ///
    /// The publish is **unconditional** (rather than gated on the reconcile
    /// having dropped something) because
    /// [`overlay_active`](BarState::overlay_active) is a pure *depth* fact: a
    /// sheet pushed straight through the [`NavigatorController`] changes it
    /// without any navigation through this handle at all, so there is nothing
    /// else that could notice. `publish`'s own inequality guard is what keeps
    /// that from waking a rebuild per frame.
    pub fn bar(&self) -> BarState {
        let live = self.router.controller().depth();
        // Tracked, like `current` below: this is the *only* subscription the
        // fullscreen flag has, and it is what makes a screen's
        // `set_fullscreen` wake the rebuild that animates the bar away.
        let fullscreen = self.fullscreen.get();
        let mut state = self.state.borrow_mut();
        state.reconcile(live);
        let reconciled = state.bar(live, fullscreen);
        drop(state);
        self.publish(reconciled);
        self.current.get()
    }

    /// Record a landed path navigation in the mirror.
    ///
    /// The recorded depth is one past whichever is deeper: the navigator's
    /// last-published depth **less whatever pops are already queued against
    /// it**, or the deepest entry already recorded. All three terms matter —
    /// `depth()` is stale between a press handler and the rebuild that applies
    /// its op (so two pushes in one frame would otherwise record the same
    /// depth), the mirror alone cannot see the transparent pages pushed
    /// straight through the controller, and a handler that closes an overlay
    /// before pushing would otherwise record one too deep, for good. See
    /// [`NavState::record_push_depth`], which owns the arithmetic and the
    /// trace.
    fn record_push(&self, location: &Location) {
        let live = self.router.controller().depth();
        let mut state = self.state.borrow_mut();
        let deepest = state.record_push_depth(live);
        state.stack.push((deepest + 1, route_meta(&location.path)));
        state.direction = BarDirection::Forward;
        state.query = location.query.clone();
        let bar = state.bar(live, self.fullscreen.get_untracked());
        drop(state);
        self.publish(bar);
    }

    // -- the overlay accounting (see `NavState::overlays`) ------------------

    /// Record that an overlay page (a sheet, a dialog, a menu) has just been
    /// pushed — call it from the helper that pushes one, before or after the
    /// push itself.
    ///
    /// Pairs with [`end_overlay`](Self::end_overlay), which **must** be wired
    /// to the pusher's own pop-completion callback
    /// (`NavigatorController::push_transparent_for_result`'s `on_result`,
    /// `frust::glyph::show_glyph_dialog`/`show_glyph_menu`'s `on_result`) —
    /// the one place that runs exactly once for every dismissal path there is.
    ///
    /// The count is taken **after** a reconcile, never before one: an overlay
    /// opened out of the overlay before it (the terminal's menu → sheet chain)
    /// would otherwise hand its own fresh count to the backstop as the stale
    /// fall to clamp. [`NavState::begin_overlay`] carries that trace.
    pub fn begin_overlay(&self) {
        let live = self.router.controller().depth();
        let mut state = self.state.borrow_mut();
        state.begin_overlay(live);
        let bar = state.bar(live, self.fullscreen.get_untracked());
        drop(state);
        self.publish(bar);
    }

    /// Record that the app has just told one of its own overlays to pop — call
    /// it immediately before the `pop`/`pop_with_result` that closes it.
    ///
    /// This is the **synchronous** half of the accounting, and the only reason
    /// the bar lights up without waiting for unrelated input: a pop is applied
    /// at the *next* rebuild, whose app logic (the bar included) has already
    /// run by then, and the pusher's completion callback is not delivered
    /// until the input pass after that. Publishing here — a rebuild earlier
    /// than either — is what dirties the shell's scope and schedules the frame
    /// that paints a lit bar.
    ///
    /// Best-effort by design: a dismissal path the app never sees simply does
    /// not call it, which costs that path its synchronous un-dim and nothing
    /// else. Calling it more often than there are open overlays cannot
    /// under-count either — it saturates.
    pub fn dismiss_overlay(&self) {
        let live = self.router.controller().depth();
        let mut state = self.state.borrow_mut();
        if state.overlays_closing < state.overlays {
            state.overlays_closing += 1;
        }
        let bar = state.bar(live, self.fullscreen.get_untracked());
        drop(state);
        self.publish(bar);
    }

    /// Record that an overlay page has actually been popped — call it from the
    /// pusher's pop-completion callback, which the navigator delivers exactly
    /// once per pushed page, on every dismissal route (an action button, a
    /// scrim tap, `Escape`, an Android back press).
    ///
    /// Saturating, like every other step here: the accounting's job is to be
    /// unable to strand a dimmed bar, so every operation on it is safe to run
    /// one time too many.
    pub fn end_overlay(&self) {
        let live = self.router.controller().depth();
        let mut state = self.state.borrow_mut();
        state.overlays = state.overlays.saturating_sub(1);
        state.overlays_closing = state.overlays_closing.saturating_sub(1);
        let bar = state.bar(live, self.fullscreen.get_untracked());
        drop(state);
        self.publish(bar);
    }

    /// The `?key=…` value the most recent path navigation carried, if any.
    ///
    /// **Read it in a screen's `Component::init`, and nowhere else.** A route
    /// builder receives only the *path* params
    /// ([`RouteParams`] captured from `:segment`s) — frust's own
    /// `ResolvedPage` never carries the query through to it, so a route like
    /// `/terminal?session=s-1` cannot read its own parameter the ordinary way
    /// (a framework gap, filed). This is the app-side answer, and it is exact
    /// precisely at `init` time: a page is built during the very push that
    /// recorded it, so "the most recent navigation" *is* this screen's own.
    /// Read later — after another push landed — it would answer for that one
    /// instead, which is why nothing else calls it.
    pub fn param(&self, key: &str) -> Option<String> {
        self.state.borrow().query.get(key).cloned()
    }

    /// The `Send + Sync` handle to the bar's fullscreen flag — what a screen
    /// hands to its `on_cleanup`. See [`BarFullscreen`].
    pub fn fullscreen(&self) -> BarFullscreen {
        BarFullscreen(self.fullscreen)
    }

    /// Publish the topmost screen's fullscreen request to the shell's bar —
    /// see [`BarState::fullscreen`]. Idempotent: re-setting the current value
    /// wakes nothing.
    pub fn set_fullscreen(&self, fullscreen: bool) {
        self.fullscreen().set(fullscreen);
    }

    /// Publish `bar` if it differs from what is already published — an
    /// unconditional `set` would wake a rebuild for every navigation that left
    /// the bar untouched.
    fn publish(&self, bar: BarState) {
        if self.current.get_untracked() != bar {
            self.current.set(bar);
        }
    }

    /// Resolve `location` **without** navigating — the pure half a
    /// [`push`](Self::push) consults, redirects and all. No screen needs this;
    /// it is how a test asserts what a push *would* do (the guard bouncing
    /// `/sessions` to `/`) without mounting a navigator.
    pub fn resolve(&self, location: &str) -> Resolution<AppState> {
        self.router.resolve(location)
    }
}

thread_local! {
    /// The one [`Nav`] per UI thread — see [`app_nav`] and the
    /// [module docs](self).
    static APP_NAV: RefCell<Option<Nav>> = const { RefCell::new(None) };
}

/// Build the app's one [`Router`] over `controller` — the full route table
/// plus the [`session_guard_with`] guard closed over `connection` — and publish
/// it as this thread's [`Nav`].
///
/// Called once from `crate::MuxrApp::init`, which owns the returned handle as
/// `crate::AppState::nav`. A headless test that drives its **own**
/// [`NavigatorController`] (every screen suite in `tests/` does, to keep the
/// screen outside a navigator and so visible to accessibility) calls this too,
/// right after `MuxrApp::init`, so the screen under test pushes through the
/// real table and the real guard rather than a second, drifting copy.
pub fn install_app_router(
    controller: &NavigatorController<AppState>,
    connection: Arc<ConnectionController>,
) -> Rc<Router<AppState>> {
    // The route table is built *before* the router (so there is no
    // circularity), which is exactly why a page builder cannot be handed the
    // router — see the module docs.
    let routes = build_routes(controller.clone());
    let router = Rc::new(
        Router::with_controller(controller, routes).redirect(session_guard_with(connection)),
    );
    APP_NAV.with(|cell| {
        *cell.borrow_mut() = Some(Nav {
            router: Rc::clone(&router),
            state: Rc::new(RefCell::new(NavState {
                stack: Vec::new(),
                last_seen_depth: 0,
                direction: BarDirection::Forward,
                pending_pop: false,
                overlays: 0,
                overlays_closing: 0,
                query: RouteParams::new(),
            })),
            current: RwSignal::new(BarState::default()),
            fullscreen: RwSignal::new(false),
        });
    });
    router
}

/// This thread's [`Nav`] — the handle a screen resolves in its
/// `Component::init`.
///
/// # Panics
///
/// Panics if [`install_app_router`] has not run on this thread (the same
/// contract `crate::connection::expect_connection_controller` documents).
/// Every call site runs after `MuxrApp::init`, so this is never hit in
/// practice; a test that mounts a screen without the composition root gets a
/// loud, immediate failure rather than a silently dead button.
pub fn app_nav() -> Nav {
    APP_NAV.with(|cell| cell.borrow().clone()).expect(
        "app_nav: routes::install_app_router must run first (MuxrApp::init calls it; a test \
         driving its own NavigatorController calls it right after)",
    )
}

// ---------------------------------------------------------------------------
// The guard
// ---------------------------------------------------------------------------

/// Muxr's session guard, closed over the app's one
/// [`ConnectionController`]: a top-level redirect (see
/// [`frust::Router::redirect`], wired on by [`install_app_router`]) bouncing an
/// unauthenticated push to `/sessions`/`/terminal` back to `/`. `/` and
/// `/settings` are never guarded.
///
/// **Capture, never context.** The controller is handed in at construction —
/// in the composition root, where it is already in hand — and read through the
/// captured `Arc`. Nothing here touches `use_context`, which is what makes the
/// guard safe to run from *any* call stack: a resolution can be triggered by a
/// press handler, and a press handler has no ambient reactive owner on desktop
/// (`frust-shell-desktop`'s `dispatch` runs outside
/// `ReactiveRuntime::with_owner`), so an ambient read would return `None` and
/// panic. The unit test below is that guarantee's tripwire — it invokes the
/// guard with no owner set at all.
///
/// **Navigation-time only.** [`frust::Router::resolve`] (which this feeds)
/// runs on `go`/`push`/`handle_location`, never on its own — so a connection
/// that drops *while* `/sessions`/`/terminal` is already on-screen does not
/// reactively bounce it back to `/`. A live, reactive bounce-on-disconnect is
/// a functional-phase concern, deliberately out of scope here (this reads the
/// signal `get_untracked`, establishing no reactive dependency at all).
pub(crate) fn session_guard_with(
    connection: Arc<ConnectionController>,
) -> impl Fn(&Location) -> Option<String> + 'static {
    move |loc: &Location| {
        let guarded = matches!(loc.path.as_str(), "/sessions" | "/terminal");
        if guarded && !connection.connected.get_untracked() {
            Some(CONNECT_LOCATION.to_string())
        } else {
            None
        }
    }
}

/// Build the full route table. `controller` is created *before* this table
/// (so there is no circularity with the [`Router`] it attaches to) and cloned
/// into every builder — all four page builders receive it today (see
/// [`home_page`]'s doc): it is how a screen pushes a page *builder* (a dialog,
/// a sheet), where [`Nav`] is how it pushes a *path*.
pub(crate) fn build_routes(controller: NavigatorController<AppState>) -> Vec<Route<AppState>> {
    vec![
        Route::new("/", {
            let controller = controller.clone();
            move |_params: &RouteParams| home_page(controller.clone())
        }),
        Route::new("/sessions", {
            let controller = controller.clone();
            move |_params: &RouteParams| sessions_page(controller.clone())
        }),
        Route::new("/terminal", {
            let controller = controller.clone();
            move |_params: &RouteParams| terminal_page(controller.clone())
        }),
        Route::new("/settings", move |_params: &RouteParams| {
            settings_page(controller.clone())
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{ConnectTarget, TrustMode};

    fn target() -> ConnectTarget {
        ConnectTarget {
            host: "mock.example".to_string(),
            port: 7777,
            trust_mode: TrustMode::System,
            label: "Mock rig".to_string(),
            offer_credential: false,
        }
    }

    /// The regression test this whole seam exists for: the guard is invoked as
    /// a **plain function call, with no reactive owner anywhere on the stack**
    /// — no `ReactiveRuntime`, no `Owner`, no context. That is precisely the
    /// state a desktop press handler runs in, and a guard reading its
    /// controller out of ambient context would panic here instead of
    /// answering.
    #[test]
    fn the_guard_answers_with_no_reactive_owner_on_the_stack() {
        let connection = Arc::new(ConnectionController::detached());
        let guard = session_guard_with(Arc::clone(&connection));

        assert_eq!(
            guard(&Location::parse("/sessions")),
            Some(CONNECT_LOCATION.to_string()),
            "an unauthenticated /sessions bounces to Connect"
        );
        assert_eq!(
            guard(&Location::parse("/terminal?session=s-1")),
            Some(CONNECT_LOCATION.to_string()),
            "…and so does /terminal, query param and all"
        );
        assert_eq!(guard(&Location::parse("/")), None, "/ is never guarded");
        assert_eq!(
            guard(&Location::parse("/settings")),
            None,
            "/settings is never guarded"
        );

        connection.connect(target());

        assert_eq!(
            guard(&Location::parse("/sessions")),
            None,
            "a connected push is not redirected"
        );
        assert_eq!(guard(&Location::parse("/terminal")), None);
    }

    // -----------------------------------------------------------------------
    // Route metadata + the bar mirror
    // -----------------------------------------------------------------------

    #[test]
    fn connect_is_the_only_route_without_a_bar() {
        assert_eq!(route_meta("/"), None, "Connect is full-bleed, no bar");
        for path in ["/sessions", "/terminal", SETTINGS_ROUTE] {
            assert!(route_meta(path).is_some(), "{path} carries a bar");
        }
    }

    #[test]
    fn only_the_stacks_first_titled_route_wears_the_brand() {
        // The brand mark is the "you are at the bottom of the stack" cue; every
        // route pushed over it gets the `‹` instead (including /terminal, whose
        // mock shows a brand — see `BarLeading::Back`'s deviation note).
        assert_eq!(
            route_meta("/sessions").map(|m| m.leading),
            Some(BarLeading::Brand)
        );
        assert_eq!(
            route_meta("/terminal").map(|m| m.leading),
            Some(BarLeading::Back)
        );
        assert_eq!(
            route_meta(SETTINGS_ROUTE).map(|m| m.leading),
            Some(BarLeading::Back)
        );
    }

    #[test]
    fn the_gear_lives_on_sessions_and_the_kebab_on_the_terminal() {
        // `/settings` has exactly one entry point in the app, and since the
        // Sessions screen no longer owns a bar it has to be route metadata.
        // `/terminal`'s `⋮` is there for the mirror-image reason: v2 of its
        // mock moved the overflow menu onto the app bar.
        assert_eq!(
            route_meta("/sessions").and_then(|m| m.action),
            Some(BarAction::Settings)
        );
        assert_eq!(
            route_meta("/terminal").and_then(|m| m.action),
            Some(BarAction::TerminalMenu)
        );
        assert_eq!(route_meta(SETTINGS_ROUTE).and_then(|m| m.action), None);
    }

    #[test]
    fn every_route_push_carries_the_theme_timed_glyph_slide() {
        // The GlyphSlide contract, at the one seam that applies it: the Glyph
        // preset (16dp directional slide + cross-fade) under `ThemeDefault`
        // timing, which is what lets frust's own `resolve_spec` swap in the
        // theme's 340ms-spatial/150ms-exit tokens — and collapse the whole
        // thing to the reduced-motion cross-fade.
        let spec = route_transition();
        assert_eq!(spec.preset, frust::PageTransition::Glyph);
        assert_eq!(spec.timing, frust::Timing::ThemeDefault);
        assert!(spec.is_animated());
        assert_eq!(spec, TransitionSpec::glyph());
    }

    /// A mirror seeded with `entries`, at `last_seen_depth`.
    fn mirror(entries: &[(usize, Option<RouteMeta>)], last_seen_depth: usize) -> NavState {
        NavState {
            stack: entries.to_vec(),
            last_seen_depth,
            direction: BarDirection::Forward,
            pending_pop: false,
            overlays: 0,
            overlays_closing: 0,
            query: RouteParams::new(),
        }
    }

    #[test]
    fn a_pop_that_bypassed_the_handle_drops_its_mirror_entry() {
        // The Android back-gesture case: the navigator popped without anyone
        // telling `Nav`, so the only evidence is the depth falling.
        let mut state = mirror(
            &[
                (2, route_meta("/sessions")),
                (3, route_meta(SETTINGS_ROUTE)),
            ],
            3,
        );
        assert!(state.reconcile(2), "the /settings entry is dropped");
        assert_eq!(state.bar(2, false).meta, route_meta("/sessions"));
        assert_eq!(
            state.bar(2, false).direction,
            BarDirection::Back,
            "…and the bar crossfades backwards"
        );
        // Idempotent: a second reconcile at the same depth changes nothing.
        assert!(!state.reconcile(2));
    }

    #[test]
    fn a_freshly_recorded_push_survives_the_stale_depth_it_was_recorded_at() {
        // The regression a depth-equality reconcile would cause: `depth()` is
        // published at the last rebuild, so the frame a press handler records a
        // push it still reports the PRE-push value. Only a depth *fall*
        // reconciles.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 1);
        assert!(!state.reconcile(1), "nothing is dropped at a stale depth");
        assert_eq!(state.bar(1, false).meta, route_meta("/sessions"));
        // …and once the navigator applies it, the entry is still there.
        assert!(!state.reconcile(2));
        assert_eq!(state.bar(2, false).meta, route_meta("/sessions"));
    }

    #[test]
    fn a_transparent_overlay_never_retitles_the_bar() {
        // A sheet/dialog is pushed straight through the NavigatorController, so
        // it adds depth with no mirror entry — and popping it must not take the
        // covered route's entry with it.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 2);
        assert!(!state.reconcile(3), "the sheet pushed depth to 3");
        assert_eq!(state.bar(3, false).meta, route_meta("/sessions"));
        assert!(!state.reconcile(2), "…and dismissing it drops nothing");
        assert_eq!(state.bar(2, false).meta, route_meta("/sessions"));
    }

    #[test]
    fn a_bar_less_route_hides_the_bar_even_at_depth() {
        // The guard bouncing an unauthenticated /sessions to / records `/`'s
        // (absent) metadata, so the bar hides rather than titling a route the
        // push never reached.
        let state = mirror(&[(2, route_meta(CONNECT_LOCATION))], 2);
        assert_eq!(state.bar(2, false).meta, None);
    }

    // -----------------------------------------------------------------------
    // The overlay flag (the modality mirror)
    // -----------------------------------------------------------------------

    #[test]
    fn an_overlay_above_the_top_route_flags_the_bar_modal() {
        // The whole rule, in one line of arithmetic: a sheet is depth with no
        // mirror entry, so the gap between the live depth and the deepest
        // recorded route IS "an overlay is up".
        let mut state = mirror(&[(2, route_meta("/sessions"))], 2);
        assert!(!state.bar(2, false).overlay_active, "no overlay at rest");

        state.reconcile(3);
        assert!(state.bar(3, false).overlay_active, "the sheet dims the bar");
        assert_eq!(
            state.bar(3, false).meta,
            route_meta("/sessions"),
            "…without retitling it"
        );

        state.reconcile(2);
        assert!(
            !state.bar(2, false).overlay_active,
            "dismissing it gives the bar back"
        );
    }

    #[test]
    fn a_stacked_overlay_keeps_the_bar_modal_until_the_last_one_closes() {
        // A sheet whose row opens a dialog: depth 4 over a depth-2 route. The
        // flag is a comparison, not a counter, so it simply stays set.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 2);
        state.reconcile(4);
        assert!(state.bar(4, false).overlay_active);
        state.reconcile(3);
        assert!(state.bar(3, false).overlay_active, "one is still up");
        state.reconcile(2);
        assert!(!state.bar(2, false).overlay_active);
    }

    #[test]
    fn a_route_pop_never_reads_its_own_stale_depth_as_an_overlay() {
        // The regression `pending_pop` exists for. `Nav::pop` drops the mirror
        // entry immediately, but `depth()` still reports the pre-pop value
        // until the next rebuild drains the queued op — which looks exactly
        // like an overlay push. Without the flag the bar would dim and go
        // inert for the frame right after a `‹` tap.
        let mut state = mirror(
            &[
                (2, route_meta("/sessions")),
                (3, route_meta(SETTINGS_ROUTE)),
            ],
            3,
        );
        state.stack.pop();
        state.pending_pop = true;
        assert!(
            !state.bar(3, false).overlay_active,
            "the stale depth is a queued pop, not a sheet"
        );

        // …and the moment the navigator applies it, depth is authoritative
        // again — including for a sheet opened right afterwards.
        state.reconcile(2);
        assert!(!state.bar(2, false).overlay_active);
        state.reconcile(3);
        assert!(
            state.bar(3, false).overlay_active,
            "a real overlay still registers once the pop has landed"
        );
    }

    // -----------------------------------------------------------------------
    // The overlay accounting (the synchronous half of the same flag)
    // -----------------------------------------------------------------------

    #[test]
    fn an_app_pushed_overlay_dims_the_bar_before_the_depth_reports_it() {
        // `Nav::begin_overlay`'s half: the push is queued, so `depth()` still
        // answers 2 — the counter is the only thing that knows a sheet is on
        // its way in.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 2);
        state.overlays = 1;
        assert!(
            state.bar(2, false).overlay_active,
            "dimmed at the stale depth"
        );
        // …and it stays dimmed once the navigator catches up, now by both
        // halves at once.
        state.reconcile(3);
        assert!(state.bar(3, false).overlay_active);
    }

    #[test]
    fn closing_an_overlay_lifts_the_bar_a_rebuild_before_the_depth_falls() {
        // THE regression this accounting exists for. The pop is queued from a
        // scrim tap / an action button, so `depth()` still counts the overlay's
        // page for one more rebuild — and that rebuild is the last one anything
        // asks for. Reading it as "a modal is up" is what left the bar dimmed
        // and inert until unrelated input.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 2);
        state.overlays = 1;
        state.reconcile(3);
        assert!(state.bar(3, false).overlay_active);

        // `Nav::dismiss_overlay`: still depth 3, but the app knows better.
        state.overlays_closing = 1;
        assert!(
            !state.bar(3, false).overlay_active,
            "the bar lights up on the frame the dismissal is enqueued"
        );

        // The pop lands; the completion callback clears both counts.
        state.reconcile(2);
        state.overlays = 0;
        state.overlays_closing = 0;
        assert!(!state.bar(2, false).overlay_active);
    }

    #[test]
    fn a_stack_of_overlays_stays_dimmed_until_the_last_one_is_closing() {
        // A sheet whose row opens a dialog: closing the dialog must not lift
        // the bar off the sheet still underneath it.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 2);
        state.overlays = 2;
        state.reconcile(4);
        assert!(state.bar(4, false).overlay_active);

        state.overlays_closing = 1;
        assert!(
            state.bar(4, false).overlay_active,
            "one of the two is on its way out; the other is still up"
        );

        // The dialog's pop lands and its completion callback runs.
        state.reconcile(3);
        state.overlays = 1;
        state.overlays_closing = 0;
        assert!(
            state.bar(3, false).overlay_active,
            "the sheet still dims it"
        );

        // Now the sheet itself.
        state.overlays_closing = 1;
        assert!(!state.bar(3, false).overlay_active);
    }

    #[test]
    fn an_overlay_that_never_passed_a_helper_still_dims_the_bar() {
        // The depth gap stays in the answer: a page pushed straight through
        // the controller (a test's `push_transparent`, a frust helper the app
        // has not wrapped) has no accounting at all.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 2);
        state.reconcile(3);
        assert!(state.bar(3, false).overlay_active);
        assert_eq!(state.overlays, 0, "…with nothing counted");
    }

    #[test]
    fn a_dismissal_the_app_never_saw_cannot_strand_a_dimmed_bar() {
        // The backstop, on the path item 7 of this fix leaves alone: an
        // Android back press pops the sheet through `BackPolicy::Pop` without
        // the widget — or the app — ever hearing about it, so neither
        // `dismiss_overlay` nor (yet) `end_overlay` has run.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 3);
        state.overlays = 1;
        assert!(state.bar(3, false).overlay_active);

        state.reconcile(2);
        assert_eq!(state.overlays, 0, "the depth fall clamps the counter flat");
        assert_eq!(state.overlays_closing, 0);
        assert!(!state.bar(2, false).overlay_active, "…and the bar is live");
    }

    #[test]
    fn the_backstop_never_fires_on_the_frame_an_overlay_is_pushed() {
        // The clamp is depth-*fall* gated for the same reason the mirror's own
        // retain is: on the frame a sheet is pushed, `depth()` still reports
        // the pre-push value, and an un-gated clamp would wipe the one piece
        // of state that knows a sheet is coming.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 2);
        state.overlays = 1;
        state.reconcile(2);
        assert_eq!(state.overlays, 1, "the stale depth clamps nothing");
        assert!(state.bar(2, false).overlay_active);
    }

    // -----------------------------------------------------------------------
    // Consulting the queued ops before mutating (see the module docs)
    // -----------------------------------------------------------------------

    #[test]
    fn a_push_recorded_while_an_overlay_is_closing_lands_over_the_route_below_it() {
        // THE Critical trace, as arithmetic. The Sessions Attach row closes the
        // sheet and pushes `/terminal` in one handler: the sheet's pop is
        // queued (so `live` still counts its page) while the push is recorded.
        // Recording one too deep is permanent — the entry is never re-derived —
        // and the next depth fall past it evicts the route's own entry.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 2);
        state.overlays = 1;
        state.reconcile(3); // the sheet's page is live

        assert_eq!(
            state.record_push_depth(3),
            3,
            "with the sheet staying up, a push lands over it"
        );

        state.overlays_closing = 1; // `dismiss_overlay`: the pop is enqueued
        assert_eq!(
            state.record_push_depth(3),
            2,
            "…but a sheet on its way out is a page the route does not stack on"
        );

        // …and the same discount answers for a queued *route* pop.
        state.overlays = 0;
        state.overlays_closing = 0;
        state.pending_pop = true;
        assert_eq!(state.record_push_depth(3), 2);
    }

    #[test]
    fn a_fresh_overlay_spends_an_unobserved_fall_before_it_counts_itself() {
        // The delegated Major: the terminal's menu pops itself from inside its
        // own paint and the loop can park before any `bar` read sees the drop,
        // leaving a fall nobody spent. The sheet the menu's completion callback
        // opens must not be what spends it.
        let mut state = mirror(&[(2, route_meta("/terminal"))], 3);
        assert_eq!(state.overlays, 0, "the menu's own count is already cleared");

        state.begin_overlay(2);
        assert_eq!(state.overlays, 1, "the sheet is counted");
        assert_eq!(
            state.last_seen_depth, 2,
            "…and the stale fall was spent on the way in, not on it"
        );
        assert!(state.bar(2, false).overlay_active, "so the bar is dimmed");

        // The next reconcile has nothing left to clamp, which is the whole
        // point: the sheet's own count survives to the dismissal, where
        // `dismiss_overlay` needs it to light the bar synchronously.
        state.reconcile(2);
        assert_eq!(state.overlays, 1, "the count survives the next pass");
        state.reconcile(3); // the sheet's page lands
        state.overlays_closing = 1; // `dismiss_overlay`
        assert!(
            !state.bar(3, false).overlay_active,
            "the dismissal lights the bar rather than stranding it dimmed"
        );
    }

    #[test]
    fn a_route_pop_and_an_open_overlay_are_still_told_apart() {
        // `pending_pop` suppresses the *depth* half only: a route pop whose
        // stale depth looks like an overlay must stay lit, while a genuinely
        // open overlay must stay dimmed even mid-pop.
        let mut state = mirror(
            &[
                (2, route_meta("/sessions")),
                (3, route_meta(SETTINGS_ROUTE)),
            ],
            3,
        );
        state.stack.pop();
        state.pending_pop = true;
        assert!(!state.bar(3, false).overlay_active);

        state.overlays = 1;
        assert!(
            state.bar(3, false).overlay_active,
            "an overlay the app counted is not a queued route pop"
        );
    }

    #[test]
    fn the_default_bar_state_is_lit_and_live() {
        // `BarState::default` seeds the published signal before the first
        // navigation; a bar that started out flagged modal (or hidden) would
        // be inert until something happened to clear it.
        assert!(!BarState::default().overlay_active);
        assert!(!BarState::default().fullscreen);
    }

    #[test]
    fn the_fullscreen_flag_is_orthogonal_to_the_route_it_rides_over() {
        // The whole channel, as arithmetic: the flag changes *only* that
        // field, so the bar keeps titling whatever route is on top and comes
        // straight back when the screen clears it on unmount.
        let state = mirror(&[(2, route_meta("/terminal"))], 2);
        assert!(!state.bar(2, false).fullscreen);

        let bar = state.bar(2, true);
        assert!(bar.fullscreen);
        assert_eq!(bar.meta, route_meta("/terminal"), "…without retitling");
        assert!(!bar.overlay_active);

        assert!(
            !state.bar(2, false).fullscreen,
            "unmount gives the bar back"
        );
    }
}
