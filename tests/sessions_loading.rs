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
use frust::authoring::{Point, any};
use frust::{
    AnyView, Column, Component, Get, NavigatorController, RwSignal, SizedBox, Stack, Theme,
    navigator, spawn_local, text,
};
use frust_core::{FrameTime, InputEvent, PaintScene, PointerPhase, RenderRoot, ScrollDelta};
use frust_reactive::ReactiveRuntime;
use frust_scene::GlyphRun;
use kurbo::{Rect, Size};
use peniko::Color;

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::sessions::di::expect_sessions_repository;
use muxr::features::sessions::presentation::pages::{sessions_page, set_loading_rain_for_tests};
use muxr::features::settings::di::expect_preferences_repository;
use muxr::features::settings::domain::entities::Preferences;
use muxr::routes::install_app_router;
use muxr::ui::splash::loading::loading_sequence;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    char_key, node_centers_by_value, node_values, pointer, pump_frames, serial, setup, surface, tap,
};

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
/// When the standalone rig's last piece has landed and the overlay stops
/// swallowing input (`loading::assembly_landed_ms` over [`PIECES`] pieces).
const LANDED_MS: f64 =
    MIN_DENSE_MS + ASSEMBLY_OFFSET_MS + PIECE_DELAYS_MS[PIECES - 1] + PIECE_IN_MS;

/// Glyph runs a frame must beat to count as "the rain is up": the dense field
/// paints one run per glyph, where the band's own text is a few dozen.
const DENSE_RUNS: usize = 100;
const PIECES: usize = 3;
/// Fixes the standalone rain's field, so a run replays exactly.
const SEED: u64 = 0x6C6F_6164;

/// A recording paint target: the glyph-id stream, its run count, and the boxes
/// it filled and clipped to.
#[derive(Default)]
struct Recorder {
    glyph_ids: Vec<u32>,
    glyph_runs: usize,
    /// Every `fill_rect`, paint order — the overlay's tone is the only fill the
    /// standalone rig paints at all.
    fills: Vec<(Point, Size)>,
    /// Every `push_clip`, paint order.
    clips: Vec<(Point, Size)>,
}

impl PaintScene for Recorder {
    fn fill_rect(&mut self, origin: Point, size: Size, _color: Color) {
        self.fills.push((origin, size));
    }
    fn push_clip(&mut self, origin: Point, size: Size) {
        self.clips.push((origin, size));
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

/// A wheel notch at `at` — hit-tested by position, exactly like a pointer.
fn wheel(at: Point) -> InputEvent {
    InputEvent::Scroll {
        position: at,
        delta: ScrollDelta::Lines(0.0, -3.0),
    }
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

/// The `index`th piece's text — a plain leaf, so what the assembly proves is
/// the stagger rather than any one screen's widgets.
fn piece_name(index: usize) -> String {
    format!("card-{index}")
}

fn pieces_for(count: usize) -> Vec<AnyView<AppState>> {
    (0..count).map(|i| any(text(piece_name(i)))).collect()
}

type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

/// The orchestrator alone on its own root, driven a frame at a time through the
/// real shell loop — rebuild, lay out shaping real text, paint. Nothing else is
/// mounted, so an event's `handled` is the overlay's own answer.
struct Rig {
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    released: Rc<Cell<bool>>,
    done: RwSignal<bool>,
    cadence_ns: u64,
    /// The frame the `released` flag flips on; `None` never settles, leaving
    /// the hold ceiling to release it.
    release_frame: Option<usize>,
    frame: usize,
}

impl Rig {
    /// `count` is how many pieces a rebuild mounts, asked with the release flag
    /// it is rebuilding under — a band settling inside the tail mounts more of
    /// them than the one that was there when the ceiling fired.
    fn mount(
        cadence_ns: u64,
        reduce_motion: bool,
        release_frame: Option<usize>,
        count: fn(bool) -> usize,
    ) -> Rig {
        let released = Rc::new(Cell::new(false));
        let flag = Rc::clone(&released);
        let done = RwSignal::new(false);
        let logic: Logic = Box::new(move |_: &mut AppState| {
            let settled = flag.get();
            loading_sequence(
                SEED,
                tone(),
                pieces_for(count(settled)),
                |pieces| any(Column(pieces)),
                settled,
                done,
            )
        });
        let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
        root.set_theme(Box::new(theme(reduce_motion)));
        Rig {
            root,
            logic,
            state: MuxrApp.init(),
            tcx: TextContext::new(),
            released,
            done,
            cadence_ns,
            release_frame,
            frame: 0,
        }
    }

    fn step(&mut self) -> (Recorder, bool) {
        if self.release_frame.is_some_and(|at| self.frame >= at) {
            self.released.set(true);
        }
        self.root.rebuild(&mut self.logic, &mut self.state);
        let text_any: &mut dyn Any = &mut self.tcx;
        self.root.layout_with_text(SURFACE, text_any);
        let mut scene = Recorder::default();
        let at = FrameTime::from_nanos(self.frame as u64 * self.cadence_ns);
        let outcome = self.root.paint(&mut scene, at);
        self.frame += 1;
        (scene, outcome.needs_frame)
    }

    fn drive(&mut self, frames: usize) -> Vec<(Recorder, bool)> {
        (0..frames).map(|_| self.step()).collect()
    }

    /// Whether the overlay consumed `event`, dispatched between two frames.
    fn swallows(&mut self, event: &InputEvent) -> bool {
        self.root.event(&mut self.state, event).handled
    }
}

/// The glyph ids the `index`th piece paints — a plain `text` shapes at
/// `TextStyle::default()`, and the theme's recolor doesn't shape.
fn piece_ids(index: usize) -> Vec<u32> {
    glyph_ids(&piece_name(index), &TextStyle::default())
}

/// Mount the loading rain alone over [`PIECES`] pieces at `cadence_ns`, so
/// every draw recorded belongs to it, and drive `frames` frames of it.
fn mount_loading_at(
    cadence_ns: u64,
    reduce_motion: bool,
    frames: usize,
    release_frame: Option<usize>,
) -> (Vec<(Recorder, bool)>, RwSignal<bool>) {
    let mut rig = Rig::mount(cadence_ns, reduce_motion, release_frame, |_| PIECES);
    let recorded = rig.drive(frames);
    (recorded, rig.done)
}

/// The overlay's own answer to `event`, asked one frame past `ms` into a run
/// released at the floor.
fn swallowed_at(ms: f64, reduce_motion: bool, event: &InputEvent) -> bool {
    let mut rig = Rig::mount(FRAME_NS, reduce_motion, Some(0), |_| PIECES);
    rig.drive(frame_at(ms) + 1);
    rig.swallows(event)
}

/// The middle of the surface — over a piece, wherever the assembly put it.
fn mid_surface() -> Point {
    Point::new(SURFACE.width / 2.0, SURFACE.height / 2.0)
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

    let (firing, needs_frame) = &frames[0];
    assert!(
        needs_frame,
        "the firing frame must ask for the frame that acts on its write"
    );
    assert_eq!(
        firing.fills.len(),
        1,
        "the one frame it paints is the band's own tone"
    );
    for (index, (scene, needs_frame)) in frames.iter().enumerate() {
        assert_eq!(scene.glyph_runs, 0, "frame {index} painted rain");
        assert!(scene.clips.is_empty(), "frame {index} opened an assembly");
        assert!(
            *needs_frame == (index == 0),
            "frame {index} kept the shell spinning"
        );
    }
    assert!(
        !swallowed_at(0.0, true, &pointer(PointerPhase::Down, mid_surface())),
        "a fired overlay is not a barrier: the tone frame takes no taps"
    );
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

#[test]
fn the_overlay_swallows_pointer_and_scroll_only_until_its_assembly_lands() {
    let _g = serial();
    let _ambient = setup();

    let held = LANDED_MS - 200.0;
    assert!(
        swallowed_at(held, false, &pointer(PointerPhase::Down, mid_surface())),
        "a tap must not reach a card nobody can see yet"
    );
    assert!(
        swallowed_at(held, false, &wheel(mid_surface())),
        "and neither must a wheel notch meant for the list under it"
    );
    assert!(
        !swallowed_at(held, false, &char_key("a")),
        "a focus-routed key was never this overlay's to take"
    );

    let landed = LANDED_MS + 100.0;
    assert!(
        landed < MIN_DENSE_MS + SEQUENCE_END_MS,
        "the overlay must still be up, or this proves nothing"
    );
    assert!(
        !swallowed_at(landed, false, &pointer(PointerPhase::Down, mid_surface())),
        "a landed assembly is the band's own picture: a tap falls through it"
    );
    assert!(
        !swallowed_at(landed, false, &wheel(mid_surface())),
        "and so does the wheel"
    );
}

#[test]
fn the_assembly_paints_clipped_to_the_band() {
    let _g = serial();
    let _ambient = setup();

    let (frames, _) = mount_loading_at(FRAME_NS, false, frame_at(LANDED_MS) + 1, Some(0));
    assert!(
        frames[frame_at(MIN_DENSE_MS)].0.clips.is_empty(),
        "nothing is clipped while the assembly is still withheld"
    );

    let landed = &frames[frame_at(LANDED_MS)].0;
    assert_eq!(
        landed.clips, landed.fills,
        "the assembly is clipped to exactly the box the tone fills — the same \
         box the live scroller beneath clips to"
    );
    assert_eq!(landed.clips.len(), 1, "and the overlay does open one");
}

#[test]
fn a_settle_inside_the_tail_still_lands_every_piece_it_mounts() {
    let _g = serial();
    let _ambient = setup();

    // Late enough that an end measured from the ceiling would cut the pieces
    // this settle mounts off mid-drop.
    let release_frame = frame_at(MAX_HOLD_MS + 1_500.0);
    let settled = frame_ms(release_frame, FRAME_NS);
    let landed = settled + PIECE_DELAYS_MS[PIECES - 1] + PIECE_IN_MS;
    let end = settled + SEQUENCE_END_MS;
    let mut rig = Rig::mount(FRAME_NS, false, Some(release_frame), |released| {
        if released { PIECES } else { 1 }
    });
    let frames = rig.drive(frame_at(end + 200.0));
    assert!(rig.done.get(), "a settle inside the tail still ends");

    let quiet = first_frame(&frames, "the sequence settling", |(_, needs_frame)| {
        !needs_frame
    });
    let fired = frame_ms(quiet - 1, FRAME_NS);
    assert!(
        fired >= landed,
        "the cue fired at {fired:.1}ms, over a piece landing at {landed:.1}ms"
    );
    lands_on("the re-based cue", fired, end, FRAME_NS);
    for index in 0..PIECES {
        assert_eq!(
            occurrences(&frames[quiet - 1].0.glyph_ids, &piece_ids(index)),
            1,
            "every piece must be on screen on the last frame the overlay paints"
        );
    }
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
/// The band's pieces: a card per seeded session, closed by the trailing row.
const BAND_PIECES: usize = SEEDED.len() + 1;
/// When the mounted band's last card has landed (see [`LANDED_MS`]).
const BAND_LANDED_MS: f64 =
    MIN_DENSE_MS + ASSEMBLY_OFFSET_MS + PIECE_DELAYS_MS[BAND_PIECES - 1] + PIECE_IN_MS;
/// [`seed_extra_session`]'s name — distinct from every [`SEEDED`] name and
/// backend/status text, so [`occurrences`] counts it cleanly.
const FOURTH_NAME: &str = "late-arrival";

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

/// Shortens the poll to its fastest offered choice before `band` ever
/// mounts, so its first real wait is this one, not the three-second default.
fn speed_up_the_poll() {
    let preferences = expect_preferences_repository();
    let done = Rc::new(Cell::new(false));
    let flag = Rc::clone(&done);
    spawn_local(async move {
        let mut poll = Preferences::defaults().poll;
        poll.layout_refresh_secs = 1;
        preferences
            .save_poll_prefs(poll)
            .await
            .expect("1s is one of PollPrefs::REFRESH_CHOICES_SECS");
        flag.set(true);
    });
    let runtime = ReactiveRuntime::get().expect("ReactiveRuntime::init must have run");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while !done.get() {
        assert!(Instant::now() < deadline, "timed out speeding up the poll");
        runtime.pump_local();
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Adds `name` straight to the shared mock repository — never through the
/// mounted screen's own controller, so the overlay's frozen snapshot (taken
/// at settle) cannot see it yet.
fn seed_extra_session(name: &str) {
    let repository = expect_sessions_repository();
    let done = Rc::new(Cell::new(false));
    let flag = Rc::clone(&done);
    let owned = name.to_string();
    spawn_local(async move {
        repository
            .create_session(owned)
            .await
            .expect("the mock never rejects a real name");
        flag.set(true);
    });
    let runtime = ReactiveRuntime::get().expect("ReactiveRuntime::init must have run");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while !done.get() {
        assert!(Instant::now() < deadline, "timed out seeding {name:?}");
        runtime.pump_local();
        std::thread::sleep(Duration::from_millis(1));
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
        Band::mount_at(false)
    }

    /// [`Band::mount`] under a theme whose motion is reduced. The default rig
    /// installs no theme of its own, so only this one names one.
    fn mount_reduced() -> Band {
        Band::mount_at(true)
    }

    fn mount_at(reduce_motion: bool) -> Band {
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
        let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
        if reduce_motion {
            root.set_theme(Box::new(theme(true)));
        }
        Band {
            nav,
            root,
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

    /// Pumps toward real time — rebuild and layout only, never paint, so
    /// this band's own clock holds still — until the live band prints
    /// `name`: how a poll's real `sleep` is given the wall time it needs.
    fn wait_for_poll(&mut self, name: &str) {
        let runtime = ReactiveRuntime::get().expect("ReactiveRuntime::init must have run");
        let deadline = Instant::now() + PUMP_DEADLINE;
        loop {
            self.root.rebuild(&mut self.logic, &mut self.state);
            let text_any: &mut dyn Any = &mut self.tcx;
            self.root.layout_with_text(surface(), text_any);
            if node_values(&self.root).iter().any(|v| v == name) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {name:?}");
            runtime.pump_local();
            std::thread::sleep(Duration::from_millis(1));
        }
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
fn a_tap_reaches_the_live_card_once_the_assembly_has_landed() {
    let _g = serial();
    let _ambient = setup();
    let mut band = Band::mount();
    band.settle_first_load();

    let scene = band.drive_to(BAND_LANDED_MS + 100.0);
    for (name, copies) in SEEDED.iter().zip(BAND_COPIES) {
        assert_eq!(
            occurrences(&scene.glyph_ids, &name_ids(name)),
            2 * copies,
            "the overlay is still up, printing its landed copy of {name} over the live one"
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
        "a landed card is live under the overlay: a tap pushes the terminal route"
    );
}

#[test]
fn reduced_motion_leaves_the_band_live_from_its_first_frame() {
    let _g = serial();
    let _ambient = setup();
    let mut band = Band::mount_reduced();
    band.settle_first_load();

    let scene = band.step();
    assert!(
        scene.glyph_runs < DENSE_RUNS,
        "no rain plays under reduced motion: {} runs",
        scene.glyph_runs
    );

    let before = band.depth();
    let card = band.center_of(SEEDED[0]);
    band.tap(card);
    band.step();
    band.step();
    assert_eq!(
        band.depth(),
        before + 1,
        "the frame after the cue is the band's own, taps included"
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

#[test]
fn a_poll_landing_inside_the_tail_reaches_only_the_live_band() {
    let _g = serial();
    let _ambient = setup();
    let mut band = Band::mount();
    speed_up_the_poll();
    band.settle_first_load();
    band.drive_to(BAND_LANDED_MS + 100.0);

    seed_extra_session(FOURTH_NAME);
    band.wait_for_poll(FOURTH_NAME);

    let mid_tail = band.step();
    for (name, copies) in SEEDED.iter().zip(BAND_COPIES) {
        assert_eq!(
            occurrences(&mid_tail.glyph_ids, &name_ids(name)),
            2 * copies,
            "the frozen assembly still prints {name} over the live band"
        );
    }
    assert_eq!(
        occurrences(&mid_tail.glyph_ids, &name_ids(FOURTH_NAME)),
        1,
        "the fourth name is the live band's alone — the assembly never mounted it"
    );

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
    assert_eq!(
        occurrences(&handed_over.glyph_ids, &name_ids(FOURTH_NAME)),
        1,
        "the live band shows every session once the handoff completes"
    );
}
