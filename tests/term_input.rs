//! Headless end-to-end coverage for the terminal **keystroke source**: the grid
//! widget as an IME client, the last-known-state diff behind it, the one-shot
//! Ctrl/Alt/Shift latch, and the key bar — no GPU, no window, no device.
//!
//! Every test drives the **real seam**: a `RenderRoot` with the grid mounted, a tap
//! that claims focus, then `InputEvent::Ime`/`InputEvent::Key` events shaped exactly
//! as each platform bridge delivers them (`ApplyEditingState` snapshots on mobile,
//! `Key::Named(Enter)` for Android's `IME_ACTION_DONE`, `Commit`/`Compose` on
//! desktop). What it asserts on is the other end of the pipe: the **bytes that
//! arrived in the session**, read back out of `DemoFeed`'s PTY-style echo and
//! compared as hex. The pure half is unit-tested next to the code; this suite proves
//! the wiring.
//!
//! It also pins the buffer **security contracts** where they are wired: a Return
//! leaves no `\n` in the buffers and the next keystroke diffs against an empty
//! baseline; a **stale** snapshot arriving into that baseline replays nothing, with
//! or without the Return, word-shaped or not, and on **every** frame of that window
//! rather than only the first; a whole-buffer replacement sends nothing and
//! re-publishes; the buffers scrub on a blur and on unmount but *not* on a key bar
//! hand-off, which is the same session still being typed into; and read-only is a
//! property of the session, so a cap over one is inert too.
//!
//! One thing here is **geometry** rather than bytes: the bar's row budget is only
//! true of a laid-out row, so the twelve caps are measured at the narrowest phone
//! width they must fit.

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
use muxr::features::terminal::presentation::keybar::{
    CAP_HEIGHT, CAP_PAD_X, ICON_SIZE, LABEL_SIZE, key_bar,
};
use muxr::features::terminal::presentation::term::{
    BarKey, InputSeam, SharedEmulator, SharedFeed, TerminalEmulator, TerminalPalette, pump_wake,
    terminal_grid,
};

mod support;
use support::{
    EPSILON, RecScene, frame_at_sized, ms, node_center_by_label, node_labels, pointer, serial,
    setup,
};

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
/// bar docked under it. **Two children, always** — the bar collapses to zero
/// height rather than unmounting, exactly as a real screen must mount it.
struct TermScreen {
    seam: InputSeam,
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
    bar_active: RwSignal<bool>,
    read_only: bool,
    /// Mount the bar even over a read-only grid. Not a shape any real screen
    /// has — it exists to prove the **gate**, which lives on the seam rather
    /// than on this wiring: a cap that somehow got mounted over a read-only
    /// session must still be inert.
    force_bar: bool,
}

impl Component for TermScreen {
    type State = ();

    fn init(&self) -> Self::State {}

    fn build(&self, _state: &mut Self::State) -> AnyView<Self::State> {
        let generation = self.generation.get();
        // A read-only terminal has no keyboard, and therefore no bar — the
        // wiring a real screen owns, modelled here.
        let active = self.bar_active.get() && (!self.read_only || self.force_bar);
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
    /// The pump's repaint generation — [`pump_wake`] derives its own "next"
    /// from this signal, so no separate published-counter field is needed
    /// here any more.
    generation: RwSignal<u64>,
    seam: InputSeam,
    bar_active: RwSignal<bool>,
    /// Whether the screen is still on the tree — cleared by
    /// [`unmount`](Self::unmount), the way `tests/terminal_live.rs` unmounts
    /// its own screen.
    mounted: Rc<std::cell::Cell<bool>>,
}

impl Mounted {
    fn new() -> Self {
        Self::mount(false, false)
    }

    fn with_read_only(read_only: bool) -> Self {
        Self::mount(read_only, false)
    }

    /// A read-only grid **with the key bar mounted over it** — see
    /// [`TermScreen::force_bar`].
    fn read_only_with_bar() -> Self {
        Self::mount(true, true)
    }

    fn mount(read_only: bool, force_bar: bool) -> Self {
        let feed = Rc::new(RefCell::new(DemoFeed::manual()));
        let emulator: SharedEmulator = Rc::new(RefCell::new(TerminalEmulator::with_defaults()));
        let seam = InputSeam::new(Rc::clone(&feed) as SharedFeed);
        let generation = RwSignal::new(0u64);
        let bar_active = RwSignal::new(false);
        let mounted = Rc::new(std::cell::Cell::new(true));

        let screen_seam = seam.clone();
        let screen_emulator = Rc::clone(&emulator);
        let logic_mounted = Rc::clone(&mounted);
        let logic: Logic = Box::new(move |_state: &mut ()| {
            if !logic_mounted.get() {
                return any(frust::SizedBox::<()>(Some(0.0), Some(0.0)));
            }
            any(component(TermScreen {
                seam: screen_seam.clone(),
                emulator: Rc::clone(&screen_emulator),
                generation,
                bar_active,
                read_only,
                force_bar,
            }))
        });

        let mut root = RenderRoot::new();
        root.set_theme(Box::new(frust_glyph::baseline()));
        let mut mounted = Mounted {
            root,
            logic,
            state: (),
            tcx: TextContext::new(),
            shared_feed: Rc::clone(&feed) as SharedFeed,
            feed,
            emulator,
            generation,
            seam,
            bar_active,
            mounted,
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

    /// One wake of the real feed pump: drain everything due into the mounted
    /// grid's emulator and publish at most one generation.
    fn pump(&mut self) -> bool {
        pump_wake(&self.shared_feed, &self.emulator, self.generation)
    }

    /// One frame at an explicit surface `width`, returning what it painted —
    /// the row-budget assertion's only handle on the bar, since a cap's width is
    /// decided by the row it is laid out in rather than by the cap.
    fn frame_at_width(&mut self, width: f64) -> RecScene {
        frame_at_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            Size::new(width, PHONE.height),
            ms(0.0),
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

    /// Tap somewhere that claims no focus, which is what frust reads as a
    /// **blur**. The bar's own left padding: on the bar's band, so the tap
    /// lands, but left of the first cap, so nothing claims. Requires the bar to
    /// be up — its caps are how the band's `y` is found.
    fn tap_outside_everything(&mut self) {
        let cap = node_center_by_label(&self.root, "Control")
            .expect("the bar must be up to find the band");
        self.tap(Point::new(1.0, cap.y));
    }

    /// How many UTF-16 units of the user's own typing the shared source is
    /// still holding — the content-free scrub assertion (see
    /// `KeystrokeSource::buffered_units`).
    fn buffered_units(&self) -> usize {
        self.seam.source.borrow().buffered_units()
    }

    /// Unmount the screen, exactly as `tests/terminal_live.rs` does: the logic
    /// closure returns a different view, so the whole element tree — the grid
    /// included — is torn down.
    fn unmount(&mut self) {
        self.mounted.set(false);
        self.frame();
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
fn a_return_leaves_no_newline_behind_and_the_next_key_starts_clean() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    mounted.type_text("ls -la\n");
    assert_eq!(mounted.drain_hex(), "6c73202d6c610d", "`ls -la`, then CR");

    // CONTRACT (guard 1): the submitted line is dropped rather than
    // kept as the next diff's baseline, so the literal `\n` iOS accumulates
    // can never be replayed to the host by a bulk re-emission.
    assert!(
        !mounted.mirror().contains('\n'),
        "no newline survives a Return: {:?}",
        mounted.mirror()
    );
    assert_eq!(mounted.mirror(), "", "…because nothing survives one");
    assert_eq!(mounted.buffered_units(), 0);

    // …and the platform, having reconciled to that empty published state
    // inside the same round trip, diffs its next keystroke against nothing.
    mounted.type_text("x");
    assert_eq!(
        mounted.drain_hex(),
        "78",
        "one byte, not the whole line again"
    );
}

#[test]
fn a_stale_post_return_snapshot_never_replays_the_submitted_line() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    // Positive precondition: the line really was typed and really was sent.
    mounted.type_text("ls\n");
    assert_eq!(mounted.drain_hex(), "6c730d", "`ls`, then CR");
    assert_eq!(mounted.mirror(), "", "the baseline was dropped");

    // CONTRACT (guard 1's other half). Dropping OUR baseline does not drop the
    // platform's: a round trip that errors, answers nil or is a frame late
    // leaves it holding the pre-Return text. This snapshot is therefore built
    // RAW — from the platform's stale buffer, not from `mirror()`, which is
    // exactly what would hide this. Diffed against nothing it reads as one
    // `added` run carrying the whole line and its `\n`, which `text_bytes` maps
    // to CR: emitted, `ls` runs a second time on the remote host.
    mounted.apply("ls\n");
    assert!(
        mounted.drain().is_empty(),
        "a stale snapshot replays NOTHING to the session"
    );
    assert_eq!(
        mounted.mirror(),
        "",
        "…and the empty baseline is re-published, pulling the platform back"
    );

    // The session is still live, and the one shot is spent: ordinary typing
    // works, and the next real Return still submits.
    mounted.type_text("x");
    assert_eq!(mounted.drain_hex(), "78");
    mounted.type_text("\n");
    assert_eq!(mounted.drain_hex(), "0d", "the next line still submits");
}

#[test]
fn a_stale_android_snapshot_carries_no_newline_and_still_replays_nothing() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    // Android's REAL submit shape: the Return leaves as an editor
    // action (`IME_ACTION_DONE` → a focus-routed Key(Enter)), never as text, and
    // `performEditorAction` does not re-poll — so the `Editable` keeps the
    // pre-submit line, with NO newline in it, until a later frame's resync.
    mounted.type_text("sudo -i");
    assert_eq!(mounted.drain_hex(), "7375646f202d69", "the line was typed");
    mounted.key(Key::Named(NamedKey::Enter));
    assert_eq!(mounted.drain_hex(), "0d", "…and the Return went out");
    assert_eq!(mounted.mirror(), "", "leaving an empty baseline");

    // RAW — the platform's own stale buffer, not `mirror()`. Diffed against
    // nothing it is one `added` run holding the whole line, and there is no
    // Return in it for a Return-keyed guard to catch.
    mounted.apply("sudo -i");
    assert!(
        mounted.drain().is_empty(),
        "a stale line replays NOTHING, newline or no newline"
    );
    assert_eq!(
        mounted.mirror(),
        "",
        "…and the empty baseline is re-published, pulling the platform back"
    );

    // The session is still live, and the shot is spent.
    mounted.type_text("x");
    assert_eq!(mounted.drain_hex(), "78");
}

#[test]
fn a_word_shaped_stale_line_replays_nothing_on_any_frame_of_the_window() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    // The password at a prompt with echo off: typed, then
    // submitted with Android's editor action, so the `Editable` is stranded
    // holding `hunter2` with NO newline — seven alphanumeric units, exactly the
    // shape a gesture commit has.
    mounted.type_text("hunter2");
    assert_eq!(mounted.drain_hex(), "68756e74657232", "the line was typed");
    mounted.key(Key::Named(NamedKey::Enter));
    assert_eq!(mounted.drain_hex(), "0d", "…and the Return went out");
    assert_eq!(mounted.mirror(), "", "leaving an empty baseline");

    // RAW, and TWICE: `performEditorAction` only counts down `imeResyncFrames`,
    // so the same stale buffer can be polled on several frames.
    for round in 1..=2 {
        mounted.apply("hunter2");
        assert!(
            mounted.drain().is_empty(),
            "round {round} replayed the password to the session"
        );
        assert_eq!(
            mounted.mirror(),
            "",
            "round {round}: the empty baseline is what is re-published"
        );
    }

    // The session is still live once the platform catches up.
    mounted.type_text("x");
    assert_eq!(mounted.drain_hex(), "78");
}

#[test]
fn a_blur_scrubs_a_live_composition_too() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.set_bar(true);

    // A composing snapshot carries the WHOLE platform mirror, so it is
    // retained typing exactly as the buffers are — and is
    // counted, and scrubbed, as such.
    mounted.root.event(
        &mut mounted.state,
        &InputEvent::Ime(ImeEvent::ApplyEditingState(EditingState {
            text: "hunter2".to_string(),
            selection_base: 7,
            selection_extent: 7,
            composing_base: 0,
            composing_extent: 7,
        })),
    );
    assert!(mounted.drain().is_empty(), "marked text is not committed");
    assert_eq!(
        mounted.published().text,
        "hunter2",
        "it is live and echoed verbatim"
    );
    assert!(mounted.buffered_units() > 0, "…and buffered while it is");

    mounted.tap_outside_everything();
    assert_eq!(
        mounted.buffered_units(),
        0,
        "a blurred session keeps none of the composition either"
    );
    assert_eq!(mounted.published().text, "");
}

#[test]
fn a_whole_buffer_replacement_emits_nothing_and_re_publishes() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.type_text("rm -rf /tmp/x");
    mounted.drain();

    // What dictation (which rewrites the field per partial), a replace-span
    // rewrite, or a bridge one frame behind all look like: one snapshot that
    // replaces the whole buffer.
    mounted.apply("wholly different text");
    assert!(
        mounted.drain().is_empty(),
        "an oversized single edit is a desync, not intent: nothing is emitted"
    );
    assert_eq!(
        mounted.mirror(),
        "rm -rf /tmp/x",
        "the mirror the source believes in is re-published, pulling the \
         platform back into step"
    );

    // The session is still live: ordinary typing on the restored baseline works.
    mounted.type_text("y");
    assert_eq!(mounted.drain_hex(), "79");
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
    for name in [
        "Control", "Alt", "Shift", "Escape", "Tab", "Up arrow", "Delete",
    ] {
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
fn the_shift_cap_sends_the_shifted_sequence_and_keeps_the_grid_focused() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.set_bar(true);
    mounted.drain();

    mounted.tap_cap("Shift");
    assert!(
        mounted.drain().is_empty(),
        "a latch key sends nothing of its own"
    );
    assert_eq!(
        mounted.toggled("Shift"),
        Some(Toggled::True),
        "the cap shows itself armed"
    );

    mounted.tap_cap("Tab");
    assert_eq!(mounted.drain_hex(), "1b5b5a", "back-tab, not \\t");

    let ime = mounted
        .root
        .ime_state()
        .expect("a bar tap must never dismiss the keyboard");
    assert!(ime.active);

    mounted.frame();
    assert_eq!(
        mounted.toggled("Shift"),
        Some(Toggled::False),
        "one shot: the sequence that used it spent it, and the bar noticed"
    );

    // …so the next Tab is the plain one again.
    mounted.tap_cap("Tab");
    assert_eq!(mounted.drain_hex(), "09");
}

#[test]
fn the_shift_latch_never_transforms_a_typed_character() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.set_bar(true);
    mounted.drain();

    // Unlike `Ctrl`, `Shift` is not on the character path at all: the IME's own
    // Shift already delivered the case, so the byte is the one it handed over.
    mounted.tap_cap("Shift");
    mounted.type_text("a");
    assert_eq!(mounted.drain_hex(), "61", "'a', byte for byte");

    mounted.frame();
    assert_eq!(
        mounted.toggled("Shift"),
        Some(Toggled::False),
        "…and the one shot is spent even though it changed nothing"
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
// The row budget
// ---------------------------------------------------------------------------

/// The narrowest phone width the bar is designed to fit its twelve caps in, in
/// logical px — the reference the arithmetic in `keybar`'s own module docs is
/// worked at, and the one width worth pinning: every wider phone spends the
/// difference on padding, and every cap shrinks by the same percentage below it.
const NARROW_PHONE_W: f64 = 360.0;

#[test]
fn twelve_caps_and_their_padding_fit_a_narrow_phone_row() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.set_bar(true);

    // Each cap paints its fill and (over it) its border at the same box, so one
    // entry per distinct x is one cap.
    let scene = mounted.frame_at_width(NARROW_PHONE_W);
    let mut caps: Vec<(Point, Size)> = Vec::new();
    for (origin, size, _, _) in &scene.rounded_fills {
        if (size.height - CAP_HEIGHT).abs() < EPSILON
            && !caps
                .iter()
                .any(|(seen, _)| (seen.x - origin.x).abs() < EPSILON)
        {
            caps.push((*origin, *size));
        }
    }
    caps.sort_by(|a, b| a.0.x.total_cmp(&b.0.x));
    assert_eq!(
        caps.len(),
        BarKey::ORDER.len(),
        "twelve caps, all painted: {caps:?}"
    );

    let (origin, size) = *caps.last().expect("a cap");
    assert!(
        origin.x + size.width <= NARROW_PHONE_W,
        "the row runs past its surface: {:.2} > {NARROW_PHONE_W}",
        origin.x + size.width
    );

    // Every cap holds what it draws with CAP_PAD_X to spare on each side — the
    // whole point of the budget, asserted against the **shaped** label rather
    // than against the estimate the flex weights are derived from.
    let theme = frust_glyph::baseline();
    let mut style = theme.type_scale.label_small.clone();
    style.size = LABEL_SIZE;
    let mut tcx = TextContext::new();
    let row_y = caps[0].0.y;
    for (key, (origin, size)) in BarKey::ORDER.iter().copied().zip(caps) {
        assert!(
            (origin.y - row_y).abs() < EPSILON,
            "{key:?} is not on the one row"
        );
        let content = match key {
            BarKey::Up | BarKey::Down | BarKey::Left | BarKey::Right => ICON_SIZE,
            _ => tcx.layout(key.label(), &style, None).size().width,
        };
        assert!(
            size.width - content >= 2.0 * CAP_PAD_X,
            "{key:?}: a {:.2}px cap around {content:.2}px of content leaves \
             less than {CAP_PAD_X}px a side",
            size.width
        );
    }
}

// ---------------------------------------------------------------------------
// Keystroke-buffer hygiene: the scrub boundaries
// ---------------------------------------------------------------------------

#[test]
fn a_blur_scrubs_what_was_typed() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.set_bar(true);
    mounted.type_text("hunter2");
    mounted.drain();
    assert!(mounted.buffered_units() > 0, "it is buffered while live");

    // A tap that claims no focus is a blur, and the surface goes with it.
    mounted.tap_outside_everything();
    assert!(
        mounted.root.ime_state().is_none(),
        "the tap really did blur (no surface is published any more)"
    );
    assert_eq!(
        mounted.buffered_units(),
        0,
        "a blurred session keeps none of it"
    );
    assert_eq!(mounted.mirror(), "");
}

#[test]
fn a_key_bar_hand_off_is_not_a_blur_and_keeps_the_line_typeable() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.set_bar(true);
    mounted.type_text("ls -l");
    mounted.drain();

    // A cap takes the focus path from the grid — the same session, still being
    // typed into, so the buffers must survive it (scrubbing here would cost
    // the user their next Backspace: a delete is only observable as a
    // shortening, and an emptied mirror has nothing to shorten).
    mounted.tap_cap("Left arrow");
    assert_eq!(mounted.drain_hex(), "1b5b44");
    assert_eq!(
        mounted.mirror(),
        "ls -l",
        "the baseline survives a hand-off"
    );

    let mut shorter = mounted.mirror();
    shorter.pop();
    mounted.apply(&shorter);
    assert_eq!(mounted.drain_hex(), "7f", "…so backspace still emits");
}

#[test]
fn unmounting_the_grid_scrubs_the_session() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.type_text("hunter2");
    mounted.drain();
    assert!(mounted.buffered_units() > 0);

    mounted.unmount();
    assert_eq!(
        mounted.buffered_units(),
        0,
        "the grid's own teardown drops what the session buffered"
    );
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

#[test]
fn a_key_bar_cap_over_a_read_only_session_emits_nothing_either() {
    let _serial = serial();
    let _owner = setup();
    // The bar mounted over a read-only grid — a shape no real screen produces
    // (see `TermScreen::force_bar`), so that what is being asserted is the
    // gate itself: read-only lives on the *seam*, so every emitter observes
    // it, not just the grid whose builder input it came from.
    let mut mounted = Mounted::read_only_with_bar();
    mounted.set_bar(true);
    assert!(
        node_labels(&mounted.root)
            .iter()
            .any(|label| label == "Escape"),
        "the bar really is mounted for this one"
    );

    mounted.tap_cap("Escape");
    mounted.tap_cap("Up arrow");
    assert!(
        mounted.drain().is_empty(),
        "a cap over a read-only session derives nothing and sends nothing"
    );

    // The latch keys are inert too, so nothing is left armed for later —
    // including `Shift`, whose one shot would otherwise outlive the gate and
    // rewrite the first sequence sent once the session took input again.
    for latch in ["Control", "Alt", "Shift"] {
        mounted.tap_cap(latch);
        assert_eq!(
            mounted.toggled(latch),
            Some(Toggled::False),
            "{latch} armed behind the read-only gate"
        );
    }
    assert!(
        mounted.drain().is_empty(),
        "and no latch tap derived a byte either"
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
