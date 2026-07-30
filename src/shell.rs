//! Muxr's persistent shell — trimmed from `examples/huddle`'s `shell.rs`
//! (the 4-tab bottom navigation bar) to fit stack-style navigation: muxr has
//! no tabs, so there is no persistent chrome to render today.
//!
//! [`root_view`] is the seam this module exists for: a thin pass-through to
//! [`frust::navigator`] now, kept as its own function (rather than inlined
//! in `lib.rs`'s `Component::build`) so Phase 2 can grow a session-guard
//! check — or any other always-mounted chrome — around the navigator
//! without the composition root itself changing shape.

use frust::{AnyView, NavigatorController, any, navigator};

use crate::AppState;

/// The app's entire root content: the router-driven navigator, unwrapped.
/// `initial` is only ever invoked on the very first build (the controller's
/// stack is already non-empty by then — see `lib.rs`'s `Component::init`,
/// which resolves the start location before the first `build` runs).
pub fn root_view(
    controller: &NavigatorController<AppState>,
    initial: impl Fn() -> AnyView<AppState> + 'static,
) -> AnyView<AppState> {
    any(navigator(controller, initial))
}
