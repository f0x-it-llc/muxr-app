//! The overlay host's root page, and the shell page the bar-bearing routes hang off.

use std::rc::Rc;
use std::sync::Arc;

use frust::{
    AnyView, Get, GetUntracked, Location, NavigatorController, Stack, View, any, icons, navigator,
};
use frust_glyph::{
    BannerVariant, TitleDirection, TitlePosition, app_bar, banner_spec, card_switcher, peer_card,
};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::features::connect::domain::entities::epoch_millis;
use crate::features::pro::di::expect_pro_controller;
use crate::features::servers::di::{
    expect_servers_controller, expect_switcher_back, keep_peers, use_last_seen,
};
use crate::features::servers::domain::entities::{FaceKind, ServerFace};
use crate::features::servers::presentation::controller::{
    ReconnectBanner, ServersSwitcherController, SwitchGate,
};
use crate::features::servers::presentation::widgets::{bar::servers_bar, faces, label_row};
use crate::features::sessions::presentation::widgets::{
    sessions_snapshot, shell_responsive, switcher_fab,
};
use crate::features::terminal::presentation::pages::open_terminal_menu;
use crate::responsive::Responsive;
use crate::routes::{
    BarAction, BarDirection, BarLeading, BarTitle, CONNECT_LOCATION, Nav, SETTINGS_ROUTE, app_nav,
    route_transition,
};
use crate::ui::palette::{Palette, active_theme};
use crate::ui::topbar::{BACK_LABEL, bar_icon, brand_leading};

const SETTINGS_LABEL: &str = "Settings";
const PHONE_CARD_SCALE: f64 = 0.62;
/// The design's value.
const TABLET_CARD_SCALE: f64 = 0.58;
/// The scaffold's default (16 px) is the Compact parity.
const TABLET_FAB_MARGIN: f64 = 24.0;
const TERMINAL_MENU_LABEL: &str = "Terminal menu";

/// Toasts mount above this host; `Stack` reconciles positionally, so the navigator keeps index 0.
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

/// Placed once and retained, so everything route-dependent derives here per rebuild. A back
/// press on the inner root falls through to the outer navigator, leaving the shell.
pub(crate) fn shell_page(
    inner: &NavigatorController<AppState>,
    overlays: &NavigatorController<AppState>,
    connection: &ConnectionController,
) -> AnyView<AppState> // erasure: one return type for the host-wrapped shell
{
    let rule = shell_responsive();
    let nav = app_nav();
    let switcher = expect_servers_controller();
    // A stage left open on another inner route would swallow its back press.
    if !nav.on_sessions() && switcher.open.get_untracked() {
        switcher.close_stage();
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
        if rule.is_tablet() {
            scaffold = scaffold.fab_margin(TABLET_FAB_MARGIN);
        }
    }
    let card_scale = if rule.is_tablet() {
        TABLET_CARD_SCALE
    } else {
        PHONE_CARD_SCALE
    };
    any(switcher_host(
        &switcher, overlays, &nav, rule, card_scale, scaffold,
    ))
}

/// Sits under the overlay host and toast layer, so dialogs and toasts outrank it. Peers are
/// built only while the stage is open or ramping closed.
fn switcher_host(
    switcher: &Arc<ServersSwitcherController>,
    overlays: &NavigatorController<AppState>,
    nav: &Nav,
    rule: Responsive,
    card_scale: f64,
    page: impl View<AppState>,
) -> impl View<AppState> {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    let now_ms = epoch_millis();
    let faces_now = switcher.faces.get();
    let live_index = faces_now
        .iter()
        .position(|face| face.kind == FaceKind::Live)
        .unwrap_or(0);
    let open = switcher.open.get();
    let peers: Vec<_> = if keep_peers(open) {
        // A plain map edit, not a signal write, so it never re-triggers this build.
        if let Some(last_seen) = use_last_seen() {
            last_seen.retain(|endpoint| faces_now.iter().any(|face| face.endpoint == endpoint));
        }
        faces_now
            .iter()
            .filter(|face| face.kind != FaceKind::Live)
            .map(|face| {
                peer_card(peer_face(&theme, &colors, face, now_ms, rule))
                    .label(label_row(&theme, &colors, face))
            })
            .collect()
    } else {
        Vec::new()
    };

    let gate = SwitchGate::standard(overlays, &expect_pro_controller(), nav);
    let add_gate = gate.clone();
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
            Rc::new(move || switcher.add(&add_gate, &nav)) as Rc<dyn Fn()>
        };
        let close = {
            let switcher = Arc::clone(switcher);
            Rc::new(move || switcher.close()) as Rc<dyn Fn()>
        };
        servers_bar::<AppState>(lock, add, close)
    };

    let mut stage = card_switcher(open, page)
        .peers(peers)
        .live_index(live_index)
        .on_select(on_select)
        .on_close(on_close)
        .action_bar(bar)
        .expanding(switcher.expanding.get())
        .card_scale(card_scale)
        // Not the back path: the shell's `DismissAnimated` page outranks a `BackFallback`, so
        // Sessions' dismiss consumer closes the stage.
        .back(&expect_switcher_back());
    if let Some(live) = faces_now
        .get(live_index)
        .filter(|f| f.kind == FaceKind::Live)
    {
        stage = stage.live_label(label_row(&theme, &colors, live));
    }
    stage
}

/// Composed here so `servers` never imports `sessions`' presentation.
fn peer_face(
    theme: &frust::Theme,
    colors: &Palette,
    face: &ServerFace,
    now_ms: i64,
    rule: Responsive,
) -> AnyView<AppState> // erasure: the snapshot and the servers faces are different view types
{
    match &face.kind {
        FaceKind::Cached {
            page: Some(page), ..
        } => any(sessions_snapshot::<AppState>(page, rule)),
        _ => faces::server_face(theme, colors, face, now_ms),
    }
}

/// A fullscreen terminal gets no bar: `app_bar_opt(None)` hands the body the bar's band.
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
    // A cached switch keeps its page on screen while offline: the title names that server.
    let switcher = expect_servers_controller();
    let mut title = meta.resolve_title(connection);
    if meta.title == BarTitle::ConnectedHost
        && let Some(host) = switcher.offline_host()
    {
        title = host;
    }
    let banner = switcher.banner.get().map(|state| {
        let variant = match state {
            ReconnectBanner::Connected => BannerVariant::Success,
            ReconnectBanner::Connecting { .. } | ReconnectBanner::Failed { .. } => {
                BannerVariant::Warning
            }
        };
        banner_spec(state.text(), variant)
    });
    let mut bar = app_bar::<AppState>(title)
        .banner(banner)
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
        Some(BarAction::TerminalMenu) => vec![bar_icon(
            icons::MORE_VERT,
            TERMINAL_MENU_LABEL,
            open_terminal_menu,
        )],
        None => Vec::new(),
    };
    Some(bar.actions(actions))
}
