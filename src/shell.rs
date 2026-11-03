//! The overlay host's root page, and the shell page the route table hangs the
//! three bar-bearing routes off.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use frust::{
    AnyView, Get, GetUntracked, Location, NavigatorController, Stack, View, any, icons, navigator,
};
use frust_glyph::{TitleDirection, TitlePosition, app_bar, card_switcher, peer_card};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::features::servers::di::{expect_servers_controller, expect_switcher_back};
use crate::features::servers::domain::entities::FaceKind;
use crate::features::servers::presentation::controller::{ServersSwitcherController, SwitchGate};
use crate::features::servers::presentation::widgets::{bar::servers_bar, faces, label_row};
use crate::features::sessions::presentation::widgets::switcher_fab;
use crate::features::terminal::presentation::pages::open_terminal_menu;
use crate::routes::{
    BarAction, BarDirection, BarLeading, CONNECT_LOCATION, Nav, SETTINGS_ROUTE, app_nav,
    route_transition,
};
use crate::ui::palette::{Palette, active_theme};
use crate::ui::topbar::{BACK_LABEL, bar_icon, brand_leading};

const SETTINGS_LABEL: &str = "Settings";
/// The card's scale at full open on a phone (the tablet card sets 0.58).
const PHONE_CARD_SCALE: f64 = 0.62;
const TERMINAL_MENU_LABEL: &str = "Terminal menu";

/// The overlay host's root page: the outer navigator, and `overlay` (the
/// cold-start splash) stacked over it. The toast layer is not here: the app
/// root mounts it above the host, so a toast also paints over the splash.
///
/// The bar belongs to [`shell_page`]'s scaffold, one page down, which keeps it
/// off every in-shell navigation. `Stack` reconciles positionally, so the
/// navigator keeps index 0 and only the overlay's pod is created and destroyed.
pub fn root_view(
    controller: &NavigatorController<AppState>,
    initial: impl Fn() -> AnyView<AppState> + 'static,
    overlay: Option<AnyView<AppState>>,
) -> AnyView<AppState> // erasure: keep accumulator into Vec<AnyView>
{
    let mut children = vec![any(
        navigator(controller, initial).root_route(Location::parse(CONNECT_LOCATION))
    )];
    children.extend(overlay);
    any(Stack(children))
}

/// The shell route's own page: the app bar over the inner navigator the three
/// bar-bearing routes land on.
///
/// Placed once and then **retained** (an in-shell navigation issues no outer
/// op at all), so everything route-dependent is derived here per rebuild from
/// the inner navigator's own published state. That navigator's **root** is the
/// landing route of the navigation that placed this page: a back press there
/// finds nothing to pop and falls through to the outer navigator, leaving the
/// shell rather than popping to a placeholder under the landing screen.
pub(crate) fn shell_page(
    inner: &NavigatorController<AppState>,
    overlays: &NavigatorController<AppState>,
    connection: &ConnectionController,
) -> AnyView<AppState> // erasure: one return type for the host-wrapped shell
{
    let nav = app_nav();
    let switcher = expect_servers_controller();
    // The stage never outlives `/sessions`: the FAB is the only way up, and
    // a stage left open on another inner route would swallow its back press.
    if !nav.on_sessions() && switcher.open.get_untracked() {
        switcher.close();
    }
    let bar = shell_bar(&nav, connection);
    let landing = nav.clone();
    let mut pages = navigator(inner, move || landing.landing_page()).transition(route_transition());
    if let Some(route) = nav.landing_route() {
        pages = pages.root_route(route);
    }
    let mut scaffold = frust::scaffold(nav.route_observer().observe(pages)).app_bar_opt(bar);
    if nav.on_sessions() {
        scaffold = scaffold.fab(switcher_fab(&switcher));
    }
    any(switcher_host(&switcher, overlays, &nav, scaffold))
}

/// The whole shell page (bar included) as the live card of the server
/// switcher stage. The host sits under the overlay host and the toast layer,
/// so dialogs (the paywall, the unverified confirm) and toasts outrank it.
///
/// Strip order is the controller's: the live card is the scaled shell page at
/// `live_index`, the peers are every other face. Peers are rebuilt from the
/// current faces on every build (after a failed switch no face is live).
fn switcher_host(
    switcher: &Arc<ServersSwitcherController>,
    overlays: &NavigatorController<AppState>,
    nav: &Nav,
    page: impl View<AppState>,
) -> impl View<AppState> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    let now_ms = now_ms();
    let faces_now = switcher.faces.get();
    let live_index = faces_now
        .iter()
        .position(|face| face.kind == FaceKind::Live)
        .unwrap_or(0);
    let peers: Vec<_> = faces_now
        .iter()
        .filter(|face| face.kind != FaceKind::Live)
        .map(|face| {
            peer_card(faces::server_face(&theme, &colors, face, now_ms))
                .label(label_row(&theme, &colors, face))
        })
        .collect();

    let gate = SwitchGate::standard(overlays, &expect_pro(), nav);
    let on_select = {
        let switcher = Arc::clone(switcher);
        move |_: &mut AppState, peer_index: usize| switcher.select(peer_index, &gate)
    };
    let on_close = {
        let switcher = Arc::clone(switcher);
        move |_: &mut AppState| switcher.close()
    };

    let bar = {
        let lock = {
            let (switcher, nav) = (Arc::clone(switcher), nav.clone());
            Rc::new(move || switcher.lock(&nav)) as Rc<dyn Fn()>
        };
        let add = {
            let (switcher, nav) = (Arc::clone(switcher), nav.clone());
            // The controller's `add` only pushes; the stage must not outlive
            // the push (its open flag, poll pause and strip would persist
            // under the add page and reappear on pop).
            Rc::new(move || {
                switcher.close();
                switcher.add(&nav);
            }) as Rc<dyn Fn()>
        };
        let close = {
            let switcher = Arc::clone(switcher);
            Rc::new(move || switcher.close()) as Rc<dyn Fn()>
        };
        servers_bar::<AppState>(lock, add, close)
    };

    let mut stage = card_switcher(switcher.open.get(), page)
        .peers(peers)
        .live_index(live_index)
        .on_select(on_select)
        .on_close(on_close)
        .action_bar(bar)
        .expanding(switcher.expanding.get())
        .card_scale(PHONE_CARD_SCALE)
        // Wired per the component contract. It is NOT the back path in this
        // shell: the shell's `DismissAnimated` page outranks a `BackFallback`,
        // so the press reaches Sessions' dismiss consumer, which closes the
        // stage (see `SessionsScreen::build`).
        .back(&expect_switcher_back());
    if let Some(live) = faces_now
        .get(live_index)
        .filter(|f| f.kind == FaceKind::Live)
    {
        stage = stage.live_label(label_row(&theme, &colors, live));
    }
    stage
}

fn expect_pro() -> Arc<crate::features::pro::presentation::controller::ProController> {
    crate::features::pro::di::expect_pro_controller()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(0))
}

/// The Glyph app bar for whatever route the inner navigator has on top.
/// Titles [`TitlePosition::Center`] — this app's own design.
///
/// Two things resolve to the same absent slot: a route with no
/// [`crate::routes::RouteMeta`], and the terminal screen's **fullscreen**
/// request. Suppressing rather than hiding is the scaffold's own idiom —
/// `app_bar_opt(None)` hands the body the bar's band, which is exactly what a
/// fullscreen terminal is asking for.
fn shell_bar(nav: &Nav, connection: &ConnectionController) -> Option<impl View<AppState>> {
    let state = nav.bar();
    let meta = state.meta?;
    if state.fullscreen {
        return None;
    }

    let direction = match state.direction {
        BarDirection::Forward => TitleDirection::Forward,
        BarDirection::Back => TitleDirection::Back,
    };
    let mut bar = app_bar::<AppState>(meta.resolve_title(connection))
        .title_direction(direction)
        .title_position(TitlePosition::Center);
    bar = match meta.leading {
        BarLeading::Brand => bar.leading(brand_leading()),
        BarLeading::Back => {
            let nav = nav.clone();
            bar.leading(bar_icon(
                icons::CHEVRON_LEFT,
                BACK_LABEL,
                move |_state: &mut AppState| nav.pop(),
            ))
        }
    };
    let actions = match meta.action {
        Some(BarAction::Settings) => {
            let nav = nav.clone();
            vec![bar_icon(
                icons::SETTINGS,
                SETTINGS_LABEL,
                move |_state: &mut AppState| nav.push(SETTINGS_ROUTE),
            )]
        }
        // Forward only: the terminal feature owns its own menu — its entries,
        // its anchor rect and every action they run.
        Some(BarAction::TerminalMenu) => vec![bar_icon(
            icons::MORE_VERT,
            TERMINAL_MENU_LABEL,
            open_terminal_menu,
        )],
        None => Vec::new(),
    };
    Some(bar.actions(actions))
}
