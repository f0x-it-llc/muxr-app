//! Muxr's full route table — a hub file: each page builder lives in its
//! owning feature's `presentation::pages` module (see each feature's own
//! module docs for the `{domain, data, presentation}` layering convention
//! `examples/huddle`'s `src/README-phase-c.md` establishes).
//!
//! Four routes, one per feature, all flat leaves today: `/` (Connect),
//! `/sessions`, `/terminal`, `/settings`. [`session_guard`] is the one guard:
//! a top-level [`frust::Router::redirect`] bouncing an unauthenticated push to
//! `/sessions`/`/terminal` back to `/`.

use frust::{AnyView, GetUntracked, Location, NavigatorController, Route, RouteParams};

use crate::AppState;
use crate::connection::expect_connection_controller;
use crate::features::connect::presentation::pages::connect_page;
use crate::features::sessions::presentation::pages::sessions_page;
use crate::features::settings::presentation::pages::settings_page;
use crate::features::terminal::presentation::pages::terminal_page;

/// The `/` route (and the navigator's initial page): Connect. Takes the
/// [`NavigatorController`] so this function fits both [`build_routes`]'s
/// `"/"` closure and the navigator's `initial` closure identically (see
/// `examples/huddle`'s `home_page` for the same dual-use shape); forwarded
/// straight through to [`connect_page`], unused there today — Phase 2's
/// push-on-successful-connect flow is the reason it stays.
pub fn home_page(controller: NavigatorController<AppState>) -> AnyView<AppState> {
    connect_page(controller)
}

/// Muxr's session guard: a top-level redirect (see [`frust::Router::redirect`],
/// wired onto the router in `crate::MuxrApp::init`) bouncing an
/// unauthenticated push to `/sessions`/`/terminal` back to `/`. `/` and
/// `/settings` are never guarded.
///
/// Reads [`crate::connection::ConnectionController::connected`] via
/// [`expect_connection_controller`] — the guard has no `&mut AppState` to
/// reach a controller through any other way, so `use_context` (via
/// `expect_controller`) underneath is the only route in.
///
/// **Navigation-time only.** [`frust::Router::resolve`] (which this feeds)
/// runs on `go`/`push`/`handle_location`, never on its own — so a connection
/// that drops *while* `/sessions`/`/terminal` is already on-screen does not
/// reactively bounce it back to `/`. A live, reactive bounce-on-disconnect is
/// a functional-phase concern, deliberately out of scope here (this reads the
/// signal `get_untracked`, establishing no reactive dependency at all).
pub fn session_guard(loc: &Location) -> Option<String> {
    let guarded = matches!(loc.path.as_str(), "/sessions" | "/terminal");
    if guarded && !expect_connection_controller().connected.get_untracked() {
        Some("/".to_string())
    } else {
        None
    }
}

/// Build the full route table. `controller` is created *before* this table
/// (so there is no circularity with the [`Router`](frust::Router) it
/// attaches to) and cloned into every builder — all four page builders
/// receive it today (see [`home_page`]'s doc); Wave 2's screens push through
/// it, and pre-wiring it here now keeps them out of this file.
pub fn build_routes(controller: NavigatorController<AppState>) -> Vec<Route<AppState>> {
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
