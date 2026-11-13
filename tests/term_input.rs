//! Headless end-to-end coverage for the terminal keystroke source: the grid as an IME client,
//! the last-known-state diff, the Ctrl/Alt/Shift latch and the key bar, asserted on the bytes
//! that reach the session through `DemoFeed`'s echo.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use frust::authoring::{
    EditingState, ImeContentType, ImeEvent, InputEvent, Key, KeyEvent, Modifiers, NamedKey,
};
use frust::{
    AnyView, Axis, Component, Either, FlexView, Get, RwSignal, Set, View, any, component, flexible,
    inflexible,
};
use frust_core::RenderRoot;
use frust_core::accesskit::Toggled;
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::features::terminal::data::feeds::demo_feed::DemoFeed;
use muxr::features::terminal::domain::feed::TerminalFeed;
use muxr::features::terminal::presentation::keybar::{
    ARROW_CAP_HEIGHT, CAP_HEIGHT, CAP_PAD_X, ICON_SIZE, LABEL_SIZE, TABLET_CAP_HEIGHT, key_bar,
    tablet_key_bar,
};
use muxr::features::terminal::presentation::term::{
    BarKey, InputSeam, SharedEmulator, SharedFeed, TerminalEmulator, TerminalPalette, pump_wake,
    terminal_grid,
};
use muxr::responsive::WidthBand;

mod support;
use support::{
    EPSILON, RecScene, frame_at_sized, ms, node_center_by_label, node_labels, pointer, serial,
    setup,
};

const FONT_SIZE: f32 = 13.0;

const PHONE: Size = Size::new(392.7, 872.7);

const TABLET: Size = Size::new(1024.0, 720.0);

const LABEL: &str = "dev \u{b7} zsh";

const IN_GRID: Point = Point::new(120.0, 120.0);

struct TermScreen {
    seam: InputSeam,
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
    bar_active: RwSignal<bool>,
    read_only: bool,
    /// Mounts the bar over a read-only grid, a shape no real screen has, to prove the seam's gate.
    force_bar: bool,
    tablet: Option<TabletBar>,
}

#[derive(Clone, Copy)]
struct TabletBar {
    band: WidthBand,
    shown: RwSignal<bool>,
    fn_row: RwSignal<bool>,
}

impl Component for TermScreen {
    type State = ();

    fn init(&self) -> Self::State {}

    fn build(&self, _state: &mut Self::State) -> impl View<Self::State> {
        let generation = self.generation.get();
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

        let bar = match self.tablet {
            Some(tablet) => {
                let shown = tablet.shown.get() && (!self.read_only || self.force_bar);
                Either::Left(tablet_key_bar::<Self::State>(
                    shown,
                    tablet.band,
                    tablet.fn_row,
                    &self.seam,
                ))
            }
            None => Either::Right(key_bar::<Self::State>(active, &self.seam)),
        };
        any(FlexView::new(
            Axis::Vertical,
            vec![flexible(1, grid), inflexible(bar)],
        ))
    }
}

type Logic = Box<dyn FnMut(&mut ()) -> AnyView<()>>;

struct Mounted {
    root: RenderRoot<(), AnyView<()>>,
    logic: Logic,
    state: (),
    tcx: TextContext,
    feed: Rc<RefCell<DemoFeed>>,
    shared_feed: SharedFeed,
    emulator: SharedEmulator,
    generation: RwSignal<u64>,
    seam: InputSeam,
    bar_active: RwSignal<bool>,
    tablet: Option<TabletBar>,
    surface: Size,
    mounted: Rc<std::cell::Cell<bool>>,
}

impl Mounted {
    fn new() -> Self {
        Self::mount(false, false)
    }

    fn with_read_only(read_only: bool) -> Self {
        Self::mount(read_only, false)
    }

    fn read_only_with_bar() -> Self {
        Self::mount(true, true)
    }

    fn tablet(band: WidthBand) -> Self {
        Self::mount_tier(false, false, Some(band))
    }

    fn read_only_tablet(force_bar: bool) -> Self {
        Self::mount_tier(true, force_bar, Some(WidthBand::Full))
    }

    fn mount(read_only: bool, force_bar: bool) -> Self {
        Self::mount_tier(read_only, force_bar, None)
    }

    fn mount_tier(read_only: bool, force_bar: bool, band: Option<WidthBand>) -> Self {
        let tablet = band.map(|band| TabletBar {
            band,
            shown: RwSignal::new(true),
            fn_row: RwSignal::new(false),
        });
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
                tablet,
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
            tablet,
            surface: if band.is_some() { TABLET } else { PHONE },
            mounted,
        };
        mounted.frame();
        // The replay's chunk 0 is due at t=0; drain it so every later drain holds only typed echo.
        mounted.drain();
        mounted
    }

    fn frame(&mut self) {
        frame_at_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            self.surface,
            ms(0.0),
        );
    }

    fn tap(&mut self, at: Point) {
        self.root.event(
            &mut self.state,
            &pointer(frust_core::PointerPhase::Down, at),
        );
        self.root
            .event(&mut self.state, &pointer(frust_core::PointerPhase::Up, at));
        self.frame();
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

    /// One snapshot per character, each built on the mirror the widget currently publishes.
    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            let mut buffer = self.mirror();
            buffer.push(ch);
            self.apply(&buffer);
        }
    }

    fn published(&self) -> EditingState {
        self.seam.published()
    }

    fn mirror(&self) -> String {
        self.published().text
    }

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

    fn pump(&mut self) -> bool {
        pump_wake(&self.shared_feed, &self.emulator, self.generation)
    }

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

    fn set_bar(&mut self, active: bool) {
        self.bar_active.set(active);
        self.frame();
    }

    fn toggled(&self, label: &str) -> Option<Toggled> {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
            .and_then(|(_, node)| node.toggled())
    }

    fn tap_cap(&mut self, label: &str) {
        let at = node_center_by_label(&self.root, label)
            .unwrap_or_else(|| panic!("no key-bar cap labelled {label:?}"));
        self.tap(at);
    }

    /// A tap on the bar's left padding claims no focus, which reads as a blur; the bar must be up.
    fn tap_outside_everything(&mut self) {
        let cap = node_center_by_label(&self.root, "Control")
            .expect("the bar must be up to find the band");
        self.tap(Point::new(1.0, cap.y));
    }

    fn buffered_units(&self) -> usize {
        self.seam.source.borrow().buffered_units()
    }

    fn unmount(&mut self) {
        self.mounted.set(false);
        self.frame();
    }
}

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

#[test]
fn typed_characters_reach_the_feed_byte_for_byte() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    mounted.type_text("ls");
    assert_eq!(mounted.drain_hex(), "6c73");

    // A smart keyboard would rewrite these into a curly quote and an em dash.
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
fn a_hardware_f2_sends_the_ss3_sequence() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    mounted.key(Key::Named(NamedKey::F2));
    assert_eq!(mounted.drain_hex(), "1b4f51");
}

#[test]
fn a_return_leaves_no_newline_behind_and_the_next_key_starts_clean() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    mounted.type_text("ls -la\n");
    assert_eq!(mounted.drain_hex(), "6c73202d6c610d", "`ls -la`, then CR");

    // The submitted line is dropped, so iOS's literal `\n` is never replayed by a bulk re-emission.
    assert!(
        !mounted.mirror().contains('\n'),
        "no newline survives a Return: {:?}",
        mounted.mirror()
    );
    assert_eq!(mounted.mirror(), "", "…because nothing survives one");
    assert_eq!(mounted.buffered_units(), 0);

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

    mounted.type_text("ls\n");
    assert_eq!(mounted.drain_hex(), "6c730d", "`ls`, then CR");
    assert_eq!(mounted.mirror(), "", "the baseline was dropped");

    // Built raw from the platform's stale buffer, not from `mirror()`, which would hide the replay.
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

    // Android submits via an editor action, so the `Editable` keeps the line with no newline.
    mounted.type_text("sudo -i");
    assert_eq!(mounted.drain_hex(), "7375646f202d69", "the line was typed");
    mounted.key(Key::Named(NamedKey::Enter));
    assert_eq!(mounted.drain_hex(), "0d", "…and the Return went out");
    assert_eq!(mounted.mirror(), "", "leaving an empty baseline");

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

    mounted.type_text("x");
    assert_eq!(mounted.drain_hex(), "78");
}

#[test]
fn a_word_shaped_stale_line_replays_nothing_on_any_frame_of_the_window() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    // A password with echo off: seven alphanumeric units, the shape a gesture commit has.
    mounted.type_text("hunter2");
    assert_eq!(mounted.drain_hex(), "68756e74657232", "the line was typed");
    mounted.key(Key::Named(NamedKey::Enter));
    assert_eq!(mounted.drain_hex(), "0d", "…and the Return went out");
    assert_eq!(mounted.mirror(), "", "leaving an empty baseline");

    // Twice: the same stale buffer can be polled on several frames.
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

    // A composing snapshot carries the whole platform mirror, so it counts as retained typing.
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

    // What dictation, a replace-span rewrite or a bridge one frame behind look like.
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

    // Android: the key event and the echoed shortening.
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

    mounted.apply("a\u{4f60}");
    assert_eq!(mounted.drain_hex(), "e4bda0");
}

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

    mounted.type_text("q");
    assert_eq!(mounted.drain_hex(), "71");

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

    // A character typed on the OS keyboard, not the bar, is the one the latch transforms.
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

    // `Shift` is not on the character path: the IME's own Shift already delivered the case.
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

/// The narrowest phone width the twelve caps must fit; a wider phone spends the rest on padding.
const NARROW_PHONE_W: f64 = 360.0;

#[test]
fn twelve_caps_and_their_padding_fit_a_narrow_phone_row() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.set_bar(true);

    // A cap paints its fill and its border at the same box, so one entry per distinct x is one cap.
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

    // Asserted against the shaped label, not the estimate the flex weights are derived from.
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

/// The Collapsed band's row budget: without `Home`/`End`, the row fits it.
const COLLAPSED_ROW_W: f64 = 580.0;

fn shows(mounted: &Mounted, label: &str) -> bool {
    node_labels(&mounted.root).iter().any(|l| l == label)
}

#[test]
fn the_tablet_tier_is_up_with_no_keyboard_and_sends_home_end_and_the_arrows() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::tablet(WidthBand::Full);
    mounted.tap(IN_GRID);
    mounted.drain();

    for name in [
        "Control",
        "Escape",
        "Home",
        "End",
        "Page up",
        "Delete",
        "Function keys",
        "Up arrow",
        "Left arrow",
        "Down arrow",
        "Right arrow",
    ] {
        assert!(shows(&mounted, name), "{name} missing with no keyboard up");
    }
    assert!(!shows(&mounted, "F1"), "the Fn row starts closed");

    mounted.tap_cap("Home");
    assert_eq!(mounted.drain_hex(), "1b5b48");
    mounted.tap_cap("End");
    assert_eq!(mounted.drain_hex(), "1b5b46");
    mounted.tap_cap("Up arrow");
    assert_eq!(mounted.drain_hex(), "1b5b41");
    mounted.tap_cap("Right arrow");
    assert_eq!(mounted.drain_hex(), "1b5b43");
    mounted.tap_cap("Shift");
    mounted.tap_cap("Home");
    assert_eq!(mounted.drain_hex(), "1b5b48", "Home has no shifted form");

    let ime = mounted
        .root
        .ime_state()
        .expect("a Tablet cap hands focus off like a Compact one");
    assert!(ime.active);
}

#[test]
fn the_fn_cap_opens_the_f_key_row_and_each_f_key_takes_one_modifier() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::tablet(WidthBand::Full);
    mounted.tap(IN_GRID);
    mounted.drain();

    mounted.tap_cap("Function keys");
    assert!(mounted.drain().is_empty(), "Fn sends nothing of its own");
    assert_eq!(mounted.toggled("Function keys"), Some(Toggled::True));
    assert!(shows(&mounted, "F1") && shows(&mounted, "F12"));

    mounted.tap_cap("F1");
    assert_eq!(mounted.drain_hex(), "1b4f50");
    mounted.tap_cap("F12");
    assert_eq!(mounted.drain_hex(), "1b5b32347e");
    mounted.tap_cap("Shift");
    mounted.tap_cap("F5");
    assert_eq!(mounted.drain_hex(), "1b5b31353b327e", "CSI 15;2~");
    mounted.tap_cap("Control");
    mounted.tap_cap("F5");
    assert_eq!(mounted.drain_hex(), "1b5b31353b357e", "CSI 15;5~");
    mounted.frame();
    assert_eq!(
        mounted.toggled("Control"),
        Some(Toggled::False),
        "the F-key spent Ctrl"
    );
    mounted.tap_cap("Alt");
    mounted.tap_cap("F1");
    assert_eq!(mounted.drain_hex(), "1b5b313b3350", "CSI 1;3P");

    // One parameter per key: Shift wins, Ctrl stays armed for what follows.
    mounted.tap_cap("Control");
    mounted.tap_cap("Shift");
    mounted.tap_cap("F6");
    assert_eq!(mounted.drain_hex(), "1b5b31373b327e");
    mounted.frame();
    assert_eq!(mounted.toggled("Control"), Some(Toggled::True));

    mounted.tap_cap("Function keys");
    assert!(!shows(&mounted, "F1"), "a second tap closes the row");
    assert_eq!(mounted.toggled("Function keys"), Some(Toggled::False));
}

#[test]
fn the_collapsed_tablet_tier_drops_home_and_end_and_fits_its_row_budget() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::tablet(WidthBand::Collapsed);
    mounted.surface = Size::new(COLLAPSED_ROW_W, TABLET.height);
    mounted.frame();
    assert!(!shows(&mounted, "Home") && !shows(&mounted, "End"));
    assert!(shows(&mounted, "Page up") && shows(&mounted, "Function keys"));

    let scene = mounted.frame_at_width(COLLAPSED_ROW_W);
    let caps_of = |height: f64| {
        let mut caps: Vec<(Point, Size)> = Vec::new();
        for (origin, size, _, _) in &scene.rounded_fills {
            let seen = |(at, _): &(Point, Size)| {
                (at.x - origin.x).abs() < EPSILON && (at.y - origin.y).abs() < EPSILON
            };
            if (size.height - height).abs() < EPSILON && !caps.iter().any(seen) {
                caps.push((*origin, *size));
            }
        }
        caps
    };
    let row = caps_of(TABLET_CAP_HEIGHT);
    assert_eq!(row.len(), BarKey::TABLET_ORDER.len() - 2, "{row:?}");
    let arrows = caps_of(ARROW_CAP_HEIGHT);
    assert_eq!(arrows.len(), 4, "the inverted-T's four cells: {arrows:?}");
    for (origin, size) in row.iter().chain(&arrows) {
        assert!(
            origin.x + size.width <= COLLAPSED_ROW_W + EPSILON,
            "a cap runs past {COLLAPSED_ROW_W}: {origin:?} {size:?}"
        );
    }
    let top = arrows
        .iter()
        .map(|(o, _)| o.y)
        .fold(f64::INFINITY, f64::min);
    let upper = arrows.iter().filter(|(o, _)| (o.y - top).abs() < EPSILON);
    assert_eq!(upper.count(), 1, "one cell on top, three under it");
}

#[test]
fn hiding_the_tablet_tier_unmounts_every_cap() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::tablet(WidthBand::Full);
    assert!(shows(&mounted, "Escape"));
    mounted.tablet.expect("a Tablet mount").shown.set(false);
    mounted.frame();
    assert!(!shows(&mounted, "Escape") && !shows(&mounted, "Home"));
}

#[test]
fn a_read_only_session_never_shows_the_tablet_tier() {
    let _serial = serial();
    let _owner = setup();
    let mounted = Mounted::read_only_tablet(false);
    assert!(
        !shows(&mounted, "Escape"),
        "the screen's own read-only gate"
    );

    // The bar is up on the first frame, before the grid publishes read-only; the seam hides it.
    let mut mounted = Mounted::read_only_tablet(true);
    assert!(shows(&mounted, "Escape"), "the forced first frame");
    mounted.frame();
    assert!(!shows(&mounted, "Escape"), "the seam's read-only gate");
}

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

    // Same session, still being typed into: scrubbing here would cost the next Backspace.
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

    // The focus gate is the read-only gate; an event at the root must still derive nothing.
    mounted.apply("ls");
    mounted.key(Key::Named(NamedKey::Enter));
    assert!(mounted.drain().is_empty(), "and it emits nothing at all");

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

    // `Shift` too: its one shot would otherwise outlive the gate.
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

#[test]
fn a_long_line_stays_byte_correct_across_a_window_re_anchor() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);

    // Past the mirror cap, so at least one re-anchor fires.
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

#[test]
fn input_and_output_share_one_feed_without_interleaving() {
    let _serial = serial();
    let _owner = setup();
    let mut mounted = Mounted::new();
    mounted.tap(IN_GRID);
    mounted.type_text("x");

    // The echo is still its own write and still first, the order `DemoFeed` documents.
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
