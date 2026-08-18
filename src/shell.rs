use frust::{AnyView, NavigatorController, Stack, any, icons, navigator};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::features::terminal::presentation::pages::open_terminal_menu;
use crate::routes::{BarAction, BarDirection, BarLeading, Nav, SETTINGS_ROUTE};
use crate::ui::toasts::toast_layer;
use crate::ui::topbar::muxr_topbar;

const SETTINGS_LABEL: &str = "Settings";
const TERMINAL_MENU_LABEL: &str = "Terminal menu";

/// The app's entire root content: a full-height navigator, the top bar
/// floating over it, the app-wide toast layer over that, and `overlay` stacked
/// over everything when there is one.
///
/// # Why the bar floats instead of sharing a band with the navigator
///
/// Floating the bar over a full-height navigator (rather than sharing a
/// layout band, where the bar's visibility becomes a layout fact that shoves
/// the page during its own transition) means a page's box never depends on
/// which route is on top; the bar instead animates its own entrance and exit
/// ([`crate::ui::topbar`]) in the page transition's own Glyph timings. The
/// cost is that a bar-bearing page owes itself the top clearance the band
/// used to hand it — one shared wrapper,
/// [`topbar_clearance`](crate::ui::topbar::topbar_clearance), so the three
/// such screens cannot drift (`/`, bar-less, wraps nothing).
pub fn root_view(
    controller: &NavigatorController<AppState>,
    initial: impl Fn() -> AnyView<AppState> + 'static,
    overlay: Option<AnyView<AppState>>,
    nav: &Nav,
    connection: &ConnectionController,
) -> AnyView<AppState> {
    let mut children = vec![
        any(navigator(controller, initial)),
        shell_bar(nav, connection),
        toast_layer(),
    ];
    children.extend(overlay);
    any(Stack(children))
}

fn shell_bar(nav: &Nav, connection: &ConnectionController) -> AnyView<AppState> {
    let state = nav.bar();
    let reverse = state.direction == BarDirection::Back;
    let Some(meta) = state.meta else {
        return any(muxr_topbar::<AppState>("").hidden(true).reverse(reverse));
    };

    let mut bar = muxr_topbar::<AppState>(meta.resolve_title(connection))
        .reverse(reverse)
        .hidden(state.fullscreen);
    bar = match meta.leading {
        BarLeading::Brand => bar,
        BarLeading::Back => {
            let nav = nav.clone();
            bar.back(move |_state: &mut AppState| nav.pop())
        }
    };
    bar = match meta.action {
        Some(BarAction::Settings) => {
            let nav = nav.clone();
            bar.action(
                icons::SETTINGS,
                SETTINGS_LABEL,
                move |_state: &mut AppState| nav.push(SETTINGS_ROUTE),
            )
        }
        // Forward only: the terminal feature owns its own menu (its entries,
        // its anchor rect and every action they run), exactly as `/settings`
        // owns everything behind the `⚙`.
        Some(BarAction::TerminalMenu) => {
            bar.action(icons::MORE_VERT, TERMINAL_MENU_LABEL, open_terminal_menu)
        }
        None => bar,
    };
    any(bar)
}
