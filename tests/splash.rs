//! Headless smoke tests for the three paint-driven splash widgets, driven through `RenderRoot`
//! with a real frame clock. Each test drives `RenderRoot::paint` itself because every
//! animation is a function of the frame clock.

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
const FRAME_NS_120: u64 = 8_333_333;
const RAIN_COL_W: f64 = 14.0;
const RAIN_ROW_H: f64 = 14.0;
const RAIN_ADVANCE_MIN: f64 = 0.35;
const RAIN_ADVANCE_MAX: f64 = 0.75;
const REFERENCE_FRAMES_PER_SECOND: f64 = 60.0;

#[derive(Default)]
struct SplashScene {
    glyph_runs: usize,
    glyph_x: Vec<f64>,
    glyph_y: Vec<f64>,
    glyph_alpha: Vec<f32>,
    images: usize,
    rounded: usize,
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

fn theme(reduce_motion: bool) -> Theme {
    let mut theme = frust_glyph::baseline();
    theme.motion.reduce_motion = reduce_motion;
    theme
}

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

fn run_frames<V: View<()>>(
    logic: impl FnMut(&mut ()) -> V,
    theme: Theme,
    frames: usize,
) -> Vec<(SplashScene, bool)> {
    run_frames_at(FRAME_NS, logic, theme, frames)
}

fn frame_at_cadence(ms: f64, cadence_ns: u64) -> usize {
    (ms / (cadence_ns as f64 / 1_000_000.0)).round() as usize + 1
}

fn frame_at(ms: f64) -> usize {
    frame_at_cadence(ms, FRAME_NS)
}

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

    assert_eq!(frames[1].0.images, 1, "one bitmap draw per painted frame");
    assert_eq!(frames[1].0.rounded, 0, "the vector fallback must not paint");

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
    let frames = run_frames(
        move |_: &mut ()| icon_phase::<()>(art.clone()),
        theme(false),
        frame_at(1_500.0),
    );

    let (last, needs_frame) = frames.last().expect("frames were painted");
    assert!(!needs_frame, "a finished icon must stop requesting frames");
    assert_eq!(last.images, 0, "a dissolved icon paints nothing");
    assert!(last.layer_alphas.is_empty());

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

#[test]
fn the_burst_throws_every_particle_then_self_finishes() {
    let _g = serial();
    let _ambient = setup();

    let frames = run_frames(
        |_: &mut ()| burst::<()>(2026),
        theme(false),
        frame_at(700.0),
    );

    let opening = &frames[0].0;
    assert_eq!(opening.glyph_runs, 14, "every particle paints on frame one");
    assert!(
        opening.glyph_alpha.iter().all(|a| (*a - 1.0).abs() < 1e-6),
        "particles start fully opaque: {:?}",
        opening.glyph_alpha
    );

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

#[test]
fn the_rain_starts_dense_on_the_column_grid() {
    let _g = serial();
    let _ambient = setup();

    let frames = run_frames(|_: &mut ()| rain::<()>(7), theme(false), 4);
    let opening = &frames[1].0;

    assert!(
        opening.glyph_runs > 24,
        "the rain starts mid-fall, not empty: {} glyphs",
        opening.glyph_runs
    );
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

    let settled = run_frames(|_: &mut ()| rain::<()>(7).clock(3_400.0), theme(false), 3);
    assert!(settled.iter().all(|(scene, _)| scene.glyph_runs == 0));
    assert!(settled.iter().all(|(_, needs_frame)| !needs_frame));

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

const FALL_WINDOW_MS: f64 = 250.0;
const FALL_SEED: u64 = 7;

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

fn column_falls(cadence_ns: u64) -> (HashMap<usize, f64>, usize) {
    let count = frame_at_cadence(FALL_WINDOW_MS, cadence_ns);
    let frames = run_frames_at(
        cadence_ns,
        |_: &mut ()| rain::<()>(FALL_SEED),
        theme(false),
        count,
    );
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

    let floor = RAIN_ADVANCE_MIN * REFERENCE_FRAMES_PER_SECOND;
    let ceiling = RAIN_ADVANCE_MAX * REFERENCE_FRAMES_PER_SECOND;
    for (rate, speed) in [("60Hz", at_60), ("120Hz", at_120)] {
        assert!(
            (floor..=ceiling).contains(&speed),
            "{rate} fell {speed:.1} rows/s, outside the source's {floor:.0}..{ceiling:.0} band"
        );
    }
    let drift = (at_120 - at_60).abs() / at_60;
    assert!(
        drift < 0.2,
        "cadence changed the fall speed: {at_60:.2} rows/s at 60Hz vs \
         {at_120:.2} rows/s at 120Hz ({:.1}% apart across {} columns)",
        drift * 100.0,
        shared.len()
    );
}
