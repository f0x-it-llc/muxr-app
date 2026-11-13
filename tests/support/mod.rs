//! Shared headless-test harness: a GPU-free recording paint target, frame/pointer/keyboard helpers, and
//! the [`setup`] seam every suite runs through before `MuxrApp::init`; unused helpers are expected.
//! [`frame`] paints at `FrameTime::ZERO`; use [`settle`] for anything on the frame clock.
#![allow(dead_code)]

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use frust::provide_context;
use frust_core::accesskit::{Node, Role};
use frust_core::{
    FrameTime, InputEvent, Key, KeyEvent, Modifiers, NamedKey, PaintScene, PointerButton,
    PointerEvent, PointerPhase, RenderRoot, View, WindowEdgeInsets, WindowInsets, WindowMetrics,
};
use frust_reactive::{FrameWaker, ReactiveRuntime};
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{Affine, Point, Rect, Shape, Size};
use peniko::{Brush, Color};
use reactive_graph::owner::Owner;

pub const W: f64 = 800.0;
pub const H: f64 = 600.0;

/// Geometry-match tolerance in logical px.
pub const EPSILON: f64 = 0.5;

pub fn surface() -> Size {
    Size::new(W, H)
}

pub fn metrics_at(width: f64, height: f64) -> WindowMetrics {
    WindowMetrics::new(Size::new(width, height), 1.0, WindowInsets::default())
}

pub fn mount_with_metrics<T>(metrics: Option<WindowMetrics>, mount: impl FnOnce() -> T) -> T {
    if let Some(metrics) = metrics {
        provide_context(metrics);
    }
    mount()
}

pub fn reprovide_metrics(metrics: WindowMetrics) {
    provide_context(metrics);
}

/// Only an IME bottom occlusion (`view_insets.bottom`, never `view_padding`).
pub fn ime_bottom_inset(bottom: f64) -> WindowInsets {
    WindowInsets::new(
        WindowEdgeInsets::ZERO,
        WindowEdgeInsets::new(0.0, 0.0, 0.0, bottom),
    )
}

/// A GPU-free paint target. `rounded` records `TextInput`/filled-`Button` boxes; a ghost or danger
/// button has a transparent fill, so its `strokes` outline is its only geometry.
/// `transforms` records each pushed `Affine`, the only way to observe paint-time slide animations.
#[derive(Default)]
pub struct RecScene {
    pub glyph_runs: usize,
    pub rounded: Vec<(Point, Size)>,
    pub strokes: Vec<(Point, Size)>,
    pub fills: Vec<(Point, Size, Color)>,
    pub paths: Vec<(Point, Size)>,
    pub rounded_fills: Vec<(Point, Size, f64, Color)>,
    pub transforms: Vec<Affine>,
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
    fn push_transform(&mut self, transform: Affine) {
        self.transforms.push(transform);
    }
}

pub fn fills_at(scene: &RecScene, origin: Point, size: Size) -> Vec<Color> {
    scene
        .fills
        .iter()
        .filter(|(o, s, _)| same(&(*o, *s), &(origin, size)))
        .map(|(_, _, color)| *color)
        .collect()
}

pub fn alpha_of(color: Color) -> f32 {
    color.components[3]
}

pub fn rounded_in(scene: &RecScene, color: Color) -> Vec<(Point, Size, f64)> {
    scene
        .rounded_fills
        .iter()
        .filter(|(_, _, _, painted)| *painted == color)
        .map(|(origin, size, radius, _)| (*origin, *size, *radius))
        .collect()
}

pub fn largest(rects: &[(Point, Size)]) -> Option<(Point, Size)> {
    rects
        .iter()
        .copied()
        .max_by(|a, b| area(a).total_cmp(&area(b)))
}

pub fn smallest(rects: &[(Point, Size)]) -> Option<(Point, Size)> {
    rects
        .iter()
        .copied()
        .min_by(|a, b| area(a).total_cmp(&area(b)))
}

pub fn by_width(rects: &[(Point, Size)], width: f64) -> Option<(Point, Size)> {
    rects
        .iter()
        .copied()
        .find(|(_, size)| (size.width - width).abs() < EPSILON)
}

/// Rects in `after` not in `before`; matching is one-to-one so repeated rows stay distinct.
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

pub fn same(a: &(Point, Size), b: &(Point, Size)) -> bool {
    (a.0.x - b.0.x).abs() < EPSILON
        && (a.0.y - b.0.y).abs() < EPSILON
        && (a.1.width - b.1.width).abs() < EPSILON
        && (a.1.height - b.1.height).abs() < EPSILON
}

fn area((_, size): &(Point, Size)) -> f64 {
    size.width * size.height
}

pub fn frame<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
) -> RecScene {
    frame_sized(root, logic, state, tcx, surface())
}

pub fn frame_sized<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
    size: Size,
) -> RecScene {
    frame_at_sized(root, logic, state, tcx, size, FrameTime::ZERO)
}

pub fn frame_at<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
    frame_time: FrameTime,
) -> RecScene {
    frame_at_sized(root, logic, state, tcx, surface(), frame_time)
}

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

/// Past every transition/enter duration in the framework (the longest is about 400ms).
pub const SETTLE_MS: f64 = 2_000.0;

/// Frames at 0 and [`SETTLE_MS`], then one more whose rebuild finalizes the transition. Required before
/// tapping a pushed page: mid-transition the navigator swallows input, and it finishes on that rebuild.
pub fn settle<S: 'static, V: View<S>>(
    root: &mut RenderRoot<S, V>,
    logic: &mut impl FnMut(&mut S) -> V,
    state: &mut S,
    tcx: &mut TextContext,
) -> RecScene {
    settle_sized(root, logic, state, tcx, surface())
}

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

pub fn ms(ms: f64) -> FrameTime {
    FrameTime::from_nanos((ms * 1_000_000.0) as u64)
}

/// Every semantics label in pass order; the only way to assert a toast's or dialog's text.
pub fn node_labels<S: 'static, V: View<S>>(root: &RenderRoot<S, V>) -> Vec<String> {
    root.semantics()
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_string))
        .collect()
}

pub fn node_values<S: 'static, V: View<S>>(root: &RenderRoot<S, V>) -> Vec<String> {
    root.semantics()
        .nodes
        .iter()
        .filter_map(|(_, node)| node.value().map(str::to_string))
        .collect()
}

pub fn node_center_by_label<S: 'static, V: View<S>>(
    root: &RenderRoot<S, V>,
    label: &str,
) -> Option<Point> {
    node_center(root, |node| node.label() == Some(label))
}

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

pub fn node_roles<S: 'static, V: View<S>>(root: &RenderRoot<S, V>) -> Vec<Role> {
    root.semantics()
        .nodes
        .iter()
        .map(|(_, node)| node.role())
        .collect()
}

pub fn node_center_by_role<S: 'static, V: View<S>>(
    root: &RenderRoot<S, V>,
    role: Role,
) -> Option<Point> {
    node_center(root, |node| node.role() == role)
}

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

/// Past the framework's 500ms long-press threshold (transcribed; the constant is private).
pub const HELD_MS: f64 = 600.0;

/// Press, hold past [`HELD_MS`], release. The two middle frames are required: the hold is measured
/// across paints, and the callback fires on the next pointer event.
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

/// A recording frame waker; the global waker is swapped by whichever test inits last, so its count is never asserted.
pub fn recording_waker() -> FrameWaker {
    let counter = Arc::new(AtomicUsize::new(0));
    Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    })
}

/// Serializes tests touching process-global reactive state; poisoning is ignored.
pub fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Installs the reactive runtime and an ambient owner, and swaps `MuxrApp::init`'s platform-backed dependencies
/// for mocks (stores, connection, repositories, link opener, offline fonts in a temp cache). The seams are
/// process-static and flip before init; required before mounting any `Component`.
pub fn setup() -> Owner {
    muxr::features::settings::di::use_mock_preferences_for_tests();
    muxr::connection::use_mock_connection_for_tests();
    muxr::features::sessions::di::use_mock_sessions_for_tests();
    muxr::features::sessions::presentation::pages::set_loading_rain_for_tests(false);
    muxr::features::settings::di::use_mock_auth_tokens_for_tests();
    muxr::features::terminal::di::use_mock_terminal_for_tests();
    muxr::features::connect::di::use_mock_saved_servers_for_tests();
    muxr::features::connect::di::use_mock_credentials_for_tests();
    // Biometric probe off: a Touch ID Mac would otherwise hand saved rows to the dock.
    muxr::features::connect::presentation::controller::set_biometrics_available_for_tests(Some(
        false,
    ));
    muxr::features::pro::di::use_mock_entitlement_for_tests();
    muxr::links::set_link_opener_for_tests(|_url| {});
    muxr::fonts::use_offline_font_source_for_tests();
    muxr::fonts::use_font_cache_dir_for_tests(
        std::env::temp_dir().join(format!("muxr-test-fonts-{}", std::process::id())),
    );
    let _runtime = ReactiveRuntime::init(recording_waker());
    let ambient = Owner::new();
    ambient.set();
    ambient
}

/// Pumps the task queue and rebuilds until `done()` is true; a 5s deadline fails the test instead of hanging.
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

pub fn pointer(phase: PointerPhase, p: Point) -> InputEvent {
    InputEvent::Pointer(PointerEvent {
        phase,
        position: p,
        button: PointerButton::Primary,
    })
}

pub fn contact(id: u32, phase: PointerPhase, p: Point) -> InputEvent {
    InputEvent::PointerContact {
        pointer_id: frust::PointerId::touch(id),
        event: PointerEvent {
            phase,
            position: p,
            button: PointerButton::Primary,
        },
    }
}

pub fn tap<S: 'static, V: View<S>>(root: &mut RenderRoot<S, V>, state: &mut S, p: Point) {
    root.event(state, &pointer(PointerPhase::Down, p));
    root.event(state, &pointer(PointerPhase::Up, p));
}

pub fn center((origin, size): (Point, Size)) -> Point {
    Point::new(origin.x + size.width / 2.0, origin.y + size.height / 2.0)
}

pub fn char_key(c: &str) -> InputEvent {
    InputEvent::Key(KeyEvent {
        key: Key::Character(c.to_string()),
        modifiers: Modifiers::default(),
        repeat: false,
    })
}

pub fn named_key(key: NamedKey) -> InputEvent {
    InputEvent::Key(KeyEvent {
        key: Key::Named(key),
        modifiers: Modifiers::default(),
        repeat: false,
    })
}

/// The switcher's gap between cards (private in the framework; transcribed).
pub const STRIP_GAP: f64 = 16.0;

/// Where strip card `card` sits when `centred` is in the middle: strip order is the live page first,
/// then peers in `faces` order.
pub fn strip_card_rect(window: Size, card_scale: f64, card: usize, centred: usize) -> Rect {
    let stride = window.width * card_scale + STRIP_GAP;
    let centre = Point::new(
        window.width / 2.0 + (card as f64 - centred as f64) * stride,
        window.height / 2.0,
    );
    Rect::from_center_size(centre, window * card_scale)
}

/// The centre of the part of `rect` on screen, `None` when none of it is.
pub fn on_screen_centre(rect: Rect, window: Size) -> Option<Point> {
    let visible = rect.intersect(Rect::from_origin_size(Point::ZERO, window));
    (visible.width() > 0.0 && visible.height() > 0.0).then(|| visible.center())
}
