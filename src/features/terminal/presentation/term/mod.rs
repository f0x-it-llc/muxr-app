//! The live terminal grid: a VT emulator fed off a `TerminalFeed`, its pump, and the input and
//! selection seams. Repaints coalesce app-side at [`REPAINT_HZ`] because the mobile frame gate
//! paces only animation frames; an echo drains synchronously, so keep a host's `build` cheap.

pub mod batch;
pub mod chip;
pub mod geometry;
pub mod input;
pub mod pinch;
pub mod selection;
pub mod style;
pub mod widget;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use frust::authoring::{
    EditCommand, EditingState, ImeContentType, ImeEvent, ImeState, InputEvent, Point, Rect,
};
use frust::{
    AnyView, GestureDetector, GetUntracked, RwSignal, ScaleEvent, ScalePhase, Set, StackView, any,
    pinch_detector, stack,
};
use frust::{on_cleanup, spawn_blocking, spawn_local};

use crate::features::terminal::domain::feed::TerminalFeed;

use geometry::CellGeometry;
use selection::{GridPoint, GridRange};

pub use chip::{CopyChipView, CopyChipWidget, CopySink, copy_chip};
pub use geometry::{FALLBACK_COLS, FALLBACK_ROWS};
pub use input::{BarKey, Emission, KeystrokeSource, ModifierLatch};
pub use pinch::{PinchIntent, PinchIntentTracker, PinchSink};
pub use style::{TerminalPalette, terminal_palette};
pub use widget::{TerminalGridView, TerminalGridWidget, terminal_grid};

pub const REPAINT_HZ: f64 = 30.0;

pub const REPAINT_INTERVAL: Duration = Duration::from_nanos((1_000_000_000.0 / REPAINT_HZ) as u64);

pub const DEFAULT_SCROLLBACK: usize = 4000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorStyle {
    /// `ESC[2 q`.
    #[default]
    Block,
    /// `ESC[4 q`.
    Underline,
    /// `ESC[6 q`.
    Bar,
}

pub type SharedEmulator = Rc<RefCell<TerminalEmulator>>;

pub type SharedFeed = Rc<RefCell<dyn TerminalFeed>>;

pub type SharedKeystrokeSource = Rc<RefCell<KeystrokeSource>>;

/// Shared by the grid widget and the key bar, so both drive one keystroke source and latch.
#[derive(Clone)]
pub struct InputSeam {
    pub source: SharedKeystrokeSource,
    pub feed: SharedFeed,
    pub revision: RwSignal<u64>,
    /// Written from `build`, read from the event and paint passes.
    read_only: Rc<Cell<bool>>,
    /// Compared across the grid's blur edge: unchanged means a real blur, not a key-cap hand-off.
    focus_claim: Rc<Cell<u64>>,
    pump: Option<PumpHandle>,
}

#[derive(Clone)]
struct PumpHandle {
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
}

impl InputSeam {
    pub fn new(feed: SharedFeed) -> Self {
        Self {
            source: Rc::new(RefCell::new(KeystrokeSource::new())),
            feed,
            revision: RwSignal::new(0),
            read_only: Rc::new(Cell::new(false)),
            focus_claim: Rc::new(Cell::new(0)),
            pump: None,
        }
    }

    pub fn wire_pump(&mut self, emulator: SharedEmulator, generation: RwSignal<u64>) {
        self.pump = Some(PumpHandle {
            emulator,
            generation,
        });
    }

    /// The one place bytes leave the app, so the read-only gate is final here.
    pub fn dispatch(&self, emission: &Emission) {
        if !emission.bytes.is_empty() && !self.read_only() {
            self.feed.borrow_mut().send_input(&emission.bytes);
            if let Some(pump) = &self.pump {
                pump_wake(&self.feed, &pump.emulator, pump.generation);
            }
        }
        let revision = self.source.borrow().revision();
        if revision != self.revision.get_untracked() {
            self.revision.set(revision);
        }
    }

    pub fn read_only(&self) -> bool {
        self.read_only.get()
    }

    /// Turning read-only on scrubs the buffered keystrokes.
    pub fn set_read_only(&self, read_only: bool) {
        if self.read_only.replace(read_only) != read_only && read_only {
            self.scrub();
        }
    }

    /// Never mirrors into `revision`: it runs from paint, where a signal write would schedule a
    /// rebuild.
    pub fn scrub(&self) {
        self.source.borrow_mut().scrub();
    }

    pub fn claim_focus(&self) -> u64 {
        let claim = self.focus_claim.get().wrapping_add(1);
        self.focus_claim.set(claim);
        claim
    }

    pub fn focus_claim(&self) -> u64 {
        self.focus_claim.get()
    }

    pub fn press_bar_key(&self, key: BarKey) -> Option<Emission> {
        if self.read_only() {
            return None;
        }
        let emission = self.source.borrow_mut().on_bar_key(key);
        self.dispatch(&emission);
        Some(emission)
    }

    pub fn latch(&self) -> ModifierLatch {
        self.source.borrow().latch()
    }

    pub fn published(&self) -> EditingState {
        self.source.borrow().published()
    }

    /// `ImeContentType::Terminal` disables autocorrect and smart quotes so a typed `"` arrives as
    /// `0x22`; spelled once here so no two widgets disagree.
    pub fn ime_state(&self, caret: Option<Rect>) -> ImeState {
        ImeState {
            active: true,
            editing: self.published(),
            caret,
            content_type: ImeContentType::Terminal,
            suppress_soft_keyboard: self.read_only(),
        }
    }

    /// Any focus-requesting widget may call this, since frust blurs on a `Down` that claims no
    /// focus. Paste is input; `Copy`/`Cut`/`SelectAll` are reads the grid answers itself.
    pub fn absorb(&self, event: &InputEvent) -> Option<Emission> {
        if self.read_only() {
            return None;
        }
        let emission = {
            let mut source = self.source.borrow_mut();
            match event {
                InputEvent::Key(key) => source.on_key(&key.key, key.modifiers),
                InputEvent::Ime(ImeEvent::ApplyEditingState(state)) => source.on_snapshot(state),
                InputEvent::Ime(ImeEvent::Commit(text)) => source.on_commit(text),
                InputEvent::Ime(ImeEvent::Compose { text, .. }) => source.on_compose(text),
                InputEvent::Ime(ImeEvent::Disabled) => source.on_composition_end(),
                InputEvent::Ime(ImeEvent::Enabled) => Emission::default(),
                InputEvent::EditCommand(EditCommand::Paste(text)) => source.on_commit(text),
                _ => return None,
            }
        };
        self.dispatch(&emission);
        Some(emission)
    }
}

pub type SharedSelection = Rc<RefCell<SelectionState>>;

#[derive(Default)]
pub struct SelectionState {
    metrics: Option<CellGeometry>,
    /// Grid-local logical px.
    pointer: Point,
    pressed: bool,
    anchor: Option<GridPoint>,
    dragging: bool,
    range: Option<GridRange>,
    /// `Some` is "the chip is up".
    chip: Option<Point>,
    haptics: u32,
    last_generation: Option<u64>,
}

/// `Rc<RefCell<_>>`, not a signal: a drag re-resolves on every `Move`, and the chip sees the
/// new state on the same paint pass with no rebuild.
#[derive(Clone)]
pub struct SelectionSeam {
    state: SharedSelection,
    emulator: SharedEmulator,
    /// Kept here because `build` re-runs mid-gesture and a fresh tracker would lose its scale.
    pinch: Rc<RefCell<PinchIntentTracker>>,
    pinching: Rc<Cell<bool>>,
}

impl SelectionSeam {
    pub fn new(emulator: SharedEmulator) -> Self {
        Self {
            state: Rc::new(RefCell::new(SelectionState::default())),
            emulator,
            pinch: Rc::new(RefCell::new(PinchIntentTracker::new())),
            pinching: Rc::new(Cell::new(false)),
        }
    }

    /// Clamps a held selection the instant a shrink is observed; a stale range would underflow
    /// `contents_between` on Copy.
    pub fn set_metrics(&self, metrics: CellGeometry) {
        let mut state = self.state.borrow_mut();
        state.metrics = Some(metrics);
        if let Some(range) = state.range {
            state.range = Some(range.clamp_to(metrics.cols, metrics.rows));
        }
    }

    pub fn cell_at(&self, at: Point) -> Option<GridPoint> {
        self.state
            .borrow()
            .metrics
            .map(|metrics| selection::cell_at(at, metrics))
    }

    pub fn track(&self, at: Point, pressed: bool) {
        let mut state = self.state.borrow_mut();
        state.pointer = at;
        state.pressed = pressed;
    }

    /// The haptic fires before the word resolves, even over a separator. A fire with no finger
    /// down goes straight to the released, chip-up state.
    pub fn begin(&self) {
        if self.pinching.get() {
            return;
        }
        frust::perform_haptic(frust::HapticFeedback::Selection);
        let mut state = self.state.borrow_mut();
        state.haptics += 1;
        let Some(metrics) = state.metrics else {
            return;
        };
        let anchor = selection::cell_at(state.pointer, metrics);
        state.anchor = Some(anchor);
        state.range = selection::word_at(self.emulator.borrow().screen(), anchor);
        if state.pressed {
            state.dragging = true;
            state.chip = None;
        } else {
            state.dragging = false;
            state.chip = state.range.map(|_| state.pointer);
        }
    }

    pub(crate) fn pinch_began(&self) {
        self.pinching.set(true);
        if self.state.borrow().dragging {
            self.clear();
        }
    }

    /// Called only on the grid's fresh first-finger `Down`: a cancelled pinch delivers no `End`,
    /// and the first finger's gesture returns to the grid late.
    pub(crate) fn pinch_ended(&self) {
        self.pinching.set(false);
    }

    pub fn extend(&self) {
        let mut state = self.state.borrow_mut();
        let (Some(metrics), Some(anchor)) = (state.metrics, state.anchor) else {
            return;
        };
        let extent = selection::cell_at(state.pointer, metrics);
        state.range = selection::resolve(self.emulator.borrow().screen(), anchor, extent);
    }

    pub fn finish(&self) {
        let mut state = self.state.borrow_mut();
        state.dragging = false;
        state.chip = state.range.map(|_| state.pointer);
    }

    /// The selection survives a stolen gesture; the next tap clears it.
    pub fn cancel_drag(&self) {
        self.state.borrow_mut().dragging = false;
    }

    pub fn is_active(&self) -> bool {
        let state = self.state.borrow();
        state.range.is_some() || state.chip.is_some()
    }

    pub fn clear(&self) {
        let mut state = self.state.borrow_mut();
        state.anchor = None;
        state.dragging = false;
        state.range = None;
        state.chip = None;
    }

    /// One call so the grid checks before claiming focus: dismissing must not raise the keyboard.
    pub fn clear_if_active(&self) -> bool {
        let active = self.is_active();
        if active {
            self.clear();
        }
        active
    }

    /// A same-generation rebuild is not new output and clears nothing.
    pub fn observe_generation(&self, generation: u64) {
        let should_clear = {
            let mut state = self.state.borrow_mut();
            let moved = state.last_generation.is_some_and(|last| last != generation);
            state.last_generation = Some(generation);
            moved && (state.range.is_some() || state.chip.is_some())
        };
        if should_clear {
            self.clear();
        }
    }

    pub fn range(&self) -> Option<GridRange> {
        self.state.borrow().range
    }

    pub fn dragging(&self) -> bool {
        self.state.borrow().dragging
    }

    pub fn pointer(&self) -> Point {
        self.state.borrow().pointer
    }

    pub fn chip_anchor(&self) -> Option<Point> {
        self.state.borrow().chip
    }

    pub fn text(&self) -> String {
        let Some(range) = self.range() else {
            return String::new();
        };
        selection::selected_text(self.emulator.borrow().screen(), range)
    }

    pub fn haptic_count(&self) -> u32 {
        self.state.borrow().haptics
    }

    pub fn metrics(&self) -> Option<CellGeometry> {
        self.state.borrow().metrics
    }
}

/// Zero-based `(col, row)`; answers synchronously whether to raise the keyboard, since the IME
/// surface publishes in the same event pass. `false` leaves the keyboard as-is.
pub type ContentTapSink = Rc<dyn Fn(u32, u32) -> bool>;

/// `(up, col, row)`, one call per line step: the vocabulary `muxrd`'s scroll RPC speaks.
pub type WheelSink = Rc<dyn Fn(bool, u32, u32)>;

/// The Copy chip stacks above the gesture detector so a press on it never reaches the grid;
/// only `on_long_press` is wired, as the grid discriminates taps and drag-scroll itself.
pub fn terminal_surface<State: 'static>(
    seam: SelectionSeam,
    grid: TerminalGridView,
    chip: CopyChipView,
) -> StackView<State> {
    let armed = seam.clone();
    stack()
        .child(
            GestureDetector::<State, _>(grid.selection(seam))
                .on_long_press(move |_state: &mut State| armed.begin()),
        )
        .child(chip)
}

/// A recognised pinch's `Begin` steals the grid's gesture with a synthesized `Cancel`.
pub fn terminal_surface_with_pinch<State: 'static>(
    seam: SelectionSeam,
    grid: TerminalGridView,
    chip: CopyChipView,
    pinch: Option<PinchSink>,
) -> AnyView<State> {
    let tracker = Rc::clone(&seam.pinch);
    let guard = seam.clone();
    let surface = terminal_surface::<State>(seam, grid, chip);
    let Some(sink) = pinch else {
        return any(surface);
    };
    any(
        pinch_detector(surface).on_scale(move |_state: &mut State, event: ScaleEvent| {
            if matches!(event.phase, ScalePhase::Begin) {
                guard.pinch_began();
            }
            let intent = tracker.borrow_mut().feed(event.phase, event.scale_delta);
            if let Some(intent) = intent {
                sink(intent, event.focal);
            }
        }),
    )
}

/// The only place naming `vt100`'s `(rows, cols)` order; everything above speaks `(cols, rows)`.
pub struct TerminalEmulator {
    parser: vt100::Parser,
    /// Drained by whoever owns the feed, which turns it into `SIGWINCH`.
    pending_resize: Option<(u16, u16)>,
    fed_bytes: usize,
    fed_writes: usize,
}

impl TerminalEmulator {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(1), cols.max(1), scrollback),
            pending_resize: None,
            fed_bytes: 0,
            fed_writes: 0,
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(FALLBACK_COLS, FALLBACK_ROWS, DEFAULT_SCROLLBACK)
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.fed_bytes += bytes.len();
        self.fed_writes += 1;
    }

    pub fn size(&self) -> (u16, u16) {
        let (rows, cols) = self.parser.screen().size();
        (cols, rows)
    }

    /// Runs inside `layout`: an unchanged size does nothing.
    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.size() == (cols, rows) {
            return false;
        }
        self.parser.screen_mut().set_size(rows, cols);
        self.pending_resize = Some((cols, rows));
        true
    }

    pub fn take_pending_resize(&mut self) -> Option<(u16, u16)> {
        self.pending_resize.take()
    }

    pub fn fed_bytes(&self) -> usize {
        self.fed_bytes
    }

    pub fn fed_writes(&self) -> usize {
        self.fed_writes
    }
}

/// Owns "next" (one generation per wake that drained anything), so `run_feed` and
/// [`InputSeam::dispatch`] writing the same signal never publish a duplicate.
pub fn pump_wake(
    feed: &RefCell<dyn TerminalFeed>,
    emulator: &RefCell<TerminalEmulator>,
    generation: RwSignal<u64>,
) -> bool {
    let writes = {
        let mut emulator = emulator.borrow_mut();
        feed.borrow_mut()
            .drain_output(&mut |bytes| emulator.feed(bytes))
    };
    if writes == 0 {
        return false;
    }
    generation.set(generation.get_untracked() + 1);
    true
}

/// No `RefCell` borrow is held across an `await`.
pub async fn run_feed(
    feed: SharedFeed,
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
    cancelled: Arc<AtomicBool>,
) {
    run_feed_watched(feed, emulator, generation, cancelled, || {}).await;
}

/// `on_tick` runs once per wake with no borrow held (it may replace the feed), final wake included.
pub async fn run_feed_watched(
    feed: SharedFeed,
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
    cancelled: Arc<AtomicBool>,
    on_tick: impl Fn(),
) {
    let start = Instant::now();
    let mut tick = 0u32;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        pump_wake(&feed, &emulator, generation);
        let open = feed.borrow().is_open();
        on_tick();
        if !open {
            return;
        }
        // An absolute schedule, so a slow frame cannot drift it.
        tick += 1;
        let deadline = REPAINT_INTERVAL * tick;
        if let Some(nap) = deadline.checked_sub(start.elapsed()) {
            let _ = spawn_blocking(move || std::thread::sleep(nap)).await;
        }
    }
}

/// Cancelled on teardown via `on_cleanup`, never `Drop`; the flag is checked before the drain.
pub fn spawn_feed_pump(feed: SharedFeed, emulator: SharedEmulator, generation: RwSignal<u64>) {
    spawn_feed_pump_watched(feed, emulator, generation, || {});
}

pub fn spawn_feed_pump_watched(
    feed: SharedFeed,
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
    on_tick: impl Fn() + 'static,
) {
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = Arc::clone(&cancelled);
        on_cleanup(move || cancelled.store(true, Ordering::Relaxed));
    }
    spawn_local(run_feed_watched(
        feed, emulator, generation, cancelled, on_tick,
    ));
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Default)]
    struct QueueFeed {
        due: VecDeque<Vec<u8>>,
    }

    impl QueueFeed {
        fn new() -> Self {
            Self::default()
        }

        fn push(&mut self, bytes: &[u8]) {
            self.due.push_back(bytes.to_vec());
        }
    }

    impl TerminalFeed for QueueFeed {
        fn drain_output(&mut self, sink: &mut dyn FnMut(&[u8])) -> usize {
            let mut writes = 0;
            while let Some(chunk) = self.due.pop_front() {
                sink(&chunk);
                writes += 1;
            }
            writes
        }

        fn send_input(&mut self, bytes: &[u8]) {
            self.push(bytes);
        }

        fn resize(&mut self, _cols: u16, _rows: u16) {}
    }

    #[test]
    fn the_repaint_interval_matches_the_documented_cap() {
        let expected = Duration::from_secs_f64(1.0 / REPAINT_HZ);
        assert!(
            REPAINT_INTERVAL.abs_diff(expected) < Duration::from_micros(1),
            "{REPAINT_INTERVAL:?}"
        );
    }

    #[test]
    fn a_fresh_emulator_holds_the_classic_default_grid() {
        let emulator = TerminalEmulator::with_defaults();
        assert_eq!(emulator.size(), (FALLBACK_COLS, FALLBACK_ROWS));
        assert_eq!(emulator.fed_bytes(), 0);
    }

    #[test]
    fn feeding_bytes_moves_the_screen_and_counts_them() {
        let mut emulator = TerminalEmulator::new(20, 4, 0);
        emulator.feed(b"hi");
        assert_eq!(emulator.fed_bytes(), 2);
        assert_eq!(emulator.fed_writes(), 1);
        assert_eq!(
            emulator.screen().cell(0, 0).map(|c| c.contents()),
            Some("h")
        );
    }

    #[test]
    fn resizing_is_a_no_op_at_an_unchanged_size_and_records_a_real_one() {
        let mut emulator = TerminalEmulator::new(20, 4, 0);
        assert!(
            !emulator.resize(20, 4),
            "the common per-layout case is free"
        );
        assert_eq!(emulator.take_pending_resize(), None);

        assert!(emulator.resize(52, 30));
        assert_eq!(emulator.size(), (52, 30));
        assert_eq!(
            emulator.take_pending_resize(),
            Some((52, 30)),
            "the session has to be told, once"
        );
        assert_eq!(emulator.take_pending_resize(), None, "and only once");
    }

    #[test]
    fn a_degenerate_resize_is_clamped_rather_than_handed_to_the_parser() {
        let mut emulator = TerminalEmulator::new(20, 4, 0);
        emulator.resize(0, 0);
        assert_eq!(emulator.size(), (1, 1), "never a zero-cell screen");
    }

    #[test]
    fn a_dispatch_between_two_wakes_keeps_one_monotonic_generation() {
        let feed: Rc<RefCell<QueueFeed>> = Rc::new(RefCell::new(QueueFeed::new()));
        let feed_dyn: SharedFeed = feed.clone();
        let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
        let generation = RwSignal::new(0u64);
        let mut input = InputSeam::new(feed_dyn.clone());
        input.wire_pump(Rc::clone(&emulator), generation);

        feed.borrow_mut().push(b"first chunk");
        assert!(pump_wake(&feed_dyn, &emulator, generation));
        let first = generation.get_untracked();
        assert_eq!(first, 1);

        input.dispatch(&Emission {
            bytes: b"q".to_vec(),
            ..Default::default()
        });
        let second = generation.get_untracked();
        assert!(
            second > first,
            "the keystroke's own echo must move the counter: {first} -> {second}"
        );

        feed.borrow_mut().push(b"second chunk");
        assert!(pump_wake(&feed_dyn, &emulator, generation));
        let third = generation.get_untracked();

        assert!(
            third > second,
            "the wake after a dispatch bump must move past it too: \
             {first} -> {second} -> {third}"
        );
        assert_ne!(
            third, second,
            "no duplicate generation across the two writers"
        );
    }

    #[test]
    fn a_selection_clears_on_the_next_generation_wake_regardless_of_writer() {
        let feed: Rc<RefCell<QueueFeed>> = Rc::new(RefCell::new(QueueFeed::new()));
        let feed_dyn: SharedFeed = feed.clone();
        let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
        emulator.borrow_mut().feed(b"hello world");
        let generation = RwSignal::new(0u64);
        let mut input = InputSeam::new(feed_dyn.clone());
        input.wire_pump(Rc::clone(&emulator), generation);
        let selection = SelectionSeam::new(Rc::clone(&emulator));

        let metrics = CellGeometry {
            font_size: 13.0,
            width: 8.0,
            height: 16.0,
            cols: 20,
            rows: 4,
        };
        selection.set_metrics(metrics);
        selection.observe_generation(generation.get_untracked());

        let arm = |selection: &SelectionSeam| {
            selection.track(Point::new(4.0, 8.0), true);
            selection.begin();
            assert!(selection.range().is_some(), "sanity: the word armed");
        };

        feed.borrow_mut().push(b"more output");
        assert!(pump_wake(&feed_dyn, &emulator, generation));
        selection.observe_generation(generation.get_untracked());

        arm(&selection);

        input.dispatch(&Emission {
            bytes: b"x".to_vec(),
            ..Default::default()
        });
        selection.observe_generation(generation.get_untracked());
        assert!(
            selection.range().is_none(),
            "the dispatch's own wake must clear a held selection"
        );

        arm(&selection);

        feed.borrow_mut().push(b"even more output");
        assert!(pump_wake(&feed_dyn, &emulator, generation));
        selection.observe_generation(generation.get_untracked());
        assert!(
            selection.range().is_none(),
            "the wake after a dispatch bump must ALSO be seen as new, and \
             clear the re-armed selection — regardless of which writer \
             produced it"
        );
    }

    thread_local! {
        /// Per-thread: `perform_haptic` runs the sink synchronously on the caller's thread.
        static THREAD_SELECTIONS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }

    fn count_selection(feedback: frust::HapticFeedback) {
        if feedback == frust::HapticFeedback::Selection {
            THREAD_SELECTIONS.with(|n| n.set(n.get() + 1));
        }
    }

    struct SinkGuard;

    impl Drop for SinkGuard {
        fn drop(&mut self) {
            frust::set_haptic_sink(None);
        }
    }

    /// Other tests' `begin()` may run meanwhile; a replaced sink reads 0 and the loop retries.
    #[test]
    fn a_selection_begin_fires_exactly_one_framework_haptic() {
        let _guard = SinkGuard;
        let mut seen = 0;
        for _ in 0..50 {
            THREAD_SELECTIONS.with(|n| n.set(0));
            frust::set_haptic_sink(Some(count_selection));
            let emulator = Rc::new(RefCell::new(TerminalEmulator::new(20, 4, 0)));
            let selection = SelectionSeam::new(emulator);
            selection.begin();
            assert_eq!(selection.haptic_count(), 1);
            seen = THREAD_SELECTIONS.with(std::cell::Cell::get);
            if seen != 0 {
                break;
            }
        }
        assert_eq!(seen, 1);
    }
}
