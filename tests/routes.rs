//! Route smoke tests: each route's page renders text headlessly through the full `MuxrApp` shell.
//! The guard is checked against `Router::resolve` and the `app_nav` handle; the top bar is not exercised here.

use frust::{AnyView, Component, Resolution};
use frust_core::RenderRoot;
use frust_text::TextContext;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::routes::{Nav, app_nav};
use muxr::{AppState, MuxrApp};

mod support;
use support::{frame, serial, setup};

type Root = RenderRoot<AppState, AnyView<AppState>>;

fn render(route: &str, connected: bool) -> support::RecScene {
    let _g = serial();
    let _ambient = setup();

    let mut root: Root = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| AnyView::new(MuxrApp.build(s));
    let mut tcx = TextContext::new();

    if connected {
        expect_connection_controller().connect(mock_target());
    }
    app_nav().go(route);
    frame(&mut root, &mut logic, &mut state, &mut tcx)
}

#[test]
fn connect_route_renders_its_placeholder() {
    let scene = render("/", false);
    assert!(
        scene.glyph_runs > 0,
        "the Connect placeholder (app bar title + centered page text) renders text"
    );
}

#[test]
fn sessions_route_renders_its_placeholder() {
    let scene = render("/sessions", true);
    assert!(
        scene.glyph_runs > 0,
        "the Sessions placeholder (app bar title + centered page text) renders text"
    );
}

#[test]
fn terminal_route_renders_its_placeholder() {
    let scene = render("/terminal", true);
    assert!(
        scene.glyph_runs > 0,
        "the Terminal placeholder (centered page text — its bar is the shell's \
         now) renders text"
    );
}

#[test]
fn settings_route_renders_its_placeholder() {
    let scene = render("/settings", true);
    assert!(
        scene.glyph_runs > 0,
        "the Settings placeholder (centered page text — its bar is the shell's \
         now) renders text"
    );
}

#[test]
fn shell_builds_at_the_connect_route_by_default() {
    let _g = serial();
    let _ambient = setup();

    let mut root: Root = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| AnyView::new(MuxrApp.build(s));
    let mut tcx = TextContext::new();

    let scene = frame(&mut root, &mut logic, &mut state, &mut tcx);
    assert!(scene.glyph_runs > 0, "the default \"/\" route renders text");
}

/// A mock connect target — plain data, none of its fields matter to the guard
/// itself (only `ConnectionController::connected` flipping `true` does).
fn mock_target() -> ConnectTarget {
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

fn resolved_path(state: &AppState, raw: &str) -> String {
    matched_path(state.nav.resolve(raw))
}

fn resolved_path_through(nav: &Nav, raw: &str) -> String {
    matched_path(nav.resolve(raw))
}

fn matched_path(resolution: Resolution<AppState>) -> String {
    match resolution {
        Resolution::Matched { location, .. } => location.path,
        Resolution::Error { location } => {
            panic!("expected a matched resolution for {location:?}, got Resolution::Error")
        }
    }
}

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

/// `/` is never redirected — it is what the guard bounces *to*. `/settings` is:
/// it lives inside the shell, so resolving it while unauthenticated would place
/// the whole connected scaffold.
#[test]
fn root_is_never_redirected_and_settings_is_guarded_with_the_rest_of_the_shell() {
    let _g = serial();
    let _ambient = setup();
    let state = MuxrApp.init();

    assert_eq!(resolved_path(&state, "/"), "/", "/ is never redirected");
    assert_eq!(
        resolved_path(&state, "/settings"),
        "/",
        "/settings bounces while unauthenticated"
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
        "…and /settings resolves normally once connected"
    );
}

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

#[test]
fn a_push_through_the_app_nav_handle_lands_on_the_apps_own_navigator() {
    let _g = serial();
    let _ambient = setup();

    let mut root: Root = RenderRoot::new();
    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| AnyView::new(MuxrApp.build(s));
    let mut tcx = TextContext::new();
    frame(&mut root, &mut logic, &mut state, &mut tcx);
    assert_eq!(state.nav.controller().depth(), 1, "only the start location");

    expect_connection_controller().connect(mock_target());
    app_nav().push("/settings");
    frame(&mut root, &mut logic, &mut state, &mut tcx);

    assert_eq!(
        state.nav.controller().depth(),
        2,
        "the push the handle made landed on the app's own page stack"
    );
}
