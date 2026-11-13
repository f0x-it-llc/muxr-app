//! `root_view` must return a stable `Stack` root so the navigator is retained when the splash overlay goes.
//! It is private to integration tests, so its two shapes are reproduced below over a probe widget.
//! Each built instance gets a new id: equal ids across frames mean retained, a new id means torn down.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use frust::{AnyView, Stack, any};
use frust_core::{
    BoxConstraints, BuildCtx, ChangeFlags, FrameTime, LayoutCtx, PaintCtx, PaintScene, RenderRoot,
    View, Widget,
};
use kurbo::{Point, Size};
use peniko::Color;

type S = ();

struct ProbeView {
    log: Rc<RefCell<Vec<u32>>>,
    next_id: Rc<Cell<u32>>,
}

fn probe(log: &Rc<RefCell<Vec<u32>>>, next_id: &Rc<Cell<u32>>) -> ProbeView {
    ProbeView {
        log: log.clone(),
        next_id: next_id.clone(),
    }
}

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

struct NullScene;

impl PaintScene for NullScene {
    fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
}

fn pre_fix_root(navigator: AnyView<S>, overlay: Option<AnyView<S>>) -> AnyView<S> {
    match overlay {
        None => navigator,
        Some(overlay) => any(Stack(vec![navigator, overlay])),
    }
}

/// Always a `Stack`, with or without an overlay; keep in sync by eye with `root_view`.
fn post_fix_root(navigator: AnyView<S>, overlay: Option<AnyView<S>>) -> AnyView<S> {
    let mut children = vec![navigator];
    children.extend(overlay);
    any(Stack(children))
}

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
    // A bare-navigator/Stack swap changes the erased root's type, tearing the subtree down and discarding retained state.
    let (mounted, handoff) = drive_handoff(pre_fix_root);
    assert_ne!(
        mounted, handoff,
        "a bare-navigator/Stack swap must discard the navigator's identity \
         across the handoff — this failing would mean the regression this \
         fix closes is no longer reproducible here, and the reproduction \
         itself needs a second look"
    );
}
