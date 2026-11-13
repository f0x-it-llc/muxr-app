//! The route table, its session guard, and the app-wide [`Nav`] handle. `/` lives on the outer
//! navigator; the bar-bearing routes are children of [`shell_route`], whose scaffold hosts an
//! inner navigator, so in-shell navigation never touches the outer stack.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

use frust::{
    BackPolicy, Either, Get, GetUntracked, Location, NavigatorController, PushOptions, Resolution,
    ResolvedPage, Route, RouteObserver, RouteParams, Router, RwSignal, Set, SizedBox,
    TransitionSpec, View, shell_route,
};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::features::connect::presentation::pages::{add_server_page, connect_page};
use crate::features::sessions::presentation::pages::sessions_page;
use crate::features::sessions::presentation::widgets::host_of;
use crate::features::settings::presentation::pages::settings_page;
use crate::features::terminal::presentation::pages::terminal_page;

pub(crate) const CONNECT_LOCATION: &str = "/";
const SESSIONS_ROUTE: &str = "/sessions";
const TERMINAL_ROUTE: &str = "/terminal";
pub const SETTINGS_ROUTE: &str = "/settings";

/// The `/terminal` query keys, shared by the screens that push it and the one that reads it.
pub const TERMINAL_SESSION_QUERY_KEY: &str = "session";
/// The zero-based tab index a pane pill targets.
pub const TERMINAL_TAB_QUERY_KEY: &str = "tab";
pub const TERMINAL_PANE_QUERY_KEY: &str = "pane";
pub const TERMINAL_SPACE_QUERY_KEY: &str = "space";

pub const CONNECT_MODE_QUERY_KEY: &str = "mode";
pub const CONNECT_MODE_ADD: &str = "add";
pub const CONNECT_SERVER_QUERY_KEY: &str = "server";
pub const ADD_SERVER_LOCATION: &str = "/?mode=add";

/// The endpoint is percent-encoded whole, so no character in it can end its value.
pub fn add_server_location(endpoint: Option<&str>) -> String {
    match endpoint {
        Some(endpoint) => format!(
            "{ADD_SERVER_LOCATION}&{CONNECT_SERVER_QUERY_KEY}={}",
            utf8_percent_encode(endpoint, NON_ALPHANUMERIC)
        ),
        None => ADD_SERVER_LOCATION.to_string(),
    }
}

fn is_add_mode(location: &Location) -> bool {
    location
        .query
        .get(CONNECT_MODE_QUERY_KEY)
        .is_some_and(|mode| mode == CONNECT_MODE_ADD)
}

pub(crate) fn home_page(overlays: NavigatorController<AppState>) -> impl View<AppState> {
    connect_page(overlays)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarTitle {
    ConnectedHost,
    Fixed(&'static str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarLeading {
    Brand,
    Back,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarAction {
    Settings,
    TerminalMenu,
}

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

/// `None` for a bar-less route; the bar-bearing routes are exactly the shell's children.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BarDirection {
    #[default]
    Forward,
    Back,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BarState {
    pub meta: Option<RouteMeta>,
    pub direction: BarDirection,
    /// The terminal's fullscreen request, published through [`Nav::set_fullscreen`].
    pub fullscreen: bool,
}

pub const fn route_transition() -> TransitionSpec {
    TransitionSpec::glyph()
}

#[derive(Clone, Copy)]
enum Verb {
    Push,
    Go,
}

/// Clones share one router. A screen resolves it with [`app_nav`] in `Component::init` and
/// keeps it in its `State`, never inside a press handler.
#[derive(Clone)]
pub struct Nav {
    router: Rc<Router<AppState>>,
    inner: NavigatorController<AppState>,
    routes: RouteObserver,
    /// Its own signal because a screen must clear it from a `Send + Sync` context.
    fullscreen: RwSignal<bool>,
    /// Kept resolved: re-resolving would re-run the guard and could hand the shell a Connect page.
    landing: Rc<RefCell<Option<ResolvedPage<AppState>>>>,
    /// Post-guard; the router's own record misses [`Self::place_shell`], which it does not apply.
    last_location: Rc<RefCell<Option<Location>>>,
    /// Honoured only while the outer generation matches, since the published stack lags a
    /// placement by one rebuild.
    pending_shell: Rc<Cell<Option<u64>>>,
    /// Read first by [`Self::shell_is_placed`], so re-entering the shell that frame places a fresh scaffold.
    pending_unwind: Rc<Cell<Option<u64>>>,
    queued_pop: Rc<Cell<Option<(bool, u64)>>>,
}

/// The only page a settle-time [`Nav::pop_marked`] may take off; plain `Send + Sync` data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageMark {
    outer: bool,
    generation: u64,
}

#[derive(Clone, Copy)]
pub struct BarFullscreen(RwSignal<bool>);

impl BarFullscreen {
    pub fn set(self, fullscreen: bool) {
        if self.0.get_untracked() != fullscreen {
            self.0.set(fullscreen);
        }
    }
}

impl Nav {
    /// Guarded; an in-shell push lands on the inner navigator and issues no outer op.
    pub fn push(&self, location: &str) {
        self.navigate(location, Verb::Push);
    }

    /// Replace semantics with no transition: no `go` may animate a page already on screen.
    pub fn go(&self, location: &str) {
        self.navigate(location, Verb::Go);
    }

    /// A chain's page count says whether it crosses the shell boundary; a bounce never pushes.
    fn navigate(&self, location: &str, verb: Verb) {
        let mut chain = None;
        let mut bounced = false;
        let mut adding = false;
        if let Resolution::Matched {
            location: resolved,
            pages,
            ..
        } = self.router.resolve(location)
        {
            bounced = resolved.path != Location::parse(location).path;
            adding = !bounced && is_add_mode(&resolved);
            *self.last_location.borrow_mut() = Some(resolved);
            chain = Some(pages);
        }
        let Some(pages) = chain else {
            self.apply(location, verb);
            return;
        };
        let placed = self.shell_is_placed();
        if pages.len() > 1 {
            if !placed {
                self.place_shell(pages, verb);
                return;
            }
        } else if adding && matches!(verb, Verb::Push) {
            self.cover(pages);
            return;
        } else if placed {
            self.unwind_shell();
            return;
        } else if bounced {
            // `/` is the outer root, so a push would stack a second Connect.
            return;
        }
        self.apply(location, verb);
    }

    fn apply(&self, location: &str, verb: Verb) {
        match verb {
            Verb::Push => self.router.push(location),
            Verb::Go => self.router.go(location),
        }
    }

    /// The leaf becomes the inner navigator's root, so a back press at the landing route falls
    /// through to the outer navigator; the page below stays for the shell's closing pop.
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

    /// Over a placed shell it must not unwind; carries the slide a router push has no seam for.
    fn cover(&self, pages: Vec<ResolvedPage<AppState>>) {
        let Some(leaf) = pages.into_iter().next_back() else {
            return;
        };
        let route = leaf.location().clone();
        self.router.controller().push_with_options(
            move || leaf.build(),
            PushOptions::opaque()
                .transition(route_transition())
                .route(route),
        );
    }

    fn shell_is_covered(&self) -> bool {
        let stack = self.router.controller().route_stack();
        let entries = stack.entries();
        let top_outside_shell = matches!(
            entries.last(),
            Some(Some(location)) if route_meta(&location.path).is_none()
        );
        top_outside_shell
            && entries
                .iter()
                .flatten()
                .any(|location| route_meta(&location.path).is_some())
    }

    /// A `go` to Connect drops the shell and anything covering it in one op; Connect pushed over
    /// it would keep the authenticated subtree reachable. The removal is still recorded so a
    /// same-frame re-entry places a fresh shell.
    fn unwind_shell(&self) {
        self.router.go(CONNECT_LOCATION);
        let generation = self.router.controller().route_generation();
        self.pending_shell.set(None);
        self.pending_unwind.set(Some(generation));
        self.queued_pop.set(Some((true, generation)));
    }

    pub(crate) fn landing_route(&self) -> Option<Location> {
        self.landing
            .borrow()
            .as_ref()
            .map(|page| page.location().clone())
    }

    /// A shell placed with no leaf opens on an empty box rather than panicking.
    pub(crate) fn landing_page(&self) -> impl View<AppState> + use<> {
        match self.landing.borrow().as_ref() {
            Some(page) => Either::Left(page.build()),
            None => Either::Right(SizedBox::<AppState>(Some(0.0), Some(0.0))),
        }
    }

    /// Checks the queued removal first, then the published stack, then the queued placement.
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

    /// Mirrors frust's innermost-first back arbitration so `‹` and Android back agree; a page
    /// covering the shell silences the inner navigator. `can_pop` is a rebuild old.
    pub fn pop(&self) {
        let outer = self.shell_is_covered() || !self.inner.can_pop();
        self.pop_on(outer);
    }

    fn pop_on(&self, outer: bool) {
        let controller = if outer {
            self.router.controller()
        } else {
            &self.inner
        };
        self.queued_pop
            .set(Some((outer, controller.route_generation())));
        controller.pop();
    }

    pub fn mark_top(&self) -> PageMark {
        let outer = self.shell_is_covered() || !self.shell_is_placed();
        let generation = if outer {
            self.router.controller().route_generation()
        } else {
            self.inner.route_generation()
        };
        PageMark { outer, generation }
    }

    /// Skipped once that stack has published a change or a pop of it is queued this frame: a late
    /// settle must not pop the page now on top.
    pub fn pop_marked(&self, mark: &PageMark) {
        let generation = if mark.outer {
            self.router.controller().route_generation()
        } else {
            self.inner.route_generation()
        };
        if generation != mark.generation || self.queued_pop.get() == Some((mark.outer, generation))
        {
            return;
        }
        self.pop_on(mark.outer);
    }

    /// Read tracked, so a navigation or fullscreen request wakes the rebuild that retitles the bar.
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

    /// Read tracked, so a navigation shows or drops the switcher FAB.
    pub fn on_sessions(&self) -> bool {
        self.routes.path() == SESSIONS_ROUTE
    }

    pub(crate) fn route_observer(&self) -> RouteObserver {
        self.routes
    }

    /// Read it only in a screen's `Component::init`: a page is built during the navigation that
    /// resolved it, so a later read answers for another navigation.
    pub fn param(&self, key: &str) -> Option<String> {
        self.last_location
            .borrow()
            .as_ref()
            .and_then(|location| location.query.get(key).cloned())
    }

    pub fn fullscreen(&self) -> BarFullscreen {
        BarFullscreen(self.fullscreen)
    }

    /// Idempotent: re-setting the current value wakes nothing.
    pub fn set_fullscreen(&self, fullscreen: bool) {
        self.fullscreen().set(fullscreen);
    }

    pub fn resolve(&self, location: &str) -> Resolution<AppState> {
        self.router.resolve(location)
    }
}

thread_local! {
    static APP_NAV: RefCell<Option<Nav>> = const { RefCell::new(None) };

    /// Seeded with a cell nobody bumps, so a screen mounted outside [`Nav::push`] still resolves one.
    static SESSIONS_BACK: RefCell<Rc<Cell<u64>>> = RefCell::new(Rc::new(Cell::new(0)));
}

/// A fresh cell per placement, so a new mount cannot inherit a bump aimed at the page it replaced.
fn publish_sessions_back_signal() -> Rc<Cell<u64>> {
    let signal = Rc::new(Cell::new(0));
    SESSIONS_BACK.with(|cell| *cell.borrow_mut() = Rc::clone(&signal));
    signal
}

/// The navigator bumps it on a back press instead of popping; Sessions turns that into its
/// disconnect confirmation. Read only in `Component::init`; never panics, unlike [`app_nav`].
pub fn sessions_back_signal() -> Rc<Cell<u64>> {
    SESSIONS_BACK.with(|cell| Rc::clone(&cell.borrow()))
}

/// `controller` is the outer navigator and `overlays` the root overlay host's; the inner one is
/// minted here. Called once from `MuxrApp::init`, and by headless tests right after it.
pub fn install_app_router(
    controller: &NavigatorController<AppState>,
    overlays: &NavigatorController<AppState>,
    connection: Arc<ConnectionController>,
) -> Rc<Router<AppState>> {
    // The table precedes the router, so the shell page resolves [`app_nav`] at build time.
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
            queued_pop: Rc::new(Cell::new(None)),
        });
    });
    router
}

/// Panics if [`install_app_router`] has not run on this thread.
pub fn app_nav() -> Nav {
    APP_NAV.with(|cell| cell.borrow().clone()).expect(
        "app_nav: routes::install_app_router must run first (MuxrApp::init calls it; a test \
         driving its own NavigatorController calls it right after)",
    )
}

/// Bounces an unauthenticated navigation into the shell's subtree back to `/`; the subtree comes
/// from [`route_meta`], so a new shell route is guarded on arrival. Reads the captured controller,
/// never context, so it is safe from an owner-less press handler. Runs at navigation time only.
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

/// `/` is declared first: the pathless shell matches the empty segment list too, and the first
/// match wins.
pub(crate) fn build_routes(
    inner: &NavigatorController<AppState>,
    overlays: &NavigatorController<AppState>,
    connection: Arc<ConnectionController>,
) -> Vec<Route<AppState>> {
    vec![
        Route::new(CONNECT_LOCATION, {
            let overlays = overlays.clone();
            move |params: &RouteParams| {
                if params
                    .get(CONNECT_MODE_QUERY_KEY)
                    .is_some_and(|mode| mode == CONNECT_MODE_ADD)
                {
                    let prefill = params.get(CONNECT_SERVER_QUERY_KEY).cloned();
                    Either::Right(add_server_page(overlays.clone(), prefill))
                } else {
                    Either::Left(home_page(overlays.clone()))
                }
            }
        }),
        shell_route(
            inner,
            {
                let inner = inner.clone();
                let overlays = overlays.clone();
                move |_params: &RouteParams| {
                    crate::shell::shell_page(&inner, &overlays, &connection)
                }
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

    /// Runs with no reactive owner on the stack, the state a desktop press handler runs in.
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

    #[test]
    fn every_bar_bearing_route_resolves_through_the_shell_page() {
        // A bar-bearing route resolves to the shell page plus its own; `/` stays one flat page.
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
    fn add_mode_is_a_flat_root_page_outside_the_guard() {
        // It must resolve flat, or a push would place a second scaffold instead of covering one.
        let router = table_router();
        assert_eq!(router.resolve(ADD_SERVER_LOCATION).page_count(), 1);
        assert!(is_add_mode(&Location::parse(ADD_SERVER_LOCATION)));
        assert!(!is_add_mode(&Location::parse(CONNECT_LOCATION)));
        let guard = session_guard_with(Arc::new(ConnectionController::detached()));
        assert_eq!(guard(&Location::parse(ADD_SERVER_LOCATION)), None);
    }

    #[test]
    fn a_hostile_prefill_round_trips_without_touching_the_query() {
        let hostile = "evil.example:1&mode=home&server=x%41=y#z?q [::1]";
        let raw = add_server_location(Some(hostile));
        let location = Location::parse(&raw);
        assert_eq!(location.path, CONNECT_LOCATION);
        assert!(is_add_mode(&location), "{raw}");
        assert_eq!(
            location
                .query
                .get(CONNECT_SERVER_QUERY_KEY)
                .map(String::as_str),
            Some(hostile),
            "{raw}"
        );
        assert_eq!(location.query.len(), 2, "no extra key leaked: {raw}");

        let params = table_router().resolve(&raw).params();
        assert_eq!(
            params.get(CONNECT_SERVER_QUERY_KEY).map(String::as_str),
            Some(hostile),
            "the route builder reads it decoded"
        );
        assert_eq!(add_server_location(None), ADD_SERVER_LOCATION);
    }

    #[test]
    fn the_shell_swallows_no_path_of_its_own() {
        let router = table_router();
        assert!(
            router.resolve("/nope").page_count() == 0,
            "unmatched errors"
        );
    }

    #[test]
    fn connect_is_the_only_route_without_a_bar() {
        assert_eq!(route_meta("/"), None, "Connect is full-bleed, no bar");
        for path in [SESSIONS_ROUTE, TERMINAL_ROUTE, SETTINGS_ROUTE] {
            assert!(route_meta(path).is_some(), "{path} carries a bar");
        }
    }

    #[test]
    fn only_the_shells_landing_route_wears_the_brand() {
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
        let spec = route_transition();
        assert_eq!(spec.preset, frust::PageTransition::Glyph);
        assert_eq!(spec.timing, frust::Timing::ThemeDefault);
        assert!(spec.is_animated());
        assert_eq!(spec, TransitionSpec::glyph());
    }

    #[test]
    fn the_default_bar_state_is_visible_and_unclaimed() {
        assert!(!BarState::default().fullscreen);
        assert_eq!(BarState::default().meta, None);
        assert_eq!(BarState::default().direction, BarDirection::Forward);
    }

    #[test]
    fn each_shell_placement_publishes_a_fresh_dismiss_cell_at_generation_zero() {
        // A mount that inherited the departed page's counter would read a stale bump as a back press.
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
        let seed = sessions_back_signal();
        assert!(
            Rc::ptr_eq(&seed, &sessions_back_signal()),
            "stable per thread"
        );
    }
}
