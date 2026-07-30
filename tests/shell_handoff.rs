//! Widget-identity test for the splash handoff — the regression `src/shell.rs`'s
//! `root_view` fix closes (code review's Finding 2; see that file's doc
//! comment and `docs/ARCHITECTURE.md`'s Shell section for the full story).
//!
//! # Why this can't call `shell::root_view` directly
//!
//! `root_view` is `pub`, but the module carrying it (`mod shell;` in
//! `src/lib.rs`) is not — an integration test under `tests/` is compiled as a
//! *separate crate* linking against `muxr` as a library, so it only sees
//! items reachable through a `pub mod` (`features`, `ui`) or a top-level
//! `pub` item (`AppState`, `MuxrApp`). A `pub fn` inside a private module is
//! still invisible outside the defining crate (Rust's effective-visibility
//! rule — the minimum of an item's own visibility and every ancestor
//! module's), so `shell::root_view` cannot be named from here at all. Making
//! it reachable would mean widening `src/lib.rs`'s module declaration (or
//! adding a re-export somewhere in the crate's public surface) — both files
//! outside this fix's scope (`src/shell.rs`, `docs/ARCHITECTURE.md`,
//! `tests/`).
//!
//! So instead this test reproduces `root_view`'s pre-fix and post-fix bodies
//! verbatim as [`pre_fix_root`]/[`post_fix_root`] (cross-reference
//! `src/shell.rs`'s current body — keep these in sync by eye if it ever
//! changes shape again) over a synthetic [`ProbeView`]/[`ProbeWidget`]
//! standing in for the navigator, and asserts on the exact framework
//! mechanism `src/shell.rs`'s doc comment cites: `AnyView::rebuild`'s
//! `dyn_rebuild` (`frust-core/src/view.rs`) tears a widget down and rebuilds
//! it fresh the instant the erased view's concrete type changes between two
//! builds, and rebuilds it in place (`View::rebuild`, no fresh `View::build`)
//! otherwise.
//!
//! [`ProbeWidget`] makes that directly observable without touching any
//! private state: each *freshly built* instance is stamped with a new
//! identity id (bumped only in [`View::build`], never in [`View::rebuild`])
//! and logs that id on every paint — so comparing the id two consecutive
//! frames painted tells us whether the navigator's widget was retained or
//! replaced across the handoff frame (the frame the overlay flips from
//! `Some` to `None`, exactly mirroring `AppState::splash_overlay`).
//!
//! `State` is `()` throughout: whether the erased root's concrete type
//! changes has nothing to do with the app's own `AppState`, so this stays
//! fully decoupled from `MuxrApp`'s internals (`routes`/`shell` are as
//! private to `lib.rs` as this mechanism is orthogonal to `AppState`).

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
// The probe: a widget whose identity (a freshly assigned id, logged on every
// paint) reveals whether it was retained across a rebuild or torn down and
// rebuilt fresh.
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

/// A throwaway probe standing in for the splash overlay — its identity is
/// never inspected, only its presence/absence in the child list matters, so
/// it gets its own log/id source nobody reads. Avoids pulling in a `Text`
/// (and the `TextContext` threading a real one would need in this harness)
/// just to occupy the second slot.
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
        // A fresh instance: stamp it with a never-reused id — the tell that
        // this frame tore down whatever was here before (or is the first
        // mount) rather than reconciling in place.
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
        // Same instance, nothing to reconcile: the mere absence of a fresh
        // `build` call this frame *is* the assertion.
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
// `root_view`'s two shapes, reproduced (see the module doc for why this test
// can't call `src/shell.rs::root_view` itself).
// ---------------------------------------------------------------------------

/// **Pre-fix.** A bare navigator when there is no overlay, a `Stack` wrapping
/// navigator+overlay when there is — the shape Finding 2 flagged: the root's
/// erased concrete type flips `StackView` <-> the navigator's own type the
/// instant `overlay` becomes `None`.
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

/// Mount `compose` with an overlay present, paint one frame, then rebuild
/// with the overlay gone and paint again — the exact handoff frame
/// `AppState::splash_overlay` returning `None` produces. Returns the probe's
/// logged id from each of the two frames.
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
    // Kept only to demonstrate the regression the fix above closes: with the
    // pre-fix bare-navigator/Stack swap, the erased root's concrete type
    // changes the instant the overlay disappears, so `AnyView::rebuild`
    // (`dyn_rebuild`) tears the whole subtree down and builds it fresh —
    // discarding whatever retained state (focus, cursor, scroll offset, …)
    // it held, one frame before it would have been rebuilt anyway.
    let (mounted, handoff) = drive_handoff(pre_fix_root);
    assert_ne!(
        mounted, handoff,
        "a bare-navigator/Stack swap must discard the navigator's identity \
         across the handoff — this failing would mean the regression this \
         fix closes is no longer reproducible here, and the reproduction \
         itself needs a second look"
    );
}
