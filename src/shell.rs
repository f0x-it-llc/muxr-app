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
//! simply disappear at the end (see [`crate::ui::splash::sequence`]'s
//! handoff note) — the root is *always* a `Stack`, with or without an
//! overlay, so that disappearance costs only the overlay's own pod (see
//! [`root_view`]'s doc for why the container can never be optional).

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
///
/// **Always returns a `Stack`, never a bare navigator.** `AnyView` erases the
/// concrete view type, and `AnyView::rebuild` tears down and rebuilds the
/// whole element the instant that erased type changes between two builds
/// (frust-core's `dyn_rebuild`) — so if this returned the navigator directly
/// when `overlay` is `None` and a `Stack` when it is `Some`, the frame the
/// splash's `overlay` flips to `None` would swap the erased root's concrete
/// type from `StackView` to `NavigatorView` and discard the entire live
/// navigator subtree (every route's retained widget state) only to rebuild
/// it fresh one frame later — exactly the frame the design promises is a
/// no-op. Keeping the root a `Stack` unconditionally (`vec![navigator]` with
/// no overlay, `vec![navigator, overlay]` with one) means the erased type
/// never changes across the handoff: `Stack`'s positional reconciliation
/// (`rebuild_children_positional`) rebuilds the common index — the
/// navigator — in place, and only the shrunk tail (the overlay) is torn
/// down.
pub fn root_view(
    controller: &NavigatorController<AppState>,
    initial: impl Fn() -> AnyView<AppState> + 'static,
    overlay: Option<AnyView<AppState>>,
) -> AnyView<AppState> {
    let navigator = any(navigator(controller, initial));
    let mut children = vec![navigator];
    children.extend(overlay);
    any(Stack(children))
}
