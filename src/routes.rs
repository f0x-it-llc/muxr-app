//! Muxr's route table, its one guard, and the app-wide navigation handle every
//! screen pushes paths through — a hub file; each page builder lives in its
//! owning feature's `presentation::pages` module. See `docs/ARCHITECTURE.md`'s
//! Navigation section for the route table shape and the overlay/back-press
//! contracts this module implements.
//!
//! **Two navigators, one table.** `/` (Connect) is a bar-less page of the
//! app's outer navigator; the other three routes are children of a
//! [`shell_route`] whose own page — [`crate::shell::shell_page`] — is the
//! scaffold that carries the app bar over a second, *inner* navigator. The
//! router splits a chain at that boundary, so navigating between `/sessions`,
//! `/terminal` and `/settings` issues **zero** ops on the outer stack and the
//! scaffold (bar included) is never rebuilt. The bar derives from the inner
//! navigator's live [`RouteObserver`] — the outer stack freezes at the shell
//! page by design, so it is never the thing chrome reads.
//!
//! **Session guard**: [`session_guard_with`] closes over
//! `Arc<ConnectionController>` **by capture, not context** — it must stay safe
//! to consult from a press handler's owner-less call stack, and fires only at
//! navigation time (a connection dropping mid-screen does not reactively bounce
//! it back). It guards the shell's whole subtree and bounces *outside* it, so a
//! bounce taken with the scaffold on the stack **unwinds** it rather than
//! stacking Connect over a live navigator ([`Nav::unwind_shell`]).
//!
//! **One router, one handle.** [`Router`]/[`NavigatorController`] are
//! `Rc`-backed (neither `Send` nor `Sync`), so the router cannot be threaded
//! through `frust::provide_context` (`Send + Sync` bound) — it lives in a
//! process `thread_local!` instead, reached via [`app_nav`] from a screen's own
//! `Component::init`, the same handle-in-a-thread-local idiom
//! `crate::ui::toasts::app_toasts` uses (minus the self-healing: a missing
//! handle here is a loud panic).
//!
//! **Overlays never touch either stack.** Every menu/sheet/dialog is a page of
//! the root overlay host (`crate::AppState::overlays`), which routes input and
//! publishes accessibility nodes for its top page only and outranks both
//! navigators for the Android back press (R44-back).
//!
//! **Leaving the shell is a question, not a pop**: the shell page carries
//! [`frust::BackPolicy::DismissAnimated`] plus a dismiss-signal cell
//! ([`sessions_back_signal`]), so a back press at `/sessions` — where the inner
//! navigator is one page deep and claims nothing — bumps the cell rather than
//! popping the live `muxrd` connection out from under the screen; the Sessions
//! screen raises its own disconnect confirmation and only a confirmed answer
//! pops.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use frust::{
    BackPolicy, Either, Get, GetUntracked, Location, NavigatorController, PushOptions, Resolution,
    ResolvedPage, Route, RouteObserver, RouteParams, Router, RwSignal, Set, SizedBox,
    TransitionSpec, View, shell_route,
};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::features::connect::presentation::pages::connect_page;
use crate::features::sessions::presentation::pages::sessions_page;
use crate::features::sessions::presentation::widgets::host_of;
use crate::features::settings::presentation::pages::settings_page;
use crate::features::terminal::presentation::pages::terminal_page;

/// Routes
pub(crate) const CONNECT_LOCATION: &str = "/";
const SESSIONS_ROUTE: &str = "/sessions";
const TERMINAL_ROUTE: &str = "/terminal";
pub const SETTINGS_ROUTE: &str = "/settings";

/// The `/terminal` query contract, shared by the screens that push it and the
/// one that reads it: the attached session's id.
pub const TERMINAL_SESSION_QUERY_KEY: &str = "session";
/// The zero-based tab index a pane pill targets.
pub const TERMINAL_TAB_QUERY_KEY: &str = "tab";
/// The wire pane id (unique within the tab's terminal panes) a pill targets.
pub const TERMINAL_PANE_QUERY_KEY: &str = "pane";
pub const TERMINAL_SPACE_QUERY_KEY: &str = "space";

/// The `/` route (and the outer navigator's initial page): Connect.
pub(crate) fn home_page(overlays: NavigatorController<AppState>) -> impl View<AppState> {
    connect_page(overlays)
}

/// Where a route's bar title comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarTitle {
    ConnectedHost,
    Fixed(&'static str),
}

/// What sits in the bar's leading slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarLeading {
    Brand,
    Back,
}

/// The bar's optional trailing action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarAction {
    Settings,
    TerminalMenu,
}

/// Everything the shell scaffold's app bar needs to render one route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RouteMeta {
    pub title: BarTitle,
    pub leading: BarLeading,
    pub action: Option<BarAction>,
}

impl RouteMeta {
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

const DISCONNECTED_TITLE: &str = "muxrd";

/// The bar metadata for `path`, or `None` for a route that carries **no** bar.
///
/// This is also the app's answer to "does `path` live inside the shell?" — the
/// three bar-bearing routes are exactly the shell's children, which is how
/// [`Nav`] recognises the scaffold on the outer stack.
pub fn route_meta(path: &str) -> Option<RouteMeta> {
    match path {
        SESSIONS_ROUTE => Some(RouteMeta {
            title: BarTitle::ConnectedHost,
            leading: BarLeading::Brand,
            action: Some(BarAction::Settings),
        }),
        TERMINAL_ROUTE => Some(RouteMeta {
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

/// Which way the bar's title crossfade travels on the next change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BarDirection {
    #[default]
    Forward,
    Back,
}

/// One frame's worth of bar state, derived live from the inner navigator's
/// [`RouteObserver`] — see [`Nav::bar`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BarState {
    /// The in-shell route's metadata, or `None` outside the shell (`/`).
    pub meta: Option<RouteMeta>,
    /// The crossfade direction for this change.
    pub direction: BarDirection,
    /// Whether the route on top has asked the bar to get out of the way
    /// entirely — the terminal screen's **fullscreen** state, published through
    /// [`Nav::set_fullscreen`].
    pub fullscreen: bool,
}

pub const fn route_transition() -> TransitionSpec {
    TransitionSpec::glyph()
}

// ---------------------------------------------------------------------------
// The app's navigation handle
// ---------------------------------------------------------------------------

/// Which verb [`Nav::navigate`] is applying — the two this app uses.
#[derive(Clone, Copy)]
enum Verb {
    Push,
    Go,
}

/// A cheap, clonable path-pushing handle over the app's one [`Router`] — what a
/// screen navigates with. See the [module docs](self) for why this exists (and
/// why it is not in frust's context).
///
/// Cloning is an `Rc` bump, so a press closure can hold its own copy; every
/// clone drives the *same* router — hence the same route table, the same
/// [`session_guard_with`] guard, and the same live route state the shell's bar
/// reads. A screen resolves one with [`app_nav`] in its `Component::init` and
/// keeps it in its own `State` — never resolves one from inside a press handler
/// (the discipline every other app-scoped dependency follows).
#[derive(Clone)]
pub struct Nav {
    router: Rc<Router<AppState>>,
    /// The shell's inner navigator — where the three bar-bearing routes live.
    inner: NavigatorController<AppState>,
    /// The inner navigator's published route state: the bar's one input, and
    /// the only stack that moves once the shell page is placed.
    routes: RouteObserver,
    /// The topmost screen's fullscreen request — see [`BarFullscreen`]. A
    /// signal of its own precisely because it is the one piece of bar state a
    /// screen must be able to clear from a `Send + Sync` context.
    fullscreen: RwSignal<bool>,
    /// The page the shell's inner navigator opens on: the leaf of the
    /// navigation that placed the scaffold, kept as the resolved page rather
    /// than a path so the navigator's root builder can re-run it every rebuild
    /// without re-resolving — a re-resolution would run the session guard
    /// again, and a connection that dropped since would hand the shell a
    /// Connect page.
    landing: Rc<RefCell<Option<ResolvedPage<AppState>>>>,
    /// The location of the most recent navigation, post-guard — what
    /// [`Self::param`] answers from. The router publishes the same fact on its
    /// own [`frust::RouteNavigator`], but only for the navigations it applies
    /// itself, and [`Self::place_shell`] is one it does not.
    last_location: Rc<RefCell<Option<Location>>>,
    /// The outer route generation at which this handle queued the shell page.
    ///
    /// [`Self::shell_is_placed`] answers from the **published** outer stack —
    /// committed fact — which lags a placement by one rebuild. An entry here is
    /// honoured only while the recorded generation still matches, so it
    /// self-invalidates on the very publish that makes the stack authoritative
    /// (frust's own `Router` keeps the same note for the same reason).
    pending_shell: Rc<Cell<Option<u64>>>,
    /// The same note for the opposite op — the generation at which this handle
    /// queued the shell page's **removal** ([`Self::unwind_shell`]), read first
    /// by [`Self::shell_is_placed`] so a navigation back into the shell on that
    /// same frame places a fresh scaffold rather than aiming at a dying one.
    pending_unwind: Rc<Cell<Option<u64>>>,
}

/// A `Send + Sync` handle to just the bar's fullscreen flag — see
/// [`BarState::fullscreen`].
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
    /// guarded, like every other resolution through this router.
    ///
    /// An in-shell push lands on the **inner** navigator and issues no outer op
    /// at all (frust's shell keep rule), carrying that navigator's default
    /// [`route_transition`] slide. The one push this handle applies itself is
    /// the one that *places* the shell page — see
    /// [`place_shell`](Self::place_shell) for the [`PushOptions`] the router
    /// builds no seam for.
    pub fn push(&self, location: &str) {
        self.navigate(location, Verb::Push);
    }

    /// Reset the stack to `location`'s matched chain (replace semantics) — a
    /// `go` is a new stack, not a deeper one.
    /// Unlike [`push`](Self::push) this carries **no** transition: the app's two
    /// `go`s are the launch location the splash hands off over and Settings'
    /// disconnect, and neither may animate a page that is already on screen.
    ///
    /// A `go` that has to *place* the shell takes [`push`](Self::push)'s own
    /// placement minus that transition, so it carries the back policy and
    /// dismiss cell a replace has no seam for — and the leave-is-a-question
    /// contract needs. Latent: every `go` in the app today resolves to `/`.
    pub fn go(&self, location: &str) {
        self.navigate(location, Verb::Go);
    }

    /// Resolve `location` once — recording where it landed for [`param`](Self::param)
    /// — and apply the chain: this handle places and unwinds the shell page
    /// itself, the router applies everything else (including the in-shell leaf
    /// its keep rule puts on the inner navigator).
    ///
    /// A resolution's page count *is* the shell question (bar-bearing routes
    /// resolve through the scaffold too; `/`, the one route outside it,
    /// resolves flat), so crossing that boundary either way is what this
    /// decides — and a **bounce** (the guard answering with another path)
    /// never pushes at all.
    fn navigate(&self, location: &str, verb: Verb) {
        let mut chain = None;
        let mut bounced = false;
        if let Resolution::Matched {
            location: resolved,
            pages,
            ..
        } = self.router.resolve(location)
        {
            bounced = resolved.path != Location::parse(location).path;
            *self.last_location.borrow_mut() = Some(resolved);
            chain = Some(pages);
        }
        let Some(pages) = chain else {
            // An unmatched location: the router raises its own error page.
            self.apply(location, verb);
            return;
        };
        let placed = self.shell_is_placed();
        if pages.len() > 1 {
            if !placed {
                self.place_shell(pages, verb);
                return;
            }
        } else if placed {
            self.unwind_shell();
            return;
        } else if bounced {
            // Already there: `/` is the outer root and the only route outside
            // the shell, so a push would stack a second Connect on it.
            return;
        }
        self.apply(location, verb);
    }

    /// Hand `location` to the router — the navigations this handle adds nothing to.
    fn apply(&self, location: &str, verb: Verb) {
        match verb {
            Verb::Push => self.router.push(location),
            Verb::Go => self.router.go(location),
        }
    }

    /// Apply a resolved chain that crosses the shell boundary with the shell
    /// page not yet on the outer stack: the chain's head is the shell page (the
    /// page below it — Connect — is retained), and its leaf becomes the inner
    /// navigator's **root**.
    ///
    /// The leaf lands as a root rather than a page pushed over a placeholder
    /// for two reasons: a back press at the landing route then finds an inner
    /// navigator with nothing to pop and falls through to the outer one — which
    /// is exactly "leave the shell" — and the scaffold's own slide-in is the
    /// only animation the entry plays, since a root page is built, not pushed.
    /// Both verbs *push* it, retaining the page below: the shell's back
    /// contract ends in a pop ([`Self::pop`]'s outer branch), which needs a
    /// page beneath to land on. `verb` picks only the entrance.
    fn place_shell(&self, pages: Vec<ResolvedPage<AppState>>, verb: Verb) {
        let controller = self.router.controller();
        let mut pages = pages.into_iter();
        let Some(shell) = pages.next() else {
            return;
        };
        *self.landing.borrow_mut() = pages.next_back();
        let route = shell.location().clone();
        let transition = match verb {
            Verb::Push => route_transition(),
            Verb::Go => TransitionSpec::NONE,
        };
        controller.push_with_options(
            move || shell.build(),
            PushOptions::opaque()
                .transition(transition)
                .back(BackPolicy::DismissAnimated)
                .dismiss_signal(publish_sessions_back_signal())
                .route(route),
        );
        self.pending_shell.set(Some(controller.route_generation()));
        self.pending_unwind.set(None);
    }

    /// Take the shell page back off the outer stack — what a navigation
    /// resolving **outside** the shell subtree does while the scaffold is still
    /// on it: the guard bouncing a push made after the connection dropped, and
    /// Settings' disconnect `go`.
    ///
    /// Pop, not push. Connect landed *over* a live shell page would retain the
    /// authenticated subtree beneath it — frust's keep rule answers "on the
    /// stack", not "on top" — leaving the next in-shell navigation to land on
    /// that hidden navigator, and a platform back press (no router navigation,
    /// so no guard) to walk back into it. The pop is the op the confirmed
    /// disconnect already applies ([`Self::pop`]'s outer branch), and it tears
    /// the page down, the inner navigator's pages with it, so the next
    /// placement opens fresh. What it reveals is the outer **root**, Connect —
    /// the one route outside the shell, and where the bounce was going.
    /// [`Self::landing`] is left alone: the leaving page rebuilds through it
    /// while the pop plays.
    fn unwind_shell(&self) {
        let controller = self.router.controller();
        controller.pop();
        self.pending_shell.set(None);
        self.pending_unwind.set(Some(controller.route_generation()));
    }

    /// The landing page's own location — the inner navigator's root route
    /// stamp, and so the first path its [`RouteObserver`] publishes.
    pub(crate) fn landing_route(&self) -> Option<Location> {
        self.landing
            .borrow()
            .as_ref()
            .map(|page| page.location().clone())
    }

    /// Build the landing page — the inner navigator's root builder, re-run
    /// every rebuild like any other page builder. A shell placed with no leaf
    /// (nothing in this table resolves that way) opens on an empty box rather
    /// than panicking.
    pub(crate) fn landing_page(&self) -> impl View<AppState> + use<> {
        match self.landing.borrow().as_ref() {
            Some(page) => Either::Left(page.build()),
            None => Either::Right(SizedBox::<AppState>(Some(0.0), Some(0.0))),
        }
    }

    /// Whether the shell page is already on the outer stack — the app-side half
    /// of frust's keep rule, asked before [`place_shell`](Self::place_shell)
    /// puts a second one there and [`unwind_shell`](Self::unwind_shell) takes
    /// one off. This handle's queued *removal* first (the committed stack still
    /// shows a page the pop has spoken for), then committed fact (an outer page
    /// stamped with an in-shell route), then its queued placement.
    fn shell_is_placed(&self) -> bool {
        let stack = self.router.controller().route_stack();
        if self.pending_unwind.get() == Some(stack.generation()) {
            return false;
        }
        let committed = stack
            .entries()
            .iter()
            .flatten()
            .any(|location| route_meta(&location.path).is_some());
        committed || self.pending_shell.get() == Some(stack.generation())
    }

    /// Pop the page the user is actually on: the inner navigator while it is
    /// poppable, else the outer one — which drops the shell page and every
    /// in-shell page with it, landing back on Connect.
    ///
    /// This mirrors frust's own innermost-first back arbitration by hand,
    /// because a `‹` press and an Android back press must agree.
    /// [`NavigatorController::can_pop`] is advisory (the depth as of the last
    /// rebuild), which is exact for every call site here: each pops once, a
    /// rebuild apart.
    pub fn pop(&self) {
        if self.inner.can_pop() {
            self.inner.pop();
        } else {
            self.router.pop();
        }
    }

    /// The bar's state for this frame, derived from the inner navigator's
    /// published route state — read **tracked**, so a navigation (or a
    /// fullscreen request) wakes the rebuild that retitles the bar.
    ///
    /// Nothing is mirrored: the route on top *is* the observer's current route,
    /// and a pop that bypassed this handle (an Android back gesture) publishes
    /// through the same channel as one that did not.
    pub fn bar(&self) -> BarState {
        BarState {
            meta: route_meta(&self.routes.path()),
            direction: if self.routes.is_back() {
                BarDirection::Back
            } else {
                BarDirection::Forward
            },
            fullscreen: self.fullscreen.get(),
        }
    }

    /// The inner navigator's route observer — what
    /// [`crate::shell::shell_page`] attaches to the navigator it mounts.
    pub(crate) fn route_observer(&self) -> RouteObserver {
        self.routes
    }

    /// The `?key=…` value the most recent path navigation carried, if any.
    ///
    /// **Read it in a screen's `Component::init`, and nowhere else.** It is
    /// exact precisely at `init` time: a page is built during the very
    /// navigation that resolved it, so "the location the router last resolved"
    /// *is* this screen's own. Read later — after another navigation landed —
    /// it would answer for that one instead. A route builder now receives the
    /// query merged into its own [`RouteParams`], which is where a screen
    /// should read it from once its call site moves.
    pub fn param(&self, key: &str) -> Option<String> {
        self.last_location
            .borrow()
            .as_ref()
            .and_then(|location| location.query.get(key).cloned())
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

    /// The dismiss-signal cell of the shell page currently on the stack — see
    /// [`sessions_back_signal`]. Seeded with a cell nobody bumps, so a screen
    /// mounted outside a [`Nav::push`] still resolves one.
    static SESSIONS_BACK: RefCell<Rc<Cell<u64>>> = RefCell::new(Rc::new(Cell::new(0)));
}

/// Mint the dismiss cell a shell placement carries, and publish it for the
/// Sessions screen that placement is about to mount.
///
/// A **fresh** cell per placement, rather than one long-lived counter: a new
/// mount starts at generation 0 and so cannot inherit a bump aimed at the page
/// it replaced, and the departed page's own cell keeps whatever it had.
fn publish_sessions_back_signal() -> Rc<Cell<u64>> {
    let signal = Rc::new(Cell::new(0));
    SESSIONS_BACK.with(|cell| *cell.borrow_mut() = Rc::clone(&signal));
    signal
}

/// This thread's shell dismiss-signal cell: the navigator bumps its generation
/// on a back press *instead of* popping the page
/// ([`BackPolicy::DismissAnimated`]), and the Sessions screen turns that bump
/// into its disconnect confirmation.
///
/// **Read it in the screen's `Component::init`, and nowhere else** — the same
/// contract, for the same reason, as [`Nav::param`].
///
/// Unlike [`app_nav`] this never panics. A screen mounted without a push
/// through [`Nav`] — a headless suite mounting `sessions_page` directly — gets
/// the inert seed cell instead, which no navigator holds and nothing bumps, so
/// the seam is simply dormant. (A suite that wants to drive it bumps that cell
/// itself, which is exactly what the navigator would have done.)
pub fn sessions_back_signal() -> Rc<Cell<u64>> {
    SESSIONS_BACK.with(|cell| Rc::clone(&cell.borrow()))
}

/// Build the app's one [`Router`] over `controller` — the full route table plus
/// the [`session_guard_with`] guard closed over `connection` — and publish it as
/// this thread's [`Nav`].
///
/// `controller` is the **outer** navigator (`/` and the shell page);
/// `overlays` the **root overlay host**'s, threaded into the route table so a
/// screen resolves it the same way it resolves everything else. The shell's own
/// inner navigator is minted here and reached only through the returned handle:
/// nothing outside this module needs to know the app runs two of them.
///
/// Called once from `crate::MuxrApp::init`, which owns the returned router as
/// `crate::AppState::nav`. A headless test that drives its **own**
/// [`NavigatorController`] calls this too, right after `MuxrApp::init`, so the
/// screen under test pushes through the real table and the real guard rather
/// than a second, drifting copy.
pub fn install_app_router(
    controller: &NavigatorController<AppState>,
    overlays: &NavigatorController<AppState>,
    connection: Arc<ConnectionController>,
) -> Rc<Router<AppState>> {
    // The route table is built *before* the router (so there is no
    // circularity), which is exactly why a page builder cannot be handed the
    // router — the shell page resolves [`app_nav`] at build time instead.
    let inner = NavigatorController::new();
    let routes = build_routes(&inner, overlays, Arc::clone(&connection));
    let router = Rc::new(
        Router::with_controller(controller, routes).redirect(session_guard_with(connection)),
    );
    APP_NAV.with(|cell| {
        *cell.borrow_mut() = Some(Nav {
            router: Rc::clone(&router),
            inner,
            routes: RouteObserver::new(),
            fullscreen: RwSignal::new(false),
            landing: Rc::new(RefCell::new(None)),
            last_location: Rc::new(RefCell::new(None)),
            pending_shell: Rc::new(Cell::new(None)),
            pending_unwind: Rc::new(Cell::new(None)),
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

/// Muxr's session guard, closed over the app's one [`ConnectionController`]: a
/// top-level redirect (see [`frust::Router::redirect`], wired on by
/// [`install_app_router`]) bouncing an unauthenticated navigation into the
/// shell's subtree back to `/`, the one route outside it.
///
/// **The guarded set is that subtree, asked of [`route_meta`]** rather than
/// spelled out, so a route added under the scaffold is guarded on arrival —
/// `/settings` included, which stopped being reachable disconnected when it
/// moved inside the shell (its one entry point is the bar's gear, and an
/// unguarded resolution would *place* the scaffold for a dead connection).
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
/// that drops *while* an in-shell route is on screen does not reactively
/// bounce it back to `/`; the next navigation the user makes is what fires,
/// and [`Nav::unwind_shell`] is what makes that bounce leave nothing
/// authenticated behind. (This reads the signal `get_untracked`, establishing
/// no reactive dependency at all.)
pub(crate) fn session_guard_with(
    connection: Arc<ConnectionController>,
) -> impl Fn(&Location) -> Option<String> + 'static {
    move |loc: &Location| {
        let guarded = route_meta(&loc.path).is_some();
        if guarded && !connection.connected.get_untracked() {
            Some(CONNECT_LOCATION.to_string())
        } else {
            None
        }
    }
}

/// Build the full route table: `/` on the outer navigator, and the three
/// bar-bearing routes as children of the pathless [`shell_route`] bound to
/// `inner`. `/` is declared **first** — a pathless shell matches the empty
/// segment list too, and the router takes the first match.
///
/// Every navigator is created *before* this table (so there is no circularity
/// with the [`Router`] either attaches to) and cloned into the builders that
/// need it:
///
/// - `overlays` — the **root overlay host**'s controller, and how a screen
///   pushes a page *builder* (a menu, a sheet, a dialog), where [`Nav`] is how
///   it pushes a *path*. Every one of the four screens takes it.
/// - `inner` — the shell's navigator. The shell page mounts it; `/sessions`
///   also reads its depth (its mock poll pauses for whatever it pushes and
///   resumes when the stack falls back), which is why that screen and not the
///   others takes a controller at all.
/// - `connection` — the shell bar's title source (`BarTitle::ConnectedHost`).
pub(crate) fn build_routes(
    inner: &NavigatorController<AppState>,
    overlays: &NavigatorController<AppState>,
    connection: Arc<ConnectionController>,
) -> Vec<Route<AppState>> {
    vec![
        Route::new(CONNECT_LOCATION, {
            let overlays = overlays.clone();
            move |_params: &RouteParams| home_page(overlays.clone())
        }),
        shell_route(
            inner,
            {
                let inner = inner.clone();
                move |_params: &RouteParams| crate::shell::shell_page(&inner, &connection)
            },
            vec![
                Route::new(SESSIONS_ROUTE, {
                    let inner = inner.clone();
                    let overlays = overlays.clone();
                    move |_params: &RouteParams| sessions_page(inner.clone(), overlays.clone())
                }),
                Route::new(TERMINAL_ROUTE, {
                    let overlays = overlays.clone();
                    move |_params: &RouteParams| terminal_page(overlays.clone())
                }),
                Route::new(SETTINGS_ROUTE, {
                    let overlays = overlays.clone();
                    move |_params: &RouteParams| settings_page(overlays.clone())
                }),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{ConnectTarget, TrustMode};

    /// A connect target the mock backend accepts — it authenticates nothing, so
    /// the credential fields are empty.
    fn target() -> ConnectTarget {
        ConnectTarget {
            host: "mock.example".to_string(),
            port: 7777,
            trust_mode: TrustMode::System,
            label: "Mock rig".to_string(),
            offer_credential: false,
            token: zeroize::Zeroizing::new(String::new()),
            remember: false,
            fingerprint: None,
        }
    }

    /// The app's table over throwaway navigators, **without** the guard: a
    /// matcher-shape fixture, so a resolution here answers for the table alone.
    fn table_router() -> Router<AppState> {
        let inner = NavigatorController::new();
        let overlays = NavigatorController::new();
        Router::with_controller(
            &NavigatorController::new(),
            build_routes(
                &inner,
                &overlays,
                Arc::new(ConnectionController::detached()),
            ),
        )
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

        connection.connect(target());

        assert_eq!(
            guard(&Location::parse("/sessions")),
            None,
            "a connected push is not redirected"
        );
        assert_eq!(guard(&Location::parse("/terminal")), None);
    }

    #[test]
    fn the_guarded_set_is_exactly_the_shells_subtree() {
        let connection = Arc::new(ConnectionController::detached());
        let guard = session_guard_with(Arc::clone(&connection));

        for path in [SESSIONS_ROUTE, TERMINAL_ROUTE, SETTINGS_ROUTE] {
            assert!(route_meta(path).is_some(), "{path} is in the shell");
            assert_eq!(
                guard(&Location::parse(path)),
                Some(CONNECT_LOCATION.to_string()),
                "{path} bounces while unauthenticated"
            );
        }
        assert_eq!(route_meta(CONNECT_LOCATION), None);
        assert_eq!(guard(&Location::parse(CONNECT_LOCATION)), None);

        connection.connect(target());

        for path in [SESSIONS_ROUTE, TERMINAL_ROUTE, SETTINGS_ROUTE] {
            assert_eq!(
                guard(&Location::parse(path)),
                None,
                "{path} resolves normally once connected"
            );
        }
    }

    // -----------------------------------------------------------------------
    // The table's shape
    // -----------------------------------------------------------------------

    #[test]
    fn every_bar_bearing_route_resolves_through_the_shell_page() {
        // The tripwire under `Nav::navigate`'s chain test: a bar-bearing route
        // resolves to the shell page PLUS its own, which is what tells the
        // handle a push has to place the scaffold before the leaf can land.
        // `/` stays one flat page. Adding a nested route under either would
        // break that discriminator here rather than at runtime.
        let router = table_router();
        assert_eq!(
            router.resolve(CONNECT_LOCATION).page_count(),
            1,
            "/ is outside the shell"
        );
        for path in [SESSIONS_ROUTE, TERMINAL_ROUTE, SETTINGS_ROUTE] {
            assert_eq!(
                router.resolve(path).page_count(),
                2,
                "{path} resolves through the shell page"
            );
        }
    }

    #[test]
    fn the_shell_swallows_no_path_of_its_own() {
        // The pathless shell matches the empty segment list, so declaration
        // order is what keeps `/` reaching Connect instead of the scaffold.
        let router = table_router();
        assert!(
            router.resolve("/nope").page_count() == 0,
            "unmatched errors"
        );
    }

    // -----------------------------------------------------------------------
    // Route metadata
    // -----------------------------------------------------------------------

    #[test]
    fn connect_is_the_only_route_without_a_bar() {
        assert_eq!(route_meta("/"), None, "Connect is full-bleed, no bar");
        for path in [SESSIONS_ROUTE, TERMINAL_ROUTE, SETTINGS_ROUTE] {
            assert!(route_meta(path).is_some(), "{path} carries a bar");
        }
    }

    #[test]
    fn only_the_shells_landing_route_wears_the_brand() {
        // The brand mark is the "you are at the bottom of the stack" cue; every
        // route pushed over it gets the `‹` instead.
        assert_eq!(
            route_meta(SESSIONS_ROUTE).map(|m| m.leading),
            Some(BarLeading::Brand)
        );
        assert_eq!(
            route_meta(TERMINAL_ROUTE).map(|m| m.leading),
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
        // Sessions screen owns no bar of its own it has to be route metadata.
        // `/terminal`'s `⋮` is there for the mirror-image reason: v2 of its
        // mock moved the overflow menu onto the app bar.
        assert_eq!(
            route_meta(SESSIONS_ROUTE).and_then(|m| m.action),
            Some(BarAction::Settings)
        );
        assert_eq!(
            route_meta(TERMINAL_ROUTE).and_then(|m| m.action),
            Some(BarAction::TerminalMenu)
        );
        assert_eq!(route_meta(SETTINGS_ROUTE).and_then(|m| m.action), None);
    }

    #[test]
    fn every_in_shell_push_carries_the_theme_timed_glyph_slide() {
        // The GlyphSlide contract, at the seam that applies it: the Glyph
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

    #[test]
    fn the_default_bar_state_is_visible_and_unclaimed() {
        // `BarState::default` is what a bar-less `/` and a not-yet-published
        // navigator both read as; a default that started out fullscreen would
        // hide the bar until something happened to clear it.
        assert!(!BarState::default().fullscreen);
        assert_eq!(BarState::default().meta, None);
        assert_eq!(BarState::default().direction, BarDirection::Forward);
    }

    // -----------------------------------------------------------------------
    // The shell's back-confirm cell
    // -----------------------------------------------------------------------

    #[test]
    fn each_shell_placement_publishes_a_fresh_dismiss_cell_at_generation_zero() {
        // The published cell is what the screen resolves in its `init`, and it
        // must be *this* placement's: a mount that inherited the departed
        // page's counter would read a stale bump as a back press it never got.
        let first = publish_sessions_back_signal();
        assert!(
            Rc::ptr_eq(&first, &sessions_back_signal()),
            "the placement publishes the very cell it carries"
        );

        first.set(4); // a back press or four against the page it belongs to.

        let second = publish_sessions_back_signal();
        assert_eq!(second.get(), 0, "the next mount starts unbumped");
        assert!(
            !Rc::ptr_eq(&first, &second),
            "…on its own cell, not the departed page's"
        );
        assert_eq!(first.get(), 4, "…which is left exactly as it was");
        assert!(Rc::ptr_eq(&second, &sessions_back_signal()));
    }

    #[test]
    fn the_seed_cell_is_inert_rather_than_absent() {
        // A screen mounted without a push through `Nav` (a headless suite
        // mounting `sessions_page` directly) still resolves a cell — unlike
        // `app_nav`, which panics — so the seam is dormant rather than fatal.
        let seed = sessions_back_signal();
        assert!(
            Rc::ptr_eq(&seed, &sessions_back_signal()),
            "stable per thread"
        );
    }
}
