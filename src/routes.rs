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

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use frust::{
    AnyView, GetUntracked, Location, NavigatorController, Resolution, Route, RouteParams, Router,
};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::features::connect::presentation::pages::connect_page;
use crate::features::sessions::presentation::pages::sessions_page;
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
// The app's navigation handle
// ---------------------------------------------------------------------------

/// A cheap, clonable path-pushing handle over the app's one [`Router`] — what
/// a screen navigates with. See the [module docs](self) for why this exists
/// (and why it is not in frust's context).
///
/// Cloning is an `Rc` bump, so a press closure can hold its own copy; every
/// clone drives the *same* router, hence the same route table and the same
/// [`session_guard_with`] guard. A screen resolves one with [`app_nav`] in its
/// `Component::init` and keeps it in its own `State` — never resolves one from
/// inside a press handler (there is nothing reactive to read here, but the
/// discipline is the same one every other app-scoped dependency follows).
#[derive(Clone)]
pub struct Nav {
    router: Rc<Router<AppState>>,
}

impl Nav {
    /// Push `location`'s page onto the stack, retaining the page below —
    /// guarded, like every other resolution through this router.
    pub fn push(&self, location: &str) {
        self.router.push(location);
    }

    /// Reset the stack to `location`'s matched chain (replace semantics).
    pub fn go(&self, location: &str) {
        self.router.go(location);
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
}
