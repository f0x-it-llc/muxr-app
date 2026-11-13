//! Headless coverage for the terminal surface's pinch wrapper: two-contact touch pinches and
//! desktop scale brackets reach the sink as one intent per gesture, the focal lands in the
//! grid's own coordinate space, and a stolen gesture leaves the grid's tap/selection untouched.

use std::cell::RefCell;
use std::rc::Rc;

use frust::authoring::{InputEvent, PaintScene, PointerPhase};
use frust::{AnyView, GetUntracked, RwSignal, ScaleEvent, ScalePhase, any};
use frust_core::RenderRoot;
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::features::terminal::presentation::term::chip::copy_chip;
use muxr::features::terminal::presentation::term::selection::GridPoint;
use muxr::features::terminal::presentation::term::{
    ContentTapSink, PinchIntent, PinchSink, SelectionSeam, SharedEmulator, TerminalEmulator,
    TerminalPalette, terminal_grid, terminal_surface_with_pinch,
};

mod support;
use support::{HELD_MS, contact, ms, pointer, serial, setup};

const PHONE: Size = Size::new(392.7, 872.7);

struct Null;
impl PaintScene for Null {
    fn fill_rect(&mut self, _o: Point, _s: Size, _c: peniko::Color) {}
    fn draw_text(&mut self, _o: Point, _t: &str) {}
}

type Logic = Box<dyn FnMut(&mut ()) -> AnyView<()>>;

struct Mounted {
    root: RenderRoot<(), AnyView<()>>,
    logic: Logic,
    tcx: TextContext,
    seam: SelectionSeam,
    intents: Rc<RefCell<Vec<(PinchIntent, Point)>>>,
    taps: Rc<RefCell<Vec<(u32, u32)>>>,
}

impl Mounted {
    fn new(with_sink: bool) -> Self {
        let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
        emulator.borrow_mut().feed(b"cargo build release");
        let seam = SelectionSeam::new(Rc::clone(&emulator));
        let generation = RwSignal::new(1u64);
        let intents: Rc<RefCell<Vec<(PinchIntent, Point)>>> = Rc::default();
        let taps: Rc<RefCell<Vec<(u32, u32)>>> = Rc::default();
        let recorder = Rc::clone(&intents);
        let sink: PinchSink =
            Rc::new(move |intent, focal| recorder.borrow_mut().push((intent, focal)));
        let tap_log = Rc::clone(&taps);
        let tap: ContentTapSink = Rc::new(move |col, row| {
            tap_log.borrow_mut().push((col, row));
            false
        });
        let view_seam = seam.clone();
        let logic: Logic = Box::new(move |_state: &mut ()| {
            let grid = terminal_grid(
                Rc::clone(&emulator),
                generation.get_untracked(),
                TerminalPalette::fallback(),
                13.0,
            )
            .on_content_tap(Rc::clone(&tap));
            any(terminal_surface_with_pinch(
                view_seam.clone(),
                grid,
                copy_chip(view_seam.clone()),
                with_sink.then(|| Rc::clone(&sink)),
            ))
        });
        let mut root = RenderRoot::new();
        root.set_theme(Box::new(frust_glyph::baseline()));
        let mut m = Self {
            root,
            logic,
            tcx: TextContext::new(),
            seam,
            intents,
            taps,
        };
        m.frame(0.0);
        m
    }

    fn frame(&mut self, at_ms: f64) {
        self.root.rebuild(&mut self.logic, &mut ());
        let tcx: &mut dyn std::any::Any = &mut self.tcx;
        self.root.layout_with_text(PHONE, tcx);
        self.root.paint(&mut Null, ms(at_ms));
    }

    fn send(&mut self, event: InputEvent) {
        self.root.event(&mut (), &event);
    }

    fn intents(&self) -> Vec<PinchIntent> {
        self.intents.borrow().iter().map(|(i, _)| *i).collect()
    }
}

fn p(x: f64, y: f64) -> Point {
    Point::new(x, y)
}

fn scale(phase: ScalePhase, delta: f64) -> InputEvent {
    InputEvent::Scale(ScaleEvent {
        phase,
        scale_delta: delta,
        focal: p(100.0, 100.0),
        velocity: 0.0,
    })
}

#[test]
fn a_two_finger_spread_enters_fullscreen_once_at_the_midpoint_cell() {
    use PointerPhase::{Down, Move, Up};
    let _guard = serial();
    let _owner = setup();
    let mut m = Mounted::new(true);
    m.send(contact(0, Down, p(100.0, 100.0)));
    m.send(contact(1, Down, p(130.0, 100.0)));
    m.send(contact(1, Move, p(160.0, 100.0)));
    m.frame(16.0);
    m.send(contact(1, Move, p(190.0, 100.0)));
    m.send(contact(1, Move, p(220.0, 100.0)));
    assert_eq!(m.intents(), [PinchIntent::EnterFullscreen]);
    let (_, focal) = m.intents.borrow()[0];
    // The focal is in the grid's own space, so it maps to the cell under the fingers.
    assert_eq!(focal, p(145.0, 100.0));
    let geo = m.seam.metrics().expect("laid out");
    assert_eq!(
        m.seam.cell_at(focal),
        Some(GridPoint {
            col: (145.0 / geo.width).floor() as u16,
            row: (100.0 / geo.height).floor() as u16,
        })
    );
    m.send(contact(1, Up, p(220.0, 100.0)));
    assert_eq!(m.intents().len(), 1);
}

#[test]
fn a_two_finger_converge_exits_fullscreen_once() {
    use PointerPhase::{Down, Move, Up};
    let _guard = serial();
    let _owner = setup();
    let mut m = Mounted::new(true);
    m.send(contact(0, Down, p(100.0, 100.0)));
    m.send(contact(1, Down, p(190.0, 100.0)));
    m.send(contact(1, Move, p(150.0, 100.0)));
    m.send(contact(1, Move, p(120.0, 100.0)));
    m.send(contact(1, Move, p(110.0, 100.0)));
    assert_eq!(m.intents(), [PinchIntent::ExitFullscreen]);
    m.send(contact(1, Up, p(110.0, 100.0)));
    assert_eq!(m.intents().len(), 1);
}

#[test]
fn desktop_scale_brackets_fire_one_intent_each() {
    let _guard = serial();
    let _owner = setup();
    let mut m = Mounted::new(true);
    for (delta, want) in [
        (1.4, PinchIntent::EnterFullscreen),
        (0.7, PinchIntent::ExitFullscreen),
    ] {
        m.intents.borrow_mut().clear();
        m.send(scale(ScalePhase::Begin, 1.0));
        m.send(scale(ScalePhase::Update, delta));
        m.send(scale(ScalePhase::End, 1.0));
        assert_eq!(m.intents(), [want]);
    }
}

#[test]
fn a_stolen_pinch_leaves_the_grid_unarmed_and_records_no_tap() {
    use PointerPhase::{Down, Move, Up};
    let _guard = serial();
    let _owner = setup();
    let mut m = Mounted::new(true);
    m.send(contact(0, Down, p(100.0, 100.0)));
    m.send(contact(1, Down, p(130.0, 100.0)));
    m.send(contact(1, Move, p(190.0, 100.0)));
    m.send(contact(1, Up, p(190.0, 100.0)));
    m.send(contact(0, Up, p(100.0, 100.0)));
    assert!(!m.seam.is_active());
    assert!(m.seam.range().is_none());
    assert!(m.taps.borrow().is_empty());
}

#[test]
fn without_a_sink_nothing_fires_and_the_single_finger_tap_still_lands() {
    use PointerPhase::{Down, Move, Up};
    let _guard = serial();
    let _owner = setup();
    let mut m = Mounted::new(false);
    m.send(contact(0, Down, p(100.0, 100.0)));
    m.send(contact(1, Down, p(130.0, 100.0)));
    m.send(contact(1, Move, p(190.0, 100.0)));
    m.send(contact(1, Up, p(190.0, 100.0)));
    m.send(contact(0, Up, p(100.0, 100.0)));
    assert!(m.intents().is_empty());
    assert!(!m.seam.is_active());
    assert!(m.seam.range().is_none());
    // Without the wrapper only the first contact's own tap reaches the grid.
    let geo = m.seam.metrics().expect("laid out");
    let first = (
        (100.0 / geo.width).floor() as u32,
        (100.0 / geo.height).floor() as u32,
    );
    assert_eq!(*m.taps.borrow(), [first]);
    m.taps.borrow_mut().clear();

    m.send(pointer(Down, p(50.0, 50.0)));
    m.send(pointer(Up, p(50.0, 50.0)));
    assert_eq!(m.taps.borrow().len(), 1);
}

/// Finger A rests on the word "cargo" on row 0; finger B starts to its right on the same row.
fn a() -> Point {
    p(30.0, 5.0)
}

fn b(x: f64) -> Point {
    p(x, 5.0)
}

fn spread(m: &mut Mounted) {
    use PointerPhase::Move;
    for x in [160.0, 190.0, 220.0] {
        m.send(contact(1, Move, b(x)));
    }
}

fn hold(m: &mut Mounted, from: f64) {
    m.frame(from);
    m.frame(from + HELD_MS);
    m.frame(from + HELD_MS + 20.0);
}

#[test]
fn an_anchored_finger_through_a_spread_never_starts_a_selection() {
    use PointerPhase::{Down, Up};
    let _guard = serial();
    let _owner = setup();
    let mut m = Mounted::new(true);
    m.send(contact(0, Down, a()));
    m.send(contact(1, Down, b(130.0)));
    spread(&mut m);
    hold(&mut m, 0.0);
    assert!(m.seam.range().is_none());
    assert_eq!(m.seam.haptic_count(), 0);
    assert_eq!(m.intents(), [PinchIntent::EnterFullscreen]);
    m.send(contact(1, Up, b(220.0)));
    m.send(contact(0, Up, a()));
    assert!(m.seam.range().is_none());
    assert!(!m.seam.is_active());
}

#[test]
fn a_selection_begun_before_the_pinch_is_cleared_at_begin() {
    use PointerPhase::Down;
    let _guard = serial();
    let _owner = setup();
    let mut m = Mounted::new(true);
    m.send(contact(0, Down, a()));
    hold(&mut m, 0.0);
    assert!(m.seam.range().is_some());
    assert!(m.seam.dragging());
    m.send(contact(1, Down, b(130.0)));
    spread(&mut m);
    assert!(m.seam.range().is_none());
    assert!(m.seam.chip_anchor().is_none());
    assert!(!m.seam.dragging());
    assert_eq!(m.intents(), [PinchIntent::EnterFullscreen]);
}

#[test]
fn a_cancelled_pinch_does_not_block_the_next_long_press() {
    use PointerPhase::{Cancel, Down, Up};
    let _guard = serial();
    let _owner = setup();
    let mut m = Mounted::new(true);
    m.send(contact(0, Down, a()));
    m.send(contact(1, Down, b(130.0)));
    spread(&mut m);
    m.send(contact(0, Cancel, a()));
    m.send(contact(1, Up, b(220.0)));
    assert!(m.seam.range().is_none());
    m.send(contact(0, Down, a()));
    hold(&mut m, 2_000.0);
    assert!(m.seam.range().is_some());
    assert_eq!(m.seam.haptic_count(), 1);
}

#[test]
fn a_plain_long_press_still_selects_with_one_haptic() {
    use PointerPhase::Down;
    let _guard = serial();
    let _owner = setup();
    let mut m = Mounted::new(true);
    m.send(contact(0, Down, a()));
    hold(&mut m, 0.0);
    assert!(m.seam.range().is_some());
    assert_eq!(m.seam.haptic_count(), 1);
    assert!(m.intents().is_empty());
}

#[test]
fn a_spread_then_lifting_the_second_finger_keeps_long_press_off_until_the_first_lifts() {
    use PointerPhase::{Down, Up};
    let _guard = serial();
    let _owner = setup();
    let mut m = Mounted::new(true);
    m.send(contact(0, Down, a()));
    m.send(contact(1, Down, b(130.0)));
    spread(&mut m);
    m.send(contact(1, Up, b(220.0)));
    hold(&mut m, 0.0);
    assert!(m.seam.range().is_none());
    assert_eq!(m.seam.haptic_count(), 0);
    assert_eq!(m.intents(), [PinchIntent::EnterFullscreen]);
    m.send(contact(0, Up, a()));
    assert!(m.seam.range().is_none());
    m.send(contact(0, Down, a()));
    hold(&mut m, 2_000.0);
    assert!(m.seam.range().is_some());
    assert_eq!(m.seam.haptic_count(), 1);
}
