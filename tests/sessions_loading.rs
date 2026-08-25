//! Clock-driven proof of the sessions loading rain: `ui::splash::loading`'s
//! orchestrator mounted alone, then the real `/sessions` band under it. Every
//! assertion is a function of real frame time, so each test drives rebuild →
//! layout → paint itself, as `tests/sequence.rs` does.
//!
//! The release edge is the observed settle, floored at [`MIN_DENSE_MS`]. The
//! mounted half settles the mock repository's first load with the frame clock
//! **pinned at zero** ([`Band::pinned`]) — a `FrameTime` delta is the only
//! thing that moves the overlay's clock — so the rain holds its dense beat
//! over the whole wait, and the advancing run that follows starts from an
//! untouched clock with the release landing exactly on that floor.
//!
//! A painted `GlyphRun` carries glyph ids, not a string, so [`glyph_ids`]
//! shapes the expected text through the same [`TextContext`] the widgets lay
//! out against and [`occurrences`] counts copies of it.

use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use frust::authoring::text::{TextContext, TextStyle};
use frust::authoring::{Point, View, any};
use frust::{
    AnyView, Column, Component, Get, NavigatorController, RwSignal, SizedBox, Stack, Theme,
    navigator, text,
};
use frust_core::{FrameTime, PaintScene, RenderRoot};
use frust_scene::GlyphRun;
use kurbo::{Rect, Size};
use peniko::Color;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::sessions::presentation::pages::{sessions_page, set_loading_rain_for_tests};
use muxr::routes::install_app_router;
use muxr::ui::splash::loading::loading_sequence;
use muxr::{AppState, MuxrApp};

mod support;
use support::{node_centers_by_value, node_values, pump_frames, serial, setup, surface, tap};

/// The design reference's phone screen, as `tests/sequence.rs` drives it.
const SURFACE: Size = Size::new(340.0, 650.0);
const FRAME_NS: u64 = 16_666_667;
/// One 120Hz frame — the cadence the rain's `frame_factor` exists for.
const FRAME_NS_120: u64 = 8_333_333;

/// Floor on the dense beat before a release takes effect
/// (`spec::LOADING_MIN_DENSE_MS`; every constant below is transcribed because
/// that module is `pub(crate)`).
const MIN_DENSE_MS: f64 = 600.0;
/// Hold ceiling, releasing a load that never settles (`spec::LOADING_MAX_HOLD_MS`).
const MAX_HOLD_MS: f64 = 8_000.0;
/// Release to first landing (`spec::LOADING_ASSEMBLY_OFFSET_MS`).
const ASSEMBLY_OFFSET_MS: f64 = 100.0;
/// A piece's entrance duration (`spec::PIECE_IN_MS`).
const PIECE_IN_MS: f64 = 500.0;
/// Per-piece stagger in source order (`spec::PIECE_DELAYS_MS`).
const PIECE_DELAYS_MS: [f64; 8] = [0.0, 130.0, 260.0, 390.0, 480.0, 580.0, 720.0, 860.0];
/// Release to the end of the whole sequence (`loading::sequence_end_ms`).
const SEQUENCE_END_MS: f64 = 1_800.0;

/// Glyph runs a frame must beat to count as "the rain is up": the dense field
/// paints one run per glyph, where the band's own text is a few dozen.
const DENSE_RUNS: usize = 100;
const PIECES: usize = 3;
/// Fixes the standalone rain's field, so a run replays exactly.
const SEED: u64 = 0x6C6F_6164;

/// A recording paint target: the glyph-id stream, its run count, its fills.
#[derive(Default)]
struct Recorder {
    glyph_ids: Vec<u32>,
    glyph_runs: usize,
    fills: usize,
}

impl PaintScene for Recorder {
    fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {
        self.fills += 1;
    }
    fn fill_rounded_rect(&mut self, _origin: Point, _size: Size, _radius: f64, _color: Color) {}
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
    fn draw_glyph_run(&mut self, run: GlyphRun) {
        self.glyph_runs += 1;
        self.glyph_ids
            .extend(run.glyphs.iter().map(|glyph| glyph.id));
    }
    fn draw_image(&mut self, _data: &peniko::ImageData, _dest: Rect) {}
}

fn theme(reduce_motion: bool) -> Theme {
    let mut theme = frust_glyph::baseline();
    theme.motion.reduce_motion = reduce_motion;
    theme
}

fn tone() -> Color {
    frust_glyph::baseline().scheme().surface
}

fn cadence_ms(cadence_ns: u64) -> f64 {
    cadence_ns as f64 / 1_000_000.0
}

fn frame_ms(frame: usize, cadence_ns: u64) -> f64 {
    frame as f64 * cadence_ms(cadence_ns)
}

/// The frame index by which `ms` of animation has elapsed at `cadence_ns` —
/// frame 0 seeds the clock at a zero delta, so it is `ms/cadence + 1`.
fn frame_at_cadence(ms: f64, cadence_ns: u64) -> usize {
    (ms / cadence_ms(cadence_ns)).round() as usize + 1
}

fn frame_at(ms: f64) -> usize {
    frame_at_cadence(ms, FRAME_NS)
}

/// The glyph ids `text` shapes to at `style` — the fingerprint a painted run of
/// the same string leaves in [`Recorder::glyph_ids`].
fn glyph_ids(text: &str, style: &TextStyle) -> Vec<u32> {
    let mut ctx = TextContext::new();
    ctx.layout(text, style, None)
        .to_scene_runs(Point::ORIGIN)
        .iter()
        .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.id))
        .collect()
}

fn occurrences(haystack: &[u32], needle: &[u32]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

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

/// When the completion cue fired: the last frame that asked for another one.
/// The overlay requests one every paint until it fires, and one on that frame.
fn fired_at(frames: &[(Recorder, bool)], cadence_ns: u64) -> f64 {
    let quiet = first_frame(frames, "the sequence settling", |(_, needs_frame)| {
        !needs_frame
    });
    frame_ms(quiet - 1, cadence_ns)
}

/// Assert `fired` landed inside the frame after its `expected` mark.
fn lands_on(what: &str, fired: f64, expected: f64, cadence_ns: u64) {
    let frame = cadence_ms(cadence_ns);
    assert!(
        fired >= expected && fired - expected <= frame,
        "{what} landed at {fired:.1}ms, not inside the frame after its {expected:.1}ms mark"
    );
}

// ---------------------------------------------------------------------------
// Part A — the orchestrator mounted alone
// ---------------------------------------------------------------------------

/// Drive `frames` frames of the real shell loop at `cadence_ns` — rebuild, lay
/// out shaping real text, paint — returning each frame's recording and whether
/// it asked for another. `tick`, run before each rebuild, is where a test flips
/// the release flag its `logic` reads.
fn drive_at<V: View<AppState>>(
    cadence_ns: u64,
    logic: &mut impl FnMut(&mut AppState) -> V,
    state: &mut AppState,
    theme: Theme,
    frames: usize,
    tick: &mut impl FnMut(usize),
) -> Vec<(Recorder, bool)> {
    let mut root: RenderRoot<AppState, V> = RenderRoot::new();
    let mut text_ctx = TextContext::new();
    root.set_theme(Box::new(theme));

    (0..frames)
        .map(|frame| {
            tick(frame);
            root.rebuild(logic, state);
            let text_any: &mut dyn Any = &mut text_ctx;
            root.layout_with_text(SURFACE, text_any);
            let mut scene = Recorder::default();
            let outcome = root.paint(&mut scene, FrameTime::from_nanos(frame as u64 * cadence_ns));
            (scene, outcome.needs_frame)
        })
        .collect()
}

/// The `index`th piece's text — a plain leaf, so what the assembly proves is
/// the stagger rather than any one screen's widgets.
fn piece_name(index: usize) -> String {
    format!("card-{index}")
}

fn pieces() -> Vec<AnyView<AppState>> {
    (0..PIECES).map(|i| any(text(piece_name(i)))).collect()
}

/// The glyph ids the `index`th piece paints — a plain `text` shapes at
/// `TextStyle::default()`, and the theme's recolor doesn't shape.
fn piece_ids(index: usize) -> Vec<u32> {
    glyph_ids(&piece_name(index), &TextStyle::default())
}

/// Mount the loading rain alone over [`pieces`] at `cadence_ns`, so every draw
/// recorded belongs to it. `release_frame` is the frame the `released` flag
/// flips on; `None` never settles, leaving the hold ceiling to release it.
fn mount_loading_at(
    cadence_ns: u64,
    reduce_motion: bool,
    frames: usize,
    release_frame: Option<usize>,
) -> (Vec<(Recorder, bool)>, RwSignal<bool>) {
    let mut state = MuxrApp.init();
    let released = Rc::new(Cell::new(false));
    let flag = Rc::clone(&released);
    let done = RwSignal::new(false);
    let mut logic = move |_: &mut AppState| -> AnyView<AppState> {
        loading_sequence(
            SEED,
            tone(),
            pieces(),
            |pieces| any(Column(pieces)),
            flag.get(),
            done,
        )
    };
    let mut tick = |frame: usize| {
        if release_frame.is_some_and(|at| frame >= at) {
            released.set(true);
        }
    };
    let frames = drive_at(
        cadence_ns,
        &mut logic,
        &mut state,
        theme(reduce_motion),
        frames,
        &mut tick,
    );
    (frames, done)
}

/// Release mid-hold at `cadence_ns`: when the cue fired, and the release
/// instant the widget observed.
fn observed_release(cadence_ns: u64) -> (f64, f64) {
    const RELEASE_MS: f64 = 2_500.0;
    let release_frame = frame_at_cadence(RELEASE_MS, cadence_ns);
    let observed = frame_ms(release_frame, cadence_ns);
    let end = observed + SEQUENCE_END_MS;
    let (frames, done) = mount_loading_at(
        cadence_ns,
        false,
        frame_at_cadence(end + 200.0, cadence_ns),
        Some(release_frame),
    );
    assert!(done.get(), "a released sequence always cues its unmount");
    (fired_at(&frames, cadence_ns), end)
}

#[test]
fn the_held_rain_stays_dense_and_assembles_nothing_while_the_band_loads() {
    let _g = serial();
    let _ambient = setup();

    let held = MAX_HOLD_MS - 1_000.0;
    let (frames, done) = mount_loading_at(FRAME_NS, false, frame_at(held) + 2, None);
    let card = piece_ids(0);

    // The clock holds at the dense boundary, so a wait outlasting the whole
    // cold-start timeline still paints the same full field.
    for at in [200.0, 3_000.0, held] {
        let (scene, needs_frame) = &frames[frame_at(at)];
        assert!(
            scene.glyph_runs > DENSE_RUNS,
            "the rain must still be dense at {at}ms: {} runs",
            scene.glyph_runs
        );
        assert_eq!(
            occurrences(&scene.glyph_ids, &card),
            0,
            "nothing may assemble at {at}ms — the band is still loading"
        );
        assert!(needs_frame, "a held rain keeps asking for frames at {at}ms");
    }
    assert!(!done.get(), "and never cues its own unmount");
}

#[test]
fn a_load_that_never_settles_releases_itself_at_the_hold_ceiling() {
    let _g = serial();
    let _ambient = setup();

    let end = MAX_HOLD_MS + SEQUENCE_END_MS;
    let (frames, done) = mount_loading_at(FRAME_NS, false, frame_at(end + 200.0), None);

    assert!(
        frames[frame_at(9_300.0)].0.glyph_runs < frames[frame_at(7_900.0)].0.glyph_runs,
        "the ceiling resumes the clock, and the rain thins from there"
    );
    assert_eq!(
        occurrences(
            &frames[frame_at(MAX_HOLD_MS + ASSEMBLY_OFFSET_MS + PIECE_IN_MS)]
                .0
                .glyph_ids,
            &piece_ids(0)
        ),
        1,
        "the first piece lands one offset after the self-release"
    );
    assert!(done.get(), "a hold nothing below it times out still ends");
    lands_on(
        "the self-released cue",
        fired_at(&frames, FRAME_NS),
        end,
        FRAME_NS,
    );
}

#[test]
fn a_settling_band_releases_the_rain_and_staggers_its_pieces_in() {
    let _g = serial();
    let _ambient = setup();

    let release_frame = frame_at(2_500.0);
    let observed = frame_ms(release_frame, FRAME_NS);
    let end = observed + SEQUENCE_END_MS;
    let (frames, done) =
        mount_loading_at(FRAME_NS, false, frame_at(end + 200.0), Some(release_frame));
    let card = piece_ids(0);

    assert!(
        frames[frame_at(3_800.0)].0.glyph_runs < frames[frame_at(2_600.0)].0.glyph_runs,
        "the rain thins from the release, not from its own dense boundary"
    );

    let opens = frame_at(observed + ASSEMBLY_OFFSET_MS);
    assert_eq!(
        occurrences(&frames[opens - 1].0.glyph_ids, &card),
        0,
        "the assembly waits out its offset after the release"
    );
    assert_eq!(
        occurrences(
            &frames[frame_at(observed + ASSEMBLY_OFFSET_MS + PIECE_IN_MS)]
                .0
                .glyph_ids,
            &card
        ),
        1,
        "and then drops the first piece in"
    );

    // Each piece self-clocks from the assembly's first paint, so the stagger is
    // the gap between two pieces' first frames.
    let first = first_frame(&frames, "the first piece", |(scene, _)| {
        occurrences(&scene.glyph_ids, &card) > 0
    });
    let third = first_frame(&frames, "the third piece", |(scene, _)| {
        occurrences(&scene.glyph_ids, &piece_ids(2)) > 0
    });
    let stagger = frame_ms(third, FRAME_NS) - frame_ms(first, FRAME_NS);
    assert!(
        (stagger - PIECE_DELAYS_MS[2]).abs() <= cadence_ms(FRAME_NS),
        "the third piece must wait its own {}ms stagger, waited {stagger:.1}ms",
        PIECE_DELAYS_MS[2]
    );

    assert!(done.get());
    lands_on(
        "the released cue",
        fired_at(&frames, FRAME_NS),
        end,
        FRAME_NS,
    );
}

#[test]
fn a_band_that_settles_instantly_still_gets_its_minimum_dense_beat() {
    let _g = serial();
    let _ambient = setup();

    let end = MIN_DENSE_MS + SEQUENCE_END_MS;
    let (frames, done) = mount_loading_at(FRAME_NS, false, frame_at(end + 200.0), Some(0));

    assert!(
        frames[frame_at(500.0)].0.glyph_runs > DENSE_RUNS,
        "a band that resolved from cache still rains through the floor"
    );
    assert!(
        frames[frame_at(1_900.0)].0.glyph_runs < frames[frame_at(700.0)].0.glyph_runs,
        "and thins only once the floor has released it"
    );
    assert!(done.get());
    lands_on(
        "the floored cue",
        fired_at(&frames, FRAME_NS),
        end,
        FRAME_NS,
    );
}

#[test]
fn reduced_motion_ends_the_sequence_on_its_first_frame() {
    let _g = serial();
    let _ambient = setup();

    let (frames, done) = mount_loading_at(FRAME_NS, true, 4, None);
    assert!(done.get(), "the cue fires immediately under reduced motion");
    for (index, (scene, needs_frame)) in frames.iter().enumerate() {
        assert_eq!(scene.glyph_runs, 0, "frame {index} painted rain");
        assert_eq!(scene.fills, 0, "frame {index} painted the band tone");
        assert!(!needs_frame, "frame {index} kept the shell spinning");
    }
}

#[test]
fn the_release_lands_on_the_same_wall_clock_at_120hz() {
    let _g = serial();
    let _ambient = setup();

    let (sixty, sixty_end) = observed_release(FRAME_NS);
    let (one_twenty, one_twenty_end) = observed_release(FRAME_NS_120);
    lands_on("the cue at 60Hz", sixty, sixty_end, FRAME_NS);
    lands_on("the cue at 120Hz", one_twenty, one_twenty_end, FRAME_NS_120);

    // Twice the frames, same wall clock — each beat is quantized to its own
    // cadence, so two runs can differ by at most the coarser frame.
    assert!(
        (one_twenty - sixty).abs() <= cadence_ms(FRAME_NS),
        "the release moved with the cadence: {sixty:.1}ms at 60Hz vs {one_twenty:.1}ms at 120Hz"
    );
}

// ---------------------------------------------------------------------------
// Part B — the real /sessions band under the overlay
// ---------------------------------------------------------------------------

/// The seeded session names the mock repository draws (`tests/sessions.rs`).
const SEEDED: [&str; 3] = ["dev", "jumping-peach", "herdr"];
/// Runs of each [`SEEDED`] name **one** rendering of the band paints: the third
/// rig is named after the backend it runs, so its card prints that string twice
/// — as the card's own title, and again in the backend badge beside it.
const BAND_COPIES: [usize; 3] = [1, 1, 2];
const PUMP_DEADLINE: Duration = Duration::from_secs(5);

type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

/// The glyph ids a card's name paints — `session_card` inks the theme's
/// `label_large` slot, and inking replaces only the color.
fn name_ids(name: &str) -> Vec<u32> {
    glyph_ids(name, &frust_glyph::baseline().type_scale.label_large)
}

/// A mock connect target, so the route guard passes on a card tap.
fn mock_target() -> ConnectTarget {
    ConnectTarget {
        host: "mock.example".to_string(),
        port: 7777,
        trust_mode: TrustMode::NoVerification,
        label: "Mock rig".to_string(),
        offer_credential: false,
        token: zeroize::Zeroizing::new(String::new()),
        remember: false,
        fingerprint: None,
    }
}

/// The `/sessions` screen mounted the way `tests/sessions.rs`' harness mounts
/// it — bottom layer of a `Stack`, an otherwise-empty navigator over it
/// carrying the real router — but driven off an advancing frame clock.
struct Band {
    nav: NavigatorController<AppState>,
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    /// The next advancing frame's index; also this band's own clock.
    frame: usize,
}

impl Band {
    /// Mount with the loading rain on (`support::setup` opts every other suite
    /// out of it) and the mock connection dialled.
    fn mount() -> Band {
        set_loading_rain_for_tests(true);
        let state = MuxrApp.init();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        let _router = install_app_router(&nav, &nav, expect_connection_controller());
        let logic_nav = nav.clone();
        let logic: Logic = Box::new(move |_: &mut AppState| {
            let screen_nav = logic_nav.clone();
            let overlay_nav = logic_nav.clone();
            any(Stack(vec![
                sessions_page(screen_nav, logic_nav.clone()),
                any(navigator(&overlay_nav, || {
                    any(SizedBox::<AppState>(
                        Some(f64::INFINITY),
                        Some(f64::INFINITY),
                    ))
                })),
            ]))
        });
        expect_connection_controller().connect(mock_target());
        Band {
            nav,
            root: RenderRoot::new(),
            logic,
            state,
            tcx: TextContext::new(),
            frame: 0,
        }
    }

    fn paint(&mut self, at: FrameTime) -> Recorder {
        self.root.rebuild(&mut self.logic, &mut self.state);
        let text_any: &mut dyn Any = &mut self.tcx;
        self.root.layout_with_text(surface(), text_any);
        let mut scene = Recorder::default();
        self.root.paint(&mut scene, at);
        scene
    }

    /// One paint at the pinned zero clock — the overlay's clock cannot move.
    fn pinned(&mut self) -> Recorder {
        self.paint(FrameTime::ZERO)
    }

    fn step(&mut self) -> Recorder {
        let scene = self.paint(FrameTime::from_nanos(self.frame as u64 * FRAME_NS));
        self.frame += 1;
        scene
    }

    /// Advance to `ms` on this band's clock, returning that frame's recording.
    fn drive_to(&mut self, ms: f64) -> Recorder {
        let until = frame_at(ms);
        let mut scene = self.step();
        while self.frame <= until {
            scene = self.step();
        }
        scene
    }

    /// Settle the mock repository's first load against the pinned clock (see
    /// the [module docs](self)), leaving this band's own clock at zero.
    fn settle_first_load(&mut self) {
        let deadline = Instant::now() + PUMP_DEADLINE;
        while !self.showing_seeded_names() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the seeded sessions; on screen: {:?}",
                node_values(&self.root)
            );
            pump_frames(
                &mut self.root,
                &mut self.logic,
                &mut self.state,
                &mut self.tcx,
                1,
            );
        }
    }

    /// Whether the live band is rendering every seeded name. The overlay
    /// forwards no semantics, so this reads the band underneath alone.
    fn showing_seeded_names(&self) -> bool {
        let values = node_values(&self.root);
        SEEDED.iter().all(|name| values.iter().any(|v| v == name))
    }

    fn center_of(&self, value: &str) -> Point {
        node_centers_by_value(&self.root, value)
            .first()
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "nothing printing {value:?}; on screen: {:?}",
                    node_values(&self.root)
                )
            })
    }

    fn tap(&mut self, at: Point) {
        tap(&mut self.root, &mut self.state, at);
    }

    /// The navigator's page-stack depth (1 = only the fixture's base page).
    fn depth(&self) -> usize {
        self.nav.depth()
    }
}

#[test]
fn the_band_opens_under_the_rain_and_swallows_a_tap_meant_for_a_card() {
    let _g = serial();
    let _ambient = setup();
    let mut band = Band::mount();

    let opening = band.pinned();
    assert!(
        opening.glyph_runs > DENSE_RUNS,
        "the band opens under a dense rain: {} runs",
        opening.glyph_runs
    );
    for name in SEEDED {
        assert_eq!(
            occurrences(&opening.glyph_ids, &name_ids(name)),
            0,
            "{name} cannot be painted before the first load has landed"
        );
    }

    band.settle_first_load();
    let covered = band.pinned();
    assert!(
        covered.glyph_runs > DENSE_RUNS,
        "the rain still covers a band whose cards are now live: {} runs",
        covered.glyph_runs
    );

    let card = band.center_of(SEEDED[0]);
    let before = band.depth();
    band.tap(card);
    band.pinned();
    assert_eq!(
        band.depth(),
        before,
        "a tap must not reach a card nobody can see"
    );
}

#[test]
fn the_overlay_hands_the_band_over_to_live_cards_when_its_sequence_ends() {
    let _g = serial();
    let _ambient = setup();
    let mut band = Band::mount();
    band.settle_first_load();

    let handed_over = band.drive_to(MIN_DENSE_MS + SEQUENCE_END_MS + 200.0);
    assert!(
        handed_over.glyph_runs < DENSE_RUNS,
        "the rain is gone, leaving the band's own text: {} runs",
        handed_over.glyph_runs
    );
    for (name, copies) in SEEDED.iter().zip(BAND_COPIES) {
        assert_eq!(
            occurrences(&handed_over.glyph_ids, &name_ids(name)),
            copies,
            "exactly one band prints {name} once the overlay has unmounted"
        );
    }

    let before = band.depth();
    let card = band.center_of(SEEDED[0]);
    band.tap(card);
    band.step();
    band.step();
    assert_eq!(
        band.depth(),
        before + 1,
        "the handed-over cards are live: a tap pushes the terminal route"
    );
}

#[test]
fn mid_assembly_the_landing_copy_paints_over_the_live_one() {
    let _g = serial();
    let _ambient = setup();
    let mut band = Band::mount();
    band.settle_first_load();

    // Past the last stagger slot and short of the unmount: the overlay's copy
    // has landed over the live band's own, which never went anywhere.
    let landed =
        MIN_DENSE_MS + ASSEMBLY_OFFSET_MS + PIECE_DELAYS_MS[PIECE_DELAYS_MS.len() - 1] + 50.0;
    let scene = band.drive_to(landed);
    for (name, copies) in SEEDED.iter().zip(BAND_COPIES) {
        assert_eq!(
            occurrences(&scene.glyph_ids, &name_ids(name)),
            2 * copies,
            "both bands print {name} mid-assembly: the landing copy over the live one"
        );
    }
}
