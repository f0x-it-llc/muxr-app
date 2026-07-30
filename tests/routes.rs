//! Route smoke tests: each of the 4 routes' placeholder page renders text
//! headlessly (no GPU, no window) — driven through the full `MuxrApp` shell
//! via `tests/support/mod.rs` (lifted from `examples/huddle`'s own harness of
//! the same shape).

use frust::{AnyView, Component};
use frust_core::RenderRoot;
use frust_text::TextContext;

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
