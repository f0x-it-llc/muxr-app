//! Widget-identity regression test: `src/shell.rs`'s `root_view` must return a stable four-slot
//! `Stack` root, never a bare navigator that swaps with a `Stack` as the splash overlay comes
//! and goes.
//!
//! Can't call `root_view` directly — its module (`mod shell;`) is private, invisible to an
//! integration test even though the fn is `pub`. So this reproduces its pre-fix/post-fix bodies
//! verbatim as [`pre_fix_root`]/[`post_fix_root`] (keep in sync by eye with `src/shell.rs` if it
//! changes shape) over a synthetic [`ProbeView`]/[`ProbeWidget`] standing in for the navigator.
//! Mechanism under test: `AnyView::rebuild`'s `dyn_rebuild` tears a widget down and rebuilds it
//! fresh the instant the erased view's concrete type changes between builds, and rebuilds it in
//! place otherwise.
//!
//! [`ProbeWidget`] makes that observable: each freshly built instance gets a new id (bumped only
//! in `build`, never `rebuild`), logged on every paint, so comparing consecutive frames' ids
//! shows whether the navigator was retained or torn down across the overlay's `Some -> None`
//! flip. `State` is `()` throughout.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use frust::{AnyView, Stack, any};
use frust_core::{
    BoxConstraints, BuildCtx, ChangeFlags, FrameTime, LayoutCtx, PaintCtx, PaintScene, RenderRoot,
    View, Widget,
};
use kurbo::{Point, Size};
use peniko::Color;

/// Test-local app state — see the module doc for why `()` is enough here.
type S = ();

// ---------------------------------------------------------------------------
// The probe: a fresh-build id logged on every paint reveals retain vs. rebuild.
// ---------------------------------------------------------------------------

struct ProbeView {
    log: Rc<RefCell<Vec<u32>>>,
    next_id: Rc<Cell<u32>>,
}

/// A probe standing in for the navigator: erased the same way
/// `shell::root_view` erases the real one (`any(navigator(...))`).
fn probe(log: &Rc<RefCell<Vec<u32>>>, next_id: &Rc<Cell<u32>>) -> ProbeView {
    ProbeView {
        log: log.clone(),
        next_id: next_id.clone(),
    }
}

/// A throwaway probe standing in for the splash overlay — only its presence/absence in the child
/// list matters, so it gets its own log/id source nobody reads. Avoids pulling in a `Text` (and
/// the `TextContext` a real one would need here) just to occupy the second slot.
fn overlay_probe() -> AnyView<S> {
    any(probe(
        &Rc::new(RefCell::new(Vec::new())),
        &Rc::new(Cell::new(0)),
    ))
}

struct ProbeWidget {
    id: u32,
    log: Rc<RefCell<Vec<u32>>>,
}

impl View<S> for ProbeView {
    type Element = ProbeWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> ProbeWidget {
        // A never-reused id: the tell that this frame tore down (or first-mounted) rather than
        // reconciling in place.
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        ProbeWidget {
            id,
            log: self.log.clone(),
        }
    }

    fn rebuild(
        &self,
        _prev: &Self,
        _element: &mut ProbeWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // No fresh `build` call this frame *is* the assertion.
        ChangeFlags::NONE
    }
}

impl Widget for ProbeWidget {
    fn layout(&mut self, _ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        bc.constrain(Size::new(1.0, 1.0))
    }

    fn paint(&mut self, _ctx: &mut PaintCtx, _scene: &mut dyn PaintScene) {
        self.log.borrow_mut().push(self.id);
    }
}

/// A paint scene that discards everything — the probe records its own
/// identity directly (see above), so nothing here needs recording.
struct NullScene;

impl PaintScene for NullScene {
    fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

// ---------------------------------------------------------------------------
// `root_view`'s two shapes, reproduced (module doc explains why).
// ---------------------------------------------------------------------------

/// **Pre-fix.** A bare navigator when there is no overlay, a `Stack` wrapping
/// navigator+overlay when there is — the buggy shape: the root's erased
/// concrete type flips `StackView` <-> the navigator's own type the instant
/// `overlay` becomes `None`.
fn pre_fix_root(navigator: AnyView<S>, overlay: Option<AnyView<S>>) -> AnyView<S> {
    match overlay {
        None => navigator,
        Some(overlay) => any(Stack(vec![navigator, overlay])),
    }
}

/// **Post-fix.** Always a `Stack`, with or without an overlay — mirrors
/// `src/shell.rs::root_view`'s current body.
fn post_fix_root(navigator: AnyView<S>, overlay: Option<AnyView<S>>) -> AnyView<S> {
    let mut children = vec![navigator];
    children.extend(overlay);
    any(Stack(children))
}

/// Mount `compose` with an overlay present, paint one frame, then rebuild with the overlay gone
/// and paint again — the handoff frame `AppState::splash_overlay` returning `None` produces.
/// Returns the probe's logged id from each frame.
fn drive_handoff(compose: fn(AnyView<S>, Option<AnyView<S>>) -> AnyView<S>) -> (u32, u32) {
    let log = Rc::new(RefCell::new(Vec::<u32>::new()));
    let next_id = Rc::new(Cell::new(0u32));
    let overlay_present = Rc::new(Cell::new(true));

    let log_for_logic = log.clone();
    let next_id_for_logic = next_id.clone();
    let overlay_present_for_logic = overlay_present.clone();
    let mut logic = move |_: &mut S| -> AnyView<S> {
        let navigator = any(probe(&log_for_logic, &next_id_for_logic));
        let overlay = if overlay_present_for_logic.get() {
            Some(overlay_probe())
        } else {
            None
        };
        compose(navigator, overlay)
    };

    let mut state: S = ();
    let mut root: RenderRoot<S, AnyView<S>> = RenderRoot::new();

    root.rebuild(&mut logic, &mut state);
    root.layout(Size::new(100.0, 100.0));
    root.paint(&mut NullScene, FrameTime::ZERO);
    let mounted_id = *log.borrow().last().expect("the probe painted on mount");

    // The handoff frame: the overlay disappears, exactly what
    // `AppState::splash_overlay` returning `None` does in `Component::build`.
    overlay_present.set(false);
    root.rebuild(&mut logic, &mut state);
    root.layout(Size::new(100.0, 100.0));
    root.paint(&mut NullScene, FrameTime::ZERO);
    let handoff_id = *log
        .borrow()
        .last()
        .expect("the probe painted after the handoff");

    (mounted_id, handoff_id)
}

#[test]
fn post_fix_root_keeps_the_navigator_across_the_handoff() {
    let (mounted, handoff) = drive_handoff(post_fix_root);
    assert_eq!(
        mounted, handoff,
        "the stable Stack root must rebuild the navigator in place across \
         the overlay's Some -> None flip, not tear it down and rebuild it \
         fresh"
    );
}

#[test]
fn pre_fix_root_tears_the_navigator_down_across_the_handoff() {
    // Demonstrates the regression the fix closes: the pre-fix bare-navigator/Stack swap changes
    // the erased root's concrete type the instant the overlay disappears, so `dyn_rebuild` tears
    // the whole subtree down and rebuilds it fresh — discarding retained state (focus, cursor,
    // scroll offset, …).
    let (mounted, handoff) = drive_handoff(pre_fix_root);
    assert_ne!(
        mounted, handoff,
        "a bare-navigator/Stack swap must discard the navigator's identity \
         across the handoff — this failing would mean the regression this \
         fix closes is no longer reproducible here, and the reproduction \
         itself needs a second look"
    );
}
