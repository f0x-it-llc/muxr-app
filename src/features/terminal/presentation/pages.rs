//! The `/terminal` route's placeholder page: centered page-name text under
//! the shell's own bar. Phase 2 replaces this with the real terminal surface.

use frust::{Align, Alignment, AnyView, NavigatorController, any, safe_area, text};

use crate::AppState;

/// The Terminal feature's placeholder page. Takes the [`NavigatorController`]
/// (unused today — see `crate::routes::build_routes`'s dual-use shape); a
/// later task pushes through it once Terminal has somewhere to navigate to.
///
/// **No bar of its own.** `crate::shell` mounts the one persistent
/// `crate::ui::topbar` above the navigator and titles this route `terminal`
/// with a `‹` from `crate::routes::route_meta` (the mock's `⋮` joins that bar's
/// trailing slot in Phase 3) — a page-owned `glyph::app_bar` here would stack a
/// second bar under it. That shell bar consumes the top window inset, which is
/// why the body's safe area still disables its own top edge.
pub fn terminal_page(_controller: NavigatorController<AppState>) -> AnyView<AppState> {
    any(safe_area(Align(Alignment::CENTER, text("Terminal").size(20.0))).top(false))
}
