//! Muxr's composition root — see `docs/ARCHITECTURE.md`'s Composition Root
//! section for the full `init`/`build` contract. Four routes over a
//! stack-style [`Router`] (no tabs — contrast `examples/huddle`'s tab
//! shell); each feature under [`features`] is a `{domain, data,
//! presentation}` slice, real `muxrd` gRPC by default with a mock behind its
//! own `di.rs` seam for headless suites.
//!
//! **Dev-machine-only scaffold**: this variant's `Cargo.toml` path-deps into
//! the Frust checkout that generated it — see that file's dependency
//! comments. Never edit [`frust::app!`]'s invocation below or `main.rs`; add
//! features above it instead.

pub mod connection;
pub mod failure;
pub mod features;
pub mod fonts;
// `net` is `pub` for the same reason every module here is: the headless suites
// in `tests/` are separate crates, and a fixture that stands a fake `muxrd` up
// needs the generated service types by name.
pub mod net;
pub mod responsive;
pub mod routes;
pub mod ui;

mod shell;

use std::rc::Rc;
use std::sync::Arc;

use frust::{AnyView, Component, Get, NavigatorController, Router, RwSignal, any, overlay_host};

use features::connect::domain::entities::SavedServer;
use features::connect::presentation::pages::connect_layout;
use features::connect::presentation::widgets::connect_pieces;
use features::connect::presentation::widgets::saved::splash_saved_servers;
use ui::palette::active_theme;
use ui::splash::icon_phase::{IconArt, splash_icon_art};
use ui::splash::sequence::{SplashPlan, claim_cold_start, reduced_splash, splash_sequence};

/// The location muxr resolves at launch — `/` (Connect), its one entry point
/// today. Named because two things read it: the router, and the splash's
/// cold-start gate, which only plays over this route.
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
/// router is shared with [`routes::app_nav`]'s handle without a second one
/// existing.
pub struct AppState {
    /// The stack-style router every route navigates through — the *same*
    /// `Rc` [`routes::Nav`] wraps; a screen reaches it through
    /// [`routes::app_nav`] since a `Component`'s callbacks never see this
    /// outer state.
    pub nav: Rc<Router<AppState>>,
    /// The **root overlay host**'s navigator: every menu, sheet and dialog in
    /// the app is a page here, wrapping the entire root view (route
    /// navigator, floating top bar, toast layer alike). The host routes
    /// input and forwards accessibility nodes for its top page only, so the
    /// app under an open overlay is inert to pointers and assistive tech
    /// alike, and outranks the route navigator for the Android back press
    /// (R44-back) so back dismisses the overlay rather than the page beneath
    /// it. `pub` for the same reason [`Self::nav`] is: the headless suites
    /// drive it directly.
    pub overlays: NavigatorController<AppState>,
    /// The app's own [`routes::Nav`] handle — resolved once here, with every
    /// other app-scoped dependency, and read by [`shell::root_view`] for the
    /// persistent top bar (which route is on top, and what its `‹`/`⚙` do).
    nav_handle: routes::Nav,
    /// The app-scoped connection handle, for the bar's `/sessions` host
    /// title. Resolved in `init` (never from a build-time context read), the
    /// same discipline every screen follows.
    connection: Arc<connection::ConnectionController>,
    /// What this launch's cold-start splash should be. `None` until the
    /// **first build** resolves it — see [`AppState::splash_plan`] for why
    /// that is deliberately not `Component::init`.
    splash: Option<SplashPlan>,
    /// The decoded app icon, decoded once alongside that decision rather than
    /// per rebuild. `None` when this launch has no splash to paint it with.
    icon: Option<IconArt>,
    /// The saved-server rows the splash's copy of the Connect screen renders —
    /// read from the repository once, beside the icon (the real store is a
    /// preferences read plus a JSON parse, too costly for a per-frame
    /// rebuild). An [`Rc`] slice so re-deriving the cue per rebuild is a
    /// refcount bump. `None` when this launch has no splash.
    saved: Option<Rc<[SavedServer]>>,
    /// Flipped `true` by the sequence's own completion cue. Read (tracked) in
    /// [`Component::build`], so that one write is what unmounts the overlay.
    splash_done: RwSignal<bool>,
}

impl AppState {
    /// Resolve — once per process — what this launch's splash should be,
    /// decoding the app icon if it will be needed.
    ///
    /// **Called from the first `build`, not from `init`.** On desktop
    /// `frust::run_with_setup` calls `root.init()` before `run_desktop`, so at
    /// `init` time the shell has seeded neither its logger nor the `Theme`
    /// context — a `reduce_motion` read there would always see the Glyph
    /// baseline's `false`. The first `build` runs inside the frame loop,
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
            // Resolved here, with the icon: this runs once per launch and
            // before the splash's first frame, so the assembled screen is
            // already real with no placeholder row to flash and correct.
            self.saved = Some(splash_saved_servers().into());
        }
        plan
    }

    /// Everything this launch's splash needs, resolved once — or `None` for a
    /// launch with no splash to play at all. Split out as the `&mut`-taking
    /// half of assembling the overlay, since the overlay host's page builder
    /// (a `Fn` re-invoked on every rebuild) can hold no `&mut AppState`; the
    /// cue it hands over is plain `Copy`/`Clone` data [`splash_overlay`]
    /// re-derives as often as the host asks for it.
    fn splash_cue(&mut self) -> Option<SplashCue> {
        let plan = self.splash_plan();
        if plan == SplashPlan::Skip {
            return None;
        }
        Some(SplashCue {
            plan,
            art: self.icon.clone()?,
            saved: Rc::clone(self.saved.as_ref()?),
            done: self.splash_done,
        })
    }
}

/// What one launch's cold-start splash is made of, once
/// [`AppState::splash_cue`] has decided — the input [`splash_overlay`]
/// rebuilds the overlay from on every pass.
#[derive(Clone)]
struct SplashCue {
    /// Which sequence to play (never [`SplashPlan::Skip`]: a skipped launch
    /// has no cue at all).
    plan: SplashPlan,
    /// The decoded app icon, decoded once with the decision rather than per
    /// rebuild.
    art: IconArt,
    /// The saved-server rows the assembled screen renders — read once with
    /// the decision (see [`AppState::saved`]).
    saved: Rc<[SavedServer]>,
    /// The sequence's own completion cue — read **tracked** by
    /// [`splash_overlay`], so the single `set(true)` that ends the splash is
    /// what unmounts it.
    done: RwSignal<bool>,
}

/// The splash overlay to stack over the navigator this frame, or `None` once
/// there is nothing left to show. The full sequence assembles the *same*
/// pieces, from the *same* saved-server list, through the *same* layout
/// function the `/` route renders, which is what makes the unmount invisible.
fn splash_overlay(cue: &SplashCue) -> Option<AnyView<AppState>> {
    // Tracked read: the cue's single `set(true)` wakes this rebuild, inside
    // the overlay host's page builder — the same `TrackedScope` the app
    // logic above it runs in.
    if cue.done.get() {
        return None;
    }
    match cue.plan {
        SplashPlan::Full => Some(splash_sequence(
            cue.art.clone(),
            SPLASH_SEED,
            connect_pieces(&active_theme(), &cue.saved),
            connect_layout,
            cue.done,
        )),
        SplashPlan::Reduced => Some(reduced_splash(cue.art.clone(), cue.done)),
        SplashPlan::Skip => None,
    }
}

/// Muxr's root [`Component`] (`Default`, as [`frust::app!`] requires):
/// stateless configuration wiring the router + shell together.
#[derive(Default)]
pub struct MuxrApp;

impl Component for MuxrApp {
    type State = AppState;

    fn init(&self) -> AppState {
        // Published once under the root owner, reachable from any screen via
        // `use_context`; handed *by value* to the session guard below, which
        // must stay safe to consult from a press handler's owner-less call
        // stack and so never reads context itself.
        let connection = connection::provide_connection_controller();

        // The failure→toast bridge: made once here (this handle outlives
        // every screen) and deliberately leaked for the process's life.
        connection
            .core()
            .failures()
            .subscribe(|failure: &failure::AppFailure| ui::toasts::app_toasts().raise(failure))
            .forget();

        features::connect::di::provide();
        features::sessions::di::provide();
        features::terminal::di::provide();
        features::settings::di::provide();

        // The controller is created before the route table so every route's
        // page can push through the same controller the router drives.
        let controller = NavigatorController::new();
        // The root overlay host's own navigator, created beside the route one
        // and handed to the route table with it (see `AppState::overlays`).
        let overlays = NavigatorController::new();
        let router = routes::install_app_router(&controller, &overlays, Arc::clone(&connection));
        router.handle_location(START_LOCATION);

        // The cold-start splash is deliberately NOT decided here — see
        // `AppState::splash_plan` for the ordering that forces it into the
        // first `build`.
        AppState {
            nav: router,
            overlays,
            // Published by `install_app_router` a line above; resolved here so
            // `build` never reaches for a thread-local.
            nav_handle: routes::app_nav(),
            connection,
            splash: None,
            icon: None,
            saved: None,
            splash_done: RwSignal::new(false),
        }
    }

    fn build(&self, state: &mut AppState) -> AnyView<AppState> {
        let controller = state.nav.controller().clone();
        // The overlay host's page builder is a `Fn` the navigator re-invokes
        // on every rebuild, so everything the root view needs is cloned out
        // of `state` here and re-assembled inside it.
        let overlays = state.overlays.clone();
        let nav_handle = state.nav_handle.clone();
        let connection = Arc::clone(&state.connection);
        let cue = state.splash_cue();

        // The root view (route navigator, floating bar, toast layer, splash)
        // becomes the host's root page, so every overlay pushed onto
        // `state.overlays` renders above all of it; the host also outranks
        // the route navigator for the Android back press (R44-back).
        any(overlay_host(&state.overlays, move || {
            // A separate clone for the route navigator's `initial` closure
            // (invoked only on the very first build); Connect pushes its QR
            // sheet onto the host, so that is the handle it is handed.
            let initial_overlays = overlays.clone();
            shell::root_view(
                &controller,
                move || routes::home_page(initial_overlays.clone()),
                cue.as_ref().and_then(splash_overlay),
                &nav_handle,
                &connection,
            )
        }))
    }
}

// Muxr's sole entry point: binds `MuxrApp` to all three platforms (Android
// JNI, iOS C-ABI, desktop's `main.rs`). Never edit this line or `main.rs` —
// add screens/state to `routes.rs`/`features` above instead. `setup` runs
// once, on the UI thread, before `Component::init`; `glyph_theme::install()`
// makes Glyph this app's one design system — a theme read that ever runs
// before it (or after it's skipped) must fall back to
// `Theme::glyph_baseline()`, never `Theme::neutral()`/`Theme::m3_baseline()`.
// `fonts::register_bundled_fonts` pushes the bundled JetBrains Mono + Symbols
// Nerd Font Mono faces into the same pre-first-frame registry
// `glyph_theme::install` uses; `fonts::apply_app_wide_theme` then forces the
// active theme's type-scale families onto JetBrains Mono (registration must
// run first — see `fonts`' own module docs). `register_cached_fonts_at_startup`
// re-registers this device's downloaded families off the blocking pool
// (never blocking the first frame on a disk scan); the font *catalog* itself
// is fetched only when the picker asks for it.
frust::app!(
    MuxrApp,
    setup = {
        frust::glyph_theme::install();
        fonts::register_bundled_fonts();
        fonts::apply_app_wide_theme();
        fonts::register_cached_fonts_at_startup();
    }
);

// Keeps frust-camera's Swift-called C export alive through release LTO (see frust_camera::ios_exports).
#[cfg(target_vendor = "apple")]
frust_camera::ios_exports!();
