//! Muxr — clean-architecture app skeleton (huddle-shaped).
//!
//! Four routes over a stack-style [`Router`] (no tabs — contrast
//! `examples/huddle`'s tab shell): `/` (Connect), `/sessions`, `/terminal`,
//! `/settings` (see `routes.rs`). Each feature under [`features`] follows
//! the `{domain, data, presentation}` clean-architecture slice
//! `examples/huddle` establishes (see its `src/README-phase-c.md`):
//! dependencies point inward only, `presentation` → `domain` ← `data`. All
//! four features run their real `data`/`domain` logic over `muxrd`'s gRPC
//! layer (`crate::net`, `crate::connection`), each also keeping a mock
//! repository behind its own `di.rs` seam for headless suites — see
//! `docs/ARCHITECTURE.md`'s Feature Slices section for the full picture.
//!
//! This file is the **composition root**: `Component::init` publishes the
//! app-scoped [`connection::ConnectionController`] via
//! [`connection::provide_connection_controller`] (and bridges its failures to
//! the app's toasts, since it outlives every screen), calls each feature's own
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
//! ([`AppState::splash_plan`], [`ui::splash::sequence`]) — and wraps the lot in
//! the **root overlay host** ([`AppState::overlays`]). Each feature's own
//! `di.rs` — called from `init`, above — is the seam that constructs and
//! `provide_context`s its repository, real by default with a mock behind a
//! test-only seam, the same shape `examples/huddle`'s `HuddleApp::init` uses
//! for its own repository.
//!
//! **Dev-machine-only scaffold**: this variant's `Cargo.toml` path-deps into
//! the Frust checkout that generated it, plus a sibling `clean-signals-rs`
//! checkout — see that file's dependency comments. Never edit
//! [`frust::app!`]'s invocation below or `main.rs`; add features above it
//! instead.

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
use features::connect::presentation::pieces::{active_theme, connect_pieces, splash_saved_servers};
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
    /// The **root overlay host**'s navigator: every menu, sheet and dialog in
    /// the app is a page here, and this host wraps the entire root view — the
    /// route navigator above, the floating top bar and the toast layer alike
    /// (see [`Component::build`], and `frust::overlay_host`'s own docs).
    ///
    /// Overlays used to be pages of the *route* navigator, which the bar
    /// floats **over** — so a menu clipped under the bar and the bar stayed
    /// tappable while a modal was up (a bar press could kill an open sheet;
    /// FINDINGS #44, device-proven on Xiaomi + OnePlus). A host above every
    /// piece of chrome fixes both for free: the navigator routes input — and
    /// forwards accessibility nodes — for its top page only, so the whole app
    /// under an open overlay is inert to pointers *and* to assistive
    /// technology, and `frust::overlay_host` additionally ranks this
    /// controller ahead of the route navigator for the Android back press
    /// (R44-back), so back dismisses the overlay rather than the page beneath
    /// it. A host with no overlays open claims nothing and defers bit for bit.
    ///
    /// `pub` for the same reason [`Self::nav`] is: the headless suites drive
    /// it (and assert that a route push never perturbs it).
    pub overlays: NavigatorController<AppState>,
    /// The app's own [`routes::Nav`] handle — resolved once here, with every
    /// other app-scoped dependency, and read by [`shell::root_view`] for the
    /// persistent top bar (which route is on top, and what its `‹`/`⚙` do).
    /// The same handle every screen resolves through [`routes::app_nav`], so
    /// the bar and the screens share one route-meta mirror.
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
    /// per rebuild (the contract [`splash_icon_art`] documents). Stays `None`
    /// when this launch has no splash to paint it with.
    icon: Option<IconArt>,
    /// The saved-server rows the splash's copy of the Connect screen renders —
    /// read from the repository once, beside the icon, for the same reason
    /// ([`splash_saved_servers`]: the shell rebuilds every frame, and the real
    /// store is a preferences read plus a JSON parse). An [`Rc`] slice so
    /// re-deriving the cue per rebuild is a refcount bump rather than a copy of
    /// every row. Stays `None` when this launch has no splash.
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
            // Resolved here, with the icon, for the same reason: this runs once
            // per launch, and it runs *before* the splash's first frame — so
            // the assembled screen is already the real one, with no placeholder
            // row to flash and then correct. The repository it reads is
            // published by `init`, which runs before any `build`
            // (`features::connect::di::provide`, called there).
            self.saved = Some(splash_saved_servers().into());
        }
        plan
    }

    /// Everything this launch's splash needs, resolved once — or `None` for a
    /// launch with no splash to play at all.
    ///
    /// The **mutable** half of the old `splash_overlay`, split out because the
    /// overlay itself is now assembled inside the overlay host's page builder
    /// (a `Fn` re-invoked on every rebuild), which can hold no `&mut AppState`.
    /// The cue it hands over is plain `Copy`/`Clone` data, so
    /// [`splash_overlay`] can re-derive the same view as often as the host asks
    /// for it.
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
/// [`AppState::splash_cue`] has decided — the input [`splash_overlay`] rebuilds
/// the overlay from on every pass.
#[derive(Clone)]
struct SplashCue {
    /// Which sequence to play (never [`SplashPlan::Skip`]: a skipped launch has
    /// no cue at all).
    plan: SplashPlan,
    /// The decoded app icon, decoded once with the decision rather than per
    /// rebuild (the contract [`splash_icon_art`] documents).
    art: IconArt,
    /// The saved-server rows the assembled screen renders — read once with the
    /// decision, for the same reason (see [`AppState::saved`]).
    saved: Rc<[SavedServer]>,
    /// The sequence's own completion cue — read **tracked** by
    /// [`splash_overlay`], so the single `set(true)` that ends the splash is
    /// what unmounts it.
    done: RwSignal<bool>,
}

/// The splash overlay to stack over the navigator this frame, or `None` once
/// there is nothing left to show.
///
/// The full sequence assembles the *same* pieces, from the *same* saved-server
/// list, through the *same* layout function the `/` route renders — clamp and
/// all — which is what makes the unmount invisible; see
/// [`crate::ui::splash::sequence`] and
/// [`features::connect::presentation::pieces`]' parity contract.
fn splash_overlay(cue: &SplashCue) -> Option<AnyView<AppState>> {
    // Tracked read: the cue's single `set(true)` wakes this rebuild. It happens
    // inside the overlay host's page builder now, which `RenderRoot::rebuild`
    // runs within the shell's own `TrackedScope` exactly like the app logic
    // above it — so the subscription is the same one it always was.
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
        // App-scoped connection state, published once under the root owner
        // (see `connection`'s module docs) — reachable from any screen via
        // `use_context`, and handed *by value* to the session guard below,
        // which must stay safe to consult from a press handler's owner-less
        // call stack and so never reads context itself.
        let connection = connection::provide_connection_controller();

        // The connection's failure→toast bridge. A screen's own controller
        // binds one per mount (`use_failure_listener`); this handle outlives
        // every screen, so its subscription is made once here and deliberately
        // leaked for the process's life. Every emission happens on the UI
        // thread (the connection controller's coordinator is the only thing
        // that settles a connect), which is what makes the thread-local
        // `app_toasts` handle the right one — see `ui::toasts`' module docs.
        connection
            .core()
            .failures()
            .subscribe(|failure: &failure::AppFailure| ui::toasts::app_toasts().raise(failure))
            .forget();

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
        // The root overlay host's own navigator, created beside the route one
        // and handed to the route table with it: every screen pushes its
        // menus/sheets/dialogs here rather than onto the route stack (see
        // `AppState::overlays`).
        let overlays = NavigatorController::new();
        let router = routes::install_app_router(&controller, &overlays, Arc::clone(&connection));
        // Resolves the start location once (see `START_LOCATION`).
        router.handle_location(START_LOCATION);

        // The cold-start splash is deliberately NOT decided here — see
        // `AppState::splash_plan` for the ordering that forces it into the
        // first `build`.
        AppState {
            nav: router,
            overlays,
            // Published by `install_app_router` a line above; resolved here so
            // `build` never reaches for a thread-local (and so the shell's bar
            // shares the screens' one route-meta mirror).
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
        // The overlay host's page builder is a `Fn` the navigator re-invokes on
        // every rebuild, so everything the former root needs is cloned out of
        // `state` here and re-assembled inside it — including the splash, whose
        // one mutable decision is spent on the way in (`AppState::splash_cue`).
        let overlays = state.overlays.clone();
        let nav_handle = state.nav_handle.clone();
        let connection = Arc::clone(&state.connection);
        let cue = state.splash_cue();

        // The whole former root — route navigator, floating bar, toast layer,
        // splash — becomes the host's root page, which is what puts every
        // overlay pushed onto `state.overlays` *above* all of it (FINDINGS
        // #44). The host also outranks the route navigator for the back press
        // (R44-back), and its own wiring runs before the inner navigator's
        // because the page builder below runs inside it.
        any(overlay_host(&state.overlays, move || {
            // A separate clone for the route navigator's `initial` closure
            // (only ever invoked on the very first build); Connect pushes its
            // QR sheet onto the host, so that is the handle it is handed.
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
// design system, unlike huddle's three-way switcher. `fonts::register_bundled_fonts`
// pushes the bundled JetBrains Mono + Symbols Nerd Font Mono faces into the
// same pre-first-frame registry `glyph_theme::install` uses for its own
// bundled faces; `fonts::apply_app_wide_theme` then forces the active theme
// to the Glyph baseline with every type-scale slot's family swapped onto
// JetBrains Mono (see `fonts`' own module docs for the full ordering
// contract and why registration must run first).
frust::app!(
    MuxrApp,
    setup = {
        frust::glyph_theme::install();
        fonts::register_bundled_fonts();
        fonts::apply_app_wide_theme();
    }
);

// Keeps frust-camera's Swift-called C export alive through release LTO (see frust_camera::ios_exports).
#[cfg(target_vendor = "apple")]
frust_camera::ios_exports!();
