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
//! [`long_press`] is the gesture counterpart: a press that spans two paints so
//! the framework's paint-clock hold timer actually crosses its threshold.
//!
//! Each of those three lays out at the default [`surface`] (800×600); the
//! `*_sized` variants take the surface explicitly, for a suite whose screen is
//! taller than 600px and would otherwise lay its last row out below the fold,
//! untappable.
//!
//! Locating what to tap has two routes, and which one works depends on where
//! the target sits:
//!
//! - **Semantics** ([`node_labels`]/[`node_values`]/[`node_center_by_label`]/
//!   [`node_centers_by_label`]/[`node_centers_by_value`]/[`node_center_by_role`])
//!   — reads labels, values
//!   and bounds straight off the accessibility tree, and is the only way to
//!   assert *what a toast said* (a recording `PaintScene` sees glyph runs, not
//!   strings). It does **not** see anything inside a `Navigator`:
//!   `NavigatorWidget` contributes no `Widget::semantics` and forwards none of
//!   its pages' (a framework gap — every pushed page is invisible to
//!   accessibility).
//! - **Recorded geometry** ([`largest`]/[`smallest`]/[`by_width`] over
//!   [`RecScene::rounded`]/[`RecScene::strokes`], plus [`added`] to diff an
//!   overlay's own chrome out of a busy frame) — what a test must use for a
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

/// Geometry comparison tolerance, in logical px — what "the same recorded
/// rect" means to [`by_width`]/[`same`] and to any suite matching painted
/// chrome by size.
pub const EPSILON: f64 = 0.5;

/// The default surface every un-sized frame helper lays out at ([`W`]×[`H`]).
pub fn surface() -> Size {
    Size::new(W, H)
}

/// A GPU-free paint target: counts glyph runs (one per painted `Text` — enough
/// to observe whether a page's placeholder text painted) and records
/// rounded-rect chrome (each `TextInput` field and filled `Button`) so a test
/// can locate a button to tap it, plus stroked outlines
/// ([`RecScene::strokes`]) — a Glyph ghost/danger button has a **transparent**
/// fill and is invisible in `rounded`, so its border is the only geometry it
/// paints.
///
/// Two more channels, for chrome whose *colour* or whose *vector-ness* is the
/// assertion (rather than its box):
///
/// - [`RecScene::fills`] — plain `fill_rect`s **with their colour**. A scrim is
///   a full-coverage rect whose only frame-to-frame difference is its alpha, so
///   a geometry-only recorder cannot tell a faded one from an opaque one (see
///   [`alpha_of`]/[`fills_at`]).
/// - [`RecScene::paths`] — filled vector paths, the one thing a
///   `frust::icon` emits. "This affordance is a vector, not a font glyph" is
///   exactly "it drew a path and contributed no run of its own".
/// - [`RecScene::rounded_fills`] — [`RecScene::rounded`]'s colour- *and*
///   radius-carrying sibling. Most chrome in this app is a **rounded** fill, so
///   a suite asserting on the *tone* of a card (or on a radius that animates)
///   needs the same channel `fills` gives plain rects.
#[derive(Default)]
pub struct RecScene {
    pub glyph_runs: usize,
    pub rounded: Vec<(Point, Size)>,
    /// `(origin, bounding-box size)` per stroked path — see the struct docs.
    pub strokes: Vec<(Point, Size)>,
    /// `(origin, size, colour)` per plain filled rect — see the struct docs.
    pub fills: Vec<(Point, Size, Color)>,
    /// `(origin, bounding-box size)` per **filled** path — every `frust::icon`
    /// paints exactly one.
    pub paths: Vec<(Point, Size)>,
    /// `(origin, size, radius, colour)` per rounded fill, in paint order — see
    /// the struct docs. Parallel to [`RecScene::rounded`], which stays
    /// geometry-only so every existing suite reads unchanged.
    pub rounded_fills: Vec<(Point, Size, f64, Color)>,
}

impl PaintScene for RecScene {
    fn fill_rect(&mut self, origin: Point, size: Size, color: Color) {
        self.fills.push((origin, size, color));
    }
    fn fill_rounded_rect(&mut self, origin: Point, size: Size, radius: f64, color: Color) {
        self.rounded.push((origin, size));
        self.rounded_fills.push((origin, size, radius, color));
    }
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
    fn draw_glyph_run(&mut self, _run: GlyphRun) {
        self.glyph_runs += 1;
    }
    fn draw_image(&mut self, _data: &peniko::ImageData, _dest: Rect) {}
    fn fill_path(&mut self, origin: Point, path: &kurbo::BezPath, _brush: &Brush) {
        let bounds = path.bounding_box();
        self.paths
            .push((origin, Size::new(bounds.width(), bounds.height())));
    }
    fn stroke_path(&mut self, origin: Point, path: &kurbo::BezPath, _width: f64, _brush: &Brush) {
        let bounds = path.bounding_box();
        self.strokes
            .push((origin, Size::new(bounds.width(), bounds.height())));
    }
}

/// Every plain fill in `scene` covering exactly `(origin, size)` (within
/// [`EPSILON`]), in paint order — how a test picks one band's fills out of a
/// frame that fills plenty of others.
pub fn fills_at(scene: &RecScene, origin: Point, size: Size) -> Vec<Color> {
    scene
        .fills
        .iter()
        .filter(|(o, s, _)| same(&(*o, *s), &(origin, size)))
        .map(|(_, _, color)| *color)
        .collect()
}

/// A colour's alpha channel.
pub fn alpha_of(color: Color) -> f32 {
    color.components[3]
}

/// Every rounded fill in `scene` painted in exactly `color`, as
/// `(origin, size, radius)` in paint order — [`fills_at`]'s sibling for chrome
/// whose **tone** is the assertion (a terminal card's `GlyphInk` body, a
/// selected pill's amber wash) and whose box is not.
pub fn rounded_in(scene: &RecScene, color: Color) -> Vec<(Point, Size, f64)> {
    scene
        .rounded_fills
        .iter()
        .filter(|(_, _, _, painted)| *painted == color)
        .map(|(origin, size, radius, _)| (*origin, *size, *radius))
        .collect()
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

/// The first recorded rect whose width matches `width` (within [`EPSILON`]) —
/// the deterministic way to pick a full-content-width child (a dialog's text
/// field) out of a frame.
pub fn by_width(rects: &[(Point, Size)], width: f64) -> Option<(Point, Size)> {
    rects
        .iter()
        .copied()
        .find(|(_, size)| (size.width - width).abs() < EPSILON)
}

/// The rects in `after` that weren't in `before` — an overlay's own chrome,
/// isolated from whatever screen is painting underneath it. Matching is
/// one-to-one (each `before` rect is consumed by at most one `after` rect), so
/// a screen that paints five identical rows stays five rows.
pub fn added(before: &[(Point, Size)], after: &[(Point, Size)]) -> Vec<(Point, Size)> {
    let mut remaining: Vec<(Point, Size)> = before.to_vec();
    let mut new = Vec::new();
    for rect in after {
        match remaining.iter().position(|candidate| same(candidate, rect)) {
            Some(index) => {
                remaining.swap_remove(index);
            }
            None => new.push(*rect),
        }
    }
    new
}

/// Whether two recorded rects are the same one, within [`EPSILON`].
pub fn same(a: &(Point, Size), b: &(Point, Size)) -> bool {
    (a.0.x - b.0.x).abs() < EPSILON
        && (a.0.y - b.0.y).abs() < EPSILON
        && (a.1.width - b.1.width).abs() < EPSILON
        && (a.1.height - b.1.height).abs() < EPSILON
}

fn area((_, size): &(Point, Size)) -> f64 {
    size.width * size.height
}

/// One frame at the default [`surface`]: rebuild the tree from `state`, lay it
/// out shaping real text, and record the paint output.
pub fn frame<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
) -> RecScene {
    frame_sized(root, logic, state, tcx, surface())
}

/// [`frame`] at an explicit surface `size` — for a screen taller than the
/// default 600px, whose lower rows would otherwise lay out below the fold and
/// be untappable.
pub fn frame_sized<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
    size: Size,
) -> RecScene {
    frame_at_sized(root, logic, state, tcx, size, FrameTime::ZERO)
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
    frame_at_sized(root, logic, state, tcx, surface(), frame_time)
}

/// [`frame_at`] at an explicit surface `size` — the one function the whole
/// frame family bottoms out in.
pub fn frame_at_sized<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
    size: Size,
    frame_time: FrameTime,
) -> RecScene {
    root.rebuild(logic, state);
    let tcx_any: &mut dyn Any = tcx;
    root.layout_with_text(size, tcx_any);
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
    settle_sized(root, logic, state, tcx, surface())
}

/// [`settle`] at an explicit surface `size` — see [`frame_sized`].
pub fn settle_sized<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
    size: Size,
) -> RecScene {
    frame_at_sized(root, logic, state, tcx, size, FrameTime::ZERO);
    frame_at_sized(root, logic, state, tcx, size, ms(SETTLE_MS));
    frame_at_sized(root, logic, state, tcx, size, ms(SETTLE_MS))
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

/// Every semantics **value** the current tree carries, in pass order.
/// [`node_labels`]'s other half: frust's `text` widget contributes a
/// `Role::Label` node carrying its run in `value` (accesskit guidance), so
/// "this name is on screen" is a value assertion, while a button/badge/alert
/// title is a label one.
pub fn node_values<S: 'static, V: View<S>>(root: &RenderRoot<S, V>) -> Vec<String> {
    root.semantics()
        .nodes
        .iter()
        .filter_map(|(_, node)| node.value().map(str::to_string))
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

/// The centers of **every** semantics node labelled `label`, in pass order —
/// [`node_center_by_label`]'s all-matches sibling, for a screen that renders
/// the same affordance once per row (an "Attach"/"Rename"/"Kill" per card).
pub fn node_centers_by_label<S: 'static, V: View<S>>(
    root: &RenderRoot<S, V>,
    label: &str,
) -> Vec<Point> {
    root.semantics()
        .nodes
        .iter()
        .filter(|(_, node)| node.label() == Some(label))
        .filter_map(|(_, node): &(_, Node)| node.bounds())
        .map(|b| Point::new((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0))
        .collect()
}

/// Every semantics **role** the current tree carries, in pass order.
///
/// [`node_labels`]/[`node_values`]' third sibling, for chrome whose presence is
/// the assertion and whose text is not: a shell bar that is *hidden* on one
/// route contributes no `Role::TitleBar` node at all, which is the cheapest
/// honest read on "there is no bar here".
pub fn node_roles<S: 'static, V: View<S>>(root: &RenderRoot<S, V>) -> Vec<Role> {
    root.semantics()
        .nodes
        .iter()
        .map(|(_, node)| node.role())
        .collect()
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

/// How far past the framework's long-press threshold
/// (`frust_widgets::gesture`'s `LONG_PRESS_MS`, 500ms) [`long_press`] holds its
/// synthetic press. Transcribed rather than imported: the constant is private
/// to that module, and a test that held for *exactly* the threshold would be
/// asserting on floating-point equality.
pub const HELD_MS: f64 = 600.0;

/// A full long-press at `p`: press, hold past [`HELD_MS`], release.
///
/// **The two frames in the middle are the whole point.** A `GestureDetector`
/// measures a hold across *paints* — the only pass carrying a clock — so the
/// first frame records the press epoch and the second (at `HELD_MS`) marks the
/// threshold as passed; the callback itself fires on the **next pointer
/// event**, which is the `Up` below (`frust_widgets::gesture`'s
/// "fire-on-release" contract). A test that sent Down/Up with no frames between
/// them would produce an ordinary tap, and one that painted only once would
/// never cross the threshold.
///
/// A press resolved as a long-press never *also* fires `on_tap`, so this is
/// also how a test proves the tap path stayed untaken.
pub fn long_press<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
    p: Point,
) {
    root.event(state, &pointer(PointerPhase::Down, p));
    frame_at(root, logic, state, tcx, FrameTime::ZERO);
    frame_at(root, logic, state, tcx, ms(HELD_MS));
    root.event(state, &pointer(PointerPhase::Up, p));
}

/// The centers of every semantics node whose **value** is `value`, in pass
/// order — [`node_centers_by_label`]'s value-side sibling, for chrome composed
/// out of plain `text` runs (a card's name, the dashed new-session row) rather
/// than out of a labelled control.
pub fn node_centers_by_value<S: 'static, V: View<S>>(
    root: &RenderRoot<S, V>,
    value: &str,
) -> Vec<Point> {
    root.semantics()
        .nodes
        .iter()
        .filter(|(_, node)| node.value() == Some(value))
        .filter_map(|(_, node): &(_, Node)| node.bounds())
        .map(|b| Point::new((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0))
        .collect()
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
