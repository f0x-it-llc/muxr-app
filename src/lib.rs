pub mod agents;
pub mod connection;
pub mod failure;
pub mod features;
pub mod fonts;
pub mod links;
pub mod net;
pub mod responsive;
pub mod routes;
pub mod ui;

mod shell;

use std::rc::Rc;

use frust::{
    AnyView, Component, Get, HapticFeedback, NavigatorController, Router, RwSignal, View, any,
    overlay_host, set_haptic_sink, stack,
};

use features::connect::domain::entities::SavedServer;
use features::connect::presentation::pages::splash_assembly;
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
        SplashPlan::Full => Some(any(full_splash(cue))), // erasure: keep accumulator into Vec<AnyView>
        SplashPlan::Reduced => Some(any(reduced_splash(cue.art.clone(), cue.done))), // erasure: keep accumulator into Vec<AnyView>
        SplashPlan::Skip => None,
    }
}

/// Connect's [`splash_assembly`] matches the route's layout, so the overlay's last frame is
/// the route's first.
fn full_splash(cue: &SplashCue) -> impl View<AppState> {
    let (pieces, assemble) = splash_assembly(&active_theme(), &cue.saved);
    splash_sequence(cue.art.clone(), SPLASH_SEED, pieces, assemble, cue.done)
}

/// Owns both registrations for the process lifetime; they unregister on drop. Each callback
/// only publishes: lifecycle on the UI thread, connectivity on the plugin's own thread.
#[derive(Default)]
struct FrustWakeSource {
    registrations: std::sync::Mutex<Option<WakeRegistrations>>,
}

struct WakeRegistrations {
    _lifecycle: frust::AppLifecycleListener,
    _connectivity: frust_connectivity::ListenerHandle,
}

impl net::wake::WakeSource for FrustWakeSource {
    fn start(&self, publisher: net::wake::WakePublisher) {
        let on_restart = publisher.clone();
        let lifecycle = frust::AppLifecycleListener::new()
            .on_restart(move || on_restart.publish(net::wake::wake_for_restart()));
        let connectivity = frust_connectivity::Connectivity::set_listener(Box::new(move |event| {
            if let Some(reason) = net::wake::wake_for_connectivity(&event) {
                publisher.publish(reason);
            }
        }));
        *self
            .registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(WakeRegistrations {
            _lifecycle: lifecycle,
            _connectivity: connectivity,
        });
    }
}

#[derive(Default)]
pub struct MuxrApp;

/// Desktop reports `NotAvailable`; the result is cosmetic and ignored.
fn perform_plugin_haptic(feedback: HapticFeedback) {
    use frust_haptics::{HapticEffect, Haptics};
    let effect = match feedback {
        HapticFeedback::Selection => HapticEffect::SelectionClick,
        HapticFeedback::Light => HapticEffect::ImpactLight,
        HapticFeedback::Medium => HapticEffect::ImpactMedium,
        HapticFeedback::Heavy => HapticEffect::ImpactHeavy,
        HapticFeedback::Success => HapticEffect::Success,
        HapticFeedback::Warning => HapticEffect::Warning,
        HapticFeedback::Error => HapticEffect::Error,
        _ => return,
    };
    let _ = Haptics::perform(effect);
}

impl Component for MuxrApp {
    type State = AppState;

    fn init(&self) -> AppState {
        set_haptic_sink(Some(perform_plugin_haptic));

        let connection = connection::provide_connection_controller();

        // Once per process: a second `init` leaves the installed source running.
        net::wake::install_wake_source(std::sync::Arc::new(FrustWakeSource::default()));

        connection
            .core()
            .failures()
            .subscribe(|failure: &failure::AppFailure| ui::toasts::app_toasts().raise(failure))
            .forget();

        features::connect::di::provide();
        features::pro::di::provide();
        // Before any sessions controller, which resolves the last-seen handle once.
        features::servers::di::provide_last_seen();
        features::servers::di::provide_controller();
        features::sessions::di::provide();
        features::terminal::di::provide();
        features::settings::di::provide();

        // Resolved outside the future: its poller has no reactive owner, so a deferred lookup
        // panics. Only restore failures need this bridge; purchases report through their outcome.
        let pro = features::pro::di::expect_pro_controller();

        pro.core()
            .failures()
            .subscribe(|failure: &failure::AppFailure| ui::toasts::app_toasts().raise(failure))
            .forget();

        let follower = std::sync::Arc::clone(&pro);
        frust::spawn_local(async move { pro.refresh().await });
        frust::spawn_local(async move { follower.follow_store_changes().await });

        let controller = NavigatorController::new();
        let overlays = NavigatorController::new();
        let router = routes::install_app_router(&controller, &overlays, connection);
        // Through the handle so a start location inside the shell places the scaffold.
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

    /// Toasts stack above the overlay host because sheets and dialogs paint over its root page.
    /// Both children keep their index: `Stack` reconciles positionally.
    fn build(&self, state: &mut AppState) -> impl View<AppState> {
        let controller = state.nav.controller().clone();
        let overlays = state.overlays.clone();
        let cue = state.splash_cue();

        let host = overlay_host(&state.overlays, move || {
            let initial_overlays = overlays.clone();
            shell::root_view(
                &controller,
                move || any(routes::home_page(initial_overlays.clone())), // erasure: keep stored view table/registry
                cue.as_ref().and_then(splash_overlay),
            )
        });
        stack().child(host).child(ui::toasts::toast_layer())
    }
}

// The sole entry point for Android JNI, iOS C-ABI and desktop `main.rs`; never edit it.
frust::app!(
    MuxrApp,
    setup = {
        frust_glyph::install();
        fonts::register_bundled_fonts();
        fonts::apply_app_wide_theme();
        fonts::register_cached_fonts_at_startup();
    }
);

// Keeps frust-camera's Swift-called C export alive through release LTO.
#[cfg(target_vendor = "apple")]
frust_camera::ios_exports!();
