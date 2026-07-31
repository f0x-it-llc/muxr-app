//! The `/terminal` route's placeholder page: centered page-name text under
//! the shell's own bar. Phase 2 replaces this with the real terminal surface.

use frust::{Align, Alignment, AnyView, NavigatorController, safe_area, text};

use crate::AppState;
use crate::ui::topbar::topbar_clearance;

/// The Terminal feature's placeholder page. Takes the [`NavigatorController`]
/// (unused today — see `crate::routes::build_routes`'s dual-use shape); a
/// later task pushes through it once Terminal has somewhere to navigate to.
///
/// **No bar of its own.** `crate::shell` mounts the one persistent
/// `crate::ui::topbar` over the navigator and titles this route `terminal`
/// with a `‹` from `crate::routes::route_meta` (the mock's `⋮` joins that bar's
/// trailing slot in Phase 3) — a page-owned `glyph::app_bar` here would stack a
/// second bar under it. That bar *floats* over a full-height page, so this page
/// reserves the band it covers itself ([`topbar_clearance`], which contributes
/// the top window inset the bar consumes — hence the body's safe area still
/// disabling its own top edge).
pub fn terminal_page(_controller: NavigatorController<AppState>) -> AnyView<AppState> {
    topbar_clearance(safe_area(Align(Alignment::CENTER, text("Terminal").size(20.0))).top(false))
}
