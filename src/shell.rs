//! Muxr's persistent shell — trimmed from `examples/huddle`'s `shell.rs`
//! (the 4-tab bottom navigation bar) to fit stack-style navigation: muxr has
//! no tabs, so the one piece of always-mounted chrome is the **top bar**.
//!
//! [`root_view`] is the seam this module exists for. Three things occupy it
//! today. The app-wide toast layer ([`crate::ui::toasts`]) and the cold-start
//! splash stack **above** the navigator — the splash only while it plays, with
//! the navigator still rendering `/` underneath for the whole sequence, which
//! is what lets the overlay simply disappear at the end (see
//! [`crate::ui::splash::sequence`]'s handoff note). The third, the muxr top bar
//! ([`crate::ui::topbar`]), sits **beside** it: navigator and bar share a
//! vertical band, so the bar is chrome the pages scroll under rather than over.
//!
//! Both arrangements answer to the same invariant: the root is *always* a
//! `Stack` and the band is *always* `[bar, navigator]`, with or without an
//! overlay and with or without a visible bar (see [`root_view`]'s doc for why
//! neither container can ever be optional).

use frust::{
    AnyView, Axis, CrossAxisAlignment, FlexView, NavigatorController, Stack, any, flexible,
    inflexible, navigator,
};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::routes::{BarAction, BarDirection, BarLeading, Nav, SETTINGS_ROUTE};
use crate::ui::toasts::toast_layer;
use crate::ui::topbar::muxr_topbar;

/// `.topbar-icon`'s `⚙` — the Sessions bar's trailing action.
const SETTINGS_GLYPH: &str = "\u{2699}";

/// The app's entire root content: a `[top bar, navigator]` band, the app-wide
/// toast layer over it, and `overlay` stacked over both when there is one.
///
/// `initial` is only ever invoked on the very first build (the controller's
/// stack is already non-empty by then — see `lib.rs`'s `Component::init`,
/// which resolves the start location before the first `build` runs).
/// `overlay` is rebuilt like any other child: the frame it becomes `None` is
/// the frame it unmounts. `nav`/`connection` are what the bar reads: the
/// route-meta mirror (`crate::routes`' module docs) says *which* route is on
/// top and which way the last change travelled; the connection controller
/// supplies `/sessions`' host title.
///
/// **Layer order is `[band, toasts, overlay?]`, and the first two indices are
/// fixed.** The toast layer ([`crate::ui::toasts::toast_layer`]) sits above the
/// band so a toast floats over whatever screen (and bar) is on top, and below
/// the splash overlay so a toast raised during a cold start doesn't punch
/// through it. It is *always* present (an idle [`frust::glyph::toast_host`]
/// paints nothing, requests no frames, and — never overriding `Widget::event` —
/// passes every pointer event straight through to the band underneath), which
/// is what keeps it out of the stable-container invariant below: only the
/// growable **tail** (the overlay) ever appears or disappears.
///
/// # The stable-container invariant (two containers, one rule)
///
/// **The root always returns a `Stack`, never a bare navigator, and the band
/// always holds exactly two children, never one.** `AnyView` erases the
/// concrete view type, and `AnyView::rebuild` tears down and rebuilds the whole
/// element the instant that erased type changes between two builds
/// (frust-core's `dyn_rebuild`) — while `Stack`/`Flex` reconcile *positionally*,
/// so a child that changes index is reconciled against a different view type
/// and suffers the same fate.
///
/// Both halves of the invariant follow:
///
/// - If this returned the navigator directly when `overlay` is `None` and a
///   `Stack` when it is `Some`, the frame the splash's `overlay` flips to
///   `None` would swap the erased root's concrete type and discard the entire
///   live navigator subtree (every route's retained widget state) only to
///   rebuild it fresh one frame later — exactly the frame the design promises
///   is a no-op.
/// - If the band dropped its bar child on the bar-less `/` route, the navigator
///   would move between index 1 and index 0 on every arrival at and departure
///   from Connect, and positional reconciliation would tear it down each time.
///   So the bar is *always* mounted and merely
///   [hidden](crate::ui::topbar::TopBarView::hidden) — zero height, no paint,
///   no semantics — on a route with no bar.
///
/// Keeping both containers fixed means the erased types never change across
/// either handoff: positional reconciliation rebuilds the common indices — the
/// band, the bar and the navigator — in place, and only the shrunk tail (the
/// overlay) is torn down.
///
/// # Known wart: the band reflows the page under a bar that appears
///
/// The navigator is the band's flexible child, so mounting or unmounting the
/// bar changes its height by `46px + top inset` — and the route *leaving* a
/// bar-less `/` relayouts to the shorter box mid-transition. The Glyph slide
/// fades the leaving page out over the first ~44% of the transition, which
/// hides most of it, but it is a real reflow rather than a pure slide. The
/// alternative (a bar floating over a full-height navigator) would put every
/// screen's own scroll content under the bar instead, which the redesign's
/// fixed conn-row strip directly contradicts.
pub fn root_view(
    controller: &NavigatorController<AppState>,
    initial: impl Fn() -> AnyView<AppState> + 'static,
    overlay: Option<AnyView<AppState>>,
    nav: &Nav,
    connection: &ConnectionController,
) -> AnyView<AppState> {
    let band = any(FlexView::new(
        Axis::Vertical,
        vec![
            inflexible(shell_bar(nav, connection)),
            flexible(1, any(navigator(controller, initial))),
        ],
    )
    .cross_axis(CrossAxisAlignment::Stretch));
    let mut children = vec![band, toast_layer()];
    children.extend(overlay);
    any(Stack(children))
}

/// This frame's top bar, assembled from the route on top.
///
/// A route with no metadata (`/` — see [`crate::routes::route_meta`]) still
/// mounts a bar; it is simply hidden, which is the invariant above. Every
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
    let mut bar = muxr_topbar::<AppState>(meta.resolve_title(connection)).reverse(reverse);
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
            bar.action(SETTINGS_GLYPH, move |_state: &mut AppState| {
                nav.push(SETTINGS_ROUTE)
            })
        }
        None => bar,
    };
    any(bar)
}
