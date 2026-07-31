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
            action: None,
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

/// One frame's worth of bar state: the route on top (if it has a bar) plus the
/// direction the last change travelled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BarState {
    /// The topmost *titled* route's metadata, or `None` for a bar-less route.
    pub meta: Option<RouteMeta>,
    /// The crossfade direction for this change.
    pub direction: BarDirection,
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
}

impl NavState {
    /// Drop every entry recorded deeper than `live` — but only once the live
    /// depth has actually *fallen* since the last observation (see
    /// [`Self::last_seen_depth`]). Returns whether anything was dropped.
    fn reconcile(&mut self, live: usize) -> bool {
        let fell = live < self.last_seen_depth;
        self.last_seen_depth = live;
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

    /// The current bar state: the deepest recorded entry's metadata.
    fn bar(&self) -> BarState {
        BarState {
            meta: self.stack.last().and_then(|(_, meta)| *meta),
            direction: self.direction,
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
                self.record_push(&location.path);
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
    pub fn go(&self, location: &str) {
        self.router.go(location);
        let meta = match self.router.resolve(location) {
            Resolution::Matched { location, .. } => route_meta(&location.path),
            Resolution::Error { .. } => None,
        };
        let mut state = self.state.borrow_mut();
        state.stack = vec![(1, meta)];
        state.direction = BarDirection::Forward;
        let bar = state.bar();
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
        let bar = state.bar();
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
    pub fn bar(&self) -> BarState {
        let live = self.router.controller().depth();
        let mut state = self.state.borrow_mut();
        let changed = state.reconcile(live);
        let reconciled = state.bar();
        drop(state);
        if changed {
            self.publish(reconciled);
        }
        self.current.get()
    }

    /// Record a landed path navigation in the mirror.
    ///
    /// The recorded depth is one past whichever is deeper: the navigator's
    /// last-published depth, or the deepest entry already recorded. Both halves
    /// matter — `depth()` is stale between a press handler and the rebuild that
    /// applies its op (so two pushes in one frame would otherwise record the
    /// same depth), while the mirror alone cannot see the transparent pages
    /// pushed straight through the controller.
    fn record_push(&self, path: &str) {
        let live = self.router.controller().depth();
        let mut state = self.state.borrow_mut();
        let deepest = state.stack.last().map_or(0, |(depth, _)| *depth).max(live);
        state.stack.push((deepest + 1, route_meta(path)));
        state.direction = BarDirection::Forward;
        let bar = state.bar();
        drop(state);
        self.publish(bar);
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
            })),
            current: RwSignal::new(BarState::default()),
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
    fn the_gear_lives_on_sessions_and_nowhere_else() {
        // `/settings` has exactly one entry point in the app, and since the
        // Sessions screen no longer owns a bar it has to be route metadata.
        assert_eq!(
            route_meta("/sessions").and_then(|m| m.action),
            Some(BarAction::Settings)
        );
        assert_eq!(route_meta("/terminal").and_then(|m| m.action), None);
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
        assert_eq!(state.bar().meta, route_meta("/sessions"));
        assert_eq!(
            state.bar().direction,
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
        assert_eq!(state.bar().meta, route_meta("/sessions"));
        // …and once the navigator applies it, the entry is still there.
        assert!(!state.reconcile(2));
        assert_eq!(state.bar().meta, route_meta("/sessions"));
    }

    #[test]
    fn a_transparent_overlay_never_retitles_the_bar() {
        // A sheet/dialog is pushed straight through the NavigatorController, so
        // it adds depth with no mirror entry — and popping it must not take the
        // covered route's entry with it.
        let mut state = mirror(&[(2, route_meta("/sessions"))], 2);
        assert!(!state.reconcile(3), "the sheet pushed depth to 3");
        assert_eq!(state.bar().meta, route_meta("/sessions"));
        assert!(!state.reconcile(2), "…and dismissing it drops nothing");
        assert_eq!(state.bar().meta, route_meta("/sessions"));
    }

    #[test]
    fn a_bar_less_route_hides_the_bar_even_at_depth() {
        // The guard bouncing an unauthenticated /sessions to / records `/`'s
        // (absent) metadata, so the bar hides rather than titling a route the
        // push never reached.
        let state = mirror(&[(2, route_meta(CONNECT_LOCATION))], 2);
        assert_eq!(state.bar().meta, None);
    }
}
