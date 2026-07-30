//! Headless smoke tests for the splash sequence's three paint-driven widgets
//! (`muxr::ui::splash`), driven through `RenderRoot` with a real frame clock —
//! no GPU, no window, no orchestrator. The pure math (phase machines, rain
//! density/alpha, particle kinematics) is unit-tested next to the code; this
//! file covers what only a real paint pass can show: what actually lands in
//! the scene, and whether each widget keeps asking for frames.
//!
//! Unlike `tests/routes.rs` these tests can't use `support::frame`, which
//! pins `FrameTime::ZERO` — every animation here is a function of the frame
//! clock, so they drive `RenderRoot::paint` themselves (the same shape
//! `examples/huddle`'s toast-entrance test uses) and record through their own
//! [`SplashScene`]. They still share the harness's `serial`/`setup` guards.
//!
//! # Frame cadence
//!
//! [`run_frames_at`]/[`frame_at_cadence`] take the cadence in nanoseconds and
//! the [`run_frames`]/[`frame_at`] wrappers pin [`FRAME_NS`] (60Hz). The rain
//! is the one widget with a genuinely *per-frame* quantity (the source
//! specifies its fall in rows per `requestAnimationFrame`), so it is the one
//! that must be exercised at [`FRAME_NS_120`] as well — see
//! [`the_rain_falls_the_same_distance_per_second_at_60hz_and_120hz`].

use std::any::Any;
use std::collections::HashMap;

use frust::authoring::View;
use frust::{Get, RwSignal, Theme};
use frust_core::{FrameTime, PaintScene, RenderRoot};
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{Point, Rect, Size};
use peniko::Color;

use muxr::ui::splash::burst::burst;
use muxr::ui::splash::icon_phase::{IconArt, IconPhase, icon_phase, splash_icon_art};
use muxr::ui::splash::rain::rain;

mod support;
use support::{serial, setup};

/// The design reference's phone screen (`.screen{height:650px}` inside a
/// 340px-wide frame), so column counts match the values the unit tests use.
const SURFACE: Size = Size::new(340.0, 650.0);
/// One 60Hz frame, in nanoseconds — the cadence every helper here defaults to.
const FRAME_NS: u64 = 16_666_667;
/// One 120Hz frame, in nanoseconds.
const FRAME_NS_120: u64 = 8_333_333;
/// The rain's column pitch, in logical px (`spec::RAIN_COL_W`, `pub(crate)`).
const RAIN_COL_W: f64 = 14.0;
/// The rain's row pitch, in logical px (`spec::RAIN_ROW_H`, likewise).
const RAIN_ROW_H: f64 = 14.0;
/// The rain's per-reference-frame fall band, in rows
/// (`spec::RAIN_ADVANCE_MIN`/`spec::RAIN_ADVANCE_MAX`).
const RAIN_ADVANCE_MIN: f64 = 0.35;
/// The top of that band (see [`RAIN_ADVANCE_MIN`]).
const RAIN_ADVANCE_MAX: f64 = 0.75;
/// Reference frames a second — the `1000 / spec::REFERENCE_FRAME_MS` the
/// per-frame band above is authored against.
const REFERENCE_FRAMES_PER_SECOND: f64 = 60.0;

/// A recording paint target capturing everything the splash widgets emit.
#[derive(Default)]
struct SplashScene {
    /// One per drawn glyph (the rain draws one run per cell, the burst one
    /// per particle).
    glyph_runs: usize,
    /// Absolute x of each drawn glyph, for grid assertions.
    glyph_x: Vec<f64>,
    /// Absolute y of each drawn glyph (parallel to [`Self::glyph_x`]), for
    /// fall-distance assertions.
    glyph_y: Vec<f64>,
    /// Alpha of each glyph's brush, for falloff assertions.
    glyph_alpha: Vec<f32>,
    /// Decoded-bitmap draws (the icon).
    images: usize,
    /// Rounded-rect fills (the icon's vector fallback tile).
    rounded: usize,
    /// Alpha of every pushed opacity layer (the icon's fade).
    layer_alphas: Vec<f32>,
}

impl PaintScene for SplashScene {
    fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
    fn fill_rounded_rect(&mut self, _origin: Point, _size: Size, _radius: f64, _color: Color) {
        self.rounded += 1;
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
    fn draw_glyph_run(&mut self, run: GlyphRun) {
        self.glyph_runs += 1;
        self.glyph_x.push(run.transform.translation().x);
        self.glyph_y.push(run.transform.translation().y);
        if let peniko::Brush::Solid(color) = run.brush {
            self.glyph_alpha.push(color.components[3]);
        }
    }
    fn draw_image(&mut self, _data: &peniko::ImageData, _dest: Rect) {
        self.images += 1;
    }
    fn push_layer(&mut self, _origin: Point, _size: Size, alpha: f32) {
        self.layer_alphas.push(alpha);
    }
}

/// A Glyph theme with `reduce_motion` forced on/off — the token every splash
/// widget gates its motion on.
fn theme(reduce_motion: bool) -> Theme {
    let mut theme = Theme::glyph_baseline();
    theme.motion.reduce_motion = reduce_motion;
    theme
}

/// Mount `logic`'s view standalone, lay it out at [`SURFACE`], and paint
/// `frames` frames at `cadence_ns`, returning each frame's recorded scene plus
/// whether that frame asked for another one.
fn run_frames_at<V: View<()>>(
    cadence_ns: u64,
    mut logic: impl FnMut(&mut ()) -> V,
    theme: Theme,
    frames: usize,
) -> Vec<(SplashScene, bool)> {
    let mut root: RenderRoot<(), V> = RenderRoot::new();
    let mut state = ();
    let mut text_ctx = TextContext::new();

    root.set_theme(Box::new(theme));
    root.rebuild(&mut logic, &mut state);
    let text_any: &mut dyn Any = &mut text_ctx;
    root.layout_with_text(SURFACE, text_any);

    (0..frames)
        .map(|frame| {
            let mut scene = SplashScene::default();
            let outcome = root.paint(&mut scene, FrameTime::from_nanos(frame as u64 * cadence_ns));
            (scene, outcome.needs_frame)
        })
        .collect()
}

/// [`run_frames_at`] at 60Hz.
fn run_frames<V: View<()>>(
    logic: impl FnMut(&mut ()) -> V,
    theme: Theme,
    frames: usize,
) -> Vec<(SplashScene, bool)> {
    run_frames_at(FRAME_NS, logic, theme, frames)
}

/// The frame index nearest `ms` of animation at `cadence_ns`. Frame 0 seeds
/// the clock (zero delta), so `ms` of motion has elapsed by frame
/// `ms/cadence + 1`.
fn frame_at_cadence(ms: f64, cadence_ns: u64) -> usize {
    (ms / (cadence_ns as f64 / 1_000_000.0)).round() as usize + 1
}

/// [`frame_at_cadence`] at 60Hz.
fn frame_at(ms: f64) -> usize {
    frame_at_cadence(ms, FRAME_NS)
}

// ---------------------------------------------------------------------------
// Icon phase
// ---------------------------------------------------------------------------

#[test]
fn the_icon_draws_the_decoded_bitmap_and_fades_it_in() {
    let _g = serial();
    let _ambient = setup();

    let art = splash_icon_art();
    assert!(
        matches!(art, IconArt::Image(_)),
        "the embedded PNG must decode — the vector fallback is a failure path"
    );

    let frames = run_frames(
        move |_: &mut ()| icon_phase::<()>(art.clone()),
        theme(false),
        frame_at(400.0),
    );

    // The bitmap goes through `frust`'s own `Image` widget, so the icon's
    // scale/fade wrapper composes over a real `draw_image`.
    assert_eq!(frames[1].0.images, 1, "one bitmap draw per painted frame");
    assert_eq!(frames[1].0.rounded, 0, "the vector fallback must not paint");

    // Spring-in: the fade layer's alpha climbs (never dips — the spring's
    // overshoot is a scale effect; opacity is clamped) and settles opaque.
    // A frame that pushed no layer painted nothing at all — alpha 0.
    let alphas: Vec<f32> = frames
        .iter()
        .map(|(scene, _)| scene.layer_alphas.first().copied().unwrap_or(0.0))
        .collect();
    assert!(alphas[0] < 0.01, "the first frame is invisible");
    for pair in alphas.windows(2) {
        assert!(
            pair[1] >= pair[0] - 1e-6,
            "the fade must not go backwards: {pair:?}"
        );
    }
    let early = alphas[frame_at(60.0)];
    assert!(
        early > 0.0 && early < 1.0,
        "mid-spring alpha is partial: {early}"
    );
    let settled = alphas[frame_at(400.0) - 1];
    assert!((settled - 1.0).abs() < 1e-6, "the hold beat is opaque");

    assert!(
        frames.iter().all(|(_, needs_frame)| *needs_frame),
        "the icon keeps requesting frames for its whole beat"
    );
}

#[test]
fn the_icon_stops_painting_and_stops_asking_for_frames_once_done() {
    let _g = serial();
    let _ambient = setup();

    let art = splash_icon_art();
    // 1000ms delay + 380ms dissolve, plus a margin.
    let frames = run_frames(
        move |_: &mut ()| icon_phase::<()>(art.clone()),
        theme(false),
        frame_at(1_500.0),
    );

    let (last, needs_frame) = frames.last().expect("frames were painted");
    assert!(!needs_frame, "a finished icon must stop requesting frames");
    assert_eq!(last.images, 0, "a dissolved icon paints nothing");
    assert!(last.layer_alphas.is_empty());

    // Mid-dissolve it is still visible, but fading.
    let dissolving = &frames[frame_at(1_100.0)].0;
    assert_eq!(dissolving.images, 1);
    let alpha = dissolving.layer_alphas[0];
    assert!(
        alpha > 0.0 && alpha < 1.0,
        "mid-dissolve alpha should be partial, got {alpha}"
    );
}

#[test]
fn the_icon_publishes_each_phase_change_once() {
    let _g = serial();
    let _ambient = setup();

    let phase: RwSignal<IconPhase> = RwSignal::new(IconPhase::SpringIn);
    let art = splash_icon_art();
    let mut root: RenderRoot<(), _> = RenderRoot::new();
    let mut state = ();
    let mut text_ctx = TextContext::new();
    let mut logic = move |_: &mut ()| icon_phase::<()>(art.clone()).on_phase(phase);

    root.set_theme(Box::new(theme(false)));
    root.rebuild(&mut logic, &mut state);
    let text_any: &mut dyn Any = &mut text_ctx;
    root.layout_with_text(SURFACE, text_any);

    let mut seen = Vec::new();
    for frame in 0..frame_at(1_500.0) {
        let mut scene = SplashScene::default();
        root.paint(&mut scene, FrameTime::from_nanos(frame as u64 * FRAME_NS));
        let current = phase.get();
        if seen.last() != Some(&current) {
            seen.push(current);
        }
    }

    assert_eq!(
        seen,
        vec![
            IconPhase::SpringIn,
            IconPhase::Hold,
            IconPhase::Dissolve,
            IconPhase::Done
        ],
        "the orchestrator's cue must arrive once per phase, in order"
    );
}

#[test]
fn the_vector_fallback_paints_a_rounded_tile_with_its_brand_glyph() {
    let _g = serial();
    let _ambient = setup();

    let frames = run_frames(
        |_: &mut ()| icon_phase::<()>(IconArt::Vector),
        theme(false),
        frame_at(200.0),
    );
    let scene = &frames[frame_at(200.0) - 1].0;
    assert_eq!(scene.images, 0, "the fallback draws no bitmap");
    assert_eq!(scene.rounded, 1, "one rounded amber tile");
    assert_eq!(scene.glyph_runs, 1, "with the brand glyph centered on it");
}

#[test]
fn the_icon_is_inert_under_reduced_motion() {
    let _g = serial();
    let _ambient = setup();

    let art = splash_icon_art();
    let frames = run_frames(
        move |_: &mut ()| icon_phase::<()>(art.clone()),
        theme(true),
        4,
    );
    for (scene, needs_frame) in &frames {
        assert_eq!(scene.images, 0, "reduced motion paints no icon");
        assert!(scene.layer_alphas.is_empty());
        assert!(!needs_frame, "reduced motion requests no frames");
    }
}

// ---------------------------------------------------------------------------
// Burst
// ---------------------------------------------------------------------------

#[test]
fn the_burst_throws_every_particle_then_self_finishes() {
    let _g = serial();
    let _ambient = setup();

    let frames = run_frames(
        |_: &mut ()| burst::<()>(2026),
        theme(false),
        frame_at(700.0),
    );

    // Frame 0 seeds the clock (zero delta), so it is the burst's t=0.
    let opening = &frames[0].0;
    assert_eq!(opening.glyph_runs, 14, "every particle paints on frame one");
    assert!(
        opening.glyph_alpha.iter().all(|a| (*a - 1.0).abs() < 1e-6),
        "particles start fully opaque: {:?}",
        opening.glyph_alpha
    );

    // Mid-flight they are still drawn, but fading.
    let mid = &frames[frame_at(300.0)].0;
    assert_eq!(mid.glyph_runs, 14);
    assert!(
        mid.glyph_alpha.iter().all(|a| *a > 0.0 && *a < 1.0),
        "particles fade as they fly: {:?}",
        mid.glyph_alpha
    );

    let (last, needs_frame) = frames.last().expect("frames were painted");
    assert_eq!(last.glyph_runs, 0, "a finished burst paints nothing");
    assert!(!needs_frame, "a finished burst stops requesting frames");
}

#[test]
fn the_burst_is_inert_under_reduced_motion() {
    let _g = serial();
    let _ambient = setup();

    for (scene, needs_frame) in run_frames(|_: &mut ()| burst::<()>(1), theme(true), 4) {
        assert_eq!(scene.glyph_runs, 0);
        assert!(!needs_frame);
    }
}

// ---------------------------------------------------------------------------
// Rain
// ---------------------------------------------------------------------------

#[test]
fn the_rain_starts_dense_on_the_column_grid() {
    let _g = serial();
    let _ambient = setup();

    let frames = run_frames(|_: &mut ()| rain::<()>(7), theme(false), 4);
    let opening = &frames[1].0;

    // 340 / 14 = 24 columns, each drawing up to a 7-glyph trail — every
    // column starts mid-fall, so the very first frame is already busy.
    assert!(
        opening.glyph_runs > 24,
        "the rain starts mid-fall, not empty: {} glyphs",
        opening.glyph_runs
    );
    // 340 / 14 = 24 columns at indices 0..=23.
    for x in &opening.glyph_x {
        let column = x / 14.0;
        assert!(
            (column - column.round()).abs() < 1e-9,
            "every glyph sits on the 14px column grid, got x={x}"
        );
        assert!(
            (0.0..24.0).contains(&column),
            "column index {column} is off-surface"
        );
    }
    assert!(frames[1].1, "a live rain keeps requesting frames");
}

#[test]
fn the_rain_thins_out_and_then_stops_painting_entirely() {
    let _g = serial();
    let _ambient = setup();

    // Past THIN_END (2800ms) + the 400ms tail.
    let frames = run_frames(|_: &mut ()| rain::<()>(7), theme(false), frame_at(3_400.0));

    let dense = frames[frame_at(200.0)].0.glyph_runs;
    let thinning = frames[frame_at(2_000.0)].0.glyph_runs;
    let draining = frames[frame_at(2_700.0)].0.glyph_runs;
    assert!(
        dense > thinning && thinning > draining,
        "the field must drain column by column: {dense} -> {thinning} -> {draining}"
    );

    let (last, needs_frame) = frames.last().expect("frames were painted");
    assert_eq!(last.glyph_runs, 0, "a settled rain paints nothing at all");
    assert!(!needs_frame, "a settled rain stops requesting frames");
}

#[test]
fn the_rains_head_is_brighter_than_its_trail() {
    let _g = serial();
    let _ambient = setup();

    let frames = run_frames(|_: &mut ()| rain::<()>(3), theme(false), 4);
    let alphas = &frames[1].0.glyph_alpha;
    let brightest = alphas.iter().cloned().fold(0.0_f32, f32::max);
    let dimmest = alphas.iter().cloned().fold(1.0_f32, f32::min);
    assert!(
        (brightest - 0.95).abs() < 1e-6,
        "the head keeps its 0.95 alpha, got {brightest}"
    );
    assert!(
        dimmest < 0.2,
        "the trail's last glyph is nearly gone, got {dimmest}"
    );
}

#[test]
fn an_external_clock_drives_the_rain_phase() {
    let _g = serial();
    let _ambient = setup();

    // Handed a clock already past THIN_END + the tail, the rain paints
    // nothing on its very first frame — the orchestrator, not the widget,
    // owns the timeline.
    let settled = run_frames(|_: &mut ()| rain::<()>(7).clock(3_400.0), theme(false), 3);
    assert!(settled.iter().all(|(scene, _)| scene.glyph_runs == 0));
    assert!(settled.iter().all(|(_, needs_frame)| !needs_frame));

    // Handed a dense clock, it paints immediately.
    let dense = run_frames(|_: &mut ()| rain::<()>(7).clock(100.0), theme(false), 3);
    assert!(dense[0].0.glyph_runs > 0);
}

#[test]
fn the_rain_never_starts_under_reduced_motion() {
    let _g = serial();
    let _ambient = setup();

    for (scene, needs_frame) in run_frames(|_: &mut ()| rain::<()>(7), theme(true), 8) {
        assert_eq!(scene.glyph_runs, 0);
        assert!(!needs_frame);
    }
}

// ---------------------------------------------------------------------------
// Rain — frame cadence
// ---------------------------------------------------------------------------

/// The window each cadence's fall is measured over, in ms. Short enough that
/// most columns are still on the surface at the end of it (an exited column
/// respawns at the top and is excluded from the measurement) — including in
/// the failure case this test exists to catch, where the 120Hz field falls at
/// twice the speed and drains twice as fast.
const FALL_WINDOW_MS: f64 = 250.0;
/// The seed fixing the measured field.
const FALL_SEED: u64 = 7;

/// Every column's head position in a recorded frame, in rows, keyed by column
/// index.
///
/// A column paints its trail *upward* from its head, so the head is the
/// lowest glyph the column drew. A head that has fallen past the surface has
/// its own glyph skipped (leaving a trail glyph as the lowest one), so a
/// column only counts here when its lowest glyph is a full row clear of the
/// bottom edge — the condition under which that glyph must be the head
/// itself.
fn head_rows(scene: &SplashScene) -> HashMap<usize, f64> {
    let mut heads: HashMap<usize, f64> = HashMap::new();
    for (x, y) in scene.glyph_x.iter().zip(&scene.glyph_y) {
        let column = (x / RAIN_COL_W).round() as usize;
        let head = heads.entry(column).or_insert(f64::MIN);
        *head = head.max(*y);
    }
    heads.retain(|_, y| *y <= SURFACE.height - RAIN_ROW_H);
    heads
        .into_iter()
        .map(|(c, y)| (c, y / RAIN_ROW_H))
        .collect()
}

/// How far each column's head fell over [`FALL_WINDOW_MS`], in rows, on a run
/// driven at `cadence_ns` — plus how many frames that run took.
///
/// A column that exited (and so respawned at the top) shows a *negative*
/// displacement and is dropped, as is one whose head left the measurable band
/// at either end.
fn column_falls(cadence_ns: u64) -> (HashMap<usize, f64>, usize) {
    let count = frame_at_cadence(FALL_WINDOW_MS, cadence_ns);
    let frames = run_frames_at(
        cadence_ns,
        |_: &mut ()| rain::<()>(FALL_SEED),
        theme(false),
        count,
    );
    // Frame 0 seeds the clock with a zero delta, so it records the field's
    // starting positions — identical across cadences, same seed, same layout.
    let start = head_rows(&frames[0].0);
    let end = head_rows(&frames[count - 1].0);
    let falls = start
        .iter()
        .filter_map(|(column, from)| {
            let to = end.get(column)?;
            (to > from).then_some((*column, to - from))
        })
        .collect();
    (falls, count)
}

#[test]
fn the_rain_falls_the_same_distance_per_second_at_60hz_and_120hz() {
    let _g = serial();
    let _ambient = setup();

    let (sixty, sixty_frames) = column_falls(FRAME_NS);
    let (one_twenty, one_twenty_frames) = column_falls(FRAME_NS_120);
    assert_eq!(
        one_twenty_frames,
        2 * sixty_frames - 1,
        "the 120Hz run must cover the same window in twice the frames"
    );

    // The same columns in both runs: the field starts identically (same seed),
    // and `frame_factor` is the reason the same ones are still on-surface at
    // the end of the window.
    let shared: Vec<usize> = sixty
        .keys()
        .filter(|column| one_twenty.contains_key(column))
        .copied()
        .collect();
    assert!(
        shared.len() >= 8,
        "too few columns stayed measurable to compare cadences: {}",
        shared.len()
    );

    let rows_per_second = |falls: &HashMap<usize, f64>| -> f64 {
        let total: f64 = shared.iter().map(|column| falls[column]).sum();
        total / shared.len() as f64 / (FALL_WINDOW_MS / 1_000.0)
    };
    let at_60 = rows_per_second(&sixty);
    let at_120 = rows_per_second(&one_twenty);

    // The source specifies the fall per *reference frame* (`rand(0.35,0.75)`
    // rows per `requestAnimationFrame` at 60Hz), so a second of falling is 60
    // of those whatever the real cadence — which is exactly what
    // `spec::REFERENCE_FRAME_MS` + `frame_factor` convert between. Without
    // that normalization the 120Hz run would fall at twice this speed.
    let floor = RAIN_ADVANCE_MIN * REFERENCE_FRAMES_PER_SECOND;
    let ceiling = RAIN_ADVANCE_MAX * REFERENCE_FRAMES_PER_SECOND;
    for (rate, speed) in [("60Hz", at_60), ("120Hz", at_120)] {
        assert!(
            (floor..=ceiling).contains(&speed),
            "{rate} fell {speed:.1} rows/s, outside the source's {floor:.0}..{ceiling:.0} band"
        );
    }
    // Twice the frames draw twice as many steps from the seeded LCG, so the
    // two fields are independent samples of the same per-frame distribution
    // rather than the same field twice — this compares distributions (the
    // spread across these seeds/columns is ~5%), and the regression it exists
    // to catch is a factor of two.
    let drift = (at_120 - at_60).abs() / at_60;
    assert!(
        drift < 0.2,
        "cadence changed the fall speed: {at_60:.2} rows/s at 60Hz vs \
         {at_120:.2} rows/s at 120Hz ({:.1}% apart across {} columns)",
        drift * 100.0,
        shared.len()
    );
}
