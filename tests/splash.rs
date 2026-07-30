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

use std::any::Any;

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
/// One 60Hz frame, in nanoseconds.
const FRAME_NS: u64 = 16_666_667;

/// A recording paint target capturing everything the splash widgets emit.
#[derive(Default)]
struct SplashScene {
    /// One per drawn glyph (the rain draws one run per cell, the burst one
    /// per particle).
    glyph_runs: usize,
    /// Absolute x of each drawn glyph, for grid assertions.
    glyph_x: Vec<f64>,
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
/// `frames` 60Hz frames, returning each frame's recorded scene plus whether
/// that frame asked for another one.
fn run_frames<V: View<()>>(
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
            let outcome = root.paint(&mut scene, FrameTime::from_nanos(frame as u64 * FRAME_NS));
            (scene, outcome.needs_frame)
        })
        .collect()
}

/// The frame index nearest `ms` of animation. Frame 0 seeds the clock (zero
/// delta), so `ms` of motion has elapsed by frame `ms/16.67 + 1`.
fn frame_at(ms: f64) -> usize {
    (ms / (FRAME_NS as f64 / 1_000_000.0)).round() as usize + 1
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
