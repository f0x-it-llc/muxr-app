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
//! | [`widget`] | the retained grid: shaping in `layout`, replay in `paint`, the caret |
//! | this module | the emulator wrapper, the 30 Hz feed pump, and [`CursorStyle`] |
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

pub mod batch;
pub mod geometry;
pub mod style;
pub mod widget;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use frust::{RwSignal, Set, on_cleanup, spawn_blocking, spawn_local};

use crate::features::terminal::domain::feed::TerminalFeed;

pub use geometry::{FALLBACK_COLS, FALLBACK_ROWS};
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
pub fn pump_wake(
    feed: &RefCell<dyn TerminalFeed>,
    emulator: &RefCell<TerminalEmulator>,
    generation: RwSignal<u64>,
    published: &mut u64,
) -> bool {
    let writes = {
        let mut emulator = emulator.borrow_mut();
        feed.borrow_mut()
            .drain_output(&mut |bytes| emulator.feed(bytes))
    };
    if writes == 0 {
        return false;
    }
    *published += 1;
    generation.set(*published);
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
    let start = Instant::now();
    let mut published = 0u64;
    let mut tick = 0u32;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return;
        }
        pump_wake(&feed, &emulator, generation, &mut published);
        if !feed.borrow().is_open() {
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
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = Arc::clone(&cancelled);
        on_cleanup(move || cancelled.store(true, Ordering::Relaxed));
    }
    spawn_local(run_feed(feed, emulator, generation, cancelled));
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
