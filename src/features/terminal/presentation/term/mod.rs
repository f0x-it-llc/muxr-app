//! The **real** terminal grid: a VT emulator fed off a
//! [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed), a
//! same-style run batcher, width-derived cell geometry, and a `View`/`Widget`
//! pair that paints it — what the `/terminal` screen mounts for a **live**
//! session; a bare `/terminal` and every headless suite render
//! [`super::widgets`]' mock transcript instead. [`style`] owns the theme→ANSI-16
//! mapping, [`batch`] same-style run batching, [`geometry`] the cell box,
//! [`input`] and [`selection`] the keystroke/selection models' pure halves,
//! [`chip`] the Copy chip, [`widget`] the retained grid; this module owns the
//! emulator wrapper, the pump, the [`InputSeam`]/[`SelectionSeam`] and the
//! [`ContentTapSink`]/[`WheelSink`] hooks.
//!
//! **The ~30 Hz coalescing contract is app-side because it has to be**: the
//! mobile frame gate only paces *animation-only* frames (pacing requires
//! `!signals_dirty`), so a signal write per chunk would force an unpaced
//! whole-app rebuild at whatever rate the session pushes bytes. [`run_feed`]
//! wakes every [`REPAINT_INTERVAL`], drains everything due into the emulator and
//! writes the generation signal **once** per wake — no byte dropped or delayed,
//! only the repaint coalesced — and between wakes nothing is dirty, so an idle
//! session costs zero frames. It sleeps to an **absolute** grid point so a slow
//! frame cannot drift the schedule; parsing stays on the UI thread;
//! [`REPAINT_HZ`] is the only knob. The one exception is the synchronous echo
//! drain: a PTY echoes before the program answers, so [`InputSeam::dispatch`]
//! drains in the event pass the keystroke arrived in. That write wakes one
//! whole-app tracked rebuild 30 times a second while output flows, so a hosting
//! screen's `build` must stay cheap.
//!
//! **Layout-blind, speaking cells**: [`ContentTapSink`]/[`WheelSink`] report cell
//! coordinates, never a pane or an RPC — mapping a cell onto a pane belongs to
//! the screen that owns the session's layout.

pub mod batch;
pub mod chip;
pub mod geometry;
pub mod input;
pub mod selection;
pub mod style;
pub mod widget;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use frust::authoring::{
    EditingState, ImeContentType, ImeEvent, ImeState, InputEvent, Point, Rect, any,
};
use frust::{GestureDetector, GetUntracked, RwSignal, Set, Stack, StackView};
use frust::{on_cleanup, spawn_blocking, spawn_local};
use frust_haptics::{HapticEffect, Haptics};

use crate::features::terminal::domain::feed::TerminalFeed;

use geometry::CellGeometry;
use selection::{GridPoint, GridRange};

pub use chip::{CopyChipView, CopyChipWidget, CopySink, copy_chip};
pub use geometry::{FALLBACK_COLS, FALLBACK_ROWS};
pub use input::{BarKey, Emission, KeystrokeSource, ModifierLatch};
pub use style::{TerminalPalette, terminal_palette};
pub use widget::{TerminalGridView, TerminalGridWidget, terminal_grid};

/// The app-side repaint coalescing cap, in Hz — **the single knob**.
pub const REPAINT_HZ: f64 = 30.0;

/// [`REPAINT_HZ`] as a period — derived, so that really is the only knob.
pub const REPAINT_INTERVAL: Duration = Duration::from_nanos((1_000_000_000.0 / REPAINT_HZ) as u64);

/// Scrollback lines the emulator retains, matching the Flutter client this
/// replaces. No scrollback *view* exists yet, but the lines have to be captured
/// from the start or scrolling back would only show what happened after.
pub const DEFAULT_SCROLLBACK: usize = 4000;

/// The caret's shape. `settings`' own `CursorStyle` is the source of this value,
/// but it stays a **widget input** rather than a preferences read — the screen
/// that owns both maps one onto the other. Variants and default are identical to
/// that enum's, so the mapping is total and obvious.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorStyle {
    /// A filled cell (`ESC[2 q`) — the default, and the settings default.
    #[default]
    Block,
    /// A bar along the bottom of the cell (`ESC[4 q`).
    Underline,
    /// A vertical bar at the cell's left edge (`ESC[6 q`).
    Bar,
}

/// The emulator, shared between the pump (which feeds it) and the grid widget
/// (which reads its screen) — an `Rc<RefCell<_>>`, not a signal, per the [module
/// docs](self).
pub type SharedEmulator = Rc<RefCell<TerminalEmulator>>;

/// The feed, shared between the pump and whatever sends input to it.
pub type SharedFeed = Rc<RefCell<dyn TerminalFeed>>;

/// The keystroke source, shared between the grid widget (the IME client) and the
/// key bar (which arms the same latch) — see [`InputSeam`]. An `Rc<RefCell<_>>`
/// for the same reason the emulator is one: it is mutated during the *event*
/// pass, where a signal per keystroke would wake a whole-app rebuild.
pub type SharedKeystrokeSource = Rc<RefCell<KeystrokeSource>>;

/// Everything the grid widget and the key bar must agree on to be one keystroke
/// source: the [`KeystrokeSource`] (and therefore the one modifier latch), the
/// feed every byte leaves through, and the one signal a state change bumps.
///
/// **The signal is here and not in the source** because [`input`] is deliberately
/// framework-free, while the key bar's armed pips are rendered: a latch change
/// has to wake a rebuild. [`dispatch`](Self::dispatch) sends the bytes and
/// mirrors the source's revision into the signal, one write per event.
///
/// **Read-only and the scrub boundaries live here too**, since both are
/// properties of *the session* rather than of one widget: a read-only session
/// must be inert whichever widget the platform is talking to, and buffered
/// keystrokes must be dropped the moment it stops taking input.
#[derive(Clone)]
pub struct InputSeam {
    /// The shared source — the diff engine and the latch.
    pub source: SharedKeystrokeSource,
    /// Where emitted bytes go.
    pub feed: SharedFeed,
    /// Where emitted bytes go.
    pub revision: RwSignal<u64>,
    /// Whether this session refuses input entirely — see the type docs. A `Cell`
    /// behind an `Rc`, like everything here: written from `build`, read from the
    /// event and paint passes, where there is no `&mut` anything.
    read_only: Rc<Cell<bool>>,
    /// How many times a widget has claimed the focus path for this seam. The grid
    /// compares it across its own blur edge to tell a **hand-off** (a key cap took
    /// the surface — the session is still being typed into) from a real blur.
    /// Monotonic; the value itself means nothing.
    focus_claim: Rc<Cell<u64>>,
    /// The session's own pump — the emulator an echo lands in and the
    /// The session's own pump — the emulator an echo lands in and the generation
    /// signal that publishes it — wired in once by
    /// [`wire_pump`](Self::wire_pump). `None` until then.
    pump: Option<PumpHandle>,
}

/// The session's own pump, as [`InputSeam::dispatch`] needs it to drain an echo.
#[derive(Clone)]
struct PumpHandle {
    /// The emulator the echoed bytes land in.
    emulator: SharedEmulator,
    /// The generation signal the drain publishes to — the same one [`run_feed`]'s
    /// scheduled loop shares. [`pump_wake`] derives the next value from the signal
    /// itself, so a rebuild sees one monotonic counter whichever writer woke it.
    generation: RwSignal<u64>,
}

impl InputSeam {
    /// A seam over `feed`, with a fresh source and a revision signal at zero. Not
    /// wired to a pump yet — see [`wire_pump`](Self::wire_pump).
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

    /// Wire this seam to the session's own feed pump — the emulator an echoed byte
    /// must land in, and the generation signal the pump publishes to. Once wired,
    /// [`dispatch`](Self::dispatch) drains the feed synchronously right after
    /// sending, instead of waiting out [`run_feed`]'s next scheduled wake. Called
    /// exactly once, from `LiveSession::enter`.
    pub fn wire_pump(&mut self, emulator: SharedEmulator, generation: RwSignal<u64>) {
        self.pump = Some(PumpHandle {
            emulator,
            generation,
        });
    }

    /// Send `emission`'s bytes to the session and wake anything reading
    /// [`revision`](Self::revision).
    ///
    /// **The one place bytes leave the app**, so a caller never talks to the feed
    /// directly: bar taps and soft-keyboard characters take the identical route
    /// out, with the latch already applied inside the source — which is what makes
    /// it the right place for the read-only gate to be final.
    ///
    /// **The synchronous echo drain**: once [`wire_pump`](Self::wire_pump) has
    /// run, sending non-empty bytes also drains the feed right here. No `RefCell`
    /// borrow is held across the call. This seam carries no counter of its own:
    /// `pump_wake` derives "next" from `generation` itself, so the drain and
    /// [`run_feed`]'s scheduled wake always agree, whichever runs first.
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

    /// Whether this session refuses input — see the type docs.
    pub fn read_only(&self) -> bool {
        self.read_only.get()
    }

    /// Publish the session's read-only posture — the grid's own
    /// [`read_only`](TerminalGridView::read_only) builder input, threaded here from
    /// its `build`/`rebuild` so the key bar and every other emitter observe the
    /// same one. Turning it **on** scrubs: a session that has stopped taking input
    /// has no reason to keep the last thing typed into it. Idempotent.
    pub fn set_read_only(&self, read_only: bool) {
        if self.read_only.replace(read_only) != read_only && read_only {
            self.scrub();
        }
    }

    /// Drop every keystroke the source is holding — see [`KeystrokeSource::scrub`]
    /// for what that covers, and the type docs for the boundaries.
    ///
    /// Deliberately does **not** mirror the source's revision into
    /// [`revision`](Self::revision): the paint pass calls this (the grid's blur
    /// edge), and a signal write from paint would wake a rebuild out of a pass that
    /// must not schedule one. Nothing rendered reads the mirror.
    pub fn scrub(&self) {
        self.source.borrow_mut().scrub();
    }

    /// Claim the focus path for this seam, returning the new claim — called by
    /// every widget that requests focus on the session's behalf (the grid's `Down`,
    /// a key cap's `Down`). See [`focus_claim`](Self::focus_claim).
    pub fn claim_focus(&self) -> u64 {
        let claim = self.focus_claim.get().wrapping_add(1);
        self.focus_claim.set(claim);
        claim
    }

    /// The current focus claim — recorded on focus gain, re-read on loss. An
    /// unchanged claim across that edge means that really was a blur.
    pub fn focus_claim(&self) -> u64 {
        self.focus_claim.get()
    }

    /// Fold a key-bar press into the shared source and send whatever it derived, or
    /// `None` when the session refuses input. The bar's counterpart to
    /// [`absorb`](Self::absorb): the gate belongs to the seam, not the cap, so a cap
    /// over a read-only session is inert without the screen remembering.
    pub fn press_bar_key(&self, key: BarKey) -> Option<Emission> {
        if self.read_only() {
            return None;
        }
        let emission = self.source.borrow_mut().on_bar_key(key);
        self.dispatch(&emission);
        Some(emission)
    }

    /// The armed modifiers — what the key bar draws its pips from.
    pub fn latch(&self) -> ModifierLatch {
        self.source.borrow().latch()
    }

    /// The editing state to publish to the platform right now.
    pub fn published(&self) -> EditingState {
        self.source.borrow().published()
    }

    /// **The** IME surface a terminal input widget publishes: the source's
    /// **The** IME surface a terminal input widget publishes: the source's
    /// reconciled editing state, an optional caret rect as the platform's
    /// candidate-placement hint, and [`ImeContentType::Terminal`] — the hint that
    /// switches off autocorrect, smart quotes/dashes and autocapitalization, so a
    /// typed `"` arrives as `0x22`. Spelled once here rather than per widget: two
    /// spellings could disagree about the content type, a silent
    /// corrupt-my-shell-command bug. `active` is always `true`: only a widget
    /// holding (or claiming) focus publishes this, and a blur drops the surface.
    pub fn ime_state(&self, caret: Option<Rect>) -> ImeState {
        ImeState {
            active: true,
            editing: self.published(),
            caret,
            content_type: ImeContentType::Terminal,
        }
    }

    /// Fold one **focus-routed** event into the source and send whatever it
    /// derived, or `None` when `event` is not one.
    ///
    /// More than one widget calls this because frust blurs on any pointer `Down`
    /// that claims no focus, and a blur drops the whole published surface — so
    /// every key cap claims focus like the grid does. Hence the seam: **whichever
    /// widget holds the focus path drives the same [`KeystrokeSource`] and
    /// publishes the same [`ime_state`](Self::ime_state)**, so the platform sees
    /// one unbroken input surface. A read-only session absorbs nothing.
    pub fn absorb(&self, event: &InputEvent) -> Option<Emission> {
        if self.read_only() {
            return None;
        }
        let emission = {
            let mut source = self.source.borrow_mut();
            match event {
                InputEvent::Key(key) => source.on_key(&key.key, key.modifiers),
                // `ApplyEditingState` is the mobile (state-sync) path;
                // `Compose`/`Commit` are the desktop (winit) shapes;
                // `Enabled`/`Disabled` bracket a composition.
                InputEvent::Ime(ImeEvent::ApplyEditingState(state)) => source.on_snapshot(state),
                InputEvent::Ime(ImeEvent::Commit(text)) => source.on_commit(text),
                InputEvent::Ime(ImeEvent::Compose { text, .. }) => source.on_compose(text),
                InputEvent::Ime(ImeEvent::Disabled) => source.on_composition_end(),
                InputEvent::Ime(ImeEvent::Enabled) => Emission::default(),
                _ => return None,
            }
        };
        self.dispatch(&emission);
        Some(emission)
    }
}

/// The live selection gesture, shared between the grid widget (which measures,
/// paints and tracks the finger), the gesture detector's `on_long_press` (which
/// starts it) and the Copy chip (which ends it) — see [`SelectionSeam`].
pub type SharedSelection = Rc<RefCell<SelectionState>>;

/// Everything one selection gesture is: what the grid last measured, where the
/// finger is, which cell the gesture anchored on, and what is selected. Private
/// fields with accessors on [`SelectionSeam`]: three widgets read this, and each
/// must go through the same clamping/resolution rules.
#[derive(Default)]
pub struct SelectionState {
    /// The cell box the grid last laid out at — the px→cell mapping.
    metrics: Option<CellGeometry>,
    /// The finger, in the grid's own local logical px.
    pointer: Point,
    /// Whether a finger is currently down (see [`SelectionSeam::begin`]).
    pressed: bool,
    /// The cell the long-press anchored on. Fixed for the whole drag.
    anchor: Option<GridPoint>,
    /// Whether a selection drag is live — what makes the loupe visible.
    dragging: bool,
    /// The selected cells, if any.
    range: Option<GridRange>,
    /// Where the drag released, i.e. where the Copy chip is anchored. `Some`
    /// **is** "the chip is up".
    chip: Option<Point>,
    /// Selection-click haptics fired — a test/diagnostic counter.
    haptics: u32,
    /// The last content generation this seam observed (see
    /// [`SelectionSeam::observe_generation`]). `None` until the grid's first
    /// build — a fresh seam has nothing selected yet.
    last_generation: Option<u64>,
}

/// Everything the grid widget, the long-press callback and the Copy chip must
/// agree on to be one selection: the shared [`SelectionState`] and the emulator
/// whose screen resolves words and yields copy text.
///
/// An `Rc<RefCell<_>>` rather than a signal for the same reason [`InputSeam`]'s
/// source is: it is mutated during the **event** pass, and a drag re-resolves both
/// endpoints on every `Move` — a signal write per move would wake a whole-app
/// rebuild 60+ times a second to change nothing but pixels. The grid asks for a
/// plain `request_redraw` instead, and a paint pass walks the whole tree, so the
/// chip (a sibling, not a child) sees the new state that frame with no rebuild.
///
/// [`begin`](Self::begin) fires the selection-click haptic — the one thing here
/// that is not state, kept at this choke point so [`selection`] stays pure.
#[derive(Clone)]
pub struct SelectionSeam {
    /// The shared gesture state.
    state: SharedSelection,
    /// The screen words resolve against and copy text comes from.
    emulator: SharedEmulator,
}

impl SelectionSeam {
    /// A seam over `emulator`, with nothing selected.
    pub fn new(emulator: SharedEmulator) -> Self {
        Self {
            state: Rc::new(RefCell::new(SelectionState::default())),
            emulator,
        }
    }

    /// Publish the cell box the grid just laid out at — the px→cell mapping every
    /// resolution below goes through. Called from `layout`.
    ///
    /// A held selection is clamped onto the new grid **here**, the instant a shrink
    /// is observed. That is what stops a stale, off-screen `range` from reaching
    /// [`selection::selected_text`] — a u16 underflow inside `vt100`'s own
    /// `contents_between` on a shrink-then-Copy — or an equally stale
    /// [`selection::segments`] paint. [`GridRange::clamp_to`] is a no-op when the
    /// grid didn't shrink, so the common path costs nothing.
    pub fn set_metrics(&self, metrics: CellGeometry) {
        let mut state = self.state.borrow_mut();
        state.metrics = Some(metrics);
        if let Some(range) = state.range {
            state.range = Some(range.clamp_to(metrics.cols, metrics.rows));
        }
    }

    /// Record the finger at `at` (grid-local logical px), and whether it is down.
    pub fn track(&self, at: Point, pressed: bool) {
        let mut state = self.state.borrow_mut();
        state.pointer = at;
        state.pressed = pressed;
    }

    /// **The long-press consumer.** Tick, anchor on the cell under the finger, and
    /// select the word there.
    ///
    /// The haptic fires **before** the word is resolved, so it fires even when the
    /// finger landed on a separator and nothing gets selected: the tick
    /// acknowledges *the gesture*. Its `Result` is ignored — desktop reports
    /// `NotAvailable` by design. A fire that arrives with **no finger down** (the
    /// fire-on-release path) resolves the word and goes straight to the released
    /// state, chip and all, rather than leaving a drag with nothing to end it.
    pub fn begin(&self) {
        let _ = Haptics::perform(HapticEffect::SelectionClick);
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

    /// Re-resolve the selection for the live finger position: the word at the
    /// **fixed** anchor merged with the word under the finger, in reading order.
    /// Both ends every time (see [`selection::resolve`]) — nothing is cached, so
    /// reversing direction mid-drag needs no special case.
    pub fn extend(&self) {
        let mut state = self.state.borrow_mut();
        let (Some(metrics), Some(anchor)) = (state.metrics, state.anchor) else {
            return;
        };
        let extent = selection::cell_at(state.pointer, metrics);
        state.range = selection::resolve(self.emulator.borrow().screen(), anchor, extent);
    }

    /// End the drag: the loupe goes away and, if anything is selected, the Copy
    /// chip appears where the finger lifted.
    pub fn finish(&self) {
        let mut state = self.state.borrow_mut();
        state.dragging = false;
        state.chip = state.range.map(|_| state.pointer);
    }

    /// Drop the drag **flag** only — the `Cancel` arm's whole vocabulary. The
    /// selection itself survives a stolen gesture; the next tap clears it.
    pub fn cancel_drag(&self) {
        self.state.borrow_mut().dragging = false;
    }

    /// Whether anything is selected or the chip is up — i.e. whether the next tap
    /// is a **clearing** tap.
    pub fn is_active(&self) -> bool {
        let state = self.state.borrow();
        state.range.is_some() || state.chip.is_some()
    }

    /// Drop the selection, the chip and the anchor.
    pub fn clear(&self) {
        let mut state = self.state.borrow_mut();
        state.anchor = None;
        state.dragging = false;
        state.range = None;
        state.chip = None;
    }

    /// Clear an existing selection, reporting whether there was one — **the
    /// clearing tap**, spelled as one call so the grid can check it *before*
    /// claiming focus: a tap that dismisses a selection must not raise a keyboard.
    pub fn clear_if_active(&self) -> bool {
        let active = self.is_active();
        if active {
            self.clear();
        }
        active
    }

    /// Note that the grid just observed content generation `generation`, clearing a
    /// held selection if the generation moved since the last observation
    /// ([`selection`]'s module docs record why new output clears).
    ///
    /// A no-op unless BOTH hold: `generation` differs from the last one seen, and
    /// something is held. A rebuild carrying the *same* generation — any app-wide
    /// rebuild this screen sits under — is not new output. Called from the grid's
    /// `build` (seeding, nothing to clear yet) and from every `rebuild`.
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

    /// The selected cells, if any.
    pub fn range(&self) -> Option<GridRange> {
        self.state.borrow().range
    }

    /// Whether a selection drag is live — what makes the loupe visible.
    pub fn dragging(&self) -> bool {
        self.state.borrow().dragging
    }

    /// The finger, in the grid's own local logical px — the loupe's focal point.
    pub fn pointer(&self) -> Point {
        self.state.borrow().pointer
    }

    /// Where the drag released, when the Copy chip is up.
    pub fn chip_anchor(&self) -> Option<Point> {
        self.state.borrow().chip
    }

    /// The selected text, as the clipboard should receive it; empty when nothing is.
    pub fn text(&self) -> String {
        let Some(range) = self.range() else {
            return String::new();
        };
        selection::selected_text(self.emulator.borrow().screen(), range)
    }

    /// Selection-click haptics fired so far — a test/diagnostic accessor.
    pub fn haptic_count(&self) -> u32 {
        self.state.borrow().haptics
    }

    /// The cell box the grid last laid out at — a test/diagnostic accessor (and the
    /// only way anything outside the grid can turn a cell into a pixel).
    pub fn metrics(&self) -> Option<CellGeometry> {
        self.state.borrow().metrics
    }
}

/// Where a **content tap** is reported: the zero-based `(col, row)` cell the
/// finger lifted on, clamped to the rendered grid. The `bool` it answers is
/// **whether this tap should raise the keyboard**.
///
/// An `Rc<dyn Fn>` for the same reason [`CopySink`] is one: installed from a
/// `build`, called from the *event* pass. Optional at the grid — with none wired
/// a tap claims focus and raises the keyboard.
///
/// The answer is **synchronous by necessity**: the grid publishes its IME surface
/// inside the event pass the release arrives in (`EventCtx` is the only route to
/// `publish_ime_state`), so a verdict needing a round trip could not be honoured.
/// Everything the answer *causes* stays asynchronous behind it. `false` leaves the
/// keyboard where it was, since the grid never dismisses one.
pub type ContentTapSink = Rc<dyn Fn(u32, u32) -> bool>;

/// Where a **wheel step** is reported: one step's direction (`up`) and the
/// zero-based `(col, row)` cell it applies at. One call per step, never a batch
/// and never a pixel amount: the grid does the px→line accumulation, so a touch
/// drag and a desktop wheel arrive in the vocabulary `muxrd`'s scroll RPC speaks.
pub type WheelSink = Rc<dyn Fn(bool, u32, u32)>;

/// The whole interactive terminal: the grid, its long-press gesture, and the Copy
/// chip over it.
///
/// ```text
/// Stack
///  ├─ GestureDetector(on_long_press = seam.begin)   ← bottom
///  │    └─ TerminalGridView (selection wired)
///  └─ CopyChipView                                  ← top, full-bleed
/// ```
///
/// 1. **The chip is above the gesture detector**, so a press on the pill never
///    reaches the grid: tapping `Copy` cannot clear the selection it is copying.
/// 2. **`on_long_press` is the only gesture wired**, fired at the threshold in
///    wall time, so a stationary hold selects with no movement. The grid runs its
///    own three-way tap/drag-scroll/long-press discrimination ([`widget`]'s touch
///    state machine) off the raw phases, because a tap has to be checked against a
///    clearing selection and a focus claim the detector knows nothing about. Both
///    recognisers use the one [`TOUCH_SLOP`](frust::input::TOUCH_SLOP).
/// 3. **The surface takes the whole offered box**, because the chip overlay is
///    full-bleed; the grid still takes only its whole-cell box inside it, so the
///    sub-cell remainder is the tight-parent case [`TerminalGridWidget`] documents.
pub fn terminal_surface<State: 'static>(
    seam: SelectionSeam,
    grid: TerminalGridView,
    chip: CopyChipView,
) -> StackView<State> {
    let armed = seam.clone();
    Stack(vec![
        any(GestureDetector::<State, _>(grid.selection(seam))
            .on_long_press(move |_state: &mut State| armed.begin())),
        any(chip),
    ])
}

/// A VT emulator sized to the grid on screen: a thin wrapper over `vt100::Parser`,
/// and the *only* place in the app that names that crate's `(rows, cols)` argument
/// order — everything above speaks `(cols, rows)`.
pub struct TerminalEmulator {
    parser: vt100::Parser,
    /// A resize the layout pass derived and the session has not been told about
    /// yet. Drained by whatever owns the feed, which turns a rotation into `SIGWINCH`.
    pending_resize: Option<(u16, u16)>,
    /// Diagnostics: bytes and writes actually parsed.
    fed_bytes: usize,
    fed_writes: usize,
}

impl TerminalEmulator {
    /// An emulator holding a blank `cols` x `rows` screen with `scrollback`
    /// lines of history.
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new(rows.max(1), cols.max(1), scrollback),
            pending_resize: None,
            fed_bytes: 0,
            fed_writes: 0,
        }
    }

    /// An emulator at the classic VT default grid (80x24) with
    /// [`DEFAULT_SCROLLBACK`] — what it holds until the first `layout`.
    pub fn with_defaults() -> Self {
        Self::new(FALLBACK_COLS, FALLBACK_ROWS, DEFAULT_SCROLLBACK)
    }

    /// The current visible screen.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Parse `bytes`.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.fed_bytes += bytes.len();
        self.fed_writes += 1;
    }

    /// The grid the emulator currently holds, as `(cols, rows)`.
    pub fn size(&self) -> (u16, u16) {
        let (rows, cols) = self.parser.screen().size();
        (cols, rows)
    }

    /// Resize the screen to `cols` x `rows`, reporting whether anything changed.
    /// `false` — the common case, once per layout pass at a stable size — does
    /// nothing at all, which matters because this runs inside `layout`. A real
    /// change also records a [`pending_resize`](Self::take_pending_resize).
    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if self.size() == (cols, rows) {
            return false;
        }
        self.parser.screen_mut().set_size(rows, cols);
        self.pending_resize = Some((cols, rows));
        true
    }

    /// Take the resize the session has not been told about yet, if any.
    pub fn take_pending_resize(&mut self) -> Option<(u16, u16)> {
        self.pending_resize.take()
    }

    /// Bytes parsed so far — a diagnostic/test accessor.
    pub fn fed_bytes(&self) -> usize {
        self.fed_bytes
    }

    /// Writes parsed so far — a diagnostic/test accessor.
    pub fn fed_writes(&self) -> usize {
        self.fed_writes
    }
}

/// One wake of the feed pump: drain everything due into the emulator and, if
/// anything arrived, publish **exactly one** new generation. Answers whether the
/// generation moved. Split out of [`run_feed`] so the one-write-per-wake rule is
/// testable without a timer.
///
/// Two callers share this wake — [`run_feed`]'s scheduled loop and
/// [`InputSeam::dispatch`]'s synchronous echo drain — and both write the same
/// `generation` signal. **This function, not either caller, is the one source of
/// truth for "next"**: it derives `generation.get_untracked() + 1` here, so
/// neither caller can republish a duplicate (invisible to the tracked rebuild,
/// dropping that wake's content) or a value lower than what is already live.
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

/// The UI-thread feed loop: wake every [`REPAINT_INTERVAL`], drain everything due,
/// publish one generation, repeat until the feed closes or `cancelled` flips.
/// Every `await` point releases its borrows first — a `RefCell` borrow held across
/// a suspension would panic the moment the grid laid out.
pub async fn run_feed(
    feed: SharedFeed,
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
    cancelled: Arc<AtomicBool>,
) {
    run_feed_watched(feed, emulator, generation, cancelled, || {}).await;
}

/// [`run_feed`] with an `on_tick` observer called once per wake, on the **UI
/// thread**, after the drain.
///
/// The one seam for *surfacing* what the feed knows beyond its bytes — a live
/// session's connection status and minted connection id, deliberately not on the
/// [`TerminalFeed`] port: a feed writes no signal, so a status a screen renders
/// must be **polled** from the side that may write signals. Mirroring it here
/// costs nothing, since this loop is already awake, on the right thread, at the
/// rate the screen repaints.
///
/// `on_tick` runs with **no borrow held** on the feed or the emulator, so an
/// observer may borrow them itself — including replacing the feed outright, which
/// is what a re-attach does. It also runs on the wake that ends the loop, *after*
/// the open/closed decision, so a finished feed publishes its final status.
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
        // Sleep to the next absolute grid point rather than "now + interval",
        // so a slow frame cannot drift the schedule.
        tick += 1;
        let deadline = REPAINT_INTERVAL * tick;
        if let Some(nap) = deadline.checked_sub(start.elapsed()) {
            let _ = spawn_blocking(move || std::thread::sleep(nap)).await;
        }
    }
}

/// Start [`run_feed`] under the current reactive owner, cancelling it on teardown.
/// Cancellation is an `on_cleanup` registration, never a `Drop` impl: the loop
/// stops at its next wake, and — because the flag is checked *before* the drain —
/// a component torn down between wakes never writes its signal again.
/// `Arc<AtomicBool>` because `on_cleanup` takes a `Send + Sync` closure.
pub fn spawn_feed_pump(feed: SharedFeed, emulator: SharedEmulator, generation: RwSignal<u64>) {
    spawn_feed_pump_watched(feed, emulator, generation, || {});
}

/// [`spawn_feed_pump`] over [`run_feed_watched`] — the same ownership and
/// cancellation contract, plus that function's per-wake observer. `on_tick` is not
/// `Send`: the loop rides the UI thread's own queue, which is what lets an
/// observer hold the `Rc`-backed feed handle it is polling.
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

    /// A queue-driven [`TerminalFeed`] fake for the pump-hygiene tests below: a
    /// test [`push`](Self::push)es exactly the chunks it wants due on the next
    /// [`pump_wake`], and `send_input` echoes synchronously — `DemoFeed`'s
    /// PTY-style contract, minus the fixture-replay machinery.
    #[derive(Default)]
    struct QueueFeed {
        due: VecDeque<Vec<u8>>,
    }

    impl QueueFeed {
        fn new() -> Self {
            Self::default()
        }

        /// Queue `bytes` as one more write, due on the next drain.
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
        // The one knob: REPAINT_HZ. Guard the derived period against drift.
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

    // One source of truth for the generation counter: both `run_feed`'s scheduled
    // loop and `InputSeam::dispatch`'s echo drain call `pump_wake` against the same
    // signal, and these pin that the two cannot disagree about "next".

    #[test]
    fn a_dispatch_between_two_wakes_keeps_one_monotonic_generation() {
        let feed: Rc<RefCell<QueueFeed>> = Rc::new(RefCell::new(QueueFeed::new()));
        let feed_dyn: SharedFeed = feed.clone();
        let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
        let generation = RwSignal::new(0u64);
        let mut input = InputSeam::new(feed_dyn.clone());
        input.wire_pump(Rc::clone(&emulator), generation);

        // Wake 1 — a `run_feed`-style wake with real replay content due.
        feed.borrow_mut().push(b"first chunk");
        assert!(pump_wake(&feed_dyn, &emulator, generation));
        let first = generation.get_untracked();
        assert_eq!(first, 1);

        // A keystroke lands mid-replay: the seam's own synchronous echo drain.
        input.dispatch(&Emission {
            bytes: b"q".to_vec(),
            ..Default::default()
        });
        let second = generation.get_untracked();
        assert!(
            second > first,
            "the keystroke's own echo must move the counter: {first} -> {second}"
        );

        // Wake 2 — the next scheduled wake, more content due. A caller deriving
        // "next" from its own stale local would publish a duplicate here.
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
        // Seed the seam's last-seen generation as the grid's first build does.
        selection.observe_generation(generation.get_untracked());

        // Select the word "hello" at (row 0, col 0), re-armed after each clear.
        let arm = |selection: &SelectionSeam| {
            selection.track(Point::new(4.0, 8.0), true);
            selection.begin();
            assert!(selection.range().is_some(), "sanity: the word armed");
        };

        // Wake 1: new content before anything is selected — a no-op on the
        // selection, but it moves the generation the next observation compares to.
        feed.borrow_mut().push(b"more output");
        assert!(pump_wake(&feed_dyn, &emulator, generation));
        selection.observe_generation(generation.get_untracked());

        arm(&selection);

        // A keystroke lands: the echo drain is the SECOND writer to this signal.
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

        // Wake 2. A repeated generation value would leave `observe_generation`'s
        // `!=` check seeing no change, and the re-armed selection standing.
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
}
