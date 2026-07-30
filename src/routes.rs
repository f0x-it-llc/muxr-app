//! Muxr's full route table — a hub file: each page builder lives in its
//! owning feature's `presentation::pages` module (see each feature's own
//! module docs for the `{domain, data, presentation}` layering convention
//! `examples/huddle`'s `src/README-phase-c.md` establishes).
//!
//! Four routes, one per feature, all flat leaves today: `/` (Connect),
//! `/sessions`, `/terminal`, `/settings`. **No guard logic yet** — every
//! route renders unconditionally. Phase 2 adds a session guard (bouncing an
//! unauthenticated push to `/sessions`/`/terminal` back to `/`), most likely
//! via [`frust::Router::redirect`] or a per-route
//! [`Route::redirect`](frust::Route::redirect) — this table's flat shape is
//! deliberately left room to grow that way without a rewrite.

use frust::{AnyView, NavigatorController, Route, RouteParams};

use crate::AppState;
use crate::features::connect::presentation::pages::connect_page;
use crate::features::sessions::presentation::pages::sessions_page;
use crate::features::settings::presentation::pages::settings_page;
use crate::features::terminal::presentation::pages::terminal_page;

/// The `/` route (and the navigator's initial page): Connect. Takes the
/// [`NavigatorController`] — unused today — so this function fits both
/// [`build_routes`]'s `"/"` closure and the navigator's `initial` closure
/// identically (see `examples/huddle`'s `home_page` for the same dual-use
/// shape); Phase 2's push-on-successful-connect flow is the reason it stays.
pub fn home_page(_controller: NavigatorController<AppState>) -> AnyView<AppState> {
    connect_page()
}

/// Build the full route table. `controller` is created *before* this table
/// (so there is no circularity with the [`Router`](frust::Router) it
/// attaches to) and cloned into every builder that might need to push a
/// nested page — unused by every route today (see [`home_page`]'s doc).
pub fn build_routes(controller: NavigatorController<AppState>) -> Vec<Route<AppState>> {
    vec![
        Route::new("/", {
            let controller = controller.clone();
            move |_params: &RouteParams| home_page(controller.clone())
        }),
        Route::new("/sessions", |_params: &RouteParams| sessions_page()),
        Route::new("/terminal", |_params: &RouteParams| terminal_page()),
        Route::new("/settings", |_params: &RouteParams| settings_page()),
    ]
}
