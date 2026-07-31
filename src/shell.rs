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

use frust::{AnyView, NavigatorController, Stack, any, navigator};

use crate::AppState;
use crate::connection::ConnectionController;
use crate::routes::{BarAction, BarDirection, BarLeading, Nav, SETTINGS_ROUTE};
use crate::ui::toasts::toast_layer;
use crate::ui::topbar::muxr_topbar;

/// `.topbar-icon`'s `⚙` — the Sessions bar's trailing action.
const SETTINGS_GLYPH: &str = "\u{2699}";

/// The app's entire root content: a full-height navigator, the top bar
/// floating over it, the app-wide toast layer over that, and `overlay` stacked
/// over everything when there is one.
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
/// # Why the bar floats instead of sharing a band with the navigator
///
/// It used to be the first child of a `Flex[bar, navigator]` band, which made
/// the bar's visibility a **layout** fact: arriving at `/sessions` shrank the
/// navigator by `46px + top inset`, so the bar snapped into existence and
/// shoved the incoming page down a frame before the page transition had
/// started to play, and leaving jerked it back up. (Round 3 of the design
/// review caught it in a slow-motion capture; the transition's own cross-fade
/// had been hiding most of it.)
///
/// Floating the bar takes that reflow off the table entirely: **the navigator
/// is always the full surface**, so a page's box never depends on which route
/// is on top, and the bar animates its own entrance and exit
/// ([`crate::ui::topbar`]) in the page transition's own Glyph timings instead.
/// The cost is that a bar-bearing page now owes itself the top clearance the
/// band used to hand it — one shared wrapper,
/// [`topbar_clearance`](crate::ui::topbar::topbar_clearance), so the three such
/// screens cannot drift (`/`, bar-less, wraps nothing). The redesign's fixed
/// conn-row strip is unaffected: it sits immediately under that clearance,
/// exactly where the band used to put it.
///
/// # The stable-`Stack` invariant (indices 0–2 fixed, only the tail grows)
///
/// **The root always returns a `Stack`, never a bare navigator, and its first
/// three children are always `[navigator, bar, toast layer]`.** `AnyView`
/// erases the concrete view type, and `AnyView::rebuild` tears down and
/// rebuilds the whole element the instant that erased type changes between two
/// builds (frust-core's `dyn_rebuild`) — while `Stack` reconciles
/// *positionally*, so a child that changes index is reconciled against a
/// different view type and suffers the same fate.
///
/// Both halves of the invariant follow:
///
/// - If this returned the navigator directly when `overlay` is `None` and a
///   `Stack` when it is `Some`, the frame the splash's `overlay` flips to
///   `None` would swap the erased root's concrete type and discard the entire
///   live navigator subtree (every route's retained widget state) only to
///   rebuild it fresh one frame later — exactly the frame the design promises
///   is a no-op.
/// - If the bar were dropped from the child list on the bar-less `/` route,
///   the toast layer would move between index 2 and index 1 on every arrival
///   at and departure from Connect, and positional reconciliation would tear
///   it down each time. So the bar is *always* mounted and merely
///   [hidden](crate::ui::topbar::TopBarView::hidden) — zero height, no paint,
///   no events, no semantics — on a route with no bar.
///
/// The toast layer is likewise *always* present (an idle
/// [`frust::glyph::toast_host`] paints nothing, requests no frames, and —
/// never overriding `Widget::event` — passes every pointer event straight
/// through), so it is a fixed index rather than a second growable slot. It
/// sits above the bar so a toast floats over whatever screen *and* chrome is
/// on top, and below the splash overlay so a toast raised during a cold start
/// doesn't punch through it — the splash covering the bar being guaranteed by
/// the same ordering, not by the bar happening to be hidden on `/`.
///
/// Keeping all three fixed means the erased types never change across either
/// handoff: positional reconciliation rebuilds indices 0–2 in place, and only
/// the shrunk tail (the overlay) is torn down.
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
