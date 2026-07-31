//! Headless tests for the cold-start splash *sequence* — the orchestrator,
//! its handoff to the `/` route, and the reduced-motion fallback.
//!
//! `tests/splash.rs` covers the three beats in isolation; this file covers
//! what only the assembled thing can show: that the beats fire in the right
//! order off one clock, that the pieces stagger in, that the overlay unmounts
//! itself, and that what is underneath when it does is the same picture.
//!
//! Like `tests/splash.rs` these can't use `support::frame` (which pins
//! `FrameTime::ZERO`): every assertion here is a function of the frame clock,
//! so each test drives rebuild → layout → paint itself, exactly as a shell's
//! frame loop does — which is also what lets the overlay's unmount actually
//! happen mid-run.
//!
//! # Frame cadence
//!
//! Every helper takes the cadence in nanoseconds ([`drive_at`],
//! [`frame_at_cadence`]) and the plain [`drive`]/[`frame_at`] wrappers pin
//! [`FRAME_NS`] (60Hz). The splash is clock-driven, not frame-driven, so the
//! same wall-clock timeline must come out of a [`FRAME_NS_120`] run —
//! [`the_timeline_lands_on_the_same_wall_clock_at_120hz`] is the test that
//! actually holds `spec::REFERENCE_FRAME_MS`'s normalization to that.
//!
//! # Reading a piece's text out of a paint pass
//!
//! A painted `GlyphRun` carries glyph *ids*, not a string. [`glyph_ids`]
//! shapes an expected string through the same [`TextContext`] the widgets lay
//! out against and compares id sequences, so "the heading is on screen" is a
//! real assertion rather than a glyph-run headcount — and counting
//! *occurrences* is what distinguishes the splash's assembling copy from the
//! live route's copy underneath it.

use std::any::Any;

use frust::authoring::text::{TextContext, TextStyle};
use frust::authoring::{Point, View, any};
use frust::{AnyView, Component, Get, RwSignal, Theme};
use frust_core::{FrameTime, PaintScene, RenderRoot};
use frust_scene::GlyphRun;
use kurbo::{Rect, Size};
use peniko::Color;

use muxr::features::connect::presentation::pages::connect_layout;
use muxr::features::connect::presentation::pieces::connect_pieces;
use muxr::ui::backdrop::page_backdrop;
use muxr::ui::splash::icon_phase::{IconArt, splash_icon_art};
use muxr::ui::splash::sequence::{
    assembly_start_ms, rain_cue_ms, reduced_splash, sequence_end_ms, splash_sequence,
};
use muxr::{AppState, MuxrApp};

mod support;
use support::{serial, setup};

/// The design reference's phone screen (`.screen{height:650px}` inside a
/// 340px-wide frame) — the same surface `tests/splash.rs` uses.
const SURFACE: Size = Size::new(340.0, 650.0);
/// One 60Hz frame, in nanoseconds — `spec::REFERENCE_FRAME_MS`'s cadence, and
/// the default every helper here pins.
const FRAME_NS: u64 = 16_666_667;
/// One 120Hz frame, in nanoseconds — the cadence the splash's `frame_factor`
/// normalization exists for.
const FRAME_NS_120: u64 = 8_333_333;
/// The reduced-motion hold (`spec::REDUCED_HOLD_MS`, which is `pub(crate)`).
const REDUCED_HOLD_MS: f64 = 200.0;
/// The heading piece's stagger (`spec::PIECE_DELAYS_MS[2]`, likewise).
const HEADING_DELAY_MS: f64 = 260.0;
/// How many blurred halos a **settled** Connect screen paints, land flashes
/// excluded: one, the saved-server row's `.dot-ok`
/// (`box-shadow:0 0 5px var(--success)`).
///
/// The splash's copy of piece 7 renders the mock's single stand-in row
/// (`pieces::splash_placeholder_row`), so this is one dot and not the live
/// repository's three. Asserting the exact residue rather than zero keeps the
/// original claim — that every *land flash* has faded — intact: a flash left
/// running would push this above one.
const RESTING_SHADOWS: usize = 1;

/// A recording paint target: the flattened glyph-id stream (for text
/// assertions), plus a count of every other draw kind the splash emits.
#[derive(Default)]
struct Recorder {
    glyph_ids: Vec<u32>,
    glyph_runs: usize,
    /// Decoded-bitmap draws — the app icon, and nothing else in this app.
    images: usize,
    /// Blurred rounded rects — the pieces' amber land flash, and nothing else.
    shadows: usize,
}

impl PaintScene for Recorder {
    fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
    fn fill_rounded_rect(&mut self, _origin: Point, _size: Size, _radius: f64, _color: Color) {}
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
    fn draw_glyph_run(&mut self, run: GlyphRun) {
        self.glyph_runs += 1;
        self.glyph_ids
            .extend(run.glyphs.iter().map(|glyph| glyph.id));
    }
    fn draw_image(&mut self, _data: &peniko::ImageData, _dest: Rect) {
        self.images += 1;
    }
    fn draw_shadow(
        &mut self,
        _origin: Point,
        _size: Size,
        _radius: f64,
        _std_dev: f64,
        _color: Color,
    ) {
        self.shadows += 1;
    }
}

/// A Glyph theme with `reduce_motion` forced on/off.
fn theme(reduce_motion: bool) -> Theme {
    let mut theme = Theme::glyph_baseline();
    theme.motion.reduce_motion = reduce_motion;
    theme
}

/// A `cadence_ns` frame, in milliseconds.
fn cadence_ms(cadence_ns: u64) -> f64 {
    cadence_ns as f64 / 1_000_000.0
}

/// `frame`'s wall-clock instant, in ms, on a run driven at `cadence_ns`.
fn frame_ms(frame: usize, cadence_ns: u64) -> f64 {
    frame as f64 * cadence_ms(cadence_ns)
}

/// The frame index by which `ms` of animation has elapsed at `cadence_ns`.
/// Frame 0 seeds the clock (zero delta), so `ms` of motion is in by frame
/// `ms/cadence + 1`.
fn frame_at_cadence(ms: f64, cadence_ns: u64) -> usize {
    (ms / cadence_ms(cadence_ns)).round() as usize + 1
}

/// [`frame_at_cadence`] at 60Hz.
fn frame_at(ms: f64) -> usize {
    frame_at_cadence(ms, FRAME_NS)
}

/// Drive `frames` frames of the real shell loop at `cadence_ns` — rebuild, lay
/// out shaping real text, paint — returning each frame's recording and whether
/// it asked for another one.
///
/// Rebuilding *every* frame (rather than laying out once up front) is what
/// makes the overlay's own unmount observable: the sequence ends by writing a
/// signal the root component reads.
fn drive_at<V: View<AppState>>(
    cadence_ns: u64,
    logic: &mut impl FnMut(&mut AppState) -> V,
    state: &mut AppState,
    theme: Theme,
    frames: usize,
) -> Vec<(Recorder, bool)> {
    let mut root: RenderRoot<AppState, V> = RenderRoot::new();
    let mut text_ctx = TextContext::new();
    root.set_theme(Box::new(theme));

    (0..frames)
        .map(|frame| {
            root.rebuild(logic, state);
            let text_any: &mut dyn Any = &mut text_ctx;
            root.layout_with_text(SURFACE, text_any);
            let mut scene = Recorder::default();
            let outcome = root.paint(&mut scene, FrameTime::from_nanos(frame as u64 * cadence_ns));
            (scene, outcome.needs_frame)
        })
        .collect()
}

/// [`drive_at`] at 60Hz.
fn drive<V: View<AppState>>(
    logic: &mut impl FnMut(&mut AppState) -> V,
    state: &mut AppState,
    theme: Theme,
    frames: usize,
) -> Vec<(Recorder, bool)> {
    drive_at(FRAME_NS, logic, state, theme, frames)
}

/// The index of the first frame satisfying `beat`, panicking with `what` if no
/// frame in the run did.
fn first_frame(
    frames: &[(Recorder, bool)],
    what: &str,
    beat: impl FnMut(&(Recorder, bool)) -> bool,
) -> usize {
    frames
        .iter()
        .position(beat)
        .unwrap_or_else(|| panic!("{what} never happened across {} frames", frames.len()))
}

/// The glyph ids `text` shapes to at `style` — the fingerprint a painted run
/// of the same string leaves in [`Recorder::glyph_ids`].
fn glyph_ids(text: &str, style: &TextStyle) -> Vec<u32> {
    let mut ctx = TextContext::new();
    ctx.layout(text, style, None)
        .to_scene_runs(Point::ORIGIN)
        .iter()
        .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.id))
        .collect()
}

/// How many times `needle` appears in `haystack` — one per copy of the string
/// on screen (the splash's assembling copy and the route's live one are two).
fn occurrences(haystack: &[u32], needle: &[u32]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// The heading piece's exact style, so its shaped ids match what
/// `pieces::heading` paints.
fn heading_ids() -> Vec<u32> {
    let theme = Theme::glyph_baseline();
    glyph_ids("Connect to server", &theme.type_scale.title_medium)
}

/// The decoded app icon, asserted to be the bitmap (not the fallback) so a
/// test that counts `images` is testing what it thinks it is.
fn decoded_icon() -> IconArt {
    let art = splash_icon_art();
    assert!(
        matches!(art, IconArt::Image(_)),
        "the embedded PNG must decode — the vector fallback is a failure path"
    );
    art
}

// ---------------------------------------------------------------------------
// The sequence, mounted on its own
// ---------------------------------------------------------------------------

/// Mount the orchestrator alone (no navigator underneath) at `cadence_ns`, so
/// every draw recorded belongs to the splash.
fn mount_sequence_at(
    cadence_ns: u64,
    reduce_motion: bool,
    frames: usize,
) -> (Vec<(Recorder, bool)>, RwSignal<bool>) {
    let mut state = MuxrApp.init();
    let art = decoded_icon();
    let done = RwSignal::new(false);
    let mut logic = move |_: &mut AppState| -> AnyView<AppState> {
        splash_sequence(
            art.clone(),
            7,
            connect_pieces(&Theme::glyph_baseline()),
            connect_layout,
            done,
        )
    };
    let frames = drive_at(
        cadence_ns,
        &mut logic,
        &mut state,
        theme(reduce_motion),
        frames,
    );
    (frames, done)
}

/// [`mount_sequence_at`] at 60Hz.
fn mount_sequence(reduce_motion: bool, frames: usize) -> (Vec<(Recorder, bool)>, RwSignal<bool>) {
    mount_sequence_at(FRAME_NS, reduce_motion, frames)
}

#[test]
fn the_sequence_walks_icon_then_rain_then_assembly_off_one_clock() {
    let _g = serial();
    let _ambient = setup();

    let (frames, done) = mount_sequence(false, frame_at(sequence_end_ms() + 200.0));
    let heading = heading_ids();

    // --- the icon beat: the bitmap is up, the rain has not started ---------
    let icon_beat = &frames[frame_at(500.0)].0;
    assert_eq!(icon_beat.images, 1, "the app icon paints on its own first");
    assert_eq!(
        icon_beat.glyph_runs, 0,
        "nothing else draws during the icon beat: {} runs",
        icon_beat.glyph_runs
    );

    // --- the rain cue: hundreds of glyphs, still no assembled screen -------
    let rain_beat = &frames[frame_at(rain_cue_ms() + 200.0)].0;
    assert!(
        rain_beat.glyph_runs > 100,
        "the rain starts dense at the cue, got {} runs",
        rain_beat.glyph_runs
    );
    assert_eq!(
        occurrences(&rain_beat.glyph_ids, &heading),
        0,
        "the assembly has not begun yet"
    );

    // --- the assembly: staggered, piece by piece --------------------------
    let before_heading = &frames[frame_at(assembly_start_ms() + HEADING_DELAY_MS - 60.0)].0;
    assert_eq!(
        occurrences(&before_heading.glyph_ids, &heading),
        0,
        "the heading waits for its own 260ms stagger"
    );
    let after_heading = &frames[frame_at(assembly_start_ms() + HEADING_DELAY_MS + 60.0)].0;
    assert_eq!(
        occurrences(&after_heading.glyph_ids, &heading),
        1,
        "and then drops in"
    );
    assert!(
        after_heading.shadows > 0,
        "a landing piece flashes amber (draw_shadow)"
    );

    // --- the end: assembled, quiet, and cueing the unmount ----------------
    let (last, needs_frame) = frames.last().expect("frames were painted");
    assert_eq!(
        occurrences(&last.glyph_ids, &heading),
        1,
        "the finished overlay is the Connect screen"
    );
    assert_eq!(last.images, 0, "the icon is long gone");
    assert_eq!(
        last.shadows, RESTING_SHADOWS,
        "every land flash has faded, leaving only the saved row's own dot glow"
    );
    assert!(
        !needs_frame,
        "a finished sequence stops requesting frames — nothing left to animate"
    );
    assert!(done.get(), "and cues its own unmount");
}

#[test]
fn the_completion_cue_fires_once_the_whole_timeline_has_run() {
    let _g = serial();
    let _ambient = setup();

    // A frame short of the end, the cue must not have fired...
    let (_, early) = mount_sequence(false, frame_at(sequence_end_ms()) - 2);
    assert!(!early.get(), "the sequence is not over before its end");

    // ...and just past it, it has.
    let (_, done) = mount_sequence(false, frame_at(sequence_end_ms()) + 2);
    assert!(done.get());
}

// ---------------------------------------------------------------------------
// Frame cadence
// ---------------------------------------------------------------------------

/// The three timeline beats a paint stream can actually witness, as
/// wall-clock ms since the overlay's first frame.
#[derive(Debug)]
struct Beats {
    /// The rain + burst cue: the first frame drawing any glyph at all (the
    /// icon beat before it draws only the bitmap).
    rain_cue: f64,
    /// The assembly: the first frame the staggered heading piece paints.
    heading: f64,
    /// The end: the frame the completion cue fired on — which is the last one
    /// that asked for another, since a fired cue early-returns without
    /// requesting and nothing else is left animating.
    end: f64,
}

/// Drive the whole sequence at `cadence_ns` and time its beats.
fn observed_beats(cadence_ns: u64) -> Beats {
    let heading = heading_ids();
    let (frames, _) = mount_sequence_at(
        cadence_ns,
        false,
        frame_at_cadence(sequence_end_ms() + 200.0, cadence_ns),
    );

    let rain_cue = first_frame(&frames, "the rain cue", |(scene, _)| scene.glyph_runs > 0);
    let heading_frame = first_frame(&frames, "the heading piece", |(scene, _)| {
        occurrences(&scene.glyph_ids, &heading) > 0
    });
    let quiet = first_frame(&frames, "the sequence settling", |(_, needs_frame)| {
        !needs_frame
    });
    Beats {
        rain_cue: frame_ms(rain_cue, cadence_ns),
        heading: frame_ms(heading_frame, cadence_ns),
        end: frame_ms(quiet - 1, cadence_ns),
    }
}

#[test]
fn the_timeline_lands_on_the_same_wall_clock_at_120hz() {
    let _g = serial();
    let _ambient = setup();

    let sixty = observed_beats(FRAME_NS);
    let one_twenty = observed_beats(FRAME_NS_120);
    let frame = cadence_ms(FRAME_NS);

    // Twice the frames, the same wall clock: the splash is clock-driven, and
    // `spec::REFERENCE_FRAME_MS`/`frame_factor` exist so the one per-frame
    // quantity (the rain's row advance) doesn't turn cadence into speed.
    // Each beat is quantized to its own cadence, so two runs can differ by at
    // most the coarser frame.
    for (what, at_60, at_120, expected, gates) in [
        (
            "the rain cue",
            sixty.rain_cue,
            one_twenty.rain_cue,
            rain_cue_ms(),
            1.0,
        ),
        (
            "the heading piece",
            sixty.heading,
            one_twenty.heading,
            assembly_start_ms() + HEADING_DELAY_MS,
            // Two quantizations compound here: the assembly gate opening, and
            // then the piece's own stagger off that gate's first painted frame.
            2.0,
        ),
        (
            "the sequence end",
            sixty.end,
            one_twenty.end,
            sequence_end_ms(),
            1.0,
        ),
    ] {
        assert!(
            (at_120 - at_60).abs() <= frame,
            "{what} moved with the cadence: {at_60:.1}ms at 60Hz vs {at_120:.1}ms at 120Hz"
        );
        for (rate, at) in [("60Hz", at_60), ("120Hz", at_120)] {
            assert!(
                at >= expected && at - expected <= gates * frame,
                "{what} at {rate} landed at {at:.1}ms — not inside the {gates} frame(s) \
                 after its {expected:.1}ms mark"
            );
        }
    }
}

#[test]
fn reduced_motion_collapses_the_sequence_on_its_first_frame() {
    let _g = serial();
    let _ambient = setup();

    // Defense in depth: the shell mounts `reduced_splash` instead, but a
    // theme that flips `reduce_motion` on *after* the mount decision must not
    // hold the app behind a four-second timeline.
    let (frames, done) = mount_sequence(true, 4);
    assert!(done.get(), "the cue fires immediately under reduced motion");
    for (index, (scene, needs_frame)) in frames.iter().enumerate() {
        assert_eq!(scene.images, 0, "no icon animation");
        assert_eq!(
            scene.shadows, RESTING_SHADOWS,
            "no land flashes — only the resting dot glow, which is chrome \
             rather than motion and so survives reduced motion"
        );
        // The firing frame asks for the one frame that acts on its write; from
        // the next one on there is nothing left to animate.
        assert!(
            !needs_frame || index == 0,
            "frame {index} kept the shell spinning"
        );
    }
    // The pieces are painted in their landed state, not withheld.
    assert_eq!(
        occurrences(&frames[0].0.glyph_ids, &heading_ids()),
        1,
        "reduced motion shows the finished screen"
    );
}

#[test]
fn the_reduced_fallback_holds_the_static_icon_then_cues() {
    let _g = serial();
    let _ambient = setup();

    let mut state = MuxrApp.init();
    let art = decoded_icon();
    let done = RwSignal::new(false);
    // Mirrors the shell (`AppState::splash_overlay`): the cue's write is what
    // *unmounts* the overlay, so this closure has to drop it once `done` is
    // set. A harness that re-mounted `reduced_splash` unconditionally could
    // never see the hold end — nor see it end too early, which is the
    // regression below.
    let mut logic = move |_: &mut AppState| -> AnyView<AppState> {
        if done.get() {
            any(page_backdrop::<AppState>())
        } else {
            reduced_splash(art.clone(), done)
        }
    };
    let frames = drive(
        &mut logic,
        &mut state,
        theme(true),
        frame_at(REDUCED_HOLD_MS + 100.0),
    );

    // The whole hold shows the icon. `reduce_motion` is on for every frame of
    // this path (it is why the path was chosen), so a cue short-circuiting on
    // it would fire on frame 0 and leave one ~16ms flicker here instead of a
    // 200ms beat.
    for (index, (scene, _)) in frames
        .iter()
        .take(frame_at(REDUCED_HOLD_MS) - 1)
        .enumerate()
    {
        assert_eq!(
            scene.images,
            1,
            "frame {index} ({:.0}ms) dropped the static icon mid-hold",
            frame_ms(index, FRAME_NS)
        );
    }

    // ...and once the hold is out, the overlay is gone.
    let after = frame_at(REDUCED_HOLD_MS + 50.0);
    assert_eq!(
        frames[after].0.images, 0,
        "the overlay must unmount once its hold has elapsed"
    );
    assert!(done.get(), "and the finished screen follows it");
    assert!(
        !frames.last().expect("frames were painted").1,
        "nothing keeps asking for frames once it has cued"
    );
}

// ---------------------------------------------------------------------------
// The whole app: the handoff
// ---------------------------------------------------------------------------

#[test]
fn the_cold_start_overlay_unmounts_onto_an_identical_connect_screen() {
    let _g = serial();
    let _ambient = setup();

    let mut state = MuxrApp.init();
    let mut logic = |s: &mut AppState| MuxrApp.build(s);
    let frames = drive(
        &mut logic,
        &mut state,
        theme(false),
        frame_at(sequence_end_ms() + 400.0),
    );
    let heading = heading_ids();

    // Whether *this* test won the process's one cold-start claim decides
    // whether an overlay ever mounted (test order within the binary is not
    // fixed) — but either way the final frame is the same: the route's own
    // Connect screen, exactly once, with nothing left animating.
    let (last, needs_frame) = frames.last().expect("frames were painted");
    assert_eq!(
        occurrences(&last.glyph_ids, &heading),
        1,
        "exactly one Connect screen is on screen after the handoff — the \
         overlay's copy is gone, the route's remains"
    );
    assert_eq!(last.images, 0, "no splash icon survives the handoff");
    assert!(!needs_frame, "and the app has settled");

    if frames[frame_at(500.0)].0.images == 1 {
        // This test claimed the launch: assert the overlay really did cover
        // the live route while it played.
        assert_eq!(
            occurrences(&frames[frame_at(500.0)].0.glyph_ids, &heading),
            1,
            "during the icon beat only the route's own copy is painted \
             (under an opaque backdrop)"
        );
        let assembling = &frames[frame_at(assembly_start_ms() + HEADING_DELAY_MS + 60.0)].0;
        assert_eq!(
            occurrences(&assembling.glyph_ids, &heading),
            2,
            "mid-assembly both copies are painted: the route's, and the \
             overlay's landing one"
        );
    }
}

#[test]
fn a_second_launch_decision_in_the_same_process_never_replays_the_splash() {
    let _g = serial();
    let _ambient = setup();

    // Whichever of these claimed the process's splash, the *second* one must
    // not play — the once-per-process rule, observed end to end rather than
    // through the pure decision function. Both are actually *built*: the
    // claim is spent on a launch's first build, not on `init` (see
    // `AppState::splash_plan`), so an unbuilt app never spends one.
    let mut logic = |s: &mut AppState| MuxrApp.build(s);
    let mut first = MuxrApp.init();
    drive(&mut logic, &mut first, theme(false), 2);

    let mut second = MuxrApp.init();
    let frames = drive(&mut logic, &mut second, theme(false), frame_at(500.0));

    assert!(
        frames.iter().all(|(scene, _)| scene.images == 0),
        "a warm start paints no splash icon at all"
    );
    assert_eq!(
        occurrences(
            &frames.last().expect("frames were painted").0.glyph_ids,
            &heading_ids()
        ),
        1,
        "it goes straight to the Connect screen"
    );
}
