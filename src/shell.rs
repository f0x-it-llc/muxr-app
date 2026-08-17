//! Muxr's persistent shell — trimmed from `examples/huddle`'s `shell.rs`
//! (the 4-tab bottom navigation bar) to fit stack-style navigation: muxr has
//! no tabs, so the one piece of always-mounted chrome is the **top bar**.
//!
//! [`root_view`] is the seam this module exists for, and it is one `Stack` all
//! the way down: a full-height navigator, the muxr top bar
//! ([`crate::ui::topbar`]) **floating over** it, the app-wide toast layer
//! ([`crate::ui::toasts`]) over that, and the cold-start splash over
//! everything — the splash only while it plays, with the navigator still
//! rendering `/` underneath for the whole sequence, which is what lets the
//! overlay simply disappear at the end (see
//! [`crate::ui::splash::sequence`]'s handoff note).
//!
//! One invariant covers the lot: the root is *always* a `Stack`, its first
//! three children are *always* `[navigator, bar, toasts]` in that order, and
//! only the overlay tail ever grows or shrinks (see [`root_view`]'s doc for
//! why none of the three can be optional, and why the bar floats rather than
//! sharing a band with the navigator).

use frust::{AnyView, NavigatorController, Stack, any, icons, navigator};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::features::terminal::presentation::pages::open_terminal_menu;
use crate::routes::{BarAction, BarDirection, BarLeading, Nav, SETTINGS_ROUTE};
use crate::ui::toasts::toast_layer;
use crate::ui::topbar::muxr_topbar;

/// `.topbar-icon`'s settings affordance: its accessible name.
///
/// The **mark** is `frust::icons::SETTINGS`, a vector — never the `⚙` glyph
/// this slot used to set as text, which is absent from the bundled Glyph faces
/// and so rendered differently on Android and not at all on iPhone. See
/// `crate::ui::topbar`'s "Chrome icons are vectors" rule.
const SETTINGS_LABEL: &str = "Settings";

/// The terminal route's overflow affordance: its accessible name.
///
/// The **mark** is `frust::icons::MORE_VERT`, a vector — never the `⋮` glyph
/// `docs/terminal.html` sets as text, for the same reason the `⚙` above is a
/// vector (see `crate::ui::topbar`'s "Chrome icons are vectors" rule). The
/// menu it opens belongs to the terminal feature; this file only forwards the
/// press (see [`shell_bar`]).
const TERMINAL_MENU_LABEL: &str = "Terminal menu";

/// The app's entire root content: a full-height navigator, the top bar
/// floating over it, the app-wide toast layer over that, and `overlay` stacked
/// over everything when there is one.
///
/// `initial` is only ever invoked on the very first build (the controller's
/// stack is already non-empty by then — `lib.rs`'s `Component::init` resolves
/// the start location before the first `build` runs). `overlay` is rebuilt
/// like any other child: the frame it becomes `None` is the frame it
/// unmounts. `nav`/`connection` are what the bar reads: the route-meta mirror
/// (`crate::routes`' module docs) says which route is on top and which way
/// the last change travelled; the connection controller supplies `/sessions`'
/// host title.
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
///
/// # The stable-`Stack` invariant (indices 0–2 fixed, only the tail grows)
///
/// **The root always returns a `Stack`, never a bare navigator, and its first
/// three children are always `[navigator, bar, toast layer]`.** `AnyView`
/// erases the concrete view type, and `AnyView::rebuild` tears down and
/// rebuilds the whole element the instant that erased type changes between
/// two builds (frust-core's `dyn_rebuild`), while `Stack` reconciles
/// *positionally* — so a child that changes index is reconciled against a
/// different view type and suffers the same fate. Returning the bare
/// navigator when `overlay` is `None` would discard the entire live navigator
/// subtree the frame the splash disappears; dropping the bar from the child
/// list on a bar-less route would shift the toast layer's index and tear it
/// down on every arrival at and departure from Connect. So the bar is
/// *always* mounted and merely
/// [hidden](crate::ui::topbar::TopBarView::hidden) — zero height, no paint,
/// no events, no semantics — on a route with no bar, and the toast layer is
/// likewise always present (an idle [`frust_glyph::toast_host`] paints
/// nothing and passes every pointer event straight through) rather than a
/// second growable slot. It sits above the bar so a toast floats over
/// whatever chrome is on top, and below the splash overlay so a toast raised
/// during a cold start doesn't punch through it.
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

/// This frame's top bar, assembled from the route on top.
///
/// A route with no metadata (`/` — see [`crate::routes::route_meta`]) still
/// mounts a bar; it is simply hidden, which is the invariant above (and what
/// it animates *to*, rather than snapping). Every
/// affordance the bar carries acts through [`Nav`], so the mirror the bar reads
/// stays the same one the navigation updates.
fn shell_bar(nav: &Nav, connection: &ConnectionController) -> AnyView<AppState> {
    // Tracked: a navigation (or a bypassing pop the mirror reconciled) wakes
    // this rebuild.
    let state = nav.bar();
    let reverse = state.direction == BarDirection::Back;
    let Some(meta) = state.meta else {
        return any(muxr_topbar::<AppState>("").hidden(true).reverse(reverse));
    };
    // Fullscreen is a **second input to the same hidden flag** the bar-less
    // `/` route sets above (never a second hide path): the terminal screen
    // publishes it through the route-meta mirror, and the bar plays the very
    // reveal it already plays on a route change. `crate::routes::BarState`'s
    // own docs carry the rationale; the screen clears the flag on unmount.
    //
    // Modality needs no input here at all: every overlay is a page of the root
    // host this whole view is the root page of (`crate::AppState::overlays`),
    // which routes input and publishes semantics for its top page only — so an
    // open overlay makes this bar inert and unaddressable without the bar
    // knowing anything about it, and paints its own scrim over it.
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
