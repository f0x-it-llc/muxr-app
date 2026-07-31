//! Muxr's persistent shell — trimmed from `examples/huddle`'s `shell.rs`
//! (the 4-tab bottom navigation bar) to fit stack-style navigation: muxr has
//! no tabs, so there is no persistent chrome to render today.
//!
//! [`root_view`] is the seam this module exists for: Phase 2 can grow a
//! session-guard check — or any other always-mounted chrome — around the
//! navigator without the composition root itself changing shape. Two things
//! occupy it today, both stacked **above** the navigator rather than in place
//! of it: the app-wide toast layer ([`crate::ui::toasts`]), always mounted,
//! and the cold-start splash, mounted as an `overlay` only while it plays.
//! The navigator keeps rendering `/` underneath for the whole sequence, which
//! is what lets the overlay simply disappear at the end (see
//! [`crate::ui::splash::sequence`]'s handoff note) — the root is *always* a
//! `Stack`, with or without an overlay, so that disappearance costs only the
//! overlay's own pod (see [`root_view`]'s doc for why the container can never
//! be optional).

use frust::{AnyView, NavigatorController, Stack, any, navigator};

use crate::AppState;
use crate::ui::toasts::toast_layer;

/// The app's entire root content: the router-driven navigator, the app-wide
/// toast layer over it, and `overlay` stacked over both when there is one.
///
/// `initial` is only ever invoked on the very first build (the controller's
/// stack is already non-empty by then — see `lib.rs`'s `Component::init`,
/// which resolves the start location before the first `build` runs).
/// `overlay` is rebuilt like any other child: the frame it becomes `None` is
/// the frame it unmounts.
///
/// **Layer order is `[navigator, toasts, overlay?]`, and the first two indices
/// are fixed.** The toast layer ([`crate::ui::toasts::toast_layer`]) sits
/// above the navigator so a toast floats over whatever screen is on top, and
/// below the splash overlay so a toast raised during a cold start doesn't
/// punch through it. It is *always* present (an idle
/// [`frust::glyph::toast_host`] paints nothing, requests no frames, and —
/// never overriding `Widget::event` — passes every pointer event straight
/// through to the navigator underneath), which is what keeps it out of the
/// stable-Stack invariant below: only the growable **tail** (the overlay) ever
/// appears or disappears, so the navigator stays at index 0 and the toast host
/// at index 1 for the whole process lifetime, both reconciled in place.
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
    let mut children = vec![navigator, toast_layer()];
    children.extend(overlay);
    any(Stack(children))
}
