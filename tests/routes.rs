//! Route smoke tests: each of the 4 routes' placeholder page renders text
//! headlessly (no GPU, no window) — driven through the full `MuxrApp` shell
//! via `tests/support/mod.rs` (lifted from `examples/huddle`'s own harness of
//! the same shape). The bottom half proves `muxr::routes::session_guard`'s
//! behavior directly against `Router::resolve` (pure — no navigator side
//! effects), so no scene inspection is needed to tell Connect apart from a
//! guarded route's own placeholder.

use frust::{AnyView, Component, Resolution};
use frust_core::RenderRoot;
use frust_text::TextContext;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
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
        "the Terminal placeholder (app bar title + centered page text) renders text"
    );
}

#[test]
fn settings_route_renders_its_placeholder() {
    let scene = render_route("/settings");
    assert!(
        scene.glyph_runs > 0,
        "the Settings placeholder (app bar title + centered page text) renders text"
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
// Session guard (`muxr::routes::session_guard`)
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
    match state.nav.resolve(raw) {
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
