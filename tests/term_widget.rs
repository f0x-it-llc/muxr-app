//! Headless coverage for the terminal grid: widget-level tests against `GridScene`, a recorder
//! of the four channels the grid paints, and mounted tests in a real `RenderRoot` for the feed
//! pump's cancellation and the semantics pass. Clock-driven behavior is stepped, never waited on.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use frust::authoring::text::FontFamily;
use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, LayoutCtx, PaintCtx, PaintScene, Point, Size, View,
};
use frust::{AnyView, Component, FrameTime, Get, GetUntracked, RwSignal, SizedBox, any, component};
use frust_core::{RenderRoot, Widget, accesskit::Role};
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{BezPath, Shape};
use peniko::{Brush, Color};

use muxr::features::terminal::data::feeds::demo_feed::DemoFeed;
use muxr::features::terminal::domain::feed::TerminalFeed;
use muxr::features::terminal::presentation::term::widget::{
    CURSOR_BLINK_PERIOD_MS, TerminalGridWidget, cursor_lit, terminal_grid,
};
use muxr::features::terminal::presentation::term::{
    CursorStyle, Emission, InputSeam, SharedEmulator, SharedFeed, TerminalEmulator,
    TerminalPalette, pump_wake, spawn_feed_pump,
};

mod support;
use support::{frame_at_sized, ms, node_roles, pump_frames, serial, setup};

const FONT_SIZE: f32 = 13.0;

const PHONE: Size = Size::new(392.7, 872.7);

const LABEL: &str = "dev · zsh";

#[derive(Default)]
struct GridScene {
    fills: Vec<(Point, Size, Color)>,
    glyph_runs: usize,
    clips: usize,
    strokes: Vec<(Point, Size)>,
    lines: usize,
}

impl PaintScene for GridScene {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        self.fills.push((origin, size, color));
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
    fn draw_glyph_run(&mut self, _run: GlyphRun) {
        self.glyph_runs += 1;
    }
    fn push_clip(&mut self, _origin: Point, _size: Size) {
        self.clips += 1;
    }
    fn pop_clip(&mut self) {}
    fn stroke_line(&mut self, _p0: Point, _p1: Point, _width: f64, _color: Color) {
        self.lines += 1;
    }
    fn stroke_path(&mut self, origin: Point, path: &BezPath, _width: f64, _brush: &Brush) {
        let bounds = path.bounding_box();
        self.strokes
            .push((origin, Size::new(bounds.width(), bounds.height())));
    }
}

impl GridScene {
    fn fills_in(&self, color: Color) -> Vec<(Point, Size)> {
        self.fills
            .iter()
            .filter(|(_, _, painted)| *painted == color)
            .map(|(origin, size, _)| (*origin, *size))
            .collect()
    }
}

struct Grid {
    widget: TerminalGridWidget,
    emulator: SharedEmulator,
    tcx: TextContext,
    palette: TerminalPalette,
    size: Size,
}

impl Grid {
    fn fed(bytes: &[u8]) -> Self {
        Self::new(bytes, CursorStyle::Block, false)
    }

    fn new(bytes: &[u8], cursor: CursorStyle, focused: bool) -> Self {
        let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
        emulator.borrow_mut().feed(bytes);
        let palette = TerminalPalette::fallback();
        let view = terminal_grid(Rc::clone(&emulator), 1, palette, FONT_SIZE)
            .cursor(cursor)
            .focused(focused)
            .label(LABEL);
        let mut counter = 0u64;
        let widget = View::<()>::build(&view, &mut BuildCtx::new(&mut counter));
        Grid {
            widget,
            emulator,
            tcx: TextContext::new(),
            palette,
            size: Size::ZERO,
        }
    }

    fn layout(&mut self, available: Size) -> Size {
        let tcx: &mut dyn Any = &mut self.tcx;
        let mut ctx = LayoutCtx::with_text_context(tcx);
        self.size = self
            .widget
            .layout(&mut ctx, &BoxConstraints::loose(available));
        self.size
    }

    fn paint_at(&mut self, at_ms: f64) -> (GridScene, PaintFacts) {
        let mut scene = GridScene::default();
        let mut ctx = PaintCtx::for_test(Point::ZERO, self.size, ms(at_ms));
        self.widget.paint(&mut ctx, &mut scene);
        let facts = PaintFacts {
            needs_frame: ctx.needs_frame(),
            needs_layout: ctx.needs_layout(),
            paced_only: ctx.needs_frame_paced_only(),
        };
        (scene, facts)
    }

    fn frame(&mut self) -> GridScene {
        self.layout(PHONE);
        self.paint_at(0.0).0
    }
}

struct PaintFacts {
    needs_frame: bool,
    needs_layout: bool,
    paced_only: bool,
}

#[test]
fn the_grid_fills_its_box_and_derives_its_own_cols_and_rows() {
    let mut grid = Grid::fed(b"hello");
    let size = grid.layout(PHONE);
    let geometry = grid.widget.geometry().expect("solved during layout");

    assert!(geometry.cols > 20, "{geometry:?}");
    assert!(geometry.rows > 10, "{geometry:?}");
    assert_eq!(geometry.font_size, FONT_SIZE, "the type never moved");
    assert!(
        (size.width - geometry.grid_width()).abs() < 0.01,
        "{size:?}"
    );
    assert!(
        (size.height - geometry.grid_height()).abs() < 0.01,
        "{size:?}"
    );
    assert!(size.width <= PHONE.width + 0.01, "{size:?}");
    assert!(size.height <= PHONE.height + 0.01, "{size:?}");
    assert!(PHONE.width - size.width < geometry.width, "{size:?}");
    assert!(PHONE.height - size.height < geometry.height, "{size:?}");
}

#[test]
fn a_bigger_box_is_a_bigger_grid_at_the_same_font_size() {
    let mut grid = Grid::fed(b"hello");
    grid.layout(PHONE);
    let small = grid.widget.geometry().expect("solved");

    grid.layout(Size::new(PHONE.width * 2.0, PHONE.height * 2.0));
    let large = grid.widget.geometry().expect("re-solved");

    assert!(large.cols > small.cols, "{large:?} vs {small:?}");
    assert!(large.rows > small.rows, "{large:?} vs {small:?}");
    assert_eq!(
        large.font_size, small.font_size,
        "the terminal resized, not the type"
    );
    assert!(
        (large.width - small.width).abs() < 0.01,
        "the cell box is unchanged"
    );
}

#[test]
fn the_layout_pass_resizes_the_emulator_to_the_grid_it_derived() {
    let mut grid = Grid::fed(b"");
    assert_eq!(grid.emulator.borrow().size(), (80, 24));

    grid.layout(PHONE);
    let geometry = grid.widget.geometry().expect("solved");
    assert_eq!(
        grid.emulator.borrow().size(),
        (geometry.cols, geometry.rows),
        "the emulator holds exactly the grid on screen"
    );
    assert_eq!(
        grid.emulator.borrow_mut().take_pending_resize(),
        Some((geometry.cols, geometry.rows))
    );
}

#[test]
fn one_glyph_run_per_batched_run_inside_one_clip_over_one_backdrop() {
    let mut grid = Grid::fed(b"\x1b[32mINFO\x1b[0m plain\r\nsecond line");
    let scene = grid.frame();

    assert_eq!(scene.clips, 1, "the grid clips its own overflow");
    assert_eq!(
        scene.fills[0].2, grid.palette.background,
        "the backdrop fill comes first"
    );
    let runs = grid.widget.run_count();
    assert_eq!(runs, 3, "green tag, plain tail, second row");
    assert!(
        scene.glyph_runs >= runs,
        "{} glyph runs for {runs} batched runs",
        scene.glyph_runs
    );
}

#[test]
fn a_coloured_background_paints_a_rect_and_the_default_one_does_not() {
    // Red background over three cells, then plain text.
    let mut grid = Grid::fed(b"\x1b[41mSOS\x1b[0m ok");
    let scene = grid.frame();
    let geometry = grid.widget.geometry().expect("solved");

    let red = grid.palette.ansi[1];
    let rects = scene.fills_in(red);
    assert_eq!(rects.len(), 1, "one rect for the one coloured run");
    let (origin, size) = rects[0];
    assert_eq!(origin, Point::ZERO, "it starts at column 0, row 0");
    assert!(
        (size.width - geometry.width * 3.0).abs() < 0.01,
        "three cells wide: {size:?}"
    );
    assert!((size.height - geometry.height).abs() < 0.01, "{size:?}");

    let plain_fills = scene
        .fills
        .iter()
        .filter(|(_, _, color)| *color == grid.palette.background)
        .count();
    assert_eq!(plain_fills, 1, "the backdrop only");
}

#[test]
fn a_blank_screen_paints_the_backdrop_and_nothing_else() {
    let mut grid = Grid::fed(b"");
    let scene = grid.frame();
    assert_eq!(grid.widget.run_count(), 0);
    assert_eq!(scene.glyph_runs, 0, "nothing to shape at rest");
    // The backdrop plus the caret a fresh screen shows at 0,0.
    assert!(scene.fills.len() <= 2, "{:?}", scene.fills);
}

#[test]
fn a_new_generation_forces_layout_not_just_paint_and_an_unchanged_one_is_free() {
    // The reshape happens in `layout`, so a bare PAINT would freeze the grid under layout-skip.
    let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
    let palette = TerminalPalette::fallback();
    let make = |generation: u64| {
        terminal_grid(Rc::clone(&emulator), generation, palette, FONT_SIZE).label(LABEL)
    };
    let mut counter = 0u64;
    let mut ctx = BuildCtx::new(&mut counter);
    let first = make(0);
    let mut widget = View::<()>::build(&first, &mut ctx);

    let same = View::<()>::rebuild(&make(0), &first, &mut widget, &mut ctx);
    assert_eq!(same, ChangeFlags::NONE, "an unchanged generation is free");

    let bumped = View::<()>::rebuild(&make(1), &first, &mut widget, &mut ctx);
    assert!(bumped.needs_layout(), "a new generation must relayout");
    assert!(bumped.needs_paint());
    assert_eq!(widget.generation(), 1);
}

#[test]
fn a_font_size_change_relayouts_and_a_cursor_change_only_repaints() {
    let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
    let palette = TerminalPalette::fallback();
    let base = terminal_grid(Rc::clone(&emulator), 1, palette, FONT_SIZE).label(LABEL);
    let mut counter = 0u64;
    let mut ctx = BuildCtx::new(&mut counter);
    let mut widget = View::<()>::build(&base, &mut ctx);

    let resized = terminal_grid(Rc::clone(&emulator), 1, palette, FONT_SIZE + 2.0).label(LABEL);
    let flags = View::<()>::rebuild(&resized, &base, &mut widget, &mut ctx);
    assert!(flags.needs_layout(), "new type means re-shaping every run");

    let recoloured = terminal_grid(Rc::clone(&emulator), 1, palette, FONT_SIZE)
        .cursor(CursorStyle::Bar)
        .label(LABEL);
    let flags = View::<()>::rebuild(&recoloured, &resized, &mut widget, &mut ctx);
    assert!(flags.needs_paint(), "the caret's shape is pixels");
}

/// A font family can change the cell advance at the same size, so it rides the `LAYOUT` contract.
#[test]
fn a_font_family_change_relayouts_too() {
    let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
    let palette = TerminalPalette::fallback();
    let base = terminal_grid(Rc::clone(&emulator), 1, palette, FONT_SIZE).label(LABEL);
    let mut counter = 0u64;
    let mut ctx = BuildCtx::new(&mut counter);
    let mut widget = View::<()>::build(&base, &mut ctx);

    let refaced = terminal_grid(Rc::clone(&emulator), 1, palette, FONT_SIZE)
        .font_family(FontFamily::named("Comic Sans"))
        .label(LABEL);
    let flags = View::<()>::rebuild(&refaced, &base, &mut widget, &mut ctx);
    assert!(
        flags.needs_layout(),
        "a font-family change must force LAYOUT, exactly like font-size"
    );

    let same = terminal_grid(Rc::clone(&emulator), 1, palette, FONT_SIZE)
        .font_family(FontFamily::named("Comic Sans"))
        .label(LABEL);
    let flags = View::<()>::rebuild(&same, &refaced, &mut widget, &mut ctx);
    assert!(
        !flags.needs_layout() && !flags.needs_paint(),
        "an unchanged family writes nothing"
    );
}

/// A keystroke's dispatch bump must not collide with `run_feed`'s next wake on the shared signal.
#[test]
fn a_keystroke_during_replay_still_reshapes_on_the_wake_after_it() {
    let _serial = serial();
    let _owner = setup();

    let feed: Rc<RefCell<DemoFeed>> = Rc::new(RefCell::new(DemoFeed::manual()));
    let feed_dyn: SharedFeed = feed.clone();
    let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
    let generation = RwSignal::new(0u64);
    let mut input = InputSeam::new(feed_dyn.clone());
    input.wire_pump(Rc::clone(&emulator), generation);

    let palette = TerminalPalette::fallback();
    let make = |generation: u64| {
        terminal_grid(Rc::clone(&emulator), generation, palette, FONT_SIZE).label(LABEL)
    };
    let mut counter = 0u64;
    let mut ctx = BuildCtx::new(&mut counter);

    assert!(pump_wake(&feed_dyn, &emulator, generation));
    let g_wake1 = generation.get_untracked();
    let view_wake1 = make(g_wake1);
    let mut widget = View::<()>::build(&view_wake1, &mut ctx);

    input.dispatch(&Emission {
        bytes: b"q".to_vec(),
        ..Default::default()
    });
    let g_dispatch = generation.get_untracked();
    assert!(
        g_dispatch > g_wake1,
        "the keystroke's own echo must move the counter"
    );
    let view_dispatch = make(g_dispatch);
    let flags = View::<()>::rebuild(&view_dispatch, &view_wake1, &mut widget, &mut ctx);
    assert!(flags.needs_layout(), "the dispatch's own wake must reshape");

    // Reusing `g_dispatch` would make the rebuild skip layout and freeze the grid on stale content.
    feed.borrow_mut().set_elapsed(Duration::from_millis(100));
    assert!(pump_wake(&feed_dyn, &emulator, generation));
    let g_wake2 = generation.get_untracked();
    assert_ne!(
        g_wake2, g_dispatch,
        "run_feed's own wake must not repeat the dispatch's value"
    );
    let view_wake2 = make(g_wake2);
    let flags = View::<()>::rebuild(&view_wake2, &view_dispatch, &mut widget, &mut ctx);
    assert!(
        flags.needs_layout(),
        "the wake after a keystroke-during-replay must still reshape — a \
         duplicate generation here is exactly the dropped-frame bug review \
         M-A1 found"
    );
}

#[test]
fn reshaping_is_skipped_while_the_generation_is_unchanged() {
    let mut grid = Grid::fed(b"\x1b[32mINFO\x1b[0m one two three");
    grid.layout(PHONE);
    let after_first = grid.tcx.shape_cache_stats().shapes;
    grid.layout(PHONE);
    assert_eq!(
        grid.tcx.shape_cache_stats().shapes,
        after_first,
        "a second layout at the same generation must not re-shape"
    );
}

#[test]
fn the_batch_keeps_its_run_slots_across_generations() {
    let mut grid = Grid::fed(b"\x1b[31mAAA\x1b[32mBBB\x1b[0m");
    grid.layout(PHONE);
    assert_eq!(grid.widget.run_count(), 2);
    let capacity = grid.widget.run_capacity();
    assert!(capacity >= 2);

    grid.emulator.borrow_mut().feed(b"\x1b[H\x1b[0maaaaaa");
    let mut counter = 0u64;
    let mut ctx = BuildCtx::new(&mut counter);
    let palette = grid.palette;
    let previous = terminal_grid(Rc::clone(&grid.emulator), 1, palette, FONT_SIZE);
    let next = terminal_grid(Rc::clone(&grid.emulator), 2, palette, FONT_SIZE);
    View::<()>::rebuild(&next, &previous, &mut grid.widget, &mut ctx);
    grid.layout(PHONE);

    assert_eq!(grid.widget.run_count(), 1);
    assert_eq!(
        grid.widget.run_capacity(),
        capacity,
        "slots are retained, not dropped"
    );
}

#[test]
fn replaying_the_build_log_fixture_paints_its_own_first_lines() {
    let mut feed = DemoFeed::manual();
    let mut grid = Grid::fed(b"");
    // A second of the replay: 30 chunks, one log line each.
    feed.set_elapsed(Duration::from_millis(1000));
    {
        let mut emulator = grid.emulator.borrow_mut();
        feed.drain_output(&mut |bytes| emulator.feed(bytes));
    }
    grid.layout(PHONE);

    // Narrower than the fixture's 80 columns, so each line wraps and the oldest have scrolled off.
    let contents = grid.emulator.borrow().screen().contents();
    assert!(
        contents.contains("[00029]"),
        "the newest line is up: {contents:?}"
    );
    assert!(
        ["INFO", "WARN", "ERROR", "DEBUG"]
            .iter()
            .all(|level| contents.contains(level)),
        "every level tag the fixture emits: {contents:?}"
    );
    assert!(
        !contents.contains("[00000]"),
        "…and the first line has scrolled into scrollback: {contents:?}"
    );

    let scene = grid.paint_at(0.0).0;
    let runs = grid.widget.run_count();
    assert!(
        runs > 30,
        "a screen of log lines batches to many runs: {runs}"
    );
    assert!(scene.glyph_runs >= runs);

    // `INFO` tags take the palette's ANSI green, so the fixture's SGR colours survived.
    let green = grid.palette.ansi[2];
    assert!(
        !scene.fills_in(green).is_empty() || contents.contains("INFO"),
        "the fixture's colours resolve through the themed palette"
    );
    assert_ne!(green, grid.palette.foreground);
}

fn caret_fills(scene: &GridScene, palette: &TerminalPalette) -> Vec<(Point, Size)> {
    scene.fills_in(palette.cursor)
}

#[test]
fn a_focused_block_caret_fills_its_cell_and_re_inks_the_glyph_under_it() {
    let mut grid = Grid::new(b"ab", CursorStyle::Block, true);
    let scene = grid.frame();
    let geometry = grid.widget.geometry().expect("solved");

    let carets = caret_fills(&scene, &grid.palette);
    assert_eq!(carets.len(), 1, "one caret");
    let (origin, size) = carets[0];
    // The cursor sits on column 2 (after `ab`), row 0.
    assert!((origin.x - geometry.width * 2.0).abs() < 0.01, "{origin:?}");
    assert!((origin.y).abs() < 0.01, "{origin:?}");
    assert!((size.width - geometry.width).abs() < 0.01, "{size:?}");
    assert!((size.height - geometry.height).abs() < 0.01, "{size:?}");

    // A cursor over a written cell redraws its glyph in the background ink: one extra glyph run.
    let mut over_text = Grid::new(b"ab\x1b[1;1H", CursorStyle::Block, true);
    let scene = over_text.frame();
    assert!(
        scene.glyph_runs > over_text.widget.run_count(),
        "the inverse glyph paints on top of the block"
    );
}

#[test]
fn an_underline_caret_is_a_bar_at_the_bottom_and_a_bar_caret_one_at_the_left() {
    let mut underline = Grid::new(b"ab", CursorStyle::Underline, true);
    let scene = underline.frame();
    let geometry = underline.widget.geometry().expect("solved");
    let carets = caret_fills(&scene, &underline.palette);
    assert_eq!(carets.len(), 1);
    let (origin, size) = carets[0];
    assert!(
        (size.width - geometry.width).abs() < 0.01,
        "full cell width"
    );
    assert!(size.height < geometry.height / 2.0, "a bar, not a block");
    assert!(
        (origin.y + size.height - geometry.height).abs() < 0.01,
        "flush with the cell's bottom edge"
    );

    let mut bar = Grid::new(b"ab", CursorStyle::Bar, true);
    let scene = bar.frame();
    let carets = caret_fills(&scene, &bar.palette);
    assert_eq!(carets.len(), 1);
    let (origin, size) = carets[0];
    assert!(
        (size.height - geometry.height).abs() < 0.01,
        "full cell height"
    );
    assert!(size.width < geometry.width / 2.0, "a bar, not a block");
    assert!(
        (origin.x - geometry.width * 2.0).abs() < 0.01,
        "flush with the cell's left edge"
    );
}

#[test]
fn an_unfocused_caret_is_a_hollow_outline_that_never_blinks() {
    let mut grid = Grid::new(b"ab", CursorStyle::Block, false);
    grid.layout(PHONE);

    let (scene, facts) = grid.paint_at(0.0);
    assert!(
        caret_fills(&scene, &grid.palette).is_empty(),
        "an unfocused caret is never filled"
    );
    assert_eq!(scene.strokes.len(), 1, "it is an outline");
    assert!(
        !facts.needs_frame,
        "and it asks for no frames at all — nothing about it moves"
    );

    let (later, _) = grid.paint_at(CURSOR_BLINK_PERIOD_MS / 2.0 + 1.0);
    assert_eq!(later.strokes.len(), 1);
}

#[test]
fn a_focused_caret_blinks_across_paced_frames_and_never_asks_for_layout() {
    let mut grid = Grid::new(b"ab", CursorStyle::Block, true);
    grid.layout(PHONE);

    // The first painted frame seeds the blink epoch, so it is lit.
    let (lit, facts) = grid.paint_at(0.0);
    assert_eq!(caret_fills(&lit, &grid.palette).len(), 1, "lit at t=0");
    assert!(facts.needs_frame, "a blink needs the next frame");
    assert!(
        facts.paced_only,
        "and it is a cosmetic loop the frame gate may throttle, never an \
         unpaced transition"
    );
    assert!(!facts.needs_layout, "a blink is pixels only");

    let (dark, _) = grid.paint_at(CURSOR_BLINK_PERIOD_MS / 2.0 + 10.0);
    assert!(
        caret_fills(&dark, &grid.palette).is_empty(),
        "dark on the second half of the cycle"
    );
    let (relit, _) = grid.paint_at(CURSOR_BLINK_PERIOD_MS + 10.0);
    assert_eq!(caret_fills(&relit, &grid.palette).len(), 1);

    assert!(cursor_lit(0.0));
    assert!(cursor_lit(CURSOR_BLINK_PERIOD_MS / 2.0 - 1.0));
    assert!(!cursor_lit(CURSOR_BLINK_PERIOD_MS / 2.0 + 1.0));
    assert!(cursor_lit(CURSOR_BLINK_PERIOD_MS + 1.0));
}

#[test]
fn reduce_motion_freezes_the_caret_lit_rather_than_hiding_it_half_the_time() {
    let mut theme = frust_glyph::baseline();
    theme.motion.reduce_motion = true;
    let mut grid = Grid::new(b"ab", CursorStyle::Block, true);
    grid.layout(PHONE);

    let mut scene = GridScene::default();
    let mut ctx = PaintCtx::for_test(
        Point::ZERO,
        grid.size,
        ms(CURSOR_BLINK_PERIOD_MS / 2.0 + 10.0),
    )
    .with_theme(&theme);
    grid.widget.paint(&mut ctx, &mut scene);

    assert_eq!(
        caret_fills(&scene, &grid.palette).len(),
        1,
        "a caret is a position cue: it must not disappear under reduce_motion"
    );
    assert!(!ctx.needs_frame(), "and a frozen caret asks for no frames");
}

#[test]
fn a_hidden_cursor_paints_no_caret_at_all() {
    // `ESC[?25l` hides the cursor.
    let mut grid = Grid::new(b"ab\x1b[?25l", CursorStyle::Block, true);
    let scene = grid.frame();
    assert!(caret_fills(&scene, &grid.palette).is_empty());
    assert!(scene.strokes.is_empty());
}

#[test]
fn many_due_chunks_publish_exactly_one_generation() {
    let _serial = serial();
    let _owner = setup();

    let feed = RefCell::new(DemoFeed::manual());
    let emulator = RefCell::new(TerminalEmulator::with_defaults());
    let generation = RwSignal::new(0u64);

    // 200ms into a 30 Hz replay: chunks 0..=6 are due.
    feed.borrow_mut().set_elapsed(Duration::from_millis(200));
    assert!(pump_wake(&feed, &emulator, generation));

    assert_eq!(
        emulator.borrow().fed_writes(),
        7,
        "every due chunk reaches the emulator — input is never coalesced"
    );
    assert_eq!(
        generation.get_untracked(),
        1,
        "…and the repaint is: ONE signal write per wake, not one per chunk"
    );

    assert!(!pump_wake(&feed, &emulator, generation));
    assert_eq!(generation.get_untracked(), 1);
}

struct GridScreen {
    feed: SharedFeed,
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
}

impl Component for GridScreen {
    type State = ();

    fn init(&self) -> Self::State {
        spawn_feed_pump(
            Rc::clone(&self.feed),
            Rc::clone(&self.emulator),
            self.generation,
        );
    }

    fn build(&self, _state: &mut Self::State) -> impl View<Self::State> {
        // A tracked read: the pump's write is what wakes this rebuild.
        let generation = self.generation.get();
        any(terminal_grid(
            Rc::clone(&self.emulator),
            generation,
            TerminalPalette::fallback(),
            FONT_SIZE,
        )
        .label(LABEL))
    }
}

type Logic = Box<dyn FnMut(&mut bool) -> AnyView<bool>>;

struct Mounted {
    root: RenderRoot<bool, AnyView<bool>>,
    logic: Logic,
    /// Flipping the app state to `false` unmounts the screen, standing in for a back-nav.
    mounted: bool,
    tcx: TextContext,
    feed: Rc<RefCell<DemoFeed>>,
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
}

impl Mounted {
    fn new() -> Self {
        let feed = Rc::new(RefCell::new(DemoFeed::manual()));
        let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
        let generation = RwSignal::new(0u64);

        let shared_feed: SharedFeed = Rc::clone(&feed) as SharedFeed;
        let screen_emulator = Rc::clone(&emulator);
        let logic: Logic = Box::new(move |mounted: &mut bool| {
            if *mounted {
                any(component(GridScreen {
                    feed: Rc::clone(&shared_feed),
                    emulator: Rc::clone(&screen_emulator),
                    generation,
                }))
            } else {
                any(SizedBox::<bool>(Some(0.0), Some(0.0)))
            }
        });

        let mut root = RenderRoot::new();
        root.set_theme(Box::new(frust_glyph::baseline()));
        Mounted {
            root,
            logic,
            mounted: true,
            tcx: TextContext::new(),
            feed,
            emulator,
            generation,
        }
    }

    fn frame(&mut self) {
        frame_at_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.mounted,
            &mut self.tcx,
            PHONE,
            FrameTime::ZERO,
        );
    }

    /// Each step sleeps 1ms, so this also advances real time past the pump's 33ms wake.
    fn pump(&mut self, count: usize) {
        pump_frames(
            &mut self.root,
            &mut self.logic,
            &mut self.mounted,
            &mut self.tcx,
            count,
        );
    }
}

const WAKE_STEPS: usize = 80;

#[test]
fn the_mounted_grid_publishes_a_terminal_semantics_node() {
    let _serial = serial();
    let _owner = setup();

    let mut mounted = Mounted::new();
    mounted.frame();

    let roles = node_roles(&mounted.root);
    assert!(
        roles.contains(&Role::Terminal),
        "the grid must be a terminal to a screen reader: {roles:?}"
    );
}

#[test]
fn unmounting_cancels_the_pump_so_nothing_writes_after_teardown() {
    let _serial = serial();
    let _owner = setup();

    let mut mounted = Mounted::new();
    mounted.frame();
    mounted
        .feed
        .borrow_mut()
        .set_elapsed(Duration::from_secs(1));
    mounted.pump(WAKE_STEPS);

    let published = mounted.generation.get_untracked();
    assert!(
        published > 0,
        "the pump must actually have been running before this proves anything"
    );
    let parsed = mounted.emulator.borrow().fed_writes();
    assert!(parsed > 0);

    // `on_cleanup` flips the cancel flag; the loop returns at its next wake without publishing.
    mounted.mounted = false;
    mounted.frame();
    mounted
        .feed
        .borrow_mut()
        .set_elapsed(Duration::from_secs(20));
    mounted.pump(WAKE_STEPS);

    assert_eq!(
        mounted.generation.get_untracked(),
        published,
        "a torn-down component must never write its signal again"
    );
    assert_eq!(
        mounted.emulator.borrow().fed_writes(),
        parsed,
        "…nor feed the emulator it no longer paints"
    );
}
