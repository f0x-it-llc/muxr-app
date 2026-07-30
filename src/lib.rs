//! Muxr — clean-architecture app skeleton (huddle-shaped).
//!
//! Four routes over a stack-style [`Router`] (no tabs — contrast
//! `examples/huddle`'s tab shell): `/` (Connect), `/sessions`, `/terminal`,
//! `/settings` (see `routes.rs`). Each feature under [`features`] follows
//! the `{domain, data, presentation}` clean-architecture slice
//! `examples/huddle` establishes (see its `src/README-phase-c.md`):
//! dependencies point inward only, `presentation` → `domain` ← `data`. Every
//! feature is an empty skeleton today (one placeholder page, no real
//! domain/data logic) — Phase 2 wires each feature's real logic and adds
//! route guards/redirects, the seam [`shell::root_view`] and
//! [`routes::build_routes`] are left thin to grow into.
//!
//! This file is the **composition root**: `Component::init` builds the
//! [`Router`]/[`NavigatorController`] pair and resolves the start location
//! once; `Component::build` mounts [`shell::root_view`] over it. No feature
//! repository exists yet to construct here — a real one joins this function
//! the same way `examples/huddle`'s `HuddleApp::init` constructs and
//! `provide_context`s each feature's repository, once a feature's `data`
//! layer has one to build.
//!
//! **Dev-machine-only scaffold**: this variant's `Cargo.toml` path-deps into
//! the Frust checkout that generated it, plus a sibling `clean-signals-rs`
//! checkout — see that file's dependency comments. Never edit
//! [`frust::app!`]'s invocation below or `main.rs`; add features above it
//! instead.

pub mod failure;
pub mod features;
pub mod ui;

mod routes;
mod shell;

use std::rc::Rc;

use frust::{AnyView, Component, NavigatorController, Router};

/// Muxr's retained state (the framework's `Component::State`): the
/// stack-style [`Router`] every screen navigates through. `Rc` so a screen's
/// `on_press` closures (rebuilt fresh every `build`) can cheaply clone a
/// handle — see `examples/huddle`'s `HuddleState::nav` for the same shape
/// (there wrapping a `RouterDeepLinks`; muxr has no deep-link source to wire
/// yet, so this wraps the plain `Router` directly).
pub struct AppState {
    /// The stack-style router every route navigates through.
    pub nav: Rc<Router<AppState>>,
}

/// Muxr's root [`Component`] (`Default`, as [`frust::app!`] requires):
/// stateless configuration wiring the router + shell together.
#[derive(Default)]
pub struct MuxrApp;

impl Component for MuxrApp {
    type State = AppState;

    fn init(&self) -> AppState {
        // The controller is created *before* the route table (so every
        // route's page can push through the same controller the router
        // drives).
        let controller = NavigatorController::new();
        let routes = routes::build_routes(controller.clone());
        let router = Router::with_controller(&controller, routes);
        // Resolves the start location once — "/" (Connect), muxr's one entry
        // point today (no cold-start deep-link source wired yet; see the
        // module docs' Phase 2 note).
        router.handle_location("/");

        AppState {
            nav: Rc::new(router),
        }
    }

    fn build(&self, state: &mut AppState) -> AnyView<AppState> {
        let controller = state.nav.controller().clone();
        // A separate clone for the navigator's `initial` closure (only ever
        // invoked on the very first build).
        let initial_controller = controller.clone();

        shell::root_view(&controller, move || {
            routes::home_page(initial_controller.clone())
        })
    }
}

// Muxr's sole entry point: one line binds `MuxrApp` to all three platforms —
// the Android JNI exports (`target_os = "android"` only), the iOS C-ABI
// exports (unconditional; self-gated to `target_os = "ios"`), and (on
// desktop) the hidden `__frust_main` that `main.rs` calls. Never edit this
// line or `main.rs` — add screens/state to `routes.rs`/`features` above
// instead. The `setup` block runs once, on the UI thread, before the root
// component's `Component::init`; `frust::glyph_theme::install()` is what
// makes Glyph this app's design system. App code that reads the active
// `Theme` (a widget's own paint-time context read, or a future feature's
// `use_context::<Theme>()`) must fall back to `Theme::glyph_baseline()` if
// this call is ever skipped — never `Theme::neutral()` (the shell's own
// no-design-system fallback) or `Theme::m3_baseline()` (huddle's
// switchable-design-language fallback): muxr commits to Glyph as its one
// design system, unlike huddle's three-way switcher.
frust::app!(
    MuxrApp,
    setup = {
        frust::glyph_theme::install();
    }
);
