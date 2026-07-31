//! Shared headless-test harness for muxr's integration tests: a GPU-free
//! paint target, a one-frame helper, a recording waker, an ambient-owner
//! setup, and an async pump-poll loop — lifted (same shape, adapted doc
//! header) from `examples/huddle`'s own `tests/support/mod.rs` (itself
//! lifted from `clean-signals-frust`'s own support module, the Frust
//! `examples/inbox` recipe), factored so every `tests/*.rs` file (each its
//! own crate root) can share it. Carries [`RecScene`] recording rounded-rect
//! geometry (field/button chrome) so a test can locate and tap a button,
//! plus generic [`pointer`]/[`tap`]/[`center`] pointer-event helpers and
//! [`char_key`]/[`named_key`] keyboard-event helpers, ready for Phase 2's
//! interactive screens even though today's route-smoke tests only need
//! [`frame`]/[`setup`]/[`serial`].
//!
//! # Overlay helpers (dialogs / sheets / toasts)
//!
//! [`frame`] always paints at `FrameTime::ZERO`, which is fine for static
//! screens but freezes anything driven off the frame clock — including the
//! navigator's own push/pop transition, during which it **blocks all input**.
//! [`frame_at`] paints at an explicit time and [`settle`] drives a pushed
//! overlay past its transition so a test can then tap it.
//!
//! Locating what to tap has two routes, and which one works depends on where
//! the target sits:
//!
//! - **Semantics** ([`node_labels`]/[`node_center_by_label`]/
//!   [`node_center_by_role`]) — reads labels and bounds straight off the
//!   accessibility tree, and is the only way to assert *what a toast said*
//!   (a recording `PaintScene` sees glyph runs, not strings). It does **not**
//!   see anything inside a `Navigator`: `NavigatorWidget` contributes no
//!   `Widget::semantics` and forwards none of its pages' (a framework gap —
//!   every pushed page is invisible to accessibility).
//! - **Recorded geometry** ([`largest`]/[`smallest`]/[`by_width`] over
//!   [`RecScene::rounded`]/[`RecScene::strokes`]) — what a test must use for a
//!   pushed page (dialog/sheet) until that gap closes.
//!
//! Not every test uses every helper.
#![allow(dead_code)]

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use frust_core::accesskit::{Node, Role};
use frust_core::{
    FrameTime, InputEvent, Key, KeyEvent, Modifiers, NamedKey, PaintScene, PointerButton,
    PointerEvent, PointerPhase, RenderRoot, View,
};
use frust_reactive::{FrameWaker, ReactiveRuntime};
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{Point, Rect, Shape, Size};
use peniko::{Brush, Color};
use reactive_graph::owner::Owner;

pub const W: f64 = 800.0;
pub const H: f64 = 600.0;

/// A GPU-free paint target: counts glyph runs (one per painted `Text` — enough
/// to observe whether a page's placeholder text painted) and records
/// rounded-rect chrome (each `TextInput` field and filled `Button`) so a test
/// can locate a button to tap it, plus stroked outlines
/// ([`RecScene::strokes`]) — a Glyph ghost/danger button has a **transparent**
/// fill and is invisible in `rounded`, so its border is the only geometry it
/// paints.
#[derive(Default)]
pub struct RecScene {
    pub glyph_runs: usize,
    pub rounded: Vec<(Point, Size)>,
    /// `(origin, bounding-box size)` per stroked path — see the struct docs.
    pub strokes: Vec<(Point, Size)>,
}

impl PaintScene for RecScene {
    fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
    fn fill_rounded_rect(&mut self, origin: Point, size: Size, _radius: f64, _color: Color) {
        self.rounded.push((origin, size));
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
    fn draw_glyph_run(&mut self, _run: GlyphRun) {
        self.glyph_runs += 1;
    }
    fn draw_image(&mut self, _data: &peniko::ImageData, _dest: Rect) {}
    fn stroke_path(&mut self, origin: Point, path: &kurbo::BezPath, _width: f64, _brush: &Brush) {
        let bounds = path.bounding_box();
        self.strokes
            .push((origin, Size::new(bounds.width(), bounds.height())));
    }
}

/// The recorded rect with the largest area, or `None` when nothing was
/// recorded — an overlay's own panel is always the biggest thing it paints, so
/// this is how a test finds it without hardcoding geometry.
pub fn largest(rects: &[(Point, Size)]) -> Option<(Point, Size)> {
    rects
        .iter()
        .copied()
        .max_by(|a, b| area(a).total_cmp(&area(b)))
}

/// The recorded rect with the smallest area — e.g. the one action button in a
/// dialog that paints a border, against the panel border around it.
pub fn smallest(rects: &[(Point, Size)]) -> Option<(Point, Size)> {
    rects
        .iter()
        .copied()
        .min_by(|a, b| area(a).total_cmp(&area(b)))
}

/// The first recorded rect whose width matches `width` (within half a pixel) —
/// the deterministic way to pick a full-content-width child (a dialog's text
/// field) out of a frame.
pub fn by_width(rects: &[(Point, Size)], width: f64) -> Option<(Point, Size)> {
    rects
        .iter()
        .copied()
        .find(|(_, size)| (size.width - width).abs() < 0.5)
}

fn area((_, size): &(Point, Size)) -> f64 {
    size.width * size.height
}

/// One frame: rebuild the tree from `state`, lay it out shaping real text, and
/// record the paint output.
pub fn frame<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
) -> RecScene {
    root.rebuild(logic, state);
    let tcx_any: &mut dyn Any = tcx;
    root.layout_with_text(Size::new(W, H), tcx_any);
    let mut scene = RecScene::default();
    root.paint(&mut scene, FrameTime::ZERO);
    scene
}

/// One frame painted at an explicit `frame_time` — [`frame`]'s
/// clock-advancing sibling, for anything driven off `PaintCtx::frame_time`
/// (navigator transitions, a dialog's enter staging, a toast's timeline).
///
/// Deliberately a separate function rather than a parameter on [`frame`]:
/// every existing test calls `frame` and none of them should have to change.
pub fn frame_at<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
    frame_time: FrameTime,
) -> RecScene {
    root.rebuild(logic, state);
    let tcx_any: &mut dyn Any = tcx;
    root.layout_with_text(Size::new(W, H), tcx_any);
    let mut scene = RecScene::default();
    root.paint(&mut scene, frame_time);
    scene
}

/// How far past a push/pop the [`settle`] helper runs its clock — comfortably
/// past every transition/enter duration in the framework (the longest is the
/// navigator's own ~400ms preset).
pub const SETTLE_MS: f64 = 2_000.0;

/// Drive a freshly pushed (or popped) overlay past its transition: one frame
/// seeding the clock at zero, one at [`SETTLE_MS`] (which *completes* the
/// transition during paint), and one more whose **rebuild** finalizes it.
///
/// **Required before tapping a pushed page.** While a transition is in flight
/// the navigator suppresses all routing to pages, so a tap sent too early is
/// silently swallowed — and a transition isn't done when its last frame
/// paints, but when the following rebuild finalizes it. That third frame is
/// the difference between a test that taps a live sheet and one that taps a
/// blocked navigator and passes for the wrong reason.
pub fn settle<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
) -> RecScene {
    frame_at(root, logic, state, tcx, FrameTime::ZERO);
    frame_at(root, logic, state, tcx, ms(SETTLE_MS));
    frame_at(root, logic, state, tcx, ms(SETTLE_MS))
}

/// A [`FrameTime`] `ms` milliseconds after the epoch.
pub fn ms(ms: f64) -> FrameTime {
    FrameTime::from_nanos((ms * 1_000_000.0) as u64)
}

/// Every semantics label the current tree carries, in pass order — the
/// accessible text of the frame, and the only way to assert *what* a toast or
/// dialog said (a recording `PaintScene` sees glyph runs, not strings).
pub fn node_labels<S: 'static, V: View<S>>(root: &RenderRoot<S, V>) -> Vec<String> {
    root.semantics()
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_string))
        .collect()
}

/// The center of the first semantics node labelled `label` — where a tap must
/// land to hit that button/row. `None` when nothing carries the label.
pub fn node_center_by_label<S: 'static, V: View<S>>(
    root: &RenderRoot<S, V>,
    label: &str,
) -> Option<Point> {
    node_center(root, |node| node.label() == Some(label))
}

/// The center of the first semantics node with `role` — for a node that has no
/// label of its own (a `TextInput` exposes only its value).
pub fn node_center_by_role<S: 'static, V: View<S>>(
    root: &RenderRoot<S, V>,
    role: Role,
) -> Option<Point> {
    node_center(root, |node| node.role() == role)
}

/// Shared body of the two `node_center_*` helpers: the center of the first
/// node matching `pred` that carries bounds.
fn node_center<S: 'static, V: View<S>>(
    root: &RenderRoot<S, V>,
    pred: impl Fn(&Node) -> bool,
) -> Option<Point> {
    root.semantics().nodes.iter().find_map(|(_, node)| {
        if !pred(node) {
            return None;
        }
        let bounds = node.bounds()?;
        Some(Point::new(
            (bounds.x0 + bounds.x1) / 2.0,
            (bounds.y0 + bounds.y1) / 2.0,
        ))
    })
}

/// A no-op-but-recording frame waker (see [`ReactiveRuntime::init`]). The global
/// waker is swapped by whichever test inits last, so the count is never asserted
/// on — installing a recording waker just exercises the real init path.
pub fn recording_waker() -> FrameWaker {
    let counter = Arc::new(AtomicUsize::new(0));
    Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })
}

/// A process-wide serialization guard for tests that touch process-global
/// reactive state (the shared background executor's timing, `set_app_theme`,
/// …). Two such tests in the same binary would otherwise race under default
/// parallel `cargo test` — the known flake this harness avoids by having each
/// such test take this lock first. Poisoning is ignored (a panicking test
/// still releases a usable guard) so one failure doesn't cascade into
/// spurious failures in the rest of the binary.
pub fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Installs the reactive runtime and an ambient owner for a test, mirroring the
/// desktop shell's startup. Returns the ambient owner (held for the test's
/// duration so component owners stay valid children of it). Required before any
/// test that mounts a `Component` (`on_cleanup`/`spawn_local`/context all need an
/// ambient owner + the installed executor).
pub fn setup() -> Owner {
    let _runtime = ReactiveRuntime::init(recording_waker());
    let ambient = Owner::new();
    ambient.set();
    ambient
}

/// Drives frames — pump the UI-thread local task queue, sleep 1ms, rebuild —
/// until `done` returns `true`, starting from an already-rendered `scene`. This
/// is the `examples/inbox` pump-poll pattern: the only sleep in this harness,
/// bounded by a 5-second deadline so a stuck async task fails the test instead of
/// hanging it.
///
/// Requires [`setup`] to have run (needs the installed [`ReactiveRuntime`]).
pub fn pump_until<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
    mut scene: RecScene,
    mut done: impl FnMut() -> bool,
) -> RecScene {
    let runtime = ReactiveRuntime::get()
        .expect("pump_until: ReactiveRuntime::init must have run (see setup())");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(
            Instant::now() < deadline,
            "pump_until: timed out waiting for the condition"
        );
        runtime.pump_local();
        std::thread::sleep(Duration::from_millis(1));
        scene = frame(root, logic, state, tcx);
    }
    scene
}

/// Pumps and rebuilds `count` more frames unconditionally — for tests that need
/// to drive frames forward without waiting on a specific condition.
///
/// Requires [`setup`] to have run.
pub fn pump_frames<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
    count: usize,
) {
    let runtime = ReactiveRuntime::get()
        .expect("pump_frames: ReactiveRuntime::init must have run (see setup())");
    for _ in 0..count {
        runtime.pump_local();
        std::thread::sleep(Duration::from_millis(1));
        frame(root, logic, state, tcx);
    }
}

/// A synthetic pointer event at `p` (logical coordinates), the shape every
/// platform boundary already normalizes to before it reaches `RenderRoot`.
pub fn pointer(phase: PointerPhase, p: Point) -> InputEvent {
    InputEvent::Pointer(PointerEvent {
        phase,
        position: p,
        button: PointerButton::Primary,
    })
}

/// A full tap (Down then Up) at `p` — fires a `Button`'s `on_press` only when
/// both land inside its bounds (see `docs/CODE_STANDARDS.md`'s
/// "fire on up-inside" convention).
pub fn tap<S: 'static, V: View<S>>(root: &mut RenderRoot<S, V>, state: &mut S, p: Point) {
    root.event(state, &pointer(PointerPhase::Down, p));
    root.event(state, &pointer(PointerPhase::Up, p));
}

/// The center point of a recorded `(origin, size)` rounded-rect — where a tap
/// should land to hit that chrome.
pub fn center((origin, size): (Point, Size)) -> Point {
    Point::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0)
}

/// A typed-character key event (already-resolved text, mirroring a platform's
/// `KeyEvent.text` — see `frust_core::event::Key::Character`'s doc).
pub fn char_key(c: &str) -> InputEvent {
    InputEvent::Key(KeyEvent {
        key: Key::Character(c.to_string()),
        modifiers: Modifiers::default(),
        repeat: false,
    })
}

/// A named (control/navigation) key event with no modifiers held.
pub fn named_key(key: NamedKey) -> InputEvent {
    InputEvent::Key(KeyEvent {
        key: Key::Named(key),
        modifiers: Modifiers::default(),
        repeat: false,
    })
}
