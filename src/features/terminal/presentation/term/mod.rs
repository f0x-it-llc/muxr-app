//! The **real** terminal grid: a VT emulator fed off a
//! [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed), a
//! same-style run batcher, width-derived cell geometry, and a `View`/`Widget`
//! pair that paints it. The productization of the renderer S9 measured
//! (`benchmarks/frust_bench`'s `s9_terminal` in the Frust checkout), minus its
//! bench scaffolding and behind an app-shaped seam.
//!
//! Nothing here is wired into the `/terminal` screen yet: that screen still
//! renders [`pieces`](super::pieces)' mock transcript, and the cutover is its
//! own task. This module is complete, tested and unmounted.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`style`] | [`TerminalPalette`] (the theme-token → ANSI-16 mapping table lives in its docs) and `CellStyle` |
//! | [`batch`] | same-style run batching, and the four vt100 traps it has to get right |
//! | [`geometry`] | the measured cell box, and the cols x rows an available box divides into |
//! | [`input`] | the keystroke source's pure half: the snapshot diff, the Ctrl/Alt/Shift latch, the key-bar byte vocabulary |
//! | [`selection`] | the selection model's pure half: word boundaries, the row-major merge, line-wise segments, the copy text |
//! | [`chip`] | the Copy chip a released selection raises, and the injectable clipboard sink |
//! | [`widget`] | the retained grid: shaping in `layout`, replay in `paint`, the caret, IME clienthood, the selection highlight and the loupe, and the touch state machine behind the two hooks below |
//! | this module | the emulator wrapper, the 30 Hz feed pump, [`CursorStyle`], the [`InputSeam`] the grid and the key bar share, the [`SelectionSeam`] the grid and the chip share, the [`ContentTapSink`]/[`WheelSink`] hooks a screen routes, and [`terminal_surface`], which assembles all of it |
//!
//! # The 30 Hz coalescing mechanism, and why it is app-side
//!
//! The framework's mobile frame gate can only pace a frame that is
//! **animation-only**: a pacing decision requires `!signals_dirty`, so *a
//! signal write can never be paced*. Every write forces an unpaced frame at
//! panel rate. A terminal that wrote a signal per received chunk would
//! therefore repaint at whatever rate the session pushes bytes — 120 forced
//! whole-app rebuilds a second on a firehose — so the coalescing has to happen
//! here:
//!
//! - [`run_feed`] wakes every [`REPAINT_INTERVAL`] (1/[`REPAINT_HZ`]), asks the
//!   feed for **everything** due, hands it all to the emulator, and then writes
//!   the generation signal **once** — one write per wake, never one per chunk.
//!   No byte is dropped or delayed; only the *repaint* is coalesced.
//! - Between wakes nothing is dirty and nothing requests a frame, so the mobile
//!   frame gate skips: an idle session costs zero frames, which is the whole
//!   point of holding the emulator in an `Rc<RefCell<_>>` **outside** the
//!   signal graph.
//! - The loop sleeps to an **absolute** grid point (`interval * tick`) rather
//!   than "now + interval", so a slow frame cannot drift the schedule.
//! - It sleeps via [`frust::spawn_blocking`], the same facade routing the rest
//!   of the app uses; both mobile shells drain the UI-thread local task queue
//!   at the top of every tick, so a wake that only sleeps costs one pump and
//!   one gate skip.
//! - Parsing runs on the UI thread. That is deliberate: it is where the
//!   incumbent parses too, and moving it off-thread is an optimization to make
//!   *with* a measurement, not before one.
//!
//! **Changing the cap is one constant: [`REPAINT_HZ`].** Nothing else derives
//! from it.
//!
//! # The one exception: a synchronous echo drain
//!
//! A real PTY echoes a keystroke before the program under it answers, and
//! coalescing that onto this module's own 30 Hz schedule would mean every
//! typed character waits up to one tick before it's visible. So
//! [`InputSeam::dispatch`] instead drains it **synchronously** — a borrow, a
//! drain, and (if anything came back) one signal write, the same shape as
//! [`pump_wake`] itself — right after
//! [`TerminalFeed::send_input`](crate::features::terminal::domain::feed::TerminalFeed::send_input),
//! in the very event pass the keystroke arrived in, rather than waiting on
//! [`run_feed`]'s own timer. [`InputSeam::wire_pump`] is what wires a seam to
//! the emulator/generation pair this needs; every lower-level seam test that
//! drives its own pump by hand instead (`tests/term_input.rs`,
//! `tests/term_selection.rs`, `tests/term_widget.rs`) leaves it unwired and
//! keeps working exactly as before. This is also what lets a drained replay
//! stop meaning "keep the timer running forever because echo is on" — see
//! `data::feeds::DemoFeed::is_open`'s own doc for the exit condition it
//! unlocks.
//!
//! # Rebuild model
//!
//! A generation write wakes one whole-app tracked rebuild (`docs/ARCHITECTURE.md`'s
//! rebuild model), 30 times a second while output is flowing. So the screen
//! that eventually hosts this grid must keep its `build` cheap — resolve the
//! palette once per build (it is a plain value from there on), and never do
//! per-cell work above the widget.
//!
//! # Scrollback
//!
//! [`DEFAULT_SCROLLBACK`] lines, matching the Flutter incumbent. A construction
//! input rather than a constant at the call site, since a settings-driven
//! scrollback is a plausible next step.
//!
//! # This module is layout-blind, and says so in cells
//!
//! [`ContentTapSink`] and [`WheelSink`] are the two hooks a content tap and a
//! scroll step leave through, and both speak **cell** coordinates on the
//! rendered grid — never a pane, a pane id or an RPC. Mapping a cell onto the
//! pane that occupies it (and turning that into a `FocusPane`/`ScrollPane`
//! call) belongs to the screen that owns the session's layout; nothing in
//! `term` knows a pane exists, which is what keeps the grid mountable by the
//! demo replay, the mock transcript and a live session alike.

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

/// The app-side repaint coalescing cap, in Hz — **the single knob** for the
/// mechanism the [module docs](self) describe. 30 Hz is the rate a terminal UI
/// ships at; the framework cannot pace a signal-driven repaint for us.
pub const REPAINT_HZ: f64 = 30.0;

/// [`REPAINT_HZ`] as a period — derived, so [`REPAINT_HZ`] really is the only
/// knob.
pub const REPAINT_INTERVAL: Duration = Duration::from_nanos((1_000_000_000.0 / REPAINT_HZ) as u64);

/// Scrollback lines the emulator retains, matching the Flutter client this
/// replaces (4000). Not yet reachable: no scrollback *view* exists until its
/// own task, but the lines have to be captured from the start or scrolling back
/// would only ever show what happened after the feature shipped.
pub const DEFAULT_SCROLLBACK: usize = 4000;

/// The caret's shape.
///
/// The settings feature's own `CursorStyle` (its terminal-appearance group) is
/// the eventual source of this value, but it stays a **widget input** rather
/// than a preferences read: `presentation` never reaches into a sibling
/// feature's layers (`docs/ARCHITECTURE.md`'s cross-feature rule), so the
/// screen that owns both — the one the cutover task writes — is what maps one
/// onto the other. Variants and default are deliberately identical to that
/// enum's, so the mapping is total and obvious.
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
/// (which reads its screen) — see the [module docs](self)' note on why this is
/// an `Rc<RefCell<_>>` and not a signal.
pub type SharedEmulator = Rc<RefCell<TerminalEmulator>>;

/// The feed, shared between the pump and whatever sends input to it.
pub type SharedFeed = Rc<RefCell<dyn TerminalFeed>>;

/// The keystroke source, shared between the grid widget (which is the IME
/// client) and the key bar (which arms the same latch) — see [`InputSeam`].
///
/// An `Rc<RefCell<_>>` for the same reason the emulator is one: it is mutated
/// during the *event* pass, where there is no reactive owner and no `&mut
/// State`, and a signal per keystroke would wake a whole-app rebuild per
/// keystroke.
pub type SharedKeystrokeSource = Rc<RefCell<KeystrokeSource>>;

/// Everything the grid widget and the key bar must agree on to be one keystroke
/// source: the [`KeystrokeSource`] (and therefore the one modifier latch), the
/// feed every byte leaves through, and the one signal a state change bumps.
///
/// # Why the signal is here and not in the source
///
/// [`input`] is deliberately framework-free — a pure diff engine with no
/// reactive dependency — but the key bar's armed pips are *rendered*, so a
/// latch change (whether from a bar tap or from a soft-keyboard character
/// **consuming** the latch) has to wake a rebuild. This wrapper is the one
/// place those two facts meet: [`dispatch`](Self::dispatch) sends the bytes and
/// mirrors the source's own revision counter into the signal, one write per
/// event, never one per byte.
///
/// # Read-only, and the scrub boundaries, live here too
///
/// Both are properties of *the session*, not of one widget: a read-only
/// session must be inert whichever widget the platform is talking to, and the
/// buffered keystrokes ([`input`]'s own "what is retained" docs) must be
/// dropped the moment the session stops taking input. So
/// [`read_only`](Self::read_only) is seam state — set by the grid from its own
/// builder input, honoured by [`dispatch`](Self::dispatch) (the byte exit),
/// [`absorb`](Self::absorb) and [`press_bar_key`](Self::press_bar_key) alike —
/// and [`scrub`](Self::scrub) is the call the grid makes on a blur, on turning
/// read-only, and on teardown.
#[derive(Clone)]
pub struct InputSeam {
    /// The shared source — the diff engine and the latch.
    pub source: SharedKeystrokeSource,
    /// Where emitted bytes go.
    pub feed: SharedFeed,
    /// The source's revision, mirrored so a `build` that reads it rebuilds when
    /// the latch (or the mirror) moves.
    pub revision: RwSignal<u64>,
    /// Whether this session refuses input entirely — see the type docs. A
    /// `Cell` behind an `Rc`, like everything else here: it is written from
    /// `build` and read from the event and paint passes, where there is no
    /// `&mut` anything.
    read_only: Rc<Cell<bool>>,
    /// How many times a widget has claimed the focus path for this seam. The
    /// grid compares it across its own blur edge to tell a **hand-off** (a key
    /// cap took the surface — the session is still being typed into) from a
    /// real blur (nothing holds it, so the buffers go). Monotonic; the value
    /// itself means nothing.
    focus_claim: Rc<Cell<u64>>,
    /// The session's own pump — the emulator an echo lands in and the
    /// generation signal that publishes it — wired in once by
    /// [`wire_pump`](Self::wire_pump). `None` until then, which is exactly
    /// what every lower-level seam test that drives its own pump by hand
    /// leaves it at (see the [module docs](self)' synchronous-echo section).
    pump: Option<PumpHandle>,
}

/// The session's own pump, as [`InputSeam::dispatch`] needs it to drain an
/// echo synchronously — see [`InputSeam::wire_pump`].
#[derive(Clone)]
struct PumpHandle {
    /// The emulator the echoed bytes land in.
    emulator: SharedEmulator,
    /// The generation signal the drain publishes to — the same one
    /// [`run_feed`]'s scheduled loop shares. [`pump_wake`] derives the next
    /// value from this signal itself, so a rebuild sees one monotonically
    /// increasing counter no matter which of the two woke it — neither
    /// caller carries a competing notion of "next" (review M-A1).
    generation: RwSignal<u64>,
}

impl InputSeam {
    /// A seam over `feed`, with a fresh source and a revision signal at zero.
    ///
    /// Not wired to a pump yet — see [`wire_pump`](Self::wire_pump).
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

    /// Wire this seam to the session's own feed pump — the emulator an
    /// echoed byte must land in, and the generation signal the pump
    /// publishes to (see the [module docs](self)' synchronous-echo section).
    ///
    /// Once wired, [`dispatch`](Self::dispatch) drains the feed
    /// synchronously right after sending, in the same event pass a keystroke
    /// arrived in, instead of waiting out [`run_feed`]'s next scheduled
    /// wake. Called exactly once, from `LiveSession::enter`
    /// (`presentation::controller`), immediately after constructing the
    /// seam, the emulator and the generation signal they end up sharing with
    /// [`spawn_feed_pump`].
    ///
    /// Every lower-level seam test (`tests/term_input.rs`,
    /// `tests/term_selection.rs`, `tests/term_widget.rs`) never calls this —
    /// each drives [`pump_wake`] by hand instead, so `dispatch` there stays a
    /// plain send with no synchronous drain, exactly as before this method
    /// existed.
    pub fn wire_pump(&mut self, emulator: SharedEmulator, generation: RwSignal<u64>) {
        self.pump = Some(PumpHandle {
            emulator,
            generation,
        });
    }

    /// Send `emission`'s bytes to the session and wake anything reading
    /// [`revision`](Self::revision).
    ///
    /// **The one place bytes leave the app**, so a caller never talks to the
    /// feed directly: bar taps and soft-keyboard characters take the identical
    /// route out (the latch having already been applied inside the source).
    /// That is what makes it the right place for the read-only gate to be
    /// final: no byte leaves a read-only session, whichever widget derived it.
    ///
    /// # The synchronous echo drain
    ///
    /// Once [`wire_pump`](Self::wire_pump) has run, sending non-empty bytes
    /// also drains the feed **right here**, synchronously — see the
    /// [module docs](self)' section on why. The `feed.borrow_mut()` above
    /// has already dropped by the time this runs (it lives for only the one
    /// statement above it), and [`pump_wake`] takes its own fresh borrows of
    /// `feed` and the pump's emulator — so no `RefCell` borrow is ever held
    /// across the call, the same discipline [`run_feed`]'s own docs require.
    /// This seam carries no counter of its own: [`pump_wake`] is the one
    /// source of truth for "next" (its own doc), deriving it from
    /// `generation` itself on every call — so this synchronous drain and
    /// [`run_feed`]'s own scheduled wake always agree, whichever runs first
    /// (review M-A1).
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
    /// [`read_only`](TerminalGridView::read_only) builder input, threaded here
    /// from its `build`/`rebuild` so the key bar and every other emitter
    /// observe the same one.
    ///
    /// Turning it **on** scrubs: a session that has stopped taking input has
    /// no reason to keep the last thing typed into it (review M6). Idempotent,
    /// so the per-rebuild call costs nothing at a stable posture.
    pub fn set_read_only(&self, read_only: bool) {
        if self.read_only.replace(read_only) != read_only && read_only {
            self.scrub();
        }
    }

    /// Drop every keystroke the source is holding — see
    /// [`KeystrokeSource::scrub`] for what that covers, and the type docs for
    /// the boundaries it is called at.
    ///
    /// Deliberately does **not** mirror the source's revision into
    /// [`revision`](Self::revision): the paint pass calls this (the grid's blur
    /// edge), and a signal write from paint would wake a whole-app rebuild out
    /// of a pass that must not schedule one. Nothing rendered reads the mirror
    /// — only the latch does, and a scrub leaves the latch alone.
    pub fn scrub(&self) {
        self.source.borrow_mut().scrub();
    }

    /// Claim the focus path for this seam, returning the new claim — called by
    /// every widget that requests focus on the session's behalf (the grid's
    /// `Down`, a key cap's `Down`).
    ///
    /// See [`focus_claim`](Self::focus_claim) for what the grid does with it.
    pub fn claim_focus(&self) -> u64 {
        let claim = self.focus_claim.get().wrapping_add(1);
        self.focus_claim.set(claim);
        claim
    }

    /// The current focus claim — the value a widget records when it gains
    /// focus and re-reads when it loses it. An unchanged claim across that
    /// edge means nothing else took the surface, i.e. that really was a blur.
    pub fn focus_claim(&self) -> u64 {
        self.focus_claim.get()
    }

    /// Fold a key-bar press into the shared source and send whatever it
    /// derived, or `None` when the session refuses input.
    ///
    /// The bar's counterpart to [`absorb`](Self::absorb), and for the same
    /// reason: the gate belongs to the seam, not to the cap, so a cap mounted
    /// over a read-only session is inert rather than depending on the screen
    /// having remembered not to mount it.
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
    /// reconciled editing state, an optional caret rect as the platform's
    /// candidate-placement hint, and [`ImeContentType::Terminal`] — the hint
    /// that switches off autocorrect, smart quotes/dashes, smart insert-delete
    /// and autocapitalization, so a typed `"` arrives as `0x22`.
    ///
    /// Spelled once here rather than per widget: the grid and the key bar both
    /// publish it (see [`absorb`](Self::absorb)), and two spellings would let
    /// them disagree about the content type — which is a silent
    /// corrupt-my-shell-command bug, not a cosmetic one.
    ///
    /// `active` is always `true`: this is only ever published by a widget that
    /// holds (or is claiming) focus, and a blur drops the whole surface at the
    /// render root instead.
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
    /// # Why more than one widget calls this
    ///
    /// Frust blurs on any pointer `Down` that claims no focus
    /// (`RenderRoot::event`: "no widget on the tapped path took focus"), and a
    /// blur drops `focus_active` *and* the whole published surface — so a key
    /// bar whose caps claimed no focus would dismiss the very keyboard it is
    /// docked to. Each cap therefore claims focus like the grid does, which
    /// makes it the focus path's owner until the grid is tapped again, which in
    /// turn makes it the widget `Key`/`Ime` events route to.
    ///
    /// That is fine — and it is why this lives on the seam: **whichever widget
    /// holds the focus path drives the same [`KeystrokeSource`] and publishes
    /// the same [`ime_state`](Self::ime_state)**, so the platform sees one
    /// unbroken input surface no matter which one was tapped last.
    ///
    /// A read-only session absorbs nothing at all: the caller reports
    /// `Ignored`, no snapshot reaches the diff engine, and nothing is
    /// buffered.
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
/// paints and tracks the finger), the gesture detector's `on_long_press`
/// callback (which starts it) and the Copy chip (which ends it) — see
/// [`SelectionSeam`].
pub type SharedSelection = Rc<RefCell<SelectionState>>;

/// Everything one selection gesture is: what the grid last measured, where the
/// finger is, which cell the gesture anchored on, and what is selected.
///
/// Private fields with accessors on [`SelectionSeam`] rather than a bag of
/// `pub`s: three widgets read this, and every one of them must go through the
/// same clamping/resolution rules.
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
    /// build — a fresh seam has nothing selected yet, so nothing to clear.
    last_generation: Option<u64>,
}

/// Everything the grid widget, the long-press callback and the Copy chip must
/// agree on to be one selection: the shared [`SelectionState`] and the emulator
/// whose screen resolves words and yields copy text.
///
/// # Why this is an `Rc<RefCell<_>>` and not a signal
///
/// The same reason [`InputSeam`]'s source is: it is mutated during the **event**
/// pass, where there is no reactive owner, and a drag re-resolves both endpoints
/// on every `Move`. A signal write per move would wake one whole-app tracked
/// rebuild per move — 60+ a second while a finger is sliding — to change nothing
/// but pixels. The grid instead asks for a plain `request_redraw`, and a frust
/// paint pass walks the whole tree, so the chip (a sibling, not a child) sees
/// the new state on that very frame with no rebuild involved.
///
/// # The one thing this type does that is not state
///
/// [`begin`](Self::begin) fires the selection-click haptic. It lives here and
/// not in [`selection`] for the same reason the revision signal lives on
/// [`InputSeam`] rather than in [`input`]: the model stays pure and testable,
/// and the one side effect sits at the single choke point every caller reaches
/// it through.
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

    /// Publish the cell box the grid just laid out at — the px→cell mapping
    /// every resolution below goes through. Called from `layout`.
    ///
    /// A held selection is clamped onto the new grid **here**, the instant a
    /// shrink is observed — a rotation, a font-size bump, a fullscreen
    /// collapse (`TerminalGridWidget::layout` resizes the emulator on any
    /// cell-grid delta, then calls this with the geometry it resized to).
    /// This is what stops a stale, off-screen `range` from ever reaching
    /// [`selection::selected_text`] (a u16 underflow inside `vt100`'s own
    /// `contents_between` on a shrink-then-Copy) or an equally stale
    /// [`selection::segments`] paint — [`GridRange::clamp_to`] is a no-op
    /// when the grid didn't shrink, so this costs nothing on the common,
    /// unchanged-size path.
    pub fn set_metrics(&self, metrics: CellGeometry) {
        let mut state = self.state.borrow_mut();
        state.metrics = Some(metrics);
        if let Some(range) = state.range {
            state.range = Some(range.clamp_to(metrics.cols, metrics.rows));
        }
    }

    /// Record the finger at `at` (grid-local logical px), and whether it is
    /// down. Called from every pointer arm of the grid's `event`.
    pub fn track(&self, at: Point, pressed: bool) {
        let mut state = self.state.borrow_mut();
        state.pointer = at;
        state.pressed = pressed;
    }

    /// **The long-press consumer.** Tick, anchor on the cell under the finger,
    /// and select the word there.
    ///
    /// The haptic fires **before** the word is resolved and therefore
    /// unconditionally — including when the finger landed on a separator and
    /// nothing gets selected. That ordering is the incumbent's (its
    /// `_onSelectionStart` ticks, then calls `selectWord`), and it is the right
    /// one: the tick acknowledges *the gesture*, and a hold that silently did
    /// nothing would feel broken rather than empty. The `Result` is ignored —
    /// haptics are cosmetic, and desktop reports `NotAvailable` by design.
    ///
    /// A fire that arrives with **no finger down** (the framework's
    /// fire-on-release delivery path, when no rebuild ran between the threshold
    /// and the lift) resolves the word and goes straight to the released state,
    /// chip and all, rather than leaving a drag live with nothing to end it.
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
    ///
    /// Both ends every time (see [`selection::resolve`]) — nothing is cached,
    /// so reversing direction mid-drag needs no special case.
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

    /// Drop the drag **flag** only — the `Cancel` arm's whole vocabulary
    /// (`docs/CODE_STANDARDS.md`'s interaction semantics: a `Cancel` never
    /// touches app state). The selection itself survives a stolen gesture; the
    /// next tap clears it.
    pub fn cancel_drag(&self) {
        self.state.borrow_mut().dragging = false;
    }

    /// Whether anything is selected or the chip is up — i.e. whether the next
    /// tap is a **clearing** tap.
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

    /// Clear an existing selection, reporting whether there was one.
    ///
    /// **The clearing tap**, spelled as one call so the grid can check it
    /// *before* claiming focus: a tap that dismisses a selection must not also
    /// summon the keyboard.
    pub fn clear_if_active(&self) -> bool {
        let active = self.is_active();
        if active {
            self.clear();
        }
        active
    }

    /// Note that the grid just observed content generation `generation`,
    /// clearing a held selection if the generation moved since the last
    /// observation (see [`selection`]'s "Streaming output clears a held
    /// selection" module docs for why).
    ///
    /// A no-op unless BOTH hold: `generation` actually differs from the last
    /// one this seam saw, and something is currently held (`is_active`) — a
    /// generation bump with no selection has nothing to clear, and a rebuild
    /// carrying the *same* generation (any app-wide rebuild the terminal
    /// screen merely happens to sit under) is not new output and must not
    /// touch a selection just because the widget repainted.
    ///
    /// Called once from [`TerminalGridView::build`](widget::TerminalGridView)
    /// (seeding `last_generation` with nothing to clear yet) and once from
    /// its `rebuild`, every time — this is where the generation the widget
    /// carries is actually observed.
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

    /// The selected text, as the clipboard should receive it (see
    /// [`selection::selected_text`]); empty when nothing is selected.
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

    /// The cell box the grid last laid out at, once it has — a test/diagnostic
    /// accessor (and the only way anything outside the grid can turn a cell
    /// into a pixel).
    pub fn metrics(&self) -> Option<CellGeometry> {
        self.state.borrow().metrics
    }
}

/// Where a **content tap** is reported: the zero-based `(col, row)` cell the
/// finger lifted on, clamped to the rendered grid.
///
/// An `Rc<dyn Fn>` for the same reason [`CopySink`] is one — it is installed
/// from a `build` and called from the *event* pass, where there is no reactive
/// owner and no `&mut State`, so the screen carries whatever it needs to route
/// the cell inside the closure. Optional at the grid
/// ([`TerminalGridView::on_content_tap`](widget::TerminalGridView::on_content_tap)):
/// with none wired a tap claims focus and reports nothing, exactly as it did
/// before this hook existed.
///
/// Cells, not panes — see the [module docs](self)' layout-blindness note.
pub type ContentTapSink = Rc<dyn Fn(u32, u32)>;

/// Where a **wheel step** is reported: one step's direction (`up`) and the
/// zero-based `(col, row)` cell it applies at.
///
/// One call per step, never a batch and never a pixel amount: the grid does the
/// px→line accumulation (see
/// [`TerminalGridView::on_wheel`](widget::TerminalGridView::on_wheel)) so a
/// touch drag and a desktop wheel arrive at the screen in the identical
/// vocabulary — the same one `muxrd`'s own scroll RPC speaks.
pub type WheelSink = Rc<dyn Fn(bool, u32, u32)>;

/// The whole interactive terminal: the grid, its long-press gesture, and the
/// Copy chip over it.
///
/// ```text
/// Stack
///  ├─ GestureDetector(on_long_press = seam.begin)   ← bottom
///  │    └─ TerminalGridView (selection wired)
///  └─ CopyChipView                                  ← top, full-bleed
/// ```
///
/// # Three things this assembly decides
///
/// 1. **The chip is above the gesture detector**, so a press on the pill never
///    reaches the grid: tapping `Copy` cannot clear the very selection it is
///    copying, and cannot start a fresh long-press.
/// 2. **`on_long_press` is the only gesture wired.** Frust's detector fires it
///    at the threshold in wall time (a paint latches a `Housekeeping` flush and
///    the next rebuild delivers it), so a perfectly stationary hold selects with
///    no movement at all. No `on_tap` and no drag handler: the grid runs its own
///    three-way tap/drag-scroll/long-press discrimination ([`widget`]'s touch
///    state machine) off the raw phases the detector forwards, because a tap has
///    to be checked against a clearing selection and a focus claim the detector
///    knows nothing about. Both recognisers use the one
///    [`TOUCH_SLOP`](frust::input::TOUCH_SLOP), so a press cannot be a scroll
///    here and a long-press there.
/// 3. **The surface takes the whole offered box**, because the chip overlay is
///    full-bleed. The grid still takes only its own whole-cell box inside it and
///    still paints its own background, so the sub-cell remainder is the same
///    tight-parent case [`TerminalGridWidget`]'s own `layout` documents — not a
///    visible seam.
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

/// A VT emulator sized to the grid on screen.
///
/// A thin wrapper over `vt100::Parser`, and the *only* place in the app that
/// names that crate's `(rows, cols)` argument order — everything above speaks
/// `(cols, rows)`, like the rest of the app's geometry.
pub struct TerminalEmulator {
    parser: vt100::Parser,
    /// A resize the layout pass derived and the session has not been told
    /// about yet. Drained by whatever owns the feed (the cutover task's
    /// screen), which is what turns a rotation into a real `SIGWINCH`.
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
    /// [`DEFAULT_SCROLLBACK`] — the size it holds for the one frame between
    /// mounting and the first `layout`, which derives the real one.
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

    /// Resize the screen to `cols` x `rows`, reporting whether anything
    /// changed.
    ///
    /// `false` — the common case, once per layout pass at a stable size — does
    /// nothing at all, which matters because this runs inside `layout`. A real
    /// change also records a [`pending_resize`](Self::take_pending_resize) for
    /// the session.
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
/// anything arrived, publish **exactly one** new generation.
///
/// Returns whether the generation moved. Split out of [`run_feed`] so the
/// one-write-per-wake rule is testable without a timer: the whole coalescing
/// mechanism lives in this function, and the loop around it only sleeps.
///
/// Two callers share this one wake — [`run_feed`]'s scheduled loop, and
/// [`InputSeam::dispatch`]'s synchronous echo drain (see the [module
/// docs](self)) — and both write the same `generation` signal. **This
/// function, not either caller, is the one source of truth for "next":** it
/// derives the value itself, `generation.get_untracked() + 1`, right here,
/// so neither caller carries a counter of its own across wakes (review
/// M-A1). An earlier version took a `&mut u64 published` from each caller
/// instead — `run_feed`'s own copy could fall out of step with a generation
/// [`InputSeam::dispatch`] had just published between two of `run_feed`'s
/// wakes, so its next wake recomputed the *same* next value a second time
/// (a duplicate the tracked rebuild never saw, dropping that wake's freshly
/// fed content) or, across two dispatches, a value *lower* than what was
/// already live.
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

/// The UI-thread feed loop: wake every [`REPAINT_INTERVAL`], drain everything
/// due, publish one generation, repeat until the feed closes or `cancelled`
/// flips.
///
/// Every `await` point releases its borrows first — a `RefCell` borrow held
/// across a suspension would panic the moment the grid laid out.
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
/// The one seam a caller has for *surfacing* whatever the feed knows beyond
/// its bytes — a live `muxrd` session's connection status and its minted
/// connection id, neither of which is on the
/// [`TerminalFeed`](crate::features::terminal::domain::feed::TerminalFeed)
/// port (deliberately: a feed writes no signal and touches no reactive state,
/// so a status a screen renders has to be *polled* from the side that may
/// write signals). Mirroring it here rather than on a timer of its own costs
/// nothing: this loop is already awake, on the right thread, at the rate the
/// screen repaints.
///
/// `on_tick` runs with **no borrow held** on either the feed or the emulator,
/// so an observer may borrow them itself — including replacing the feed
/// outright, which is what a session re-attach does. It also runs on the wake
/// that ends the loop, *after* the open/closed decision is taken: a feed that
/// has just finished publishes its final status before nothing polls it again.
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

/// Start [`run_feed`] under the current reactive owner, cancelling it on
/// teardown.
///
/// Cancellation is an `on_cleanup` registration, never a `Drop` impl
/// (`docs/CODE_STANDARDS.md`'s State & Reactivity conventions): the loop stops
/// at its next wake, and — because the flag is checked *before* the drain — a
/// component torn down between wakes never writes its signal again.
/// `Arc<AtomicBool>` rather than `Rc<Cell<_>>` because `on_cleanup` takes a
/// `Send + Sync` closure, even though both ends live on the UI thread.
pub fn spawn_feed_pump(feed: SharedFeed, emulator: SharedEmulator, generation: RwSignal<u64>) {
    spawn_feed_pump_watched(feed, emulator, generation, || {});
}

/// [`spawn_feed_pump`] over [`run_feed_watched`] — the same ownership and
/// cancellation contract, plus the per-wake observer that function documents.
///
/// `on_tick` is not `Send`: the loop it rides in is spawned on the UI thread's
/// own queue, which is exactly what lets an observer hold the `Rc`-backed feed
/// handle it is polling.
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

    /// A queue-driven [`TerminalFeed`] fake for the pump-hygiene tests below:
    /// a test [`push`](Self::push)es exactly the chunks it wants due on the
    /// next [`pump_wake`] call, and [`TerminalFeed::send_input`] echoes
    /// synchronously — the same PTY-style contract `data::feeds::DemoFeed`
    /// implements, minus its own wall-clock/fixture-replay machinery, which
    /// these tests (review M-A1's interleaving regressions) have no use for.
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

    // -----------------------------------------------------------------------
    // Review M-A1: one source of truth for the generation counter
    // -----------------------------------------------------------------------
    //
    // Both `run_feed`'s scheduled loop and `InputSeam::dispatch`'s
    // synchronous echo drain call `pump_wake` against the same `generation`
    // signal. These pin that the two can no longer disagree about "next" —
    // see `pump_wake`'s own doc for the defect this replaces.

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

        // A keystroke lands mid-replay: `InputSeam::dispatch`'s own
        // synchronous echo drain — the seam's writer.
        input.dispatch(&Emission {
            bytes: b"q".to_vec(),
            ..Default::default()
        });
        let second = generation.get_untracked();
        assert!(
            second > first,
            "the keystroke's own echo must move the counter: {first} -> {second}"
        );

        // Wake 2 — `run_feed`'s next scheduled wake, with more real replay
        // content due. Under the pre-fix bug this recomputed `second` a
        // second time (both callers derived "next" from their own stale
        // local rather than from `generation` itself), publishing a
        // duplicate.
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
        // Seed the seam's last-seen generation exactly as
        // `TerminalGridView::build` does on the grid's first build — nothing
        // to clear yet.
        selection.observe_generation(generation.get_untracked());

        // Select the word "hello" at (row 0, col 0) — the panel's probe
        // scenario re-arms this same word after each clear below.
        let arm = |selection: &SelectionSeam| {
            selection.track(Point::new(4.0, 8.0), true);
            selection.begin();
            assert!(selection.range().is_some(), "sanity: the word armed");
        };

        // Wake 1 (`run_feed`-style): new content arrives before anything is
        // selected — a no-op on the selection, but it moves the generation
        // the seam will compare its next observation against.
        feed.borrow_mut().push(b"more output");
        assert!(pump_wake(&feed_dyn, &emulator, generation));
        selection.observe_generation(generation.get_untracked());

        arm(&selection);

        // A keystroke lands: `InputSeam::dispatch`'s own synchronous echo
        // drain is the SECOND writer to this same generation signal.
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

        // Wake 2 (`run_feed`-style, again). Under the pre-fix bug this
        // repeated the dispatch's own generation value, so
        // `observe_generation`'s `!=` check saw no change and left the
        // re-armed selection alone — exactly the hazard `selection`'s module
        // docs forbid.
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
