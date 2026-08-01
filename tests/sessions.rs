//! Headless behavior coverage for the `/sessions` screen
//! (`src/features/sessions/**`) — no GPU, no window: a `RenderRoot` over the
//! real page builder, driven through `tests/support`.
//!
//! # The harness, and why the screen is *beside* a navigator rather than in one
//!
//! In production `sessions_page` is a navigator page. Mounting it that way here
//! would make every assertion geometric: `NavigatorWidget` contributes no
//! `Widget::semantics` and forwards none of its pages' (the framework gap
//! `tests/ui_substrate.rs` documents), so nothing inside a navigator appears in
//! the accessibility tree at all.
//!
//! So [`Harness`] mounts the screen as the **bottom layer of a `Stack`**, with
//! an otherwise-empty navigator — driven by the *same* `NavigatorController`
//! the screen is handed — stacked over it. The screen's own chrome therefore
//! stays reachable by semantics label (badges, alerts) and by text value (every
//! run), while the dialogs/sheet/route pushes the screen makes are still
//! exercised **end to end** through a real navigator — located, like
//! `ui_substrate.rs`'s, by painted geometry.
//!
//! The fixture deliberately mounts **no top bar**: the muxr bar is app shell
//! chrome now (a fixed-index child of `muxr::shell::root_view`'s root `Stack`,
//! floating over the navigator), so everything it carries — the host title, the
//! brand, the `⚙` — is asserted in `tests/navigation_motion.rs` instead. What
//! this suite still owes the bar is the *negative*: that the screen grew none
//! of it back.
//!
//! The screen does, however, reserve the band that bar covers
//! (`ui::topbar::topbar_clearance`, which every bar-bearing page wraps its body
//! in now that the bar floats rather than displacing it), so **everything this
//! suite locates geometrically sits 46px lower than it did when the shell
//! handed the page a shortened box**. Every probe here is relative — widths,
//! before/after diffs, semantics-derived tap points — so the shift is invisible
//! to them; the resting seam itself is asserted in `navigation_motion.rs`,
//! which mounts the real shell and can see the bar it must line up with.
//!
//! The navigator's base page is a **full-bleed, childless** `SizedBox`, which
//! matters twice: it paints nothing and handles nothing, so every tap falls
//! through the navigator to the screen below (`Stack` routes top-down and only
//! stops at a child that reports `Handled`) — *and* the navigator stays inside
//! `Stack`'s hit test even with an empty stack, which is what lets a pop's
//! `on_result` flush at all (`NavigatorWidget::event` is where pending results
//! are delivered, so an event that never reaches the navigator never delivers
//! them).
//!
//! Locating an overlay's chrome uses one extra trick over `ui_substrate.rs`'s:
//! the screen underneath paints plenty of its own rects, so each test diffs the
//! frame *before* opening an overlay against the one after
//! (`support::added`) and addresses only what the overlay itself added.
//!
//! Because the fixture drives its own navigator, [`Harness::mount`] also calls
//! [`muxr::routes::install_app_router`] over that controller — the app's one
//! path-pushing handle, which the screen resolves in its `Component::init`,
//! then points at *this* navigator, carrying the real route table and the real
//! session guard with it (that guard is what
//! [`a_card_tap_pushes_the_terminal_route_and_the_guard_still_applies`]
//! exercises).
//!
//! # The redesigned screen's interaction model, and how it is driven here
//!
//! The cards carry no buttons: **a tap attaches, a long-press opens the
//! management sheet**. A tap is [`support::tap`] on the card's own name run; a
//! long-press is [`support::long_press`], which spans the two paints the
//! framework's paint-clock hold timer needs (see that helper's docs). The
//! sheet's rows are located geometrically, like every other pushed page.
//!
//! # Async
//!
//! Every repository call is real async work spawned onto the UI-thread queue
//! (see `data`'s mock latency), so a mutation is only visible after pumping —
//! [`Harness::pump_until_values`] is the deadline-bounded wait every test uses
//! instead of a fixed frame count.

use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, NavigatorController, SizedBox, Stack, WindowMetrics, any, navigator,
    provide_context,
};
use frust_core::{NamedKey, PointerPhase, RenderRoot, WindowInsets};
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::sessions::presentation::pages::sessions_page;
use muxr::routes::install_app_router;
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, by_width, center, char_key, frame, largest, long_press, named_key,
    node_centers_by_label, node_centers_by_value, node_labels, node_values, pointer, pump_frames,
    serial, settle, setup, smallest, tap,
};

/// A point in the top-left corner — scrim on every overlay this screen pushes
/// (the dialog centers its panel, the sheet anchors its own to the bottom).
const SCRIM_POINT: Point = Point::new(4.0, 4.0);
/// The panel gutter `glyph::dialog` and `ui::sheet` share, transcribed — a
/// full-bleed child of either is laid out at the panel width minus twice this.
const PANEL_PADDING: f64 = 20.0;
/// `.sessions-list{padding:…16px}`, transcribed: a card spans the viewport
/// minus twice this, which is how a test tells card chrome from everything
/// else painted around it.
const LIST_PAD_X: f64 = 16.0;
/// A hairline border's path is inset half a stroke width on each side, so a
/// recorded border's bounding box is this much narrower than the box it
/// outlines (`frust-widgets`' own `BORDER_WIDTH`, transcribed).
const ROW_BORDER_INSET: f64 = 1.0;
/// `.sess-card{border-left:2px}`, transcribed: a **running** card paints its
/// surface fill inset by the accent's width, an idle one doesn't paint the
/// accent at all — the cheapest honest read on "this card is accented".
const ACCENT_WIDTH: f64 = 2.0;
/// How long a pumped wait may run before the test fails rather than hangs.
const PUMP_DEADLINE: Duration = Duration::from_secs(5);
/// The scrolling list column's max width past
/// `crate::responsive::WIDE_BREAKPOINT_PX` (FINDINGS #21's width-clamp pass),
/// transcribed (the constant itself is private to `presentation::pages`).
const SESSIONS_CLAMP_WIDTH: f64 = 640.0;

/// The seeded session names `data`'s mock repository ships — the redesign's
/// three cards, in order.
const SEEDED: [&str; 3] = ["dev", "jumping-peach", "herdr"];
/// How many of those are *running* (the third card is idle).
const RUNNING: usize = 2;

// ---------------------------------------------------------------------------
// Local harness helpers
// ---------------------------------------------------------------------------

/// A mock connect target. `offer_credential` decides whether the credential
/// banner is offered on this connection.
fn mock_target(offer_credential: bool) -> ConnectTarget {
    ConnectTarget {
        host: "mock.example".to_string(),
        port: 7777,
        trust_mode: TrustMode::TrustOnFirstUse,
        label: "Mock rig".to_string(),
        offer_credential,
    }
}

/// A [`WindowMetrics`] whose size is `size` — see `mount_with_metrics`.
fn metrics_at(size: Size) -> WindowMetrics {
    WindowMetrics::new(size, 1.0, WindowInsets::default())
}

/// [`Harness::mount`], first publishing `metrics` into context when `Some` —
/// the shape `muxr`'s pages read via `use_context::<WindowMetrics>()` for the
/// responsive width-clamp pass (FINDINGS #21). `None` leaves context unset,
/// exactly the state every other test in this suite mounts under (no shell
/// has published yet), so this helper is purely additive.
fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Harness {
    if let Some(metrics) = metrics {
        provide_context(metrics);
    }
    Harness::mount()
}

/// The app-logic closure `RenderRoot::rebuild` drives.
type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

/// The screen, an overlay navigator over it, and everything a frame needs.
struct Harness {
    nav: NavigatorController<AppState>,
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
}

impl Harness {
    /// Mount the screen (see the [module docs](self) for the shape), leaving
    /// the connection **disconnected** and the first load un-awaited.
    fn mount() -> Self {
        // The real composition root: it publishes the app-scoped connection
        // controller and calls every feature's `di::provide` — including this
        // feature's repository. Its own router/navigator go unused; the
        // harness drives its own controller so no `MuxrApp::build` (and so no
        // cold-start splash) is ever involved.
        let state = MuxrApp.init();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        // Re-point the app's one path-pushing handle (`routes::app_nav`, which
        // the screen resolves in its own `init`) at this fixture's navigator,
        // real route table and real session guard included.
        let _router = install_app_router(&nav, expect_connection_controller());
        let logic_nav = nav.clone();
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            let screen_nav = logic_nav.clone();
            let overlay_nav = logic_nav.clone();
            any(Stack(vec![
                sessions_page(screen_nav),
                any(navigator(&overlay_nav, || {
                    // Full-bleed and childless — see the module docs.
                    any(SizedBox::<AppState>(
                        Some(f64::INFINITY),
                        Some(f64::INFINITY),
                    ))
                })),
            ]))
        });
        let mut harness = Harness {
            nav,
            root: RenderRoot::new(),
            logic,
            state,
            tcx: TextContext::new(),
        };
        harness.frame();
        harness
    }

    /// Mount, connect (mock), and wait for the first session load to land.
    fn connected() -> Self {
        let mut harness = Harness::mount();
        expect_connection_controller().connect(mock_target(false));
        harness.await_sessions();
        harness
    }

    /// One frame at the zero clock.
    fn frame(&mut self) -> RecScene {
        frame(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
        )
    }

    /// Drive a pushed/popped overlay past the navigator's input-blocking
    /// transition, returning the last frame's paint record.
    fn settle(&mut self) -> RecScene {
        settle(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
        )
    }

    /// Settle, then deliver a pop's result: the navigator flushes a pusher's
    /// `on_result` at the start of the first event pass **after** the pop's
    /// rebuild, so a harmless `Move` is what actually hands the answer over
    /// (`ui_substrate.rs`'s idiom).
    fn flush(&mut self) {
        self.settle();
        self.settle();
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, SCRIM_POINT));
    }

    /// Pump the UI-thread task queue and rebuild until `done` accepts the
    /// harness, or fail at [`PUMP_DEADLINE`] rather than hang.
    fn pump_until(&mut self, what: &str, done: impl Fn(&Harness) -> bool) {
        let deadline = Instant::now() + PUMP_DEADLINE;
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; on screen: {:?}",
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

    /// [`pump_until`](Self::pump_until) over the rendered semantics values.
    fn pump_until_values(&mut self, what: &str, done: impl Fn(&[String]) -> bool) {
        self.pump_until(what, move |h| done(&node_values(&h.root)));
    }

    /// Wait for the async session load to land (the screen renders every
    /// seeded name).
    fn await_sessions(&mut self) {
        self.pump_until_values("the seeded sessions", |values| {
            SEEDED.iter().all(|name| values.iter().any(|v| v == name))
        });
    }

    /// A full tap at `point`.
    fn tap(&mut self, point: Point) {
        tap(&mut self.root, &mut self.state, point);
    }

    /// Tap the first node whose printed text is `value` — how a test presses a
    /// card (its name run) or the dashed new-session row, neither of which is
    /// a labelled control any more.
    fn tap_value(&mut self, value: &str) {
        let point = self.center_of(value);
        self.tap(point);
    }

    /// Tap the first node *labelled* `label` (a badge, a button, the top bar's
    /// icon).
    fn tap_label(&mut self, label: &str) {
        let point = node_centers_by_label(&self.root, label)
            .first()
            .copied()
            .unwrap_or_else(|| {
                panic!(
                    "nothing labelled {label:?}; labels on screen: {:?}",
                    node_labels(&self.root)
                )
            });
        self.tap(point);
    }

    /// Where the first node printing `value` sits.
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

    /// Long-press the card named `name` — the management sheet's one entry
    /// point.
    fn long_press_card(&mut self, name: &str) {
        let point = self.center_of(name);
        long_press(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            point,
        );
    }

    /// Type `text` into whatever holds focus, one already-resolved character
    /// event at a time (the shape a platform delivers).
    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.root.event(&mut self.state, &char_key(&c.to_string()));
        }
    }

    /// Press a named key against the focused chain.
    fn press(&mut self, key: NamedKey) {
        self.root.event(&mut self.state, &named_key(key));
    }

    /// How many session cards are on screen — counted by the card-width
    /// surface fill each one paints, so the count never depends on which
    /// *names* a test happens to have created.
    fn card_count(&mut self) -> usize {
        let scene = self.frame();
        card_backs(&scene).len()
    }

    /// The navigator's page-stack depth (1 = only the harness's empty base
    /// page).
    fn depth(&self) -> usize {
        self.nav.depth()
    }
}

/// One `.sess-card` surface fill per card. Every card — running or idle —
/// reserves its 2px `border-left`, so its surface starts exactly
/// [`ACCENT_WIDTH`] inside the list's left padding and spans the rest: that
/// pair of numbers is what identifies a card among everything else painted,
/// with no colour read at all.
fn card_backs(scene: &RecScene) -> Vec<(Point, Size)> {
    let width = support::W - 2.0 * LIST_PAD_X - ACCENT_WIDTH;
    scene
        .rounded
        .iter()
        .copied()
        .filter(|(origin, size)| {
            (size.width - width).abs() < EPSILON
                && (origin.x - (LIST_PAD_X + ACCENT_WIDTH)).abs() < EPSILON
        })
        .collect()
}

/// The **accent** rects in `scene` — the full-width fill a card paints *under*
/// its surface, and only when it is running. Counting these against
/// [`card_backs`] is the cheapest honest read on "this card is accented and
/// that one isn't", without asserting on an exact colour or alpha.
fn accent_bars(scene: &RecScene) -> Vec<(Point, Size)> {
    let width = support::W - 2.0 * LIST_PAD_X;
    scene
        .rounded
        .iter()
        .copied()
        .filter(|(origin, size)| {
            (size.width - width).abs() < EPSILON && (origin.x - LIST_PAD_X).abs() < EPSILON
        })
        .collect()
}

/// Open a dialog by tapping the node labelled `label`, then return the rects
/// the dialog itself added to the frame.
fn open_dialog_by_label(h: &mut Harness, label: &str) -> Chrome {
    let before = h.frame();
    h.tap_label(label);
    let after = h.settle();
    Chrome::diff(&before, &after)
}

/// …and by tapping the node printing `value` (a card, the dashed row).
fn open_dialog_by_value(h: &mut Harness, value: &str) -> Chrome {
    let before = h.frame();
    h.tap_value(value);
    let after = h.settle();
    Chrome::diff(&before, &after)
}

/// Long-press the card named `name` and return the management sheet's own
/// chrome.
fn open_manage_sheet(h: &mut Harness, name: &str) -> Chrome {
    let before = h.frame();
    h.long_press_card(name);
    let after = h.settle();
    Chrome::diff(&before, &after)
}

/// The chrome a pushed overlay added to the frame — the screen underneath is
/// diffed out (see [`added`]).
struct Chrome {
    rounded: Vec<(Point, Size)>,
    strokes: Vec<(Point, Size)>,
}

impl Chrome {
    fn diff(before: &RecScene, after: &RecScene) -> Self {
        Chrome {
            rounded: added(&before.rounded, &after.rounded),
            strokes: added(&before.strokes, &after.strokes),
        }
    }

    /// The overlay's own panel — the biggest rect it painted.
    fn panel(&self) -> (Point, Size) {
        largest(&self.rounded).expect("an overlay always paints its own panel")
    }

    /// The overlay's full-content-width child: a dialog's name field, or the
    /// first of a sheet's rows (see [`PANEL_PADDING`]).
    fn content_child(&self) -> (Point, Size) {
        let content_w = self.panel().1.width - 2.0 * PANEL_PADDING;
        by_width(&self.rounded, content_w)
            .unwrap_or_else(|| panic!("no full-width child in {:?}", self.rounded))
    }

    /// **Every** full-content-width filled child, top to bottom — the
    /// management sheet's non-destructive rows (`Attach`, then `Rename…`).
    fn content_rows(&self) -> Vec<(Point, Size)> {
        let content_w = self.panel().1.width - 2.0 * PANEL_PADDING;
        let mut rows: Vec<(Point, Size)> = self
            .rounded
            .iter()
            .copied()
            .filter(|(_, size)| (size.width - content_w).abs() < EPSILON)
            .collect();
        rows.sort_by(|a, b| a.0.y.total_cmp(&b.0.y));
        rows
    }

    /// The sheet's destructive row — the last one, and the only one that
    /// paints **no** fill (a Glyph danger button is a transparent box with an
    /// error border, where its two neighbours are filled `Secondary` rows).
    ///
    /// Located among the borders rather than the fills, and by a width one
    /// [`ROW_BORDER_INSET`] under the content width: a stroke's recorded
    /// bounding box is its path's, which a border inset by half its own
    /// hairline on each side makes exactly that much narrower than the box it
    /// outlines.
    fn destructive_row(&self) -> (Point, Size) {
        let content_w = self.panel().1.width - 2.0 * PANEL_PADDING;
        let mut borders: Vec<(Point, Size)> = self
            .strokes
            .iter()
            .copied()
            .filter(|(_, size)| (size.width - (content_w - ROW_BORDER_INSET)).abs() < EPSILON)
            .collect();
        borders.sort_by(|a, b| a.0.y.total_cmp(&b.0.y));
        let row = *borders
            .last()
            .unwrap_or_else(|| panic!("no bordered row in {:?}", self.strokes));
        let lowest_fill = self
            .content_rows()
            .last()
            .map(|(origin, _)| origin.y)
            .unwrap_or_default();
        assert!(
            row.0.y > lowest_fill,
            "the destructive row is the sheet's last: {row:?} vs fills ending at {lowest_fill}"
        );
        row
    }
}

// ---------------------------------------------------------------------------
// 1 — the fixed server strip
// ---------------------------------------------------------------------------

#[test]
fn the_conn_row_prints_the_connections_facts() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    assert!(
        values.iter().any(|v| v == "mock.example:7777"),
        "the strip prints `host:port`: {values:?}"
    );
    assert!(
        values.iter().any(|v| v == "zellij 0.44.3 \u{b7} 36ms"),
        "…and the dim version · latency meta: {values:?}"
    );
    // The bar that titles the host (and the brand beside it) is the *shell's*
    // now, not this screen's — `tests/navigation_motion.rs` owns those
    // assertions. What must stay true here is that this screen mounts no bar
    // of its own: nothing on it prints the brand wordmark.
    assert!(
        !values.iter().any(|v| v == "muxr"),
        "the screen no longer mounts a top bar of its own: {values:?}"
    );

    // The TLS posture is a *badge* (a `Role::Status` node, hence a label). The
    // mock connects trust-on-first-use — the one posture that skips
    // certificate validation — and the redesign's `.warn-badge` is what it
    // earns; a validated posture would print no chip at all.
    let labels = node_labels(&h.root);
    assert!(
        labels.iter().any(|l| l == "insecure"),
        "…and the insecure warning chip: {labels:?}"
    );
}

// ---------------------------------------------------------------------------
// 2 — the cards
// ---------------------------------------------------------------------------

#[test]
fn the_seeded_sessions_render_as_cards_with_their_own_third_metric() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    for name in SEEDED {
        assert!(
            values.iter().any(|v| v == name),
            "the seeded session {name:?} renders: {values:?}"
        );
    }
    // The `.sess-meta` row: two fixed spans plus one that varies by what the
    // session *is* — clients for a running zellij session, spaces for herdr,
    // saved tabs for an idle one.
    for span in [
        "\u{25a3} 9 panes",
        "\u{25e7} 4 tabs",
        "\u{25ce} 0 clients",
        "\u{25c6} 5 spaces",
        "0 tabs saved",
    ] {
        assert!(
            values.iter().any(|v| v == span),
            "the meta row prints {span:?}: {values:?}"
        );
    }
    // The redesigned card has no creation line at all.
    assert!(
        !values.iter().any(|v| v.contains("created")),
        "no `created` line survives the redesign: {values:?}"
    );
    // …and no per-card buttons: attach/rename/kill are gesture-driven now.
    let labels = node_labels(&h.root);
    for gone in ["Attach", "Rename", "Kill", "Switch", "+"] {
        assert!(
            !labels.iter().any(|l| l == gone),
            "the {gone:?} button row is gone: {labels:?}"
        );
    }
    assert!(
        labels.iter().any(|l| l == "running") && labels.iter().any(|l| l == "resurrectible"),
        "both status chips are reachable from the seed: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == "zellij") && labels.iter().any(|l| l == "herdr"),
        "…and both backend chips: {labels:?}"
    );
}

#[test]
fn an_idle_card_carries_no_accent_and_no_pane_pills() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    let scene = h.frame();

    assert_eq!(card_backs(&scene).len(), SEEDED.len(), "one fill per card");
    assert_eq!(
        accent_bars(&scene).len(),
        RUNNING,
        "only the running cards paint the success accent: {:?}",
        accent_bars(&scene)
    );

    // Pane pills: the two running cards preview two each, the idle one draws
    // no `.sess-panes` row at all.
    let values = node_values(&h.root);
    for pill in ["btop", "ed@precision:~"] {
        assert!(
            values.iter().any(|v| v == pill),
            "a running card previews {pill:?}: {values:?}"
        );
    }
    assert_eq!(
        values.iter().filter(|v| *v == "\u{b7}\u{b7}\u{b7}").count(),
        2,
        "herdr's two placeholder pills, and no others: {values:?}"
    );
}

// ---------------------------------------------------------------------------
// 3 — tap = attach
// ---------------------------------------------------------------------------

/// Tap the first card and report the resulting page-stack depth plus the rects
/// the pushed page added to the frame.
fn tap_outcome(connected: bool) -> (usize, usize) {
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target(false));
    h.await_sessions();
    if !connected {
        expect_connection_controller().disconnect();
        h.frame();
    }

    let before = h.frame();
    h.tap_value(SEEDED[0]);
    let after = h.settle();
    (h.depth(), added(&before.rounded, &after.rounded).len())
}

#[test]
fn a_card_tap_pushes_the_terminal_route_and_the_guard_still_applies() {
    let _g = serial();
    let _ambient = setup();

    // Sequential, not interleaved: each harness re-provides the app-scoped
    // connection controller, so only the most recently mounted one is the live
    // handle the route guard reads.
    let (connected_depth, connected_rects) = tap_outcome(true);
    let (guarded_depth, guarded_rects) = tap_outcome(false);

    assert_eq!(connected_depth, 2, "a card tap pushes a page");
    assert_eq!(guarded_depth, 2, "…and so does the bounce");
    // The terminal placeholder is an app bar over centered text and paints no
    // rounded chrome at all; the Connect screen the session guard bounces a
    // disconnected attach to paints its eight pieces (mark tile, badge, two
    // fields, the CTA). That gap is what identifies *which* route the push
    // resolved to, without either count being hardcoded.
    assert!(
        connected_rects < guarded_rects,
        "a connected tap lands on /terminal ({connected_rects} rects) while a \
         disconnected one is bounced to Connect ({guarded_rects} rects)"
    );
}

// ---------------------------------------------------------------------------
// 4 — long-press = the management sheet (and never an attach)
// ---------------------------------------------------------------------------

#[test]
fn a_long_press_opens_the_management_sheet_without_attaching() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let sheet = open_manage_sheet(&mut h, SEEDED[0]);
    assert_eq!(h.depth(), 2, "the sheet is pushed as its own page");

    // Bottom-anchored: the panel's own bottom edge overshoots the viewport by
    // one corner radius (`ui::sheet`'s top-rounded-only trick).
    let (origin, size) = sheet.panel();
    assert!(
        origin.y + size.height > support::H,
        "a sheet, not a dialog: {origin:?}/{size:?}"
    );
    // Two ordinary rows (Attach, Rename…) plus the destructive one, which
    // paints a border rather than a fill.
    assert_eq!(
        sheet.content_rows().len(),
        2,
        "Attach and Rename… : {:?}",
        sheet.content_rows()
    );
    sheet.destructive_row();

    // Exactly **one** page was pushed: had the press also fired the tap,
    // `/terminal` would be on the stack too and dismissing the sheet would
    // leave the navigator at depth 2 rather than back at the list.
    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 1, "a scrim tap dismisses it, and nothing else");
    assert!(
        node_values(&h.root).iter().any(|v| v == SEEDED[0]),
        "…back to the list, not to the terminal"
    );
}

#[test]
fn the_sheets_rename_row_round_trips() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let sheet = open_manage_sheet(&mut h, SEEDED[0]);
    // Row order is the sheet's own: Attach, then Rename….
    let rename = sheet.content_rows()[1];
    let before = h.frame();
    h.tap(center(rename));
    let after = h.settle();
    let dialog = Chrome::diff(&before, &after);
    assert_eq!(
        h.depth(),
        2,
        "the sheet popped and the rename dialog took its place"
    );

    let field = dialog.content_child();
    // Tap past the seeded text so the caret lands at its end, then append.
    h.tap(Point::new(
        field.0.x + field.1.width - 4.0,
        field.0.y + field.1.height / 2.0,
    ));
    h.type_text("-2");
    h.press(NamedKey::Enter);
    h.flush();

    h.pump_until_values("the renamed card", |values| {
        values.iter().any(|v| v == "dev-2")
    });
    let values = node_values(&h.root);
    assert!(
        !values.iter().any(|v| v == "dev"),
        "the old name is gone, not duplicated: {values:?}"
    );
    assert_eq!(
        h.card_count(),
        SEEDED.len(),
        "a rename never changes the list length"
    );
}

#[test]
fn the_sheets_kill_row_confirms_first_then_removes_the_card() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    // A dismissed confirmation kills nothing.
    kill_through_the_sheet(&mut h, SEEDED[0], false);
    assert_eq!(h.depth(), 1, "the scrim tap closed the dialog");
    assert_eq!(h.card_count(), SEEDED.len(), "…and nothing was killed");

    // Confirming does.
    kill_through_the_sheet(&mut h, SEEDED[0], true);
    h.pump_until_values("the killed card to disappear", |values| {
        !values.iter().any(|v| v == SEEDED[0])
    });
    assert_eq!(h.depth(), 1);
    assert_eq!(h.card_count(), SEEDED.len() - 1);
}

/// Long-press `name`, take the sheet's Kill… row, and either confirm the
/// dialog it raises or dismiss it by scrim.
fn kill_through_the_sheet(h: &mut Harness, name: &str, confirm: bool) {
    let sheet = open_manage_sheet(h, name);
    let kill = sheet.destructive_row();
    let before = h.frame();
    h.tap(center(kill));
    let after = h.settle();
    let dialog = Chrome::diff(&before, &after);

    if confirm {
        // The danger action is the smallest border the dialog painted (the
        // panel's own being the other one).
        let action = smallest(&dialog.strokes).expect("the danger action strokes its border");
        h.tap(center(action));
    } else {
        h.tap(SCRIM_POINT);
    }
    h.flush();
    pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 4);
}

// ---------------------------------------------------------------------------
// 5 — create: the dashed row, and the empty state
// ---------------------------------------------------------------------------

#[test]
fn the_dashed_row_creates_a_session() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    assert_eq!(h.card_count(), SEEDED.len());

    let dialog = open_dialog_by_value(&mut h, "new session");
    assert_eq!(h.depth(), 2, "the dialog is pushed as its own page");

    h.tap(center(dialog.content_child()));
    h.type_text("staging");
    h.press(NamedKey::Enter);
    h.flush();

    h.pump_until_values("the created session's card", |values| {
        values.iter().any(|v| v == "staging")
    });
    assert_eq!(h.depth(), 1, "the dialog closed behind it");
    assert_eq!(
        h.card_count(),
        SEEDED.len() + 1,
        "the new card joined the list rather than replacing one"
    );
}

#[test]
fn killing_every_session_shows_the_empty_state_and_creates_from_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    for _ in 0..SEEDED.len() {
        let name = node_values(&h.root)
            .into_iter()
            .find(|v| SEEDED.contains(&v.as_str()))
            .expect("a card is still on screen");
        kill_through_the_sheet(&mut h, &name, true);
        h.pump_until_values("the killed card to disappear", move |values| {
            !values.contains(&name)
        });
    }

    let labels = node_labels(&h.root);
    assert!(
        labels.iter().any(|l| l == "No sessions"),
        "an emptied list shows the empty state: {labels:?}"
    );
    assert_eq!(h.card_count(), 0, "…and no cards");

    // The empty state's own create action is the other of the screen's two
    // create entry points (the dashed row is gone with the list).
    let dialog = open_dialog_by_label(&mut h, "+ new session");
    h.tap(center(dialog.content_child()));
    h.type_text("first");
    h.press(NamedKey::Enter);
    h.flush();
    h.pump_until_values("a session created from the empty state", |values| {
        values.iter().any(|v| v == "first")
    });
    assert!(
        !node_labels(&h.root).iter().any(|l| l == "No sessions"),
        "…and creating one leaves the empty state behind"
    );
}

// ---------------------------------------------------------------------------
// 6 — the settings slot moved out (see `tests/navigation_motion.rs`)
// ---------------------------------------------------------------------------

/// The `⚙` that reaches `/settings` is the *shell bar*'s trailing action now
/// (`muxr::routes::BarAction::Settings`), not this screen's — so it must not be
/// reachable from anything this screen mounts. Its behavior is asserted in
/// `tests/navigation_motion.rs`, where the bar actually lives.
#[test]
fn the_screen_carries_no_settings_affordance_of_its_own() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let labels = node_labels(&h.root);
    assert!(
        !labels.iter().any(|l| l == "\u{2699}"),
        "the gear moved to the shell's bar: {labels:?}"
    );
}

// ---------------------------------------------------------------------------
// 7 — the save-credential offer (unchanged behavior)
// ---------------------------------------------------------------------------

/// The banner's own title, as `glyph::alert` exposes it (label = title).
const OFFER_TITLE: &str = "Save this server?";

#[test]
fn the_credential_offer_banner_shows_and_dismisses() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target(true));
    h.await_sessions();

    assert!(
        node_labels(&h.root).iter().any(|l| l == OFFER_TITLE),
        "a connection that offered its credential raises the banner here, \
         not on Connect: {:?}",
        node_labels(&h.root)
    );

    h.tap_label("Not now");
    h.frame();

    assert!(
        !node_labels(&h.root).iter().any(|l| l == OFFER_TITLE),
        "dismissing clears the offer"
    );
}

#[test]
fn accepting_the_credential_offer_clears_it_and_confirms_by_toast() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target(true));
    h.await_sessions();
    app_toasts().clear();

    h.tap_label("Save");
    h.frame();

    assert!(
        !node_labels(&h.root).iter().any(|l| l == OFFER_TITLE),
        "accepting clears the offer"
    );
    let pending = app_toasts().pending_untracked();
    assert!(
        pending
            .iter()
            .any(|m| m.contains("Mock rig") && m.contains("mock")),
        "…and says so, honestly labelled as a mock save: {pending:?}"
    );
}

// ---------------------------------------------------------------------------
// 8 — responsive width-clamp pass (FINDINGS #21)
// ---------------------------------------------------------------------------

/// [`card_backs`]'s clamp-aware sibling: a card's surface fill, generalized
/// over the box the list column is actually laid out at (`card_backs` bakes
/// in `support::W`, which the clamp deliberately narrows).
fn card_backs_in(scene: &RecScene, left: f64, width: f64) -> Vec<(Point, Size)> {
    let card_width = width - ACCENT_WIDTH;
    scene
        .rounded
        .iter()
        .copied()
        .filter(|(origin, size)| {
            (size.width - card_width).abs() < EPSILON
                && (origin.x - (left + ACCENT_WIDTH)).abs() < EPSILON
        })
        .collect()
}

#[test]
fn wide_metrics_clamp_and_center_the_list_column_but_not_the_conn_row_strip() {
    let _g = serial();
    let _ambient = setup();
    let mut h = mount_with_metrics(Some(metrics_at(support::surface())));
    expect_connection_controller().connect(mock_target(false));
    h.await_sessions();

    let scene = h.frame();
    let margin = (support::W - SESSIONS_CLAMP_WIDTH) / 2.0;
    let list_left = margin + LIST_PAD_X;
    let list_width = SESSIONS_CLAMP_WIDTH - 2.0 * LIST_PAD_X;
    let cards = card_backs_in(&scene, list_left, list_width);
    assert_eq!(
        cards.len(),
        SEEDED.len(),
        "every seeded card still renders, just narrower and centered"
    );

    // The conn-row strip is the module's deliberate exception: it stays
    // full-bleed regardless of the list column's own clamp (see
    // `SESSIONS_CLAMP_WIDTH`'s doc in `presentation::pages`). The strip's
    // panel itself paints no fill (`Color::TRANSPARENT`, so nothing lands in
    // `scene.rounded` for it directly), but its leading success dot
    // (`pieces::DOT`-sized, the one 7×7 rounded fill in this frame) is
    // painted at the strip's own `STRIP_PAD_X` — unaffected by any centering
    // margin — which is exactly the full-bleed proof.
    const STRIP_PAD_X: f64 = 16.0;
    const DOT: f64 = 7.0;
    let dot = scene
        .rounded
        .iter()
        .find(|(_, size)| (size.width - DOT).abs() < EPSILON && (size.height - DOT).abs() < EPSILON)
        .copied()
        .expect("the strip's success dot paints its own rounded fill");
    assert!(
        (dot.0.x - STRIP_PAD_X).abs() < EPSILON,
        "the strip is not centered/clamped like the list column: {dot:?}"
    );
}

#[test]
fn narrow_metrics_leave_the_list_column_unclamped_like_no_metrics_at_all() {
    let _g = serial();
    let _ambient = setup();
    // Explicit narrow metrics, even though the harness still paints at the
    // full-width default surface — proving the branch keys on the
    // *published* width, not the paint surface.
    let mut h = mount_with_metrics(Some(metrics_at(Size::new(
        400.0,
        support::surface().height,
    ))));
    expect_connection_controller().connect(mock_target(false));
    h.await_sessions();

    let scene = h.frame();
    let cards = card_backs(&scene);
    assert_eq!(
        cards.len(),
        SEEDED.len(),
        "narrow metrics take the same unclamped branch as no metrics at all"
    );
}
