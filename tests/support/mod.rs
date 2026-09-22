//! Shared headless-test harness: a GPU-free recording paint target ([`RecScene`]), frame/pointer/
//! keyboard helpers, and the [`setup`] ambient-owner + mock-opt-in seam every suite runs through
//! before `MuxrApp::init`. Factored so every `tests/*.rs` file (each its own crate root) can
//! share it; not every test uses every helper.
//!
//! [`frame`] paints at `FrameTime::ZERO`; use [`frame_at`]/[`settle`] for anything driven off the
//! frame clock — a navigator push/pop transition **blocks all input** mid-flight. [`long_press`]
//! spans two paints so the hold timer crosses its threshold.
//!
//! To locate what to tap: semantics helpers (`node_labels`/`node_center_by_label`/etc.) read the
//! accessibility tree and are the only way to assert what a toast said, but see nothing inside a
//! `Navigator` (no child semantics forwarded); use the recorded-geometry helpers
//! (`largest`/`smallest`/`by_width`/`added`) for a pushed page instead.
#![allow(dead_code)]

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use frust_core::accesskit::{Node, Role};
use frust_core::{
    FrameTime, InputEvent, Key, KeyEvent, Modifiers, NamedKey, PaintScene, PointerButton,
    PointerEvent, PointerPhase, RenderRoot, View, WindowEdgeInsets, WindowInsets,
};
use frust_reactive::{FrameWaker, ReactiveRuntime};
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{Affine, Point, Rect, Shape, Size};
use peniko::{Brush, Color};
use reactive_graph::owner::Owner;

pub const W: f64 = 800.0;
pub const H: f64 = 600.0;

/// Geometry-match tolerance in logical px — what "the same recorded rect" means to [`same`].
pub const EPSILON: f64 = 0.5;

/// The default surface every un-sized frame helper lays out at ([`W`]×[`H`]).
pub fn surface() -> Size {
    Size::new(W, H)
}

/// A [`WindowInsets`] with only an IME bottom occlusion (`view_insets.bottom`, never
/// `view_padding` — system bars aren't the IME). Publish via [`RenderRoot::set_insets`] before
/// the frame that should see the keyboard up.
pub fn ime_bottom_inset(bottom: f64) -> WindowInsets {
    WindowInsets::new(
        WindowEdgeInsets::ZERO,
        WindowEdgeInsets::new(0.0, 0.0, 0.0, bottom),
    )
}

/// A GPU-free paint target. `glyph_runs` counts painted text runs; `rounded` records each
/// `TextInput`/filled-`Button`'s box so a test can locate one to tap (a ghost/danger button has a
/// **transparent** fill, so its `strokes` outline is the only geometry it paints). `fills`/
/// `rounded_fills` carry colour (and radius) for tone assertions a box alone can't make — e.g. a
/// scrim rect that only changes alpha frame to frame ([`alpha_of`]/[`fills_at`]). `paths` is
/// filled vector geometry, the one thing `frust::icon` emits (a path with no glyph run).
/// `transforms` is every pushed `Affine`, recorded standalone rather than composed into the other
/// channels' coordinates — the only way to observe a widget that animates via a paint-time
/// transform push (e.g. `frust_glyph`'s sliding sheet panel) instead of moving pod origins.
#[derive(Default)]
pub struct RecScene {
    pub glyph_runs: usize,
    pub rounded: Vec<(Point, Size)>,
    /// `(origin, bbox)` per stroked path.
    pub strokes: Vec<(Point, Size)>,
    /// `(origin, size, colour)` per plain filled rect.
    pub fills: Vec<(Point, Size, Color)>,
    /// `(origin, bbox)` per filled path — one per `frust::icon`.
    pub paths: Vec<(Point, Size)>,
    /// `(origin, size, radius, colour)` per rounded fill, paint order.
    pub rounded_fills: Vec<(Point, Size, f64, Color)>,
    /// Every `push_transform`, paint order.
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

/// Plain fills in `scene` matching `(origin, size)` within [`EPSILON`], paint order — how a test
/// picks one band's fills out of a frame that fills plenty of others.
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

/// Rounded fills in `scene` painted exactly `color`, as `(origin, size, radius)`, paint order —
/// [`fills_at`]'s sibling for chrome whose tone (not box) is the assertion.
pub fn rounded_in(scene: &RecScene, color: Color) -> Vec<(Point, Size, f64)> {
    scene
        .rounded_fills
        .iter()
        .filter(|(_, _, _, painted)| *painted == color)
        .map(|(origin, size, radius, _)| (*origin, *size, *radius))
        .collect()
}

/// The recorded rect with the largest area, or `None` if nothing was recorded — an overlay's own
/// panel is always its biggest paint, so this finds it without hardcoding geometry.
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

/// The first recorded rect whose width matches `width` (within [`EPSILON`]) — the deterministic
/// way to pick a full-content-width child (a dialog's text field) out of a frame.
pub fn by_width(rects: &[(Point, Size)], width: f64) -> Option<(Point, Size)> {
    rects
        .iter()
        .copied()
        .find(|(_, size)| (size.width - width).abs() < EPSILON)
}

/// The rects in `after` that weren't in `before` — an overlay's own chrome, isolated from the
/// screen underneath it. Matching is one-to-one, so a screen painting five identical rows stays
/// five rows rather than being consumed away.
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

/// One frame painted at an explicit `frame_time` — [`frame`]'s clock-advancing sibling, for
/// anything driven off `PaintCtx::frame_time` (navigator transitions, a dialog's enter staging,
/// a toast's timeline).
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

/// Drive a freshly pushed/popped overlay past its transition: frames at 0 and [`SETTLE_MS`]
/// (completing the transition during paint), then one more whose **rebuild** finalizes it.
///
/// **Required before tapping a pushed page**: mid-transition the navigator suppresses all
/// routing (a tap sent too early is silently swallowed), and the transition isn't done when its
/// last frame paints but when the *following rebuild* finalizes it — the third frame is what
/// separates a test that taps a live sheet from one that taps a blocked navigator and passes for
/// the wrong reason.
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

/// Every semantics **value** the current tree carries, in pass order. [`node_labels`]'s other
/// half: `text` contributes a `Role::Label` node carrying its run in `value` (accesskit
/// guidance), so "this name is on screen" is a value assertion, a button/badge title a label one.
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

/// The centers of **every** node labelled `label` — [`node_center_by_label`]'s all-matches
/// sibling, for a per-row affordance ("Attach"/"Rename"/"Kill" repeated per card).
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

/// Every semantics **role** the current tree carries, in pass order. [`node_labels`]/
/// [`node_values`]'s third sibling, for chrome whose presence (not text) is the assertion: a
/// shell bar hidden on one route contributes no `Role::TitleBar` node at all.
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

/// How far past the framework's long-press threshold (`frust_widgets::gesture`'s
/// `LONG_PRESS_MS`, 500ms) [`long_press`] holds — transcribed rather than imported (private
/// constant), and past rather than at the threshold to avoid a float-equality assertion.
pub const HELD_MS: f64 = 600.0;

/// A full long-press at `p`: press, hold past [`HELD_MS`], release.
///
/// The two middle frames are required: `GestureDetector` measures a hold across *paints* (the
/// only pass with a clock), and its callback fires on the *next pointer event* — the `Up` below
/// (`frust_widgets::gesture`'s fire-on-release contract). A long-press never also fires `on_tap`,
/// so this doubles as proof the tap path stayed untaken.
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

/// The centers of every semantics node whose **value** is `value`, in pass order —
/// [`node_centers_by_label`]'s value-side sibling, for chrome built of plain `text` runs (a
/// card's name, the dashed new-session row) rather than a labelled control.
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

/// A process-wide serialization guard for tests touching process-global reactive state (the
/// shared executor's timing, `set_app_theme`, …) — avoids a known race under parallel
/// `cargo test`. Poisoning is ignored so one panicking test doesn't cascade failures onto others.
pub fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Installs the reactive runtime and an ambient owner (returned, held for the test's duration),
/// mirroring desktop-shell startup. Required before mounting any `Component`. Also opts this
/// process out of `MuxrApp::init`'s real platform-backed dependencies — an integration binary
/// links the normal (non-`cfg(test)`) library build, so this is the one seam every `tests/*.rs`
/// file goes through without editing each individually:
///
/// - `settings`/`connect` `di`: mock preferences, auth tokens, saved-server list, and credential
///   store — a real one would read/write this machine's own data and leak state across runs; the
///   credential store's real biometric gate also answers "unavailable" off-device.
/// - `connection`: mock `muxrd` client — no real socket opened; the mock supplies every fact
///   these suites assert on (e.g. the sessions strip's `zellij 0.44.3 · 36ms`).
/// - `sessions`/`terminal` `di`: mock repositories — a real one would fail "not connected"
///   against the mock-only connection above.
/// - `pro` `di`: publishes the scriptable entitlement mock, so a suite can script store answers.
/// - `sessions` `pages`: the list band's loading rain, which would hold the band over a pinned
///   `frame` clock it never advances past.
/// - `links`: a no-op link opener, so no tapped link ever reaches this machine's browser.
/// - `fonts`: offline download source (no suite reaches the network by omission) and a throwaway
///   temp cache dir (nothing writes into this machine's real app data).
pub fn setup() -> Owner {
    muxr::features::settings::di::use_mock_preferences_for_tests();
    muxr::connection::use_mock_connection_for_tests();
    muxr::features::sessions::di::use_mock_sessions_for_tests();
    muxr::features::sessions::presentation::pages::set_loading_rain_for_tests(false);
    muxr::features::settings::di::use_mock_auth_tokens_for_tests();
    muxr::features::terminal::di::use_mock_terminal_for_tests();
    muxr::features::connect::di::use_mock_saved_servers_for_tests();
    muxr::features::connect::di::use_mock_credentials_for_tests();
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

/// Pumps the UI-thread task queue, sleeps 1ms, and rebuilds until `done()` returns `true` — the
/// `examples/inbox` pump-poll pattern. The harness's only sleep; a 5s deadline fails the test
/// instead of hanging it. Requires [`setup`] (needs the installed [`ReactiveRuntime`]).
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

/// Pumps and rebuilds `count` more frames unconditionally, for tests that drive frames forward
/// without waiting on a specific condition. Requires [`setup`].
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

/// A full tap (Down then Up) at `p` — fires a `Button`'s `on_press` only when both land inside
/// its bounds (`docs/CODE_STANDARDS.md`'s "fire on up-inside" convention).
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
