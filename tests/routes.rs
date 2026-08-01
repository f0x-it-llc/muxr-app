//! Route smoke tests: each of the 4 routes' placeholder page renders text
//! headlessly (no GPU, no window) — driven through the full `MuxrApp` shell
//! via `tests/support/mod.rs` (lifted from `examples/huddle`'s own harness of
//! the same shape). The middle section proves the session guard's behavior
//! directly against `Router::resolve` (pure — no navigator side effects), so no
//! scene inspection is needed to tell Connect apart from a guarded route's own
//! placeholder; the last section proves the same guard applies to the handle
//! screens actually push through (`muxr::routes::app_nav`), and that the handle
//! drives the app's own navigator rather than a copy of it.
//!
//! The shell's persistent top bar is deliberately **not** exercised here: these
//! tests navigate with a bare `Router::go`, which is the one navigation entry
//! point that never records the bar's route-meta mirror (only
//! `muxr::routes::Nav` does). `tests/navigation_motion.rs` owns the bar.

use frust::{AnyView, Component, Resolution};
use frust_core::RenderRoot;
use frust_text::TextContext;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::routes::{Nav, app_nav};
use muxr::{AppState, MuxrApp};

mod support;
use support::{frame, serial, setup};

type Root = RenderRoot<AppState, AnyView<AppState>>;

/// Mounts `MuxrApp`, navigates to `route`, and renders one frame.
fn render_route(route: &str) -> support::RecScene {
    let _g = serial();
    let _ambient = setup();

    let mut root: Root = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| MuxrApp.build(s);
    let mut tcx = TextContext::new();

    state.nav.go(route);
    frame(&mut root, &mut logic, &mut state, &mut tcx)
}

#[test]
fn connect_route_renders_its_placeholder() {
    let scene = render_route("/");
    assert!(
        scene.glyph_runs > 0,
        "the Connect placeholder (app bar title + centered page text) renders text"
    );
}

#[test]
fn sessions_route_renders_its_placeholder() {
    let scene = render_route("/sessions");
    assert!(
        scene.glyph_runs > 0,
        "the Sessions placeholder (app bar title + centered page text) renders text"
    );
}

#[test]
fn terminal_route_renders_its_placeholder() {
    let scene = render_route("/terminal");
    assert!(
        scene.glyph_runs > 0,
        "the Terminal placeholder (centered page text — its bar is the shell's \
         now, and a bare `Router::go` never records the bar mirror) renders text"
    );
}

#[test]
fn settings_route_renders_its_placeholder() {
    let scene = render_route("/settings");
    assert!(
        scene.glyph_runs > 0,
        "the Settings placeholder (centered page text — its bar is the shell's \
         now, and a bare `Router::go` never records the bar mirror) renders text"
    );
}

/// The shell builds at "/" without any explicit navigation — the same
/// `Router::handle_location("/")` call `MuxrApp::init` makes.
#[test]
fn shell_builds_at_the_connect_route_by_default() {
    let _g = serial();
    let _ambient = setup();

    let mut root: Root = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| MuxrApp.build(s);
    let mut tcx = TextContext::new();

    let scene = frame(&mut root, &mut logic, &mut state, &mut tcx);
    assert!(scene.glyph_runs > 0, "the default \"/\" route renders text");
}

// ---------------------------------------------------------------------------
// Session guard (`muxr::routes::session_guard_with`)
// ---------------------------------------------------------------------------

/// A mock connect target — plain data, none of its fields matter to the guard
/// itself (only `ConnectionController::connected` flipping `true` does).
fn mock_target() -> ConnectTarget {
    ConnectTarget {
        host: "mock.example".to_string(),
        port: 7777,
        trust_mode: TrustMode::System,
        label: "Mock rig".to_string(),
        offer_credential: false,
    }
}

/// The final resolved path for `raw`, post-redirect — panics on an error
/// resolution (every location this suite resolves is a real route or a
/// redirect to one).
fn resolved_path(state: &AppState, raw: &str) -> String {
    matched_path(state.nav.resolve(raw))
}

/// [`resolved_path`] through the app's published [`Nav`] handle — the object
/// every screen actually pushes with.
fn resolved_path_through(nav: &Nav, raw: &str) -> String {
    matched_path(nav.resolve(raw))
}

/// The matched path out of a [`Resolution`], or a panic.
fn matched_path(resolution: Resolution<AppState>) -> String {
    match resolution {
        Resolution::Matched { location, .. } => location.path,
        Resolution::Error { location } => {
            panic!("expected a matched resolution for {location:?}, got Resolution::Error")
        }
    }
}

/// An unauthenticated push to `/sessions` bounces to Connect (`/`).
#[test]
fn unauthenticated_sessions_push_redirects_to_connect() {
    let _g = serial();
    let _ambient = setup();
    let state = MuxrApp.init();

    assert_eq!(
        resolved_path(&state, "/sessions"),
        "/",
        "an unauthenticated push to /sessions bounces to Connect"
    );
}

/// An unauthenticated push to `/terminal` bounces to Connect (`/`).
#[test]
fn unauthenticated_terminal_push_redirects_to_connect() {
    let _g = serial();
    let _ambient = setup();
    let state = MuxrApp.init();

    assert_eq!(
        resolved_path(&state, "/terminal"),
        "/",
        "an unauthenticated push to /terminal bounces to Connect"
    );
}

/// Once the mock `ConnectionController::connect` has run, `/sessions` and
/// `/terminal` resolve normally — the guard only fires while unauthenticated.
#[test]
fn sessions_and_terminal_resolve_normally_once_connected() {
    let _g = serial();
    let _ambient = setup();
    let state = MuxrApp.init();

    expect_connection_controller().connect(mock_target());

    assert_eq!(
        resolved_path(&state, "/sessions"),
        "/sessions",
        "an authenticated push to /sessions is not redirected"
    );
    assert_eq!(
        resolved_path(&state, "/terminal"),
        "/terminal",
        "an authenticated push to /terminal is not redirected"
    );
}

/// `/` and `/settings` are never redirected, authenticated or not — the guard
/// only ever matches `/sessions`/`/terminal`.
#[test]
fn root_and_settings_are_never_redirected() {
    let _g = serial();
    let _ambient = setup();
    let state = MuxrApp.init();

    assert_eq!(resolved_path(&state, "/"), "/", "/ is never redirected");
    assert_eq!(
        resolved_path(&state, "/settings"),
        "/settings",
        "/settings is never redirected while unauthenticated"
    );

    expect_connection_controller().connect(mock_target());

    assert_eq!(
        resolved_path(&state, "/"),
        "/",
        "/ is never redirected once connected either"
    );
    assert_eq!(
        resolved_path(&state, "/settings"),
        "/settings",
        "/settings is never redirected once connected either"
    );
}

// ---------------------------------------------------------------------------
// The published nav handle (`muxr::routes::app_nav`)
// ---------------------------------------------------------------------------

/// The handle every screen pushes through is the **same guarded router** the
/// tests above resolve against — there is no second, unguarded path into
/// `/sessions`/`/terminal` any more.
#[test]
fn a_push_through_the_app_nav_handle_is_guarded_too() {
    let _g = serial();
    let _ambient = setup();
    let _state = MuxrApp.init();
    let nav = app_nav();

    assert_eq!(
        resolved_path_through(&nav, "/sessions"),
        "/",
        "an unauthenticated push through the handle bounces to Connect"
    );
    assert_eq!(
        resolved_path_through(&nav, "/terminal?session=s-1"),
        "/",
        "…query param and all (the guard matches on the path)"
    );

    expect_connection_controller().connect(mock_target());

    assert_eq!(
        resolved_path_through(&nav, "/sessions"),
        "/sessions",
        "a connected push through the handle lands"
    );
    assert_eq!(
        resolved_path_through(&nav, "/terminal?session=s-1"),
        "/terminal",
        "…and so does the attach location"
    );
}

/// …and pushing through it drives the app's **own** navigator: the handle
/// wraps the router `AppState::nav` holds, not a second one over the same
/// controller.
#[test]
fn a_push_through_the_app_nav_handle_lands_on_the_apps_own_navigator() {
    let _g = serial();
    let _ambient = setup();

    let mut root: Root = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| MuxrApp.build(s);
    let mut tcx = TextContext::new();
    frame(&mut root, &mut logic, &mut state, &mut tcx);
    assert_eq!(state.nav.controller().depth(), 1, "only the start location");

    // `/settings` is unguarded, so this asserts the plumbing rather than the
    // redirect the test above covers.
    app_nav().push("/settings");
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    assert_eq!(
        state.nav.controller().depth(),
        2,
        "the push the handle made landed on the app's own page stack"
    );
}

// ---------------------------------------------------------------------------
// The overlay accounting, through the same public handle
// ---------------------------------------------------------------------------

/// The bar's modality flag answers **synchronously**, without a frame in
/// between — which is the whole point of `Nav`'s overlay accounting.
///
/// `NavigatorController::depth` is published by the navigator's own rebuild, a
/// rebuild *after* the app logic that reads it, so a depth-derived answer is
/// always one frame behind the push or pop that changed it. On the dismissal
/// side that frame is the last one anything asks for, which is what used to
/// leave the bar dimmed and inert until unrelated input.
/// (`tests/navigation_motion.rs`' section 3c asserts the same property end to
/// end, on the frames production actually runs; this one asserts the handle's
/// own contract.)
#[test]
fn the_overlay_accounting_dims_and_lifts_the_bar_with_no_frame_in_between() {
    let _g = serial();
    let _ambient = setup();

    let mut root: Root = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| MuxrApp.build(s);
    let mut tcx = TextContext::new();
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    let nav = app_nav();
    nav.push("/settings");
    frame(&mut root, &mut logic, &mut state, &mut tcx);
    assert!(!nav.bar().overlay_active, "a settled route is not modal");

    // Not one frame between these three calls: each publishes on its own.
    nav.begin_overlay();
    assert!(
        nav.bar().overlay_active,
        "the bar goes modal on the frame the overlay is pushed, not the next"
    );

    nav.dismiss_overlay();
    assert!(
        !nav.bar().overlay_active,
        "…and comes back on the frame the dismissal is enqueued"
    );

    nav.end_overlay();
    assert!(!nav.bar().overlay_active, "the completion callback agrees");
}

/// Every step of the accounting saturates, so a dismissal path that reports
/// itself twice (or a completion callback for an overlay the app already
/// closed) cannot push the count below zero and dim a bar nothing covers.
#[test]
fn the_overlay_accounting_cannot_be_driven_negative() {
    let _g = serial();
    let _ambient = setup();

    let mut root: Root = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| MuxrApp.build(s);
    let mut tcx = TextContext::new();
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    let nav = app_nav();
    nav.push("/settings");
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    nav.begin_overlay();
    nav.dismiss_overlay();
    nav.dismiss_overlay();
    nav.end_overlay();
    nav.end_overlay();
    assert!(!nav.bar().overlay_active, "still lit");

    // …and the next real overlay still registers.
    nav.begin_overlay();
    assert!(nav.bar().overlay_active);
}

/// A `go` clears the accounting: `Router::go` **replaces** the top page, and a
/// replaced page's pop-completion callback is dropped rather than delivered —
/// so an overlay that was on top when the app navigated away would otherwise
/// leave its count (and a permanently dimmed bar) behind it.
#[test]
fn a_go_clears_an_overlay_the_replaced_page_never_reported() {
    let _g = serial();
    let _ambient = setup();

    let mut root: Root = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| MuxrApp.build(s);
    let mut tcx = TextContext::new();
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    // Driven from the start location on purpose: `Router::go` *replaces* the
    // top page rather than resetting the stack, so a `go` from a pushed route
    // leaves the live depth one past the reseeded mirror and the depth half of
    // the flag answers `true` on its own — a pre-existing wrinkle this test is
    // not about (and one no bar ever shows, `/` being the only `go`
    // destination in the app and a bar-less route).
    let nav = app_nav();
    nav.begin_overlay();
    assert!(
        nav.bar().overlay_active,
        "an overlay over the start location"
    );

    nav.go("/settings");
    assert!(
        !nav.bar().overlay_active,
        "the stack the overlay belonged to is gone, and so is its count"
    );
}
