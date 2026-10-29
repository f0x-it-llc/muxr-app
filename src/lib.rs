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
    AnyView, Component, Get, NavigatorController, Router, RwSignal, View, any, overlay_host, stack,
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

/// The full sequence over the screen the route will paint: Connect's own
/// [`splash_assembly`] picks the phone column or the Tablet arrangement for
/// the window's size class, so the overlay's last frame is the route's first.
fn full_splash(cue: &SplashCue) -> impl View<AppState> {
    let (pieces, assemble) = splash_assembly(&active_theme(), &cue.saved);
    splash_sequence(cue.art.clone(), SPLASH_SEED, pieces, assemble, cue.done)
}

/// The production wake source: the app lifecycle's return-from-background and
/// the platform's default-network changes, both mapped in `net::wake`.
///
/// The listener and the connectivity handle unregister on drop, so this value
/// owns them for the process lifetime (the installed source is never dropped).
/// Both callbacks only do one non-blocking publish: the lifecycle one runs
/// synchronously on the UI thread, the connectivity one on the plugin's
/// `frust-connectivity-events` thread.
#[derive(Default)]
struct FrustWakeSource {
    registrations: std::sync::Mutex<Option<WakeRegistrations>>,
}

struct WakeRegistrations {
    // Held only so they stay registered.
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

impl Component for MuxrApp {
    type State = AppState;

    fn init(&self) -> AppState {
        let connection = connection::provide_connection_controller();

        // Once for the process: a second `init` finds the source installed and
        // leaves it running.
        net::wake::install_wake_source(std::sync::Arc::new(FrustWakeSource::default()));

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
        // panic on its first poll. The store's failures that no caller owns
        // bridge to toasts off this same instance — a restore's, which its
        // callers never re-announce. A purchase reports through its outcome
        // (the sheet announces it once), and the background refreshes and the
        // offer lookup fail quietly, so the bridge never doubles a message.
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

    /// The overlay host, and the toast layer stacked above it. Sheets, side
    /// sheets and dialogs are pages the host paints over its root page, so a
    /// toast mounted inside that page would sit behind every one of them.
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
