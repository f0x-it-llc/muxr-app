//! The overlay host's root page, and the shell page the route table hangs the
//! three bar-bearing routes off.

use frust::{AnyView, Location, NavigatorController, Stack, View, any, icons, navigator};
use frust_glyph::{TitleDirection, TitlePosition, app_bar};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::features::terminal::presentation::pages::open_terminal_menu;
use crate::routes::{
    BarAction, BarDirection, BarLeading, CONNECT_LOCATION, Nav, SETTINGS_ROUTE, app_nav,
    route_transition,
};
use crate::ui::topbar::{BACK_LABEL, bar_icon, brand_leading};

const SETTINGS_LABEL: &str = "Settings";
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
    connection: &ConnectionController,
) -> impl View<AppState> + use<> {
    let nav = app_nav();
    let bar = shell_bar(&nav, connection);
    let landing = nav.clone();
    let mut pages = navigator(inner, move || landing.landing_page()).transition(route_transition());
    if let Some(route) = nav.landing_route() {
        pages = pages.root_route(route);
    }
    frust::scaffold(nav.route_observer().observe(pages)).app_bar_opt(bar)
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
