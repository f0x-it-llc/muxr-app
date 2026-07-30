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
//! Not every test uses every helper.
#![allow(dead_code)]

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use frust_core::{
    FrameTime, InputEvent, Key, KeyEvent, Modifiers, NamedKey, PaintScene, PointerButton,
    PointerEvent, PointerPhase, RenderRoot, View,
};
use frust_reactive::{FrameWaker, ReactiveRuntime};
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{Point, Rect, Size};
use peniko::Color;
use reactive_graph::owner::Owner;

pub const W: f64 = 800.0;
pub const H: f64 = 600.0;

/// A GPU-free paint target: counts glyph runs (one per painted `Text` — enough
/// to observe whether a page's placeholder text painted) and records
/// rounded-rect chrome (each `TextInput` field and `Button`) so a test can
/// locate a button to tap it.
#[derive(Default)]
pub struct RecScene {
    pub glyph_runs: usize,
    pub rounded: Vec<(Point, Size)>,
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
