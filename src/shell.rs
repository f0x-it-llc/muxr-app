//! Muxr's persistent shell — trimmed from `examples/huddle`'s `shell.rs`
//! (the 4-tab bottom navigation bar) to fit stack-style navigation: muxr has
//! no tabs, so there is no persistent chrome to render today.
//!
//! [`root_view`] is the seam this module exists for: Phase 2 can grow a
//! session-guard check — or any other always-mounted chrome — around the
//! navigator without the composition root itself changing shape. Its one
//! occupant today is the cold-start splash, mounted as an `overlay` **above**
//! the navigator rather than in place of it: the navigator keeps rendering
//! `/` underneath for the whole sequence, which is what lets the overlay
//! simply disappear at the end instead of handing anything over (see
//! [`crate::ui::splash::sequence`]'s handoff note).

use frust::{AnyView, NavigatorController, Stack, any, navigator};

use crate::AppState;

/// The app's entire root content: the router-driven navigator, with `overlay`
/// stacked over it when there is one.
///
/// `initial` is only ever invoked on the very first build (the controller's
/// stack is already non-empty by then — see `lib.rs`'s `Component::init`,
/// which resolves the start location before the first `build` runs).
/// `overlay` is rebuilt like any other child: the frame it becomes `None` is
/// the frame it unmounts.
pub fn root_view(
    controller: &NavigatorController<AppState>,
    initial: impl Fn() -> AnyView<AppState> + 'static,
    overlay: Option<AnyView<AppState>>,
) -> AnyView<AppState> {
    let navigator = any(navigator(controller, initial));
    match overlay {
        // A bare navigator when nothing is over it, so the ordinary case
        // costs no extra container.
        None => navigator,
        Some(overlay) => any(Stack(vec![navigator, overlay])),
    }
}
