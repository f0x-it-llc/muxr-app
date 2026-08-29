pub mod connection;
pub mod failure;
pub mod features;
pub mod fonts;
pub mod net;
pub mod responsive;
pub mod routes;
pub mod ui;

mod shell;

use std::rc::Rc;

use frust::{AnyView, Component, Get, NavigatorController, Router, RwSignal, any, overlay_host};

use features::connect::domain::entities::SavedServer;
use features::connect::presentation::pages::connect_layout;
use features::connect::presentation::widgets::connect_pieces;
use features::connect::presentation::widgets::saved::splash_saved_servers;
use ui::palette::active_theme;
use ui::splash::icon_phase::{IconArt, splash_icon_art};
use ui::splash::sequence::{SplashPlan, claim_cold_start, reduced_splash, splash_sequence};

const START_LOCATION: &str = "/";

const SPLASH_SEED: u64 = 0x6D75_7872; // "muxr"

pub struct AppState {
    pub nav: Rc<Router<AppState>>,
    pub overlays: NavigatorController<AppState>,
    splash: Option<SplashPlan>,
    icon: Option<IconArt>,
    saved: Option<Rc<[SavedServer]>>,
    splash_done: RwSignal<bool>,
}

impl AppState {
    fn splash_plan(&mut self) -> SplashPlan {
        if let Some(plan) = self.splash {
            return plan;
        }
        let plan = claim_cold_start(START_LOCATION, active_theme().motion.reduce_motion);
        self.splash = Some(plan);
        if plan != SplashPlan::Skip {
            self.icon = Some(splash_icon_art());
            self.saved = Some(splash_saved_servers().into());
        }
        plan
    }

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

#[derive(Clone)]
struct SplashCue {
    plan: SplashPlan,
    art: IconArt,
    saved: Rc<[SavedServer]>,
    done: RwSignal<bool>,
}

fn splash_overlay(cue: &SplashCue) -> Option<AnyView<AppState>> {
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

#[derive(Default)]
pub struct MuxrApp;

impl Component for MuxrApp {
    type State = AppState;

    fn init(&self) -> AppState {
        let connection = connection::provide_connection_controller();

        connection
            .core()
            .failures()
            .subscribe(|failure: &failure::AppFailure| ui::toasts::app_toasts().raise(failure))
            .forget();

        features::connect::di::provide();
        features::pro::di::provide();
        features::sessions::di::provide();
        features::terminal::di::provide();
        features::settings::di::provide();

        // Resolved here, not inside the future: the poller driving it installs
        // no ambient reactive owner, so a lookup deferred into the body would
        // panic on its first poll. The store's own failures bridge to toasts
        // off this same instance — refresh, purchase and restore alike, each
        // announced once, by nobody else.
        let pro = features::pro::di::expect_pro_controller();

        pro.core()
            .failures()
            .subscribe(|failure: &failure::AppFailure| ui::toasts::app_toasts().raise(failure))
            .forget();

        frust::spawn_local(async move { pro.refresh().await });

        let controller = NavigatorController::new();
        let overlays = NavigatorController::new();
        let router = routes::install_app_router(&controller, &overlays, connection);
        // Through the handle rather than `Router::handle_location`: a start
        // location inside the shell has to place the scaffold the way every
        // other navigation does (see `routes::Nav::go`).
        routes::app_nav().go(START_LOCATION);

        AppState {
            nav: router,
            overlays,
            splash: None,
            icon: None,
            saved: None,
            splash_done: RwSignal::new(false),
        }
    }

    fn build(&self, state: &mut AppState) -> AnyView<AppState> {
        let controller = state.nav.controller().clone();
        let overlays = state.overlays.clone();
        let cue = state.splash_cue();

        any(overlay_host(&state.overlays, move || {
            let initial_overlays = overlays.clone();
            shell::root_view(
                &controller,
                move || routes::home_page(initial_overlays.clone()),
                cue.as_ref().and_then(splash_overlay),
            )
        }))
    }
}

// Muxr's sole entry point: binds `MuxrApp` to all three platforms (Android
// JNI, iOS C-ABI, desktop's `main.rs`). Never edit this line or `main.rs` —
// add screens/state to `routes.rs`/`features` above instead.
frust::app!(
    MuxrApp,
    setup = {
        frust_glyph::install();
        fonts::register_bundled_fonts();
        fonts::apply_app_wide_theme();
        fonts::register_cached_fonts_at_startup();
    }
);

// Keeps frust-camera's Swift-called C export alive through release LTO (see frust_camera::ios_exports).
#[cfg(target_vendor = "apple")]
frust_camera::ios_exports!();
