//! Headless end-to-end coverage for the terminal **keystroke source**: the grid
//! widget as an IME client, the last-known-state diff behind it, the one-shot
//! Ctrl/Alt latch, and the key bar — no GPU, no window, no device.
//!
//! Every test here drives the **real seam**: a `RenderRoot` with the grid
//! mounted, a tap that claims focus, and then `InputEvent::Ime`/`InputEvent::Key`
//! events shaped exactly as each platform bridge delivers them
//! (`ApplyEditingState` snapshots on mobile, `Key::Named(Enter)` for Android's
//! `IME_ACTION_DONE`, `Commit`/`Compose` on desktop). What it asserts on is the
//! other end of the pipe: the **bytes that arrived in the session**, read back
//! out of `DemoFeed`'s PTY-style echo and compared as hex.
//!
//! The pure half of the model — the diff algebra, the latch table, the rolling
//! window's publish count — is unit-tested next to the code, in
//! `presentation::term::input`. This suite exists to prove the wiring: that a
//! tap really publishes an `ImeContentType::Terminal` surface, that a snapshot
//! really reaches the source through the focus path, and that the bar and the
//! soft keyboard really share one latch.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use frust::authoring::{
    EditingState, ImeContentType, ImeEvent, InputEvent, Key, KeyEvent, Modifiers, NamedKey,
};
use frust::{
    AnyView, Axis, Component, FlexView, Get, RwSignal, Set, any, component, flexible, inflexible,
};
use frust_core::RenderRoot;
use frust_core::accesskit::Toggled;
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::features::terminal::data::feeds::demo_feed::DemoFeed;
use muxr::features::terminal::domain::feed::TerminalFeed;
use muxr::features::terminal::presentation::keybar::key_bar;
use muxr::features::terminal::presentation::term::{
    InputSeam, SharedEmulator, SharedFeed, TerminalEmulator, TerminalPalette, pump_wake,
    terminal_grid,
};

mod support;
use support::{frame_at_sized, ms, node_center_by_label, node_labels, pointer, serial, setup};

/// The font size every test lays out at — the app's terminal default.
const FONT_SIZE: f32 = 13.0;

/// A phone-shaped surface, in logical px.
const PHONE: Size = Size::new(392.7, 872.7);

/// The grid's accessible name.
const LABEL: &str = "dev \u{b7} zsh";

/// A point comfortably inside the grid (which takes every row above the bar).
const IN_GRID: Point = Point::new(120.0, 120.0);

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// The one-screen component under test: the grid over a demo feed, with the key
/// bar docked under it.
///
/// **Two children, always** — the bar collapses to zero height rather than
/// unmounting, exactly as a real screen must mount it (see `keybar`'s module
/// docs on positional reconciliation and the focus it would otherwise drop).
struct TermScreen {
    seam: InputSeam,
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
    bar_active: RwSignal<bool>,
    read_only: bool,
}

impl Component for TermScreen {
    type State = ();

    fn init(&self) -> Self::State {}

    fn build(&self, _state: &mut Self::State) -> AnyView<Self::State> {
        let generation = self.generation.get();
        // A read-only terminal has no keyboard, and therefore no bar — the
        // wiring a real screen owns, modelled here.
        let active = self.bar_active.get() && !self.read_only;
        let grid = terminal_grid(
            Rc::clone(&self.emulator),
            generation,
            TerminalPalette::fallback(),
            FONT_SIZE,
        )
        .label(LABEL)
        .input(self.seam.clone())
        .read_only(self.read_only);

        any(FlexView::new(
            Axis::Vertical,
            vec![
                flexible(1, grid),
                inflexible(key_bar::<Self::State>(active, &self.seam)),
            ],
        ))
    }
}

type Logic = Box<dyn FnMut(&mut ()) -> AnyView<()>>;

/// The mounted screen plus the handles a test needs to poke and read it.
struct Mounted {
    root: RenderRoot<(), AnyView<()>>,
    logic: Logic,
    state: (),
    tcx: TextContext,
    feed: Rc<RefCell<DemoFeed>>,
    /// The same feed the seam emits into, type-erased — what
    /// [`pump_wake`](muxr::features::terminal::presentation::term::pump_wake)
    /// takes.
    shared_feed: SharedFeed,
    /// The emulator the mounted grid paints: where an echoed keystroke lands.
    emulator: SharedEmulator,
    /// The pump's repaint generation, and how many it has published.
    generation: RwSignal<u64>,
    published: u64,
    seam: InputSeam,
    bar_active: RwSignal<bool>,
}

impl Mounted {
    fn new() -> Self {
        Self::with_read_only(false)
    }

    fn with_read_only(read_only: bool) -> Self {
        let feed = Rc::new(RefCell::new(DemoFeed::manual()));
        let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
        let seam = InputSeam::new(Rc::clone(&feed) as SharedFeed);
        let generation = RwSignal::new(0u64);
        let bar_active = RwSignal::new(false);

        let screen_seam = seam.clone();
        let screen_emulator = Rc::clone(&emulator);
        let logic: Logic = Box::new(move |_state: &mut ()| {
            any(component(TermScreen {
                seam: screen_seam.clone(),
                emulator: Rc::clone(&screen_emulator),
                generation,
                bar_active,
                read_only,
            }))
        });

        let mut root = RenderRoot::new();
        root.set_theme(Box::new(frust::Theme::glyph_baseline()));
        let mut mounted = Mounted {
            root,
            logic,
            state: (),
            tcx: TextContext::new(),
            shared_feed: Rc::clone(&feed) as SharedFeed,
            feed,
            emulator,
            generation,
            published: 0,
            seam,
            bar_active,
        };
        mounted.frame();
        // The replay's chunk 0 is due at t=0; drain it once so every later
        // drain is the echo of what this suite typed and nothing else.
        mounted.drain();
        mounted
    }

    /// One laid-out, painted frame at the phone surface.
    fn frame(&mut self) {
        frame_at_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            PHONE,
            ms(0.0),
        );
    }

    /// Tap `at` (a `Down` claims focus; the `Up` completes the gesture).
    fn tap(&mut self, at: Point) {
        self.root.event(
            &mut self.state,
            &pointer(frust_core::PointerPhase::Down, at),
        );
        self.root
            .event(&mut self.state, &pointer(frust_core::PointerPhase::Up, at));
        self.frame();
    }

    /// Deliver one mobile snapshot down the focus path, exactly as
    /// `nativeImeApply`/`imeRoundTrip` do.
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

    /// Type `text` the way a soft keyboard does: one snapshot per character,
    /// each built on **the mirror the widget currently publishes** (both bridges
    /// apply a re-anchor synchronously — see `term::input`'s module docs).
    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let mut buffer = self.mirror();
            buffer.push(ch);
            self.apply(&buffer);
        }
    }

    /// The editing state the widget currently publishes to the platform.
    fn published(&self) -> EditingState {
        self.seam.published()
    }

    fn mirror(&self) -> String {
        self.published().text
    }

    /// Deliver one focus-routed key event.
    fn key(&mut self, key: Key) {
        self.root.event(
            &mut self.state,
            &InputEvent::Key(KeyEvent {
                key,
                modifiers: Modifiers::default(),
                repeat: false,
            }),
        );
    }

    /// Everything the session has been sent since the last drain, as one byte
    /// run (the demo feed echoes a wake's whole input as one write).
    fn drain(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        self.feed
            .borrow_mut()
            .drain_output(&mut |bytes| out.extend_from_slice(bytes));
        out
    }

    /// [`drain`](Self::drain) as lowercase hex — how every byte assertion here
    /// reads.
    fn drain_hex(&mut self) -> String {
        self.drain().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// One wake of task 08's real feed pump: drain everything due into the
    /// mounted grid's emulator and publish at most one generation.
    fn pump(&mut self) -> bool {
        pump_wake(
            &self.shared_feed,
            &self.emulator,
            self.generation,
            &mut self.published,
        )
    }

    /// Show (or hide) the key bar, as a screen would from the published
    /// `ImeState.active`.
    fn set_bar(&mut self, active: bool) {
        self.bar_active.set(active);
        self.frame();
    }

    /// The `Toggled` state of the semantics node labelled `label`.
    fn toggled(&self, label: &str) -> Option<Toggled> {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
            .and_then(|(_, node)| node.toggled())
    }

    /// Tap the key-bar cap named `label`.
    fn tap_cap(&mut self, label: &str) {
        let at = node_center_by_label(&self.root, label)
            .unwrap_or_else(|| panic!("no key-bar cap labelled {label:?}"));
        self.tap(at);
    }
}

// ---------------------------------------------------------------------------
// IME clienthood
// ---------------------------------------------------------------------------

#[test]
fn tapping_the_grid_publishes_a_terminal_ime_surface() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();

    assert!(
        mounted.root.ime_state().is_none(),
        "an untapped grid asks for no keyboard"
    );

    mounted.tap(IN_GRID);
    let ime = mounted
        .root
        .ime_state()
        .expect("the tap must claim focus and publish a surface");
    assert!(ime.active, "…and raise the keyboard");
    assert_eq!(
        ime.content_type,
        ImeContentType::Terminal,
        "the hint that switches off autocorrect, smart quotes/dashes and \
         autocapitalization platform-side"
    );
    assert_eq!(ime.editing.text, "", "a fresh source publishes nothing yet");
}

#[test]
fn the_published_state_never_carries_an_invented_character() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.type_text("ls -la");

    let published = mounted.published();
    assert_eq!(published.text, "ls -la");
    assert!(
        !published.text.contains('\u{200b}'),
        "no sentinel: every character in the mirror is one the user typed"
    );
    assert_eq!(published.composing_base, -1);
}

// ---------------------------------------------------------------------------
// The bytes, end to end
// ---------------------------------------------------------------------------

#[test]
fn typed_characters_reach_the_feed_byte_for_byte() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    mounted.type_text("ls");
    assert_eq!(mounted.drain_hex(), "6c73");

    // The two rewrites a smart keyboard would have made, and must not:
    // a straight double quote, and two hyphens (never an em dash).
    mounted.type_text("\"");
    assert_eq!(mounted.drain_hex(), "22");
    mounted.type_text("--");
    assert_eq!(mounted.drain_hex(), "2d2d");
}

#[test]
fn a_swipe_typed_word_arrives_as_one_contiguous_burst() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    // The zero-composing norm: a whole word committed in ONE snapshot.
    mounted.apply("status");
    assert_eq!(mounted.drain_hex(), "737461747573");
}

#[test]
fn both_platforms_return_paths_send_carriage_return() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    // iOS: a literal "\n" appears inside the snapshot.
    mounted.type_text("a\n");
    assert_eq!(mounted.drain_hex(), "610d");

    // Android: `IME_ACTION_DONE` arrives as a focus-routed Key(Enter).
    mounted.key(Key::Named(NamedKey::Enter));
    assert_eq!(mounted.drain_hex(), "0d");
}

#[test]
fn a_backspace_reaches_the_session_exactly_once_on_either_platform() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.type_text("ab");
    mounted.drain();

    // Android: the key event AND the echoed shortening.
    mounted.key(Key::Named(NamedKey::Backspace));
    mounted.apply("a");
    assert_eq!(mounted.drain_hex(), "7f", "one byte, not two");

    // iOS: the shortening alone.
    mounted.apply("");
    assert_eq!(mounted.drain_hex(), "7f");
}

#[test]
fn typed_bytes_appear_in_the_grid_through_the_feeds_echo() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.type_text("hi");

    // The whole pipe, end to end and through the *real* pump: the bytes the
    // keystroke source emitted went out through `send_input`, came back on the
    // demo feed's PTY-style echo, and were parsed by the emulator the mounted
    // grid paints.
    assert!(mounted.pump(), "the echo is due, so the pump publishes");
    let emulator = Rc::clone(&mounted.emulator);
    let emulator = emulator.borrow();
    let screen = emulator.screen();
    let (row, col) = screen.cursor_position();
    assert_eq!((row, col), (0, 2), "two cells printed, caret past them");
    let cells: String = (0..2)
        .filter_map(|col| screen.cell(0, col).map(|cell| cell.contents()))
        .collect();
    assert_eq!(cells, "hi", "the typed characters are on the grid");
}

#[test]
fn a_composing_range_emits_nothing_and_leaves_the_published_state_alone() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.type_text("a");
    mounted.drain();

    let composing = EditingState {
        text: "ani".to_string(),
        selection_base: 3,
        selection_extent: 3,
        composing_base: 1,
        composing_extent: 3,
    };
    mounted.root.event(
        &mut mounted.state,
        &InputEvent::Ime(ImeEvent::ApplyEditingState(composing.clone())),
    );
    assert!(mounted.drain().is_empty(), "marked text is not committed");
    assert_eq!(
        mounted.published(),
        composing,
        "the surface echoes the composition verbatim, so republishing it \
         cannot destroy the platform's composition state"
    );

    // Committing diffs against the untouched baseline: one burst, no deletes.
    mounted.apply("a\u{4f60}");
    assert_eq!(mounted.drain_hex(), "e4bda0");
}

// ---------------------------------------------------------------------------
// The key bar
// ---------------------------------------------------------------------------

#[test]
fn the_key_bar_is_absent_until_the_ime_is_up() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    let resting = node_labels(&mounted.root);
    assert!(
        !resting.iter().any(|label| label == "Escape"),
        "the resting screen is bar-free: {resting:?}"
    );

    mounted.set_bar(true);
    let raised = node_labels(&mounted.root);
    for name in ["Control", "Alt", "Escape", "Tab", "Up arrow", "Delete"] {
        assert!(
            raised.iter().any(|label| label == name),
            "{name} missing from {raised:?}"
        );
    }

    mounted.set_bar(false);
    assert!(
        !node_labels(&mounted.root)
            .iter()
            .any(|label| label == "Escape"),
        "…and it collapses again when the keyboard goes"
    );
}

#[test]
fn a_key_bar_tap_sends_its_sequence_and_keeps_the_grid_focused() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.set_bar(true);
    mounted.drain();

    mounted.tap_cap("Escape");
    assert_eq!(mounted.drain_hex(), "1b");

    mounted.tap_cap("Up arrow");
    assert_eq!(mounted.drain_hex(), "1b5b41");

    mounted.tap_cap("Delete");
    assert_eq!(mounted.drain_hex(), "1b5b337e", "forward delete, not \\x7f");

    let ime = mounted
        .root
        .ime_state()
        .expect("a bar tap must never dismiss the keyboard");
    assert!(ime.active);
    assert_eq!(ime.content_type, ImeContentType::Terminal);

    // Typing still lands: the cap that now holds the focus path folds the
    // snapshot into the same source the grid would have.
    mounted.type_text("q");
    assert_eq!(mounted.drain_hex(), "71");

    // …and tapping back into the grid hands the focus path straight back.
    mounted.tap(IN_GRID);
    mounted.type_text("z");
    assert_eq!(mounted.drain_hex(), "7a");
}

#[test]
fn the_ctrl_latch_transforms_the_next_soft_keyboard_character() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.set_bar(true);
    mounted.drain();

    mounted.tap_cap("Control");
    assert!(
        mounted.drain().is_empty(),
        "a latch key sends nothing of its own"
    );
    assert_eq!(
        mounted.toggled("Control"),
        Some(Toggled::True),
        "the cap shows itself armed"
    );

    // …and now a character typed on the OS keyboard — not on the bar — is the
    // one the latch transforms. This is the whole reason the latch lives at the
    // emission choke point.
    mounted.type_text("c");
    assert_eq!(mounted.drain_hex(), "03");

    mounted.frame();
    assert_eq!(
        mounted.toggled("Control"),
        Some(Toggled::False),
        "one shot: the latch disarmed itself, and the bar noticed"
    );
}

#[test]
fn an_arrow_cap_leaves_the_latch_armed_for_the_character_after_it() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.set_bar(true);
    mounted.drain();

    mounted.tap_cap("Control");
    mounted.tap_cap("Left arrow");
    assert_eq!(mounted.drain_hex(), "1b5b44", "the sequence passes through");
    assert_eq!(
        mounted.toggled("Control"),
        Some(Toggled::True),
        "a multi-unit emission does not consume the latch"
    );
    mounted.type_text("a");
    assert_eq!(mounted.drain_hex(), "01", "Ctrl+a");
}

// ---------------------------------------------------------------------------
// Read-only
// ---------------------------------------------------------------------------

#[test]
fn a_read_only_grid_is_never_an_ime_client_and_emits_nothing() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::with_read_only(true);

    mounted.tap(IN_GRID);
    assert!(
        mounted.root.ime_state().is_none(),
        "a read-only grid claims no focus and publishes no surface"
    );

    // The focus gate IS the read-only gate, but prove the bytes too: an event
    // delivered straight at the root must still derive nothing.
    mounted.apply("ls");
    mounted.key(Key::Named(NamedKey::Enter));
    assert!(mounted.drain().is_empty(), "and it emits nothing at all");

    // No keyboard, so no bar — even when the screen asks for one.
    mounted.set_bar(true);
    assert!(
        !node_labels(&mounted.root)
            .iter()
            .any(|label| label == "Escape"),
        "a read-only terminal has no key bar"
    );
}

// ---------------------------------------------------------------------------
// The rolling window, through the real seam
// ---------------------------------------------------------------------------

#[test]
fn a_long_line_stays_byte_correct_across_a_window_re_anchor() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    // Comfortably past the mirror cap, so at least one re-anchor fires.
    let line: String = (0..200)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    mounted.type_text(&line);

    assert_eq!(
        mounted.drain(),
        line.as_bytes(),
        "every keystroke reached the session, in order, across the trim"
    );
    assert!(
        line.contains(&mounted.mirror()),
        "the republished window is a slice of what was actually typed"
    );

    // The trim did not cost the user their backspace: deleting into the
    // re-anchored window still emits.
    let mut shorter = mounted.mirror();
    shorter.pop();
    mounted.apply(&shorter);
    assert_eq!(
        mounted
            .drain()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        "7f"
    );
}

// ---------------------------------------------------------------------------
// Feed plumbing (the pump still owns output; input is the other direction)
// ---------------------------------------------------------------------------

#[test]
fn input_and_output_share_one_feed_without_interleaving() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.type_text("x");

    // Let the replay's own chunks come due alongside the echo: the echo is
    // still its own write, and still first (the demo feed's documented order).
    mounted
        .feed
        .borrow_mut()
        .set_elapsed(Duration::from_millis(200));
    let mut writes: Vec<Vec<u8>> = Vec::new();
    mounted
        .feed
        .borrow_mut()
        .drain_output(&mut |bytes| writes.push(bytes.to_vec()));
    assert_eq!(writes.first().map(Vec::as_slice), Some(b"x".as_slice()));
    assert!(writes.len() > 1, "the replay kept flowing too");
}
