//! Muxr — clean-architecture app skeleton (huddle-shaped).
//!
//! Four routes over a stack-style [`Router`] (no tabs — contrast
//! `examples/huddle`'s tab shell): `/` (Connect), `/sessions`, `/terminal`,
//! `/settings` (see `routes.rs`). Each feature under [`features`] follows
//! the `{domain, data, presentation}` clean-architecture slice
//! `examples/huddle` establishes (see its `src/README-phase-c.md`):
//! dependencies point inward only, `presentation` → `domain` ← `data`. Only
//! `connect`'s presentation layer is real today (a static Connect screen, no
//! domain/data logic behind it); the other three are placeholder pages —
//! Phase 2 wires each feature's real logic and adds route guards/redirects,
//! the seam [`shell::root_view`] and [`routes::build_routes`] are left thin
//! to grow into.
//!
//! This file is the **composition root**: `Component::init` publishes the
//! app-scoped mock [`connection::ConnectionController`] via
//! [`connection::provide_connection_controller`], calls each feature's own
//! [`features::connect::di::provide`]-shaped wiring stub, then builds the
//! [`Router`]/[`NavigatorController`] pair through
//! [`routes::install_app_router`] (which wires the session guard — closed over
//! that connection controller — as the router's top-level redirect, and
//! publishes the router as the app-wide [`routes::Nav`] handle every screen
//! pushes paths through, and whose route-meta mirror the shell's persistent
//! top bar reads) and resolves the start location once;
//! `Component::build` mounts [`shell::root_view`] over it — the full-height
//! navigator, the top bar floating over it, and the toast layer — plus, on the
//! first build of a cold start, the splash overlay
//! ([`AppState::splash_plan`], [`ui::splash::sequence`]). No feature
//! repository exists yet to construct here — a real one joins this function
//! the same way `examples/huddle`'s `HuddleApp::init` constructs and
//! `provide_context`s each feature's repository, once a feature's `data`
//! layer has one to build; each feature's own `di.rs` is the seam that will
//! grow it, called from here.
//!
//! **Dev-machine-only scaffold**: this variant's `Cargo.toml` path-deps into
//! the Frust checkout that generated it, plus a sibling `clean-signals-rs`
//! checkout — see that file's dependency comments. Never edit
//! [`frust::app!`]'s invocation below or `main.rs`; add features above it
//! instead.

pub mod connection;
pub mod failure;
pub mod features;
pub mod routes;
pub mod ui;

mod shell;

use std::rc::Rc;
use std::sync::Arc;

use frust::{AnyView, Component, Get, NavigatorController, Router, RwSignal};

use features::connect::presentation::pages::connect_layout;
use features::connect::presentation::pieces::{active_theme, connect_pieces};
use ui::splash::icon_phase::{IconArt, splash_icon_art};
use ui::splash::sequence::{SplashPlan, claim_cold_start, reduced_splash, splash_sequence};

/// The location muxr resolves at launch — `/` (Connect), its one entry point
/// today (no cold-start deep-link source is wired yet; see the module docs'
/// Phase 2 note). Named because two things read it: the router, and the
/// splash's cold-start gate, which only plays over this route.
const START_LOCATION: &str = "/";

/// The seed fixing the splash's rain field and burst particles.
///
/// Deliberately a constant rather than a per-launch value: `SplashRng` exists
/// so the sequence is replayable, the difference between two seeded fields is
/// imperceptible across a three-second splash, and a fixed seed keeps the
/// headless sequence test asserting on one exact field.
const SPLASH_SEED: u64 = 0x6D75_7872; // "muxr"

/// Muxr's retained state (the framework's `Component::State`): the
/// stack-style [`Router`] every screen navigates through. `Rc` so the same
/// router can be shared with [`routes::app_nav`]'s handle without a second
/// one existing — see `examples/huddle`'s `HuddleState::nav` for the same
/// shape (there wrapping a `RouterDeepLinks`; muxr has no deep-link source to
/// wire yet, so this wraps the plain `Router` directly).
pub struct AppState {
    /// The stack-style router every route navigates through — the *same*
    /// `Rc` [`routes::Nav`] wraps (see that module's docs: a screen reaches it
    /// through [`routes::app_nav`], because a `Component`'s callbacks never
    /// see this outer state).
    pub nav: Rc<Router<AppState>>,
    /// The app's own [`routes::Nav`] handle — resolved once here, with every
    /// other app-scoped dependency, and read by [`shell::root_view`] for the
    /// persistent top bar (which route is on top, and what its `‹`/`⚙` do).
    /// The same handle every screen resolves through [`routes::app_nav`], so
    /// the bar and the screens share one route-meta mirror.
    nav_handle: routes::Nav,
    /// The app-scoped mock connection handle, for the bar's `/sessions` host
    /// title. Resolved in `init` (never from a build-time context read), the
    /// same discipline every screen follows.
    connection: Arc<connection::ConnectionController>,
    /// What this launch's cold-start splash should be. `None` until the
    /// **first build** resolves it — see [`AppState::splash_plan`] for why
    /// that is deliberately not `Component::init`.
    splash: Option<SplashPlan>,
    /// The decoded app icon, decoded once alongside that decision rather than
    /// per rebuild (the contract [`splash_icon_art`] documents). Stays `None`
    /// when this launch has no splash to paint it with.
    icon: Option<IconArt>,
    /// Flipped `true` by the sequence's own completion cue. Read (tracked) in
    /// [`Component::build`], so that one write is what unmounts the overlay.
    splash_done: RwSignal<bool>,
}

impl AppState {
    /// Resolve — once per process — what this launch's splash should be,
    /// decoding the app icon if it will be needed.
    ///
    /// **Called from the first `build`, not from `init`.** On desktop
    /// `frust::run_with_setup` calls `root.init()` *before*
    /// `run_desktop`, so at `init` time the shell has neither installed its
    /// logger nor seeded the `Theme` context — a `reduce_motion` read there is
    /// guaranteed to see the Glyph baseline's `false` no matter what the app's
    /// theme actually says. The first `build` runs inside the frame loop,
    /// after `apply_theme`, where the read is real.
    ///
    /// Idempotent regardless: [`claim_cold_start`] spends a process-wide
    /// atomic, and the answer is cached here for every later build.
    fn splash_plan(&mut self) -> SplashPlan {
        if let Some(plan) = self.splash {
            return plan;
        }
        let plan = claim_cold_start(START_LOCATION, active_theme().motion.reduce_motion);
        self.splash = Some(plan);
        if plan != SplashPlan::Skip {
            self.icon = Some(splash_icon_art());
        }
        plan
    }

    /// The splash overlay to stack over the navigator this frame, or `None`
    /// once there is nothing left to show.
    ///
    /// The full sequence assembles the *same* pieces through the *same*
    /// layout function the `/` route renders, which is what makes the
    /// unmount invisible — see [`crate::ui::splash::sequence`].
    fn splash_overlay(&mut self) -> Option<AnyView<AppState>> {
        let plan = self.splash_plan();
        // Tracked read: the cue's single `set(true)` wakes this rebuild.
        if plan == SplashPlan::Skip || self.splash_done.get() {
            return None;
        }
        let art = self.icon.clone()?;
        match plan {
            SplashPlan::Full => Some(splash_sequence(
                art,
                SPLASH_SEED,
                connect_pieces(&active_theme()),
                connect_layout,
                self.splash_done,
            )),
            SplashPlan::Reduced => Some(reduced_splash(art, self.splash_done)),
            SplashPlan::Skip => None,
        }
    }
}

/// Muxr's root [`Component`] (`Default`, as [`frust::app!`] requires):
/// stateless configuration wiring the router + shell together.
#[derive(Default)]
pub struct MuxrApp;

impl Component for MuxrApp {
    type State = AppState;

    fn init(&self) -> AppState {
        // App-scoped mock connection state, published once under the root
        // owner (see `connection`'s module docs) — reachable from any screen
        // via `use_context`, and handed *by value* to the session guard below,
        // which must stay safe to consult from a press handler's owner-less
        // call stack and so never reads context itself.
        let connection = connection::provide_connection_controller();

        // Each feature's own composition-root wiring seam — empty today (see
        // each `di.rs`'s doc), grows real repo construction once a feature's
        // `data` layer has one to build.
        features::connect::di::provide();
        features::sessions::di::provide();
        features::terminal::di::provide();
        features::settings::di::provide();

        // The controller is created *before* the route table (so every
        // route's page can push through the same controller the router
        // drives). `install_app_router` builds the table + the guarded router
        // over it and publishes the router as the app-wide `routes::Nav` every
        // screen resolves in its own `Component::init` — this is the app's
        // ONE `Router`, and the only place one is ever constructed.
        let controller = NavigatorController::new();
        let router = routes::install_app_router(&controller, Arc::clone(&connection));
        // Resolves the start location once (see `START_LOCATION`).
        router.handle_location(START_LOCATION);

        // The cold-start splash is deliberately NOT decided here — see
        // `AppState::splash_plan` for the ordering that forces it into the
        // first `build`.
        AppState {
            nav: router,
            // Published by `install_app_router` a line above; resolved here so
            // `build` never reaches for a thread-local (and so the shell's bar
            // shares the screens' one route-meta mirror).
            nav_handle: routes::app_nav(),
            connection,
            splash: None,
            icon: None,
            splash_done: RwSignal::new(false),
        }
    }

    fn build(&self, state: &mut AppState) -> AnyView<AppState> {
        let controller = state.nav.controller().clone();
        // A separate clone for the navigator's `initial` closure (only ever
        // invoked on the very first build).
        let initial_controller = controller.clone();
        let overlay = state.splash_overlay();

        shell::root_view(
            &controller,
            move || routes::home_page(initial_controller.clone()),
            overlay,
            &state.nav_handle,
            &state.connection,
        )
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
