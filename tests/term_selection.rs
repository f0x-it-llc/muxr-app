//! Headless end-to-end coverage for the terminal's selection, loupe and copy: a stationary hold
//! selects at the threshold, the haptic fires once per gesture, a clearing tap precedes the focus
//! claim, and `Copy` reaches an injected clipboard sink.

use std::cell::RefCell;
use std::rc::Rc;

use frust::authoring::{Affine, EditingState, ImeEvent, InputEvent, PaintScene, PointerPhase};
use frust::{AnyView, GetUntracked, RwSignal, Set, any};
use frust_core::RenderRoot;
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{BezPath, Point, Shape, Size, Vec2};
use peniko::{Brush, Color};

use muxr::features::terminal::data::feeds::demo_feed::DemoFeed;
use muxr::features::terminal::domain::feed::TerminalFeed;
use muxr::features::terminal::presentation::term::chip::{
    CHIP_H, CHIP_W, COPIED_TOAST, CopySink, chip_rect, copy_chip,
};
use muxr::features::terminal::presentation::term::geometry::CellGeometry;
use muxr::features::terminal::presentation::term::selection::{GridPoint, GridRange, segments};
use muxr::features::terminal::presentation::term::widget::{
    LOUPE_H, LOUPE_MARGIN, LOUPE_SCALE, LOUPE_W, loupe_box, loupe_content_offset,
};
use muxr::features::terminal::presentation::term::{
    InputSeam, SelectionSeam, SharedEmulator, SharedFeed, TerminalEmulator, TerminalPalette,
    terminal_grid, terminal_surface,
};
use muxr::ui::toasts::app_toasts;

mod support;
use support::{ms, node_center_by_label, node_labels, pointer, serial, setup};

const FONT_SIZE: f32 = 13.0;

const PHONE: Size = Size::new(392.7, 872.7);

const LABEL: &str = "dev \u{b7} zsh";

/// Comfortably past the 500ms long-press threshold.
const HELD_MS: f64 = 600.0;

/// Words split by every kind of separator, a second line to drag onto, and a `/`-separated path.
const FIXTURE: &[u8] = b"cargo build release\r\nusr local bin\r\nalpha beta gamma";

#[derive(Default)]
struct SelScene {
    fills: Vec<(Point, Size, Color)>,
    rounded: Vec<(Point, Size, f64)>,
    /// The loupe pushes exactly one rounded clip, and nothing else in this tree does.
    rounded_clips: Vec<(Point, Size, f64)>,
    transforms: Vec<Affine>,
    shadows: Vec<(Point, Size)>,
    strokes: Vec<(Point, Size)>,
    paths: Vec<(Point, Size)>,
    glyph_runs: usize,
}

impl PaintScene for SelScene {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        self.fills.push((origin, size, color));
    }
    fn fill_rounded_rect(&mut self, origin: Point, size: Size, radius: f64, _color: Color) {
        self.rounded.push((origin, size, radius));
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
    fn draw_glyph_run(&mut self, _run: GlyphRun) {
        self.glyph_runs += 1;
    }
    fn push_clip(&mut self, _origin: Point, _size: Size) {}
    fn push_clip_rounded(&mut self, origin: Point, size: Size, radius: f64) {
        self.rounded_clips.push((origin, size, radius));
    }
    fn pop_clip(&mut self) {}
    fn push_transform(&mut self, transform: Affine) {
        self.transforms.push(transform);
    }
    fn pop_transform(&mut self) {}
    fn draw_shadow(
        &mut self,
        origin: Point,
        size: Size,
        _radius: f64,
        _std_dev: f64,
        _color: Color,
    ) {
        self.shadows.push((origin, size));
    }
    fn stroke_line(&mut self, _p0: Point, _p1: Point, _width: f64, _color: Color) {}
    fn stroke_path(&mut self, origin: Point, path: &BezPath, _width: f64, _brush: &Brush) {
        let bounds = path.bounding_box();
        self.strokes
            .push((origin, Size::new(bounds.width(), bounds.height())));
    }
    fn fill_path(&mut self, origin: Point, path: &BezPath, _brush: &Brush) {
        let bounds = path.bounding_box();
        self.paths
            .push((origin, Size::new(bounds.width(), bounds.height())));
    }
}

impl SelScene {
    fn fills_in(&self, color: Color) -> Vec<(Point, Size)> {
        self.fills
            .iter()
            .filter(|(_, _, painted)| *painted == color)
            .map(|(origin, size, _)| (*origin, *size))
            .collect()
    }

    fn pill(&self) -> Option<(Point, Size, f64)> {
        self.rounded
            .iter()
            .copied()
            .find(|(_, size, _)| near(size.width, CHIP_W) && near(size.height, CHIP_H))
    }
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.01
}

type Logic = Box<dyn FnMut(&mut ()) -> AnyView<()>>;

struct Mounted {
    root: RenderRoot<(), AnyView<()>>,
    logic: Logic,
    state: (),
    tcx: TextContext,
    emulator: SharedEmulator,
    selection: SelectionSeam,
    input: InputSeam,
    feed: Rc<RefCell<DemoFeed>>,
    generation: RwSignal<u64>,
    copied: Rc<RefCell<Vec<String>>>,
}

impl Mounted {
    fn new() -> Self {
        Self::with_read_only(false)
    }

    fn with_read_only(read_only: bool) -> Self {
        let feed = Rc::new(RefCell::new(DemoFeed::manual()));
        let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
        emulator.borrow_mut().feed(FIXTURE);
        let input = InputSeam::new(Rc::clone(&feed) as SharedFeed);
        let selection = SelectionSeam::new(Rc::clone(&emulator));
        let generation = RwSignal::new(1u64);

        let copied: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let recorder = Rc::clone(&copied);
        let sink: CopySink = Rc::new(move |text: &str| {
            recorder.borrow_mut().push(text.to_string());
            true
        });

        let view_emulator = Rc::clone(&emulator);
        let view_selection = selection.clone();
        let stored_input = input.clone();
        let logic: Logic = Box::new(move |_state: &mut ()| {
            let grid = terminal_grid(
                Rc::clone(&view_emulator),
                generation.get_untracked(),
                TerminalPalette::fallback(),
                FONT_SIZE,
            )
            .label(LABEL)
            .input(input.clone())
            .read_only(read_only);
            any(terminal_surface(
                view_selection.clone(),
                grid,
                copy_chip(view_selection.clone()).copy_sink(Rc::clone(&sink)),
            ))
        });

        let mut root = RenderRoot::new();
        root.set_theme(Box::new(frust_glyph::baseline()));
        let mut mounted = Mounted {
            root,
            logic,
            state: (),
            tcx: TextContext::new(),
            emulator,
            selection,
            input: stored_input,
            feed,
            generation,
            copied,
        };
        // `DemoFeed`'s chunk 0 is due at t=0; drain it so later drains hold only typed bytes.
        mounted.drain();
        mounted
    }

    fn advance_generation(&mut self) {
        let next = self.generation.get_untracked() + 1;
        self.generation.set(next);
    }

    fn frame(&mut self, at_ms: f64) -> SelScene {
        self.root.rebuild(&mut self.logic, &mut self.state);
        let tcx: &mut dyn std::any::Any = &mut self.tcx;
        self.root.layout_with_text(PHONE, tcx);
        let mut scene = SelScene::default();
        self.root.paint(&mut scene, ms(at_ms));
        scene
    }

    fn event(&mut self, phase: PointerPhase, at: Point) {
        self.root.event(&mut self.state, &pointer(phase, at));
    }

    /// Requires at least one frame first: the cell box is solved in `layout`.
    fn cell(&self, row: u16, col: u16) -> Point {
        let geo = self
            .selection
            .metrics()
            .expect("the grid publishes its geometry on the first layout");
        Point::new(
            (f64::from(col) + 0.5) * geo.width,
            (f64::from(row) + 0.5) * geo.height,
        )
    }

    /// No pointer event but the `Down`. The first frame records the press epoch, the second crosses
    /// the threshold, and the third's rebuild drains the flush and fires `on_long_press`.
    fn hold(&mut self, at: Point) {
        self.event(PointerPhase::Down, at);
        self.frame(0.0);
        self.frame(HELD_MS);
        self.frame(HELD_MS + 20.0);
    }

    fn text(&self) -> String {
        self.selection.text()
    }

    fn apply(&mut self, text: &str) {
        let end = text.encode_utf16().count() as i32;
        let state = EditingState {
            text: text.to_string(),
            selection_base: end,
            selection_extent: end,
            composing_base: -1,
            composing_extent: -1,
        };
        self.root.event(
            &mut self.state,
            &InputEvent::Ime(ImeEvent::ApplyEditingState(state)),
        );
    }

    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let mut buffer = self.mirror();
            buffer.push(ch);
            self.apply(&buffer);
        }
    }

    fn published(&self) -> EditingState {
        self.input.published()
    }

    fn mirror(&self) -> String {
        self.published().text
    }

    fn buffered_units(&self) -> usize {
        self.input.source.borrow().buffered_units()
    }

    fn drain(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        self.feed
            .borrow_mut()
            .drain_output(&mut |bytes| out.extend_from_slice(bytes));
        out
    }

    fn drain_hex(&mut self) -> String {
        self.drain().iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn word(mounted: &Mounted, row: u16, col: u16) -> GridRange {
    let emulator = mounted.emulator.borrow();
    muxr::features::terminal::presentation::term::selection::word_at(
        emulator.screen(),
        GridPoint::new(row, col),
    )
    .expect("the fixture cell is word material")
}

#[test]
fn a_stationary_hold_selects_the_word_under_the_finger_and_ticks_once() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    // `build` is at row 0, cols 6..=10.
    mounted.hold(mounted.cell(0, 8));

    assert_eq!(
        mounted.selection.range(),
        Some(word(&mounted, 0, 8)),
        "a stationary hold selects the word under the finger, with no Move and \
         no Up — frust's threshold-fire long-press, first real consumer"
    );
    assert_eq!(mounted.text(), "build");
    assert!(
        mounted.selection.dragging(),
        "the finger is still down, so the drag (and the loupe) is live"
    );
    assert_eq!(
        mounted.selection.haptic_count(),
        1,
        "exactly one selection-click per gesture start"
    );
    assert!(
        mounted.selection.chip_anchor().is_none(),
        "no chip until the finger lifts"
    );
}

#[test]
fn the_haptic_stays_at_one_across_the_whole_gesture() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    mounted.hold(mounted.cell(0, 8));
    assert_eq!(mounted.selection.haptic_count(), 1);

    mounted.event(PointerPhase::Move, mounted.cell(0, 14));
    mounted.frame(700.0);
    mounted.event(PointerPhase::Move, mounted.cell(1, 2));
    mounted.event(PointerPhase::Up, mounted.cell(1, 2));
    mounted.frame(800.0);

    assert_eq!(
        mounted.selection.haptic_count(),
        1,
        "one gesture, one tick — the fire-exactly-once guarantee reaching a \
         real consumer"
    );
}

#[test]
fn a_hold_on_a_separator_still_ticks_but_selects_nothing() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    // Column 5 of row 0 is the space between `cargo` and `build`.
    mounted.hold(mounted.cell(0, 5));

    assert_eq!(
        mounted.selection.range(),
        None,
        "landing on a separator is a no-op"
    );
    assert_eq!(
        mounted.selection.haptic_count(),
        1,
        "the tick acknowledges the gesture, not the result — the incumbent \
         fires it before it resolves a word, and so do we"
    );

    mounted.event(PointerPhase::Up, mounted.cell(0, 5));
    assert!(mounted.selection.chip_anchor().is_none());
}

#[test]
fn a_drag_extends_word_wise_across_lines_and_the_release_raises_the_chip() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    mounted.hold(mounted.cell(0, 8)); // `build`
    mounted.event(PointerPhase::Move, mounted.cell(1, 5)); // `local` on row 1

    let range = mounted.selection.range().expect("selected");
    assert_eq!(
        range,
        word(&mounted, 0, 8).merge(word(&mounted, 1, 5)),
        "both ends re-resolve and merge in reading order"
    );
    assert_eq!(
        mounted.text(),
        "build release\nusr local",
        "the copy text is line-wise, not a rectangle, and joins distinct lines \
         with a newline"
    );

    mounted.event(PointerPhase::Move, mounted.cell(0, 1)); // `cargo`
    assert_eq!(
        mounted.selection.range(),
        Some(word(&mounted, 0, 1).merge(word(&mounted, 0, 8))),
        "the anchor is fixed; only the live end moves"
    );

    let release = mounted.cell(0, 1);
    mounted.event(PointerPhase::Up, release);
    assert!(!mounted.selection.dragging(), "the drag is over");
    assert_eq!(
        mounted.selection.chip_anchor(),
        Some(release),
        "the chip anchors where the finger lifted"
    );
}

#[test]
fn a_drag_that_never_leaves_one_word_still_selects_exactly_that_word() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    mounted.hold(mounted.cell(0, 8));
    mounted.event(PointerPhase::Move, mounted.cell(0, 9));
    assert_eq!(mounted.selection.range(), Some(word(&mounted, 0, 8)));
    assert_eq!(mounted.text(), "build");
}

#[test]
fn the_selection_paints_one_line_wise_stripe_per_row_in_the_palettes_wash() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    mounted.hold(mounted.cell(0, 8));
    mounted.event(PointerPhase::Move, mounted.cell(1, 5));
    let scene = mounted.frame(700.0);

    let geo = mounted.selection.metrics().expect("solved");
    let range = mounted.selection.range().expect("selected");
    let expected = segments(range, geo.cols);
    assert_eq!(expected.len(), 2, "two rows, two stripes");

    let wash = TerminalPalette::fallback().selection;
    let painted = scene.fills_in(wash);
    // The loupe re-paints the wash under its transform: the first `expected.len()` are the grid's.
    assert!(
        painted.len() >= expected.len(),
        "{} stripes painted for {} segments",
        painted.len(),
        expected.len()
    );
    for (stripe, (origin, size)) in expected.iter().zip(painted.iter()) {
        assert!(
            near(origin.x, f64::from(stripe.col) * geo.width)
                && near(origin.y, f64::from(stripe.row) * geo.height),
            "{stripe:?} vs {origin:?}"
        );
        assert!(
            near(size.width, f64::from(stripe.span) * geo.width) && near(size.height, geo.height),
            "{stripe:?} vs {size:?}"
        );
    }
}

#[test]
fn the_loupe_is_a_magnified_re_render_visible_only_while_the_drag_is_live() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();

    let idle = mounted.frame(0.0);
    assert!(idle.rounded_clips.is_empty());
    assert!(idle.transforms.is_empty());

    let at = mounted.cell(3, 10);
    mounted.hold(at);
    let dragging = mounted.frame(700.0);

    let window = loupe_box(at, PHONE);
    assert_eq!(
        dragging.rounded_clips.len(),
        1,
        "the loupe clips itself to one rounded window"
    );
    let (origin, size, _) = dragging.rounded_clips[0];
    assert!(
        near(origin.x, window.x0) && near(origin.y, window.y0),
        "{origin:?}"
    );
    assert!(
        near(size.width, LOUPE_W) && near(size.height, LOUPE_H),
        "{size:?}"
    );
    assert_eq!(
        dragging.transforms.len(),
        1,
        "…and re-paints the cells under exactly one transform"
    );
    // A re-render, not a bitmap blow-up: the same runs are submitted again inside the window.
    assert!(
        dragging.glyph_runs > 0,
        "the magnified window submits real glyph runs"
    );
    assert!(
        !dragging.shadows.is_empty(),
        "the window floats over the grid"
    );

    mounted.event(PointerPhase::Up, at);
    let released = mounted.frame(800.0);
    assert!(released.rounded_clips.is_empty(), "no loupe after the lift");
    assert!(released.transforms.is_empty());
}

#[test]
fn the_loupe_re_centres_on_the_finger_when_its_box_is_clamped_to_an_edge() {
    // The window floats above the finger, so away from every edge the offset is (0, float height).
    let float_y = 28.0 + LOUPE_H / 2.0;
    let free = Point::new(200.0, 400.0);
    let window = loupe_box(free, PHONE);
    assert_eq!(loupe_content_offset(free, window), Vec2::new(0.0, float_y));
    assert!(near(window.center().x, free.x));

    // In the corner the window pins to the margin and the offset keeps the finger's cell in view.
    let corner = Point::new(2.0, 2.0);
    let pinned = loupe_box(corner, PHONE);
    assert_eq!((pinned.x0, pinned.y0), (LOUPE_MARGIN, LOUPE_MARGIN));
    let offset = loupe_content_offset(corner, pinned);
    assert_eq!(
        offset,
        corner.to_vec2() - pinned.center().to_vec2(),
        "the offset is derived from the CLAMPED centre, not the wanted one"
    );
    assert!(
        offset.x < 0.0 && offset.y < 0.0,
        "the finger is up and left of the pinned window's centre: {offset:?}"
    );
    assert_ne!(
        offset.x, 0.0,
        "a clamped window means a non-zero horizontal recentre — the whole \
         point of the rule"
    );

    // The bottom edge does not clamp: the window floats above the finger.
    let far = Point::new(PHONE.width - 2.0, PHONE.height - 2.0);
    let pinned = loupe_box(far, PHONE);
    assert_eq!(pinned.x1, PHONE.width - LOUPE_MARGIN, "pinned right");
    assert!(pinned.y1 < PHONE.height - LOUPE_MARGIN, "not pinned bottom");
    let offset = loupe_content_offset(far, pinned);
    assert!(offset.x > 0.0, "…recentred left-of-finger: {offset:?}");
    assert_eq!(offset.y, float_y, "the vertical float is untouched");

    let short = Size::new(300.0, 200.0);
    let below = Point::new(150.0, 300.0);
    let pinned = loupe_box(below, short);
    assert_eq!(pinned.y1, short.height - LOUPE_MARGIN);
    assert!(
        loupe_content_offset(below, pinned).y > float_y,
        "the finger is further below the pinned window than the float height"
    );

    assert_eq!((LOUPE_W, LOUPE_H), (116.0, 46.0));
    assert_eq!(LOUPE_SCALE, 1.75);
    assert_eq!(LOUPE_MARGIN, 4.0);
}

#[test]
fn a_tap_that_clears_a_selection_does_not_also_focus_the_grid() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    mounted.hold(mounted.cell(0, 8));
    mounted.event(PointerPhase::Up, mounted.cell(0, 8));
    mounted.frame(700.0);
    assert!(mounted.selection.range().is_some(), "something to clear");
    // The hold claimed focus without raising a keyboard, so the next assertion is about ordering.
    assert!(mounted.root.is_focus_active());
    assert!(
        mounted.root.ime_state().is_none(),
        "a long press selects a word; it does not summon a keyboard over the \
         thing it just selected"
    );

    let elsewhere = mounted.cell(2, 2);
    mounted.event(PointerPhase::Down, elsewhere);
    mounted.event(PointerPhase::Up, elsewhere);
    mounted.frame(800.0);

    assert_eq!(mounted.selection.range(), None, "the tap cleared it");
    assert!(mounted.selection.chip_anchor().is_none());
    assert!(
        !mounted.root.is_focus_active(),
        "a tap that dismisses a selection must NOT also (re)focus the grid — \
         the clearing check runs first and returns Handled without ever \
         reaching the focus claim, so the root reads it as a blur"
    );
    assert!(
        mounted.root.ime_state().is_none(),
        "…and must NOT summon the keyboard"
    );

    mounted.event(PointerPhase::Down, elsewhere);
    mounted.event(PointerPhase::Up, elsewhere);
    assert!(mounted.root.is_focus_active(), "an ordinary tap focuses");
    assert!(
        mounted.root.ime_state().is_some(),
        "…and raises the keyboard"
    );
}

#[test]
fn a_clearing_tap_is_a_hand_off_not_a_departure_and_the_typed_line_survives_it() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    let focus_at = mounted.cell(4, 0);
    mounted.event(PointerPhase::Down, focus_at);
    mounted.event(PointerPhase::Up, focus_at);
    mounted.frame(10.0);
    assert!(
        mounted.root.is_focus_active(),
        "sanity: the grid is focused"
    );
    mounted.type_text("ls -l");
    assert_eq!(mounted.mirror(), "ls -l", "sanity: the line is mid-command");
    assert!(
        mounted.buffered_units() > 0,
        "sanity: the line is buffered before the dismiss"
    );
    mounted.drain(); // discard the typing's own echo before the byte assertion below

    mounted.hold(mounted.cell(0, 8)); // "build"
    mounted.event(PointerPhase::Up, mounted.cell(0, 8));
    mounted.frame(HELD_MS + 40.0);
    assert!(
        mounted.selection.range().is_some(),
        "sanity: something to clear"
    );

    // Far from the chip: a Down on its Dismiss half clears through `ChipTarget::Dismiss` instead.
    // The grid consumes this Down, yet the framework still blurs the focus path on it.
    let elsewhere = Point::new(PHONE.width - 20.0, PHONE.height - 20.0);
    assert!(
        !chip_rect(mounted.cell(0, 8), PHONE).contains(elsewhere),
        "sanity: the clearing tap must miss the chip's own pill"
    );
    mounted.event(PointerPhase::Down, elsewhere);
    mounted.event(PointerPhase::Up, elsewhere);
    mounted.frame(HELD_MS + 100.0);

    assert_eq!(
        mounted.selection.range(),
        None,
        "sanity: the tap dismissed the selection"
    );
    assert!(
        !mounted.root.is_focus_active(),
        "the dismissing tap still does not (re)focus the grid — unchanged \
         from the original behavior"
    );
    assert!(
        mounted.root.ime_state().is_none(),
        "…and still does not summon the keyboard"
    );

    // A Down the grid consumed is a hand-off, not a blur, so the in-progress baseline survives.
    assert!(
        mounted.buffered_units() > 0,
        "a dismissing tap the grid itself consumed must NOT scrub the \
         in-progress input baseline — the session is plainly still being \
         typed into"
    );
    assert_eq!(mounted.mirror(), "ls -l", "the whole line, untouched");

    // `Ime` routes along the focus path, so the grid is re-focused before the Backspace snapshot.
    mounted.event(PointerPhase::Down, focus_at);
    mounted.event(PointerPhase::Up, focus_at);
    mounted.frame(HELD_MS + 200.0);
    assert!(
        mounted.root.is_focus_active(),
        "the next ordinary tap refocuses"
    );
    mounted.drain();
    let mut shorter = mounted.mirror();
    shorter.pop();
    mounted.apply(&shorter);
    assert_eq!(
        mounted.drain_hex(),
        "7f",
        "the next shortening snapshot still emits a real backspace"
    );
}

fn select_and_release(mounted: &mut Mounted) -> Point {
    mounted.frame(0.0);
    mounted.hold(mounted.cell(0, 8));
    mounted.event(PointerPhase::Move, mounted.cell(0, 14));
    let release = mounted.cell(0, 14);
    mounted.event(PointerPhase::Up, release);
    mounted.frame(700.0);
    release
}

#[test]
fn the_chip_paints_exactly_where_the_incumbent_puts_it() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    let release = select_and_release(&mut mounted);
    let scene = mounted.frame(720.0);

    let (origin, size, _) = scene.pill().expect("the Copy pill painted");
    let expected = chip_rect(release, PHONE);
    assert!(
        near(origin.x, expected.x0) && near(origin.y, expected.y0),
        "{origin:?} vs {expected:?}"
    );
    assert!(near(size.width, CHIP_W) && near(size.height, CHIP_H));
    // Two vector marks, never font glyphs.
    assert!(
        scene.paths.len() >= 2,
        "a copy mark and a dismiss mark: {:?}",
        scene.paths
    );
    let labels = node_labels(&mounted.root);
    assert!(labels.contains(&"Copy".to_string()), "{labels:?}");
    assert!(
        labels.contains(&"Dismiss selection".to_string()),
        "{labels:?}"
    );
}

#[test]
fn copy_hands_the_exact_selected_text_to_the_sensitive_clipboard_seam() {
    let _serial = serial();
    let _owner = setup();
    app_toasts().clear();
    let mut mounted = Mounted::new();
    select_and_release(&mut mounted);

    let expected = mounted.text();
    assert_eq!(expected, "build release", "the fixture selection");

    let copy = node_center_by_label(&mounted.root, "Copy").expect("the chip announces Copy");
    mounted.event(PointerPhase::Down, copy);
    mounted.event(PointerPhase::Up, copy);

    assert_eq!(
        mounted.copied.borrow().as_slice(),
        &[expected],
        "the exact string reaches the clipboard seam, once"
    );
    assert_eq!(
        mounted.selection.range(),
        None,
        "a copy dismisses its own selection"
    );
    assert!(mounted.selection.chip_anchor().is_none(), "and its chip");
    assert!(
        app_toasts()
            .pending_untracked()
            .contains(&COPIED_TOAST.to_string()),
        "{:?}",
        app_toasts().pending_untracked()
    );

    let after = mounted.frame(800.0);
    assert!(after.pill().is_none());
    assert!(!node_labels(&mounted.root).contains(&"Copy".to_string()));
}

#[test]
fn dismiss_clears_the_selection_without_copying_anything() {
    let _serial = serial();
    let _owner = setup();
    app_toasts().clear();
    let mut mounted = Mounted::new();
    select_and_release(&mut mounted);

    let close =
        node_center_by_label(&mounted.root, "Dismiss selection").expect("the chip announces it");
    mounted.event(PointerPhase::Down, close);
    mounted.event(PointerPhase::Up, close);

    assert!(
        mounted.copied.borrow().is_empty(),
        "dismiss never touches the clipboard"
    );
    assert_eq!(mounted.selection.range(), None);
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "…and says nothing"
    );
    assert!(mounted.frame(800.0).pill().is_none());
}

#[test]
fn pressing_the_chip_never_reaches_the_grid_underneath() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    select_and_release(&mut mounted);

    // The chip sits above the gesture detector, so its `Down` must not be the grid's clearing tap.
    let copy = node_center_by_label(&mounted.root, "Copy").expect("announced");
    mounted.event(PointerPhase::Down, copy);
    assert!(
        mounted.selection.range().is_some(),
        "the press on the pill did not clear the selection it is about to copy"
    );
    mounted.event(PointerPhase::Up, copy);
    assert_eq!(mounted.copied.borrow().len(), 1);
}

#[test]
fn a_read_only_grid_still_selects_and_still_copies() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::with_read_only(true);
    select_and_release(&mut mounted);

    assert_eq!(mounted.text(), "build release");
    let copy = node_center_by_label(&mounted.root, "Copy").expect("announced");
    mounted.event(PointerPhase::Down, copy);
    mounted.event(PointerPhase::Up, copy);
    assert_eq!(mounted.copied.borrow().as_slice(), &["build release"]);
    assert!(
        !mounted.root.is_focus_active(),
        "…while still refusing focus, which is what read-only means for input"
    );
}

#[test]
fn copy_after_a_grid_shrink_returns_clamped_text_never_panics() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    // A multi-row selection whose begin column (6) is about to outlive the grid.
    mounted.hold(mounted.cell(0, 8));
    mounted.event(PointerPhase::Move, mounted.cell(1, 5));
    assert_eq!(
        mounted.text(),
        "build release\nusr local",
        "sanity: selected"
    );

    // `layout` resizes the emulator, then calls `set_metrics`; this replays that pair, narrower.
    let geo = mounted.selection.metrics().expect("laid out once already");
    let narrow = CellGeometry { cols: 5, ..geo };
    mounted
        .emulator
        .borrow_mut()
        .resize(narrow.cols, narrow.rows);
    mounted.selection.set_metrics(narrow);

    // A stale `begin.col` past the 5-col screen would underflow a `u16` in vt100 (overflow-checks).
    let text = mounted.text();
    assert!(
        !text.is_empty(),
        "the clamped selection still copies something: {text:?}"
    );
    assert!(
        text.chars().all(|c| c != '\0'),
        "no raw off-screen padding leaked through: {text:?}"
    );
}

#[test]
fn a_generation_bump_clears_a_held_selection_and_its_chip() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    let release = select_and_release(&mut mounted);
    assert!(mounted.selection.range().is_some(), "sanity: selected");
    assert_eq!(mounted.selection.chip_anchor(), Some(release));

    // New output arrived, so the screen the selection addresses may have scrolled.
    mounted.advance_generation();
    let scene = mounted.frame(900.0);

    assert_eq!(
        mounted.selection.range(),
        None,
        "a held selection is cleared rather than silently retargeted under \
         streaming output"
    );
    assert!(
        mounted.selection.chip_anchor().is_none(),
        "…and its chip goes with it"
    );
    assert!(scene.pill().is_none(), "…gone from the paint too");
    assert!(
        !node_labels(&mounted.root).contains(&"Copy".to_string()),
        "…and from the a11y tree"
    );
}

#[test]
fn a_generation_bump_with_nothing_selected_is_a_no_op() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    mounted.advance_generation();
    mounted.frame(20.0);
    mounted.hold(mounted.cell(0, 8));
    assert_eq!(mounted.text(), "build");
}

#[test]
fn a_rebuild_that_repeats_the_same_generation_does_not_clear_a_selection() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    select_and_release(&mut mounted);
    assert!(mounted.selection.range().is_some(), "sanity: selected");

    // Frames without a generation bump repaint the same content and keep the selection.
    mounted.frame(720.0);
    mounted.frame(740.0);

    assert!(mounted.selection.range().is_some());
    assert!(mounted.selection.chip_anchor().is_some());
}

#[test]
fn there_are_no_handles_no_double_tap_select_and_no_select_all() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.frame(0.0);

    let at = mounted.cell(0, 8);
    for _ in 0..2 {
        mounted.event(PointerPhase::Down, at);
        mounted.event(PointerPhase::Up, at);
    }
    assert_eq!(
        mounted.selection.range(),
        None,
        "double-tap-to-select is deliberately not implemented"
    );

    select_and_release(&mut mounted);
    let labels = node_labels(&mounted.root);
    let controls: Vec<_> = labels
        .iter()
        .filter(|label| label.as_str() != LABEL)
        .collect();
    assert_eq!(
        controls.len(),
        2,
        "only Copy and Dismiss selection: {labels:?}"
    );
    assert!(
        !labels.iter().any(|label| label.contains("Select all")),
        "{labels:?}"
    );
}
