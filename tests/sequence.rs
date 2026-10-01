//! Headless tests for the cold-start splash *sequence* — the orchestrator, its handoff to the
//! `/` route, and the reduced-motion fallback.
//!
//! `tests/splash.rs` covers the three beats in isolation; this file covers what only the
//! assembled thing can show: beats firing in order off one clock, widgets staggering in, the
//! overlay unmounting itself onto an identical picture underneath. Like `tests/splash.rs` these
//! can't use `support::frame` (pins `FrameTime::ZERO`): every assertion here is a function of the
//! frame clock, so each test drives rebuild → layout → paint itself, exactly as a shell's frame
//! loop does — which is also what lets the overlay's unmount happen mid-run.
//!
//! **Frame cadence.** Every helper takes the cadence in nanoseconds ([`drive_at`],
//! [`frame_at_cadence`]); the plain [`drive`]/[`frame_at`] wrappers pin [`FRAME_NS`] (60Hz). The
//! splash is clock-driven, not frame-driven, so the same wall-clock timeline must come out of a
//! [`FRAME_NS_120`] run too, and out of an uneven one ([`janked_clock`]) with a frame dropped.
//!
//! **Reading a widget's text out of a paint pass.** A painted `GlyphRun` carries glyph *ids*, not
//! a string. [`glyph_ids`] shapes an expected string through the same [`TextContext`] the widgets
//! lay out against and compares id sequences — counting *occurrences* distinguishes the splash's
//! assembling copy from the live route's copy underneath it.

use std::any::Any;

use frust::authoring::text::{TextContext, TextStyle};
use frust::authoring::{Point, View, any};
use frust::{AnyView, Component, Get, RwSignal, Theme};
use frust_core::{FrameTime, PaintScene, RenderRoot};
use frust_scene::GlyphRun;
use kurbo::{Rect, Size};
use peniko::Color;

use muxr::features::connect::domain::entities::SavedServer;
use muxr::features::connect::presentation::pages::connect_layout;
use muxr::features::connect::presentation::widgets::connect_pieces;
use muxr::features::connect::presentation::widgets::getting_started::GETTING_STARTED_BODY;
use muxr::features::connect::presentation::widgets::saved::splash_saved_servers;
use muxr::features::connect::presentation::widgets::tablet::{
    tablet_splash_layout, tablet_splash_pieces,
};
use muxr::ui::backdrop::page_backdrop;
use muxr::ui::splash::icon_phase::{IconArt, splash_icon_art};
use muxr::ui::splash::sequence::{
    assembly_start_ms, dissolve_cue_ms, reduced_splash, sequence_end_ms, splash_sequence,
};
use muxr::{AppState, MuxrApp};

mod support;
use support::{serial, setup};

/// The design reference's phone screen (`.screen{height:650px}` inside a
/// 340px-wide frame) — the same surface `tests/splash.rs` uses.
const SURFACE: Size = Size::new(340.0, 650.0);
/// One 60Hz frame — `spec::REFERENCE_FRAME_MS`'s cadence, and the default every helper pins.
const FRAME_NS: u64 = 16_666_667;
/// One 120Hz frame — the cadence the clock-driven timeline must land identically on.
const FRAME_NS_120: u64 = 8_333_333;
/// The reduced-motion hold (`spec::REDUCED_HOLD_MS`, which is `pub(crate)`).
const REDUCED_HOLD_MS: f64 = 200.0;
/// The heading piece's stagger (`spec::PIECE_DELAYS_MS[2]`, likewise).
const HEADING_DELAY_MS: f64 = 260.0;
/// How many blurred halos a **settled** Connect screen paints, land flashes excluded: one per
/// saved-server row's `.dot-ok` glow — three, since piece 7 renders the real repository's list and
/// every headless suite runs on `MockSavedServersRepository::seeded`'s three rigs. Asserting that
/// exact residue rather than zero keeps the claim that every *land flash* has faded: one still
/// running would push the count above three.
const RESTING_SHADOWS: usize = 3;
/// Every frame of a rain-less run stays under this many glyph runs: a settled Connect screen's own
/// 24 (measured, seeded store) plus slack — the burst adds 14 over it, a rain field hundreds.
const RAIN_FREE_CEILING: usize = 44;
/// The burst's particle count (`spec::BURST_COUNT`, also `pub(crate)`).
const BURST_COUNT: usize = 14;
/// A full-band Tablet window (`tests/connect.rs`'s `FULL_TABLET`): the class the composition
/// root hands the sequence the Tablet pieces on.
const FULL_TABLET: Size = Size::new(1024.0, 720.0);

/// A recording paint target: the flattened glyph-id stream, plus a count of every other draw kind.
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
    let mut theme = frust_glyph::baseline();
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

/// The frame index by which `ms` of animation has elapsed at `cadence_ns`. Frame 0 seeds the
/// clock (zero delta), so `ms` of motion is in by frame `ms/cadence + 1`.
fn frame_at_cadence(ms: f64, cadence_ns: u64) -> usize {
    (ms / cadence_ms(cadence_ns)).round() as usize + 1
}

/// [`frame_at_cadence`] at 60Hz.
fn frame_at(ms: f64) -> usize {
    frame_at_cadence(ms, FRAME_NS)
}

/// Drive the real shell loop once per entry in `clock` (wall-clock instants, in nanoseconds) —
/// rebuild, lay out shaping real text, paint — returning each frame's recording and whether it
/// asked for another one. Rebuilding *every* frame (rather than once up front) is what makes the
/// overlay's own unmount observable: the sequence ends by writing a signal the root reads.
fn drive_clock<V: View<AppState>>(
    clock: &[u64],
    logic: &mut impl FnMut(&mut AppState) -> V,
    state: &mut AppState,
    theme: Theme,
) -> Vec<(Recorder, bool)> {
    drive_clock_sized(clock, logic, state, theme, SURFACE)
}

/// [`drive_clock`] laid out at an explicit `surface` — a Tablet window, where the phone's
/// [`SURFACE`] could not hold the two-column arrangement.
fn drive_clock_sized<V: View<AppState>>(
    clock: &[u64],
    logic: &mut impl FnMut(&mut AppState) -> V,
    state: &mut AppState,
    theme: Theme,
    surface: Size,
) -> Vec<(Recorder, bool)> {
    let mut root: RenderRoot<AppState, V> = RenderRoot::new();
    let mut text_ctx = TextContext::new();
    root.set_theme(Box::new(theme));

    clock
        .iter()
        .map(|at| {
            root.rebuild(logic, state);
            let text_any: &mut dyn Any = &mut text_ctx;
            root.layout_with_text(surface, text_any);
            let mut scene = Recorder::default();
            let outcome = root.paint(&mut scene, FrameTime::from_nanos(*at));
            (scene, outcome.needs_frame)
        })
        .collect()
}

/// A steady `frames`-long clock at `cadence_ns`.
fn steady_clock(cadence_ns: u64, frames: usize) -> Vec<u64> {
    (0..frames as u64).map(|frame| frame * cadence_ns).collect()
}

/// A 60Hz clock with one long frame `gap_ms` wide straddling `straddling_ms` — the cold-start jank
/// (a GC pause, a late first layout) that opens a gate well past its nominal instant.
fn janked_clock(gap_ms: f64, straddling_ms: f64, frames: usize) -> Vec<u64> {
    let gap_ns = (gap_ms * 1_000_000.0) as u64;
    let straddled_ns = (straddling_ms * 1_000_000.0) as u64;
    steady_clock(FRAME_NS, frames)
        .into_iter()
        .map(|at| if at >= straddled_ns { at + gap_ns } else { at })
        .collect()
}

/// [`drive_clock`] over a steady `frames`-long run at `cadence_ns`.
fn drive_at<V: View<AppState>>(
    cadence_ns: u64,
    logic: &mut impl FnMut(&mut AppState) -> V,
    state: &mut AppState,
    theme: Theme,
    frames: usize,
) -> Vec<(Recorder, bool)> {
    drive_clock(&steady_clock(cadence_ns, frames), logic, state, theme)
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

/// The index of the first frame satisfying `beat`; panics with `what` if no frame did.
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

/// How many times `needle` appears in `haystack` — one per copy of that string on screen.
fn occurrences(haystack: &[u32], needle: &[u32]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// The heading piece's exact style, so its shaped ids match what `widgets::badge::heading` paints.
fn heading_ids() -> Vec<u32> {
    let theme = frust_glyph::baseline();
    glyph_ids("Connect to server", &theme.type_scale.title_medium)
}

/// The opening of the Tablet panel's orientation card at its exact style, so its shaped ids
/// match what `widgets::getting_started::getting_started_card` paints — the one piece on screen
/// only in the Tablet arrangement (the phone keeps that paragraph in a dialog). A prefix well
/// inside the card's first line, so no soft wrap can split the fingerprint.
fn orientation_ids() -> Vec<u32> {
    const OPENING: &str = "Muxr is the mobile client";
    assert!(GETTING_STARTED_BODY.starts_with(OPENING));
    let theme = frust_glyph::baseline();
    glyph_ids(OPENING, &theme.type_scale.body_small)
}

/// How many of `scene`'s glyphs came from the burst's particle alphabet (`spec::BURST_CHARS`).
fn burst_glyphs(scene: &Recorder) -> usize {
    // `spec::BURST_CHARS` at `spec::BURST_FONT_SIZE` (`pub(crate)`), on the mono face `burst` uses.
    let theme = frust_glyph::baseline();
    let style = TextStyle {
        family: theme.type_scale.body_medium.family.clone(),
        size: 13.0,
        ..TextStyle::default()
    };
    let alphabet: Vec<u32> = ["▣", "◧", "◎", "▤", "0", "1", "#"]
        .iter()
        .flat_map(|glyph| glyph_ids(glyph, &style))
        .collect();
    scene
        .glyph_ids
        .iter()
        .filter(|id| alphabet.contains(id))
        .count()
}

/// The decoded app icon, asserted to be the bitmap so an `images` count means what it claims.
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

/// Mount the orchestrator alone (no navigator underneath) on `clock`, so every draw recorded
/// belongs to the splash. `saved` is piece 7's rows: `None` reads them exactly as the app does
/// (resolved *after* `MuxrApp::init` publishes the repository, `AppState::splash_plan`'s
/// ordering), `Some(..)` pins a list so a test can mount the store state it wants.
fn mount_sequence_on(
    clock: &[u64],
    reduce_motion: bool,
    saved: Option<Vec<SavedServer>>,
) -> (Vec<(Recorder, bool)>, RwSignal<bool>) {
    let mut state = MuxrApp.init();
    let saved = saved.unwrap_or_else(splash_saved_servers);
    let art = decoded_icon();
    let done = RwSignal::new(false);
    let mut logic = move |_: &mut AppState| -> AnyView<AppState> {
        splash_sequence(
            art.clone(),
            7,
            connect_pieces(&frust_glyph::baseline(), &saved),
            connect_layout,
            done,
        )
    };
    let frames = drive_clock(clock, &mut logic, &mut state, theme(reduce_motion));
    (frames, done)
}

/// [`mount_sequence_on`] over a steady `frames`-long run at `cadence_ns`.
fn mount_sequence_at(
    cadence_ns: u64,
    reduce_motion: bool,
    frames: usize,
    saved: Option<Vec<SavedServer>>,
) -> (Vec<(Recorder, bool)>, RwSignal<bool>) {
    mount_sequence_on(&steady_clock(cadence_ns, frames), reduce_motion, saved)
}

/// [`mount_sequence_at`] at 60Hz, over the repository's real list — what the app itself
/// assembles, which under every headless suite is the seeded mock's three rigs.
fn mount_sequence(reduce_motion: bool, frames: usize) -> (Vec<(Recorder, bool)>, RwSignal<bool>) {
    mount_sequence_at(FRAME_NS, reduce_motion, frames, None)
}

#[test]
fn the_sequence_walks_icon_then_burst_and_assembly_off_one_clock() {
    let _g = serial();
    let _ambient = setup();

    let (frames, done) = mount_sequence(false, frame_at(sequence_end_ms() + 200.0));
    let heading = heading_ids();

    // Icon beat: bitmap up, nothing else started.
    let icon_beat = &frames[frame_at(500.0)].0;
    assert_eq!(icon_beat.images, 1, "the app icon paints on its own first");
    assert_eq!(
        icon_beat.glyph_runs, 0,
        "nothing else draws during the icon beat: {} runs",
        icon_beat.glyph_runs
    );

    // Dissolve cue: the burst flies, the assembly is under way, the staggered heading is not. No
    // piece paints text this early, so the particles account for every glyph on the frame.
    let burst_beat = &frames[frame_at(dissolve_cue_ms() + 100.0)].0;
    assert_eq!(
        (burst_glyphs(burst_beat), burst_beat.glyph_ids.len()),
        (BURST_COUNT, BURST_COUNT),
        "the dissolve must paint {BURST_COUNT} burst particles and no other text: {} of {} \
         glyphs came from the particle alphabet",
        burst_glyphs(burst_beat),
        burst_beat.glyph_ids.len()
    );
    assert_eq!(
        occurrences(&burst_beat.glyph_ids, &heading),
        0,
        "the heading has not reached its stagger yet"
    );

    // No rain at any instant: every frame stays under the settled screen's own runs plus slack.
    for (index, (scene, _)) in frames.iter().enumerate() {
        assert!(
            scene.glyph_runs <= RAIN_FREE_CEILING,
            "frame {index} ({:.0}ms) painted {} runs against a {RAIN_FREE_CEILING}-run ceiling — \
             that is a rain field, and the cold start has none",
            frame_ms(index, FRAME_NS),
            scene.glyph_runs
        );
    }

    // Assembly: staggered, piece by piece.
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

    // End: assembled, quiet, cueing the unmount.
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
fn a_tablet_window_assembles_the_tablet_screen_rather_than_the_phone_column() {
    let _g = serial();
    let _ambient = setup();

    // What the composition root hands the sequence on a full-band Tablet window (its own read of
    // the class), mounted under that window's metrics and laid out at that window.
    let mut state = MuxrApp.init();
    let saved = splash_saved_servers();
    let art = decoded_icon();
    let done = RwSignal::new(false);
    let baseline = frust_glyph::baseline();
    let mut logic = move |_: &mut AppState| -> AnyView<AppState> {
        let assemble_theme = baseline.clone();
        splash_sequence(
            art.clone(),
            7,
            tablet_splash_pieces(&baseline, &saved, true),
            move |pieces| tablet_splash_layout(&assemble_theme, true, pieces),
            done,
        )
    };
    let metrics = support::metrics_at(FULL_TABLET.width, FULL_TABLET.height);
    let frames = support::mount_with_metrics(Some(metrics), || {
        drive_clock_sized(
            &steady_clock(FRAME_NS, frame_at(sequence_end_ms() + 200.0)),
            &mut logic,
            &mut state,
            theme(false),
            FULL_TABLET,
        )
    });
    let orientation = orientation_ids();
    let heading = heading_ids();

    // Icon beat: the screen has not started.
    assert_eq!(
        frames[frame_at(500.0)].0.glyph_runs,
        0,
        "nothing of the screen draws during the icon beat"
    );
    // The card is the panel's third piece: cued with the rest, staggered behind the brand row.
    let gate = &frames[frame_at(assembly_start_ms() + 60.0)].0;
    assert_eq!(
        occurrences(&gate.glyph_ids, &orientation),
        0,
        "the orientation card waits for its own stagger"
    );

    // End: the Tablet screen, assembled — the panel's orientation card beside the form's heading.
    let (last, needs_frame) = frames.last().expect("frames were painted");
    assert_eq!(
        occurrences(&last.glyph_ids, &orientation),
        1,
        "the finished overlay is the Tablet screen: its panel's orientation card is painted"
    );
    assert_eq!(
        occurrences(&last.glyph_ids, &heading),
        1,
        "…beside the form's heading"
    );
    assert_eq!(last.images, 0, "the icon is long gone");
    assert!(
        !needs_frame,
        "a finished sequence stops requesting frames — nothing left to animate"
    );
    assert!(done.get(), "and cues its own unmount");
}

#[test]
fn a_store_with_no_saved_servers_assembles_no_saved_card_at_all() {
    let _g = serial();
    let _ambient = setup();

    // Regression: the splash used to paint one hardcoded "Home rig" row whatever the store held, so
    // an empty-store user watched a card assemble then vanish at the route handoff. Piece 7's rows
    // are an input now — empty list in, empty list out. Each row's status dot is a glowing swatch
    // (`draw_shadow`) and nothing else blurs on a settled overlay, so the count *is* the rows.
    let end = frame_at(sequence_end_ms() + 200.0);
    let (empty, _) = mount_sequence_at(FRAME_NS, false, end, Some(Vec::new()));
    let (last, _) = empty.last().expect("frames were painted");
    assert_eq!(
        last.shadows, 0,
        "no saved servers must assemble no saved rows"
    );

    // Control: the seeded store still paints one dot per stored rig, so the assertion above is
    // about the data, not the dot having stopped painting.
    let (seeded, _) = mount_sequence_at(FRAME_NS, false, end, None);
    let (last_seeded, _) = seeded.last().expect("frames were painted");
    assert_eq!(
        last_seeded.shadows, RESTING_SHADOWS,
        "the seeded store's rows still assemble"
    );
}

#[test]
fn the_completion_cue_fires_once_the_whole_timeline_has_run() {
    let _g = serial();
    let _ambient = setup();

    // A frame short of the end: must not have fired yet.
    let (_, early) = mount_sequence(false, frame_at(sequence_end_ms()) - 2);
    assert!(!early.get(), "the sequence is not over before its end");

    // Just past it: has fired.
    let (_, done) = mount_sequence(false, frame_at(sequence_end_ms()) + 2);
    assert!(done.get());
}

#[test]
fn a_frame_gap_straddling_the_gate_delays_the_unmount_rather_than_cutting_it_short() {
    let _g = serial();
    let _ambient = setup();

    // The nominal run's settled last frame is what the janked run's own last frame must match.
    let (nominal, _) = mount_sequence(false, frame_at(sequence_end_ms() + 200.0));
    let settled = &nominal.last().expect("frames were painted").0;
    let heading = heading_ids();

    let gap_ms = 200.0;
    let clock = janked_clock(
        gap_ms,
        assembly_start_ms(),
        frame_at(sequence_end_ms() + gap_ms + 200.0),
    );
    let (frames, done) = mount_sequence_on(&clock, false, None);
    let at = |frame: usize| clock[frame] as f64 / 1_000_000.0;
    assert!(done.get(), "the janked run still cues its own unmount");

    // The gate opens on the long frame's far side; a fired cue stops asking for frames, so the
    // last frame that asked for one is the frame it fired on — the overlay's last painted frame.
    let gate_open = first_frame(&frames, "the assembly gate opening", |(scene, _)| {
        scene.glyph_runs > 0
    });
    let quiet = first_frame(&frames, "the sequence settling", |(_, needs_frame)| {
        !needs_frame
    });
    let firing = &frames[quiet - 1].0;
    assert!(
        at(gate_open) >= assembly_start_ms() + gap_ms,
        "the long frame must straddle the gate, which opened at {:.0}ms",
        at(gate_open)
    );

    assert_eq!(
        firing.shadows, settled.shadows,
        "a piece was still flashing on the overlay's last frame: {} blurred rects against the \
         settled screen's {}",
        firing.shadows, settled.shadows
    );
    assert_eq!(
        (firing.glyph_runs, occurrences(&firing.glyph_ids, &heading)),
        (
            settled.glyph_runs,
            occurrences(&settled.glyph_ids, &heading)
        ),
        "the overlay must unmount onto the settled screen, never a mid-assembly one"
    );
    assert!(
        at(quiet - 1) - at(gate_open) >= sequence_end_ms() - assembly_start_ms(),
        "the cue fired {:.0}ms after the gate opened at {:.0}ms — short of the assembly's own \
         {:.0}ms window",
        at(quiet - 1) - at(gate_open),
        at(gate_open),
        sequence_end_ms() - assembly_start_ms()
    );
}

// ---------------------------------------------------------------------------
// Frame cadence
// ---------------------------------------------------------------------------

/// The timeline beats a paint stream can witness, in wall-clock ms since the overlay's first frame.
#[derive(Debug)]
struct Beats {
    /// First frame drawing any glyph at all (the icon beat before it draws only the bitmap).
    dissolve_cue: f64,
    /// First frame the staggered heading piece paints.
    heading: f64,
    /// The frame the completion cue fired on — the last one that asked for another, since a
    /// fired cue early-returns without requesting.
    end: f64,
}

/// Drive the whole sequence at `cadence_ns` and time its beats.
fn observed_beats(cadence_ns: u64) -> Beats {
    let heading = heading_ids();
    let (frames, _) = mount_sequence_at(
        cadence_ns,
        false,
        frame_at_cadence(sequence_end_ms() + 200.0, cadence_ns),
        None,
    );

    let dissolve_cue = first_frame(&frames, "the dissolve cue", |(scene, _)| {
        scene.glyph_runs > 0
    });
    let heading_frame = first_frame(&frames, "the heading piece", |(scene, _)| {
        occurrences(&scene.glyph_ids, &heading) > 0
    });
    let quiet = first_frame(&frames, "the sequence settling", |(_, needs_frame)| {
        !needs_frame
    });
    Beats {
        dissolve_cue: frame_ms(dissolve_cue, cadence_ns),
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

    // Twice the frames, same wall clock: every beat is driven by its widget's own accumulated
    // clock rather than by a per-frame step, so cadence never turns into speed. Each beat is
    // quantized to its own cadence, so two runs can differ by at most the coarser frame.
    for (what, at_60, at_120, expected, gates) in [
        (
            "the dissolve cue",
            sixty.dissolve_cue,
            one_twenty.dissolve_cue,
            dissolve_cue_ms(),
            1.0,
        ),
        (
            "the heading piece",
            sixty.heading,
            one_twenty.heading,
            assembly_start_ms() + HEADING_DELAY_MS,
            // Two quantizations compound: the assembly gate opening, then the piece's stagger.
            2.0,
        ),
        (
            "the sequence end",
            sixty.end,
            one_twenty.end,
            sequence_end_ms(),
            // Two quantizations again: the assembly gate opening, then the cue riding it.
            2.0,
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

    // Defense in depth: the shell mounts `reduced_splash` instead, but a theme that flips
    // `reduce_motion` on *after* the mount decision must not hold the app behind the full timeline.
    let (frames, done) = mount_sequence(true, 4);
    assert!(done.get(), "the cue fires immediately under reduced motion");
    for (index, (scene, needs_frame)) in frames.iter().enumerate() {
        assert_eq!(scene.images, 0, "no icon animation");
        assert_eq!(
            scene.shadows, RESTING_SHADOWS,
            "no land flashes — only the resting dot glow, which is chrome \
             rather than motion and so survives reduced motion"
        );
        // The firing frame asks for the one frame that acts on its write; nothing left after.
        assert!(
            !needs_frame || index == 0,
            "frame {index} kept the shell spinning"
        );
    }
    // Pieces are painted in their landed state, not withheld.
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
    // Mirrors the shell (`AppState::splash_overlay`): the cue's write *unmounts* the overlay, so
    // this closure drops it once `done` is set — re-mounting it would never see the hold end.
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

    // The whole hold shows the icon. `reduce_motion` is on for every frame here (why this path was
    // chosen), so a cue short-circuiting on it would fire on frame 0 — a ~16ms flicker, not a beat.
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

    // Once the hold is out, the overlay is gone.
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

    // Whether *this* test won the process's one cold-start claim decides whether an overlay ever
    // mounted (test order in the binary isn't fixed) — either way the final frame is the same.
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
        // This test claimed the launch: assert the overlay really covered the live route.
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

    // Whichever of these claimed the process's splash, the *second* must not play — the
    // once-per-process rule, observed end to end. Both are actually *built*: the claim is spent on
    // a launch's first build, not `init` (`AppState::splash_plan`), so an unbuilt app spends none.
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
