//! Headless behavior coverage for the `/sessions` screen
//! (`src/features/sessions/**`) — no GPU, no window: a `RenderRoot` over the
//! real page builder, driven through `tests/support`.
//!
//! `NavigatorWidget` publishes no semantics for its pages, so [`Harness`] mounts
//! the screen as the bottom layer of a `Stack`, with an otherwise-empty
//! navigator (the *same* `NavigatorController` the screen is handed) over it:
//! the chrome stays reachable by label/value, while dialog/sheet/route pushes
//! are exercised end to end through a real navigator, located by painted
//! geometry ([`support::added`]). [`Harness::mount`] also installs the real app
//! router over that navigator, so a push carries the real route table and
//! session guard.
//!
//! No top bar is mounted (that's shell chrome now, `tests/navigation_motion.rs`'
//! job, mounted through the real shell scaffold that reserves the bar's own
//! band); this suite only owes the negative, that the screen grew no bar of
//! its own back — this screen reserves no clearance either any more (the
//! scaffold's `app_bar` slot owns that in the real app), so every geometric
//! probe here is relative rather than absolute.
//!
//! Cards carry no buttons: a tap attaches, a long-press opens the management
//! sheet. Every repository call is real async work on the UI-thread queue, so
//! [`Harness::pump_until_values`] deadline-waits for a mutation.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, GetUntracked, NavigatorController, Set, SizedBox, Stack, WindowMetrics,
    any, navigator, provide_context,
};
use frust_core::{NamedKey, PointerPhase, RenderRoot};
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::agents::{AgentStatus, AgentSummary};
use muxr::connection::{ConnectTarget, ServerBackend, TrustMode, expect_connection_controller};
use muxr::failure::AppFailure;
use muxr::features::sessions::data::repositories::mock_sessions_repository::{
    MockSessionsRepository, SEEDED_DEV_ID, SEEDED_HERDR_ID,
};
use muxr::features::sessions::di::SharedSessionsRepository;
use muxr::features::sessions::domain::entities::{
    CreateSessionParams, LayoutSummary, RenameRequest, Session, SessionAgent, SessionBackend,
    SessionStatus, ThirdMetric,
};
use muxr::features::sessions::domain::repositories::SessionsRepository;
use muxr::features::sessions::presentation::controllers::{POLL_PERIOD_SECS, SessionsController};
use muxr::features::sessions::presentation::pages::{resolve_poll_interval, sessions_page};
use muxr::features::settings::data::repositories::MockPreferencesRepository;
use muxr::features::settings::di::SharedPreferencesRepository;
use muxr::features::settings::domain::entities::{AppearancePrefs, PollPrefs, Preferences};
use muxr::features::settings::domain::repositories::PreferencesRepository;
use muxr::routes::install_app_router;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, by_width, center, char_key, frame_sized, largest, long_press,
    named_key, node_centers_by_label, node_centers_by_value, node_labels, node_values, pointer,
    pump_frames, serial, settle_sized, setup, smallest, tap,
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
/// `crate::responsive::WIDE_BREAKPOINT_PX` — the responsive width-clamp pass
/// — transcribed (the constant itself is private to `presentation::pages`).
const SESSIONS_CLAMP_WIDTH: f64 = 640.0;
/// A landscape phone's height: wide enough to clamp, still Compact.
const PHONE_LANDSCAPE_H: f64 = 390.0;
/// The Tablet windows the master-detail suite lays out at, and a phone's.
const FULL_TABLET: Size = Size::new(1024.0, 720.0);
const COLLAPSED_TABLET: Size = Size::new(720.0, 720.0);
const PHONE: Size = Size::new(390.0, 844.0);

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
        trust_mode: TrustMode::NoVerification,
        label: "Mock rig".to_string(),
        offer_credential,
        token: zeroize::Zeroizing::new(String::new()),
        remember: false,
        fingerprint: None,
    }
}

/// [`Harness::mount`], first publishing `metrics` into context when `Some` —
/// the shape `muxr`'s pages read for the responsive width-clamp pass. `None`
/// leaves context unset, the state every other test in this suite mounts under.
fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Harness {
    support::mount_with_metrics(metrics, Harness::mount)
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
    /// The size every frame lays out at.
    surface: Size,
}

impl Harness {
    /// Mount the screen (see the [module docs](self) for the shape), leaving
    /// the connection **disconnected** and the first load un-awaited.
    fn mount() -> Self {
        Harness::mount_over(None, support::surface())
    }

    /// [`mount`](Self::mount) laid out at `surface`, over `repository` in
    /// place of the seeded mock when one is given.
    fn mount_over(repository: Option<SharedSessionsRepository>, surface: Size) -> Self {
        // The real composition root, unbuilt: publishes DI including this
        // feature's repository, but never plays a splash.
        let state = MuxrApp.init();
        if let Some(repository) = repository {
            provide_context(repository);
        }
        let nav: NavigatorController<AppState> = NavigatorController::new();
        // Re-points `routes::app_nav()` at this fixture's own navigator, real
        // route table and session guard included.
        let _router = install_app_router(&nav, &nav, expect_connection_controller());
        let logic_nav = nav.clone();
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            let screen_nav = logic_nav.clone();
            let overlay_nav = logic_nav.clone();
            any(Stack(vec![
                // One controller plays both of the screen's navigators here
                // (route stack + overlay host, two in production —
                // `AppState::nav`/`overlays`), keeping every overlay a page of
                // the one mounted navigator, drivable and locatable here.
                any(sessions_page(screen_nav, logic_nav.clone())),
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
            surface,
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
        frame_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            self.surface,
        )
    }

    /// Drive a pushed/popped overlay past the navigator's input-blocking
    /// transition, returning the last frame's paint record.
    fn settle(&mut self) -> RecScene {
        settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            self.surface,
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
            // `pump_frames` lays out at the default surface.
            if self.surface != support::surface() {
                self.frame();
            }
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
    paths: Vec<(Point, Size)>,
}

impl Chrome {
    fn diff(before: &RecScene, after: &RecScene) -> Self {
        Chrome {
            rounded: added(&before.rounded, &after.rounded),
            strokes: added(&before.strokes, &after.strokes),
            paths: added(&before.paths, &after.paths),
        }
    }

    /// The overlay's own panel — the biggest surface it filled.
    ///
    /// Both channels, because the two overlays paint their panel differently: a
    /// dialog's is a plain rounded rect, while the Glyph sheet's is a *path*
    /// (only its top corners are rounded, which `fill_rounded_rect` cannot
    /// express).
    fn panel(&self) -> (Point, Size) {
        let mut candidates = self.rounded.clone();
        candidates.extend(self.paths.iter().copied());
        largest(&candidates).expect("an overlay always paints its own panel")
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
        values.iter().any(|v| v == "zellij 0.44.3"),
        "the strip prints every detected backend's version: {values:?}"
    );
    assert!(
        values.iter().any(|v| v == "36ms"),
        "…and the live latency as its own trailing run: {values:?}"
    );
    // The endpoint is the *bar's* now: printing it here too ran the two runs
    // into each other on a phone-width strip, and the bar above already
    // titles the connected host.
    assert!(
        !values.iter().any(|v| v.contains("mock.example")),
        "no endpoint anywhere in the strip: {values:?}"
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
    // mock connects with no verification — the one posture that skips
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
// 2b — the agent strip
// ---------------------------------------------------------------------------

/// Mount over the seeded mock after `seed` adjusted it, connected and loaded.
fn connected_with(seed: impl FnOnce(&MockSessionsRepository)) -> Harness {
    let mock = Arc::new(MockSessionsRepository::seeded());
    seed(&mock);
    let mut h = Harness::mount_over(Some(mock), support::surface());
    expect_connection_controller().connect(mock_target(false));
    h.await_sessions();
    h
}

fn summary(blocked: u32, done: u32, working: u32, idle: u32, kinds: &[&str]) -> AgentSummary {
    AgentSummary {
        total: blocked + done + working + idle,
        blocked,
        done,
        working,
        idle,
        unknown: 0,
        kinds: kinds.iter().map(|k| k.to_string()).collect(),
    }
}

fn position(labels: &[String], wanted: &str) -> usize {
    labels
        .iter()
        .position(|l| l == wanted)
        .unwrap_or_else(|| panic!("{wanted:?} missing: {labels:?}"))
}

#[test]
fn a_herdr_card_with_agents_renders_the_strip_and_the_needs_you_rollup() {
    let _g = serial();
    let _ambient = setup();
    let mut h = connected_with(|_| {});
    h.frame();

    let labels = node_labels(&h.root);
    let values = node_values(&h.root);
    // One tile per seeded kind, the count, and the group's spoken summary.
    for tile in ["C", "X", "G"] {
        assert!(
            labels.iter().any(|l| l == tile),
            "tile {tile:?}: {labels:?}"
        );
    }
    assert!(values.iter().any(|v| v == "4 agents"), "count: {values:?}");
    assert!(
        labels
            .iter()
            .any(|l| l == "4 agents: 1 needs you, 1 done, 1 working, 1 idle"),
        "summary line: {labels:?}"
    );
    // Pills in order blocked, done, working, idle.
    let order: Vec<usize> = ["\u{25c9} 1", "\u{2713} 1", "\u{25cf} 1", "\u{25cb} 1"]
        .iter()
        .map(|p| position(&labels, p))
        .collect();
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "pill order: {order:?}"
    );
    // A blocked agent: the herdr card's badge reads `needs you`, so only the
    // zellij card still says `running`.
    assert_eq!(labels.iter().filter(|l| *l == "needs you").count(), 1);
    assert_eq!(labels.iter().filter(|l| *l == "running").count(), 1);
}

#[test]
fn zero_count_pills_are_absent_and_an_unblocked_card_stays_running() {
    let _g = serial();
    let _ambient = setup();
    let mut h = connected_with(|repo| {
        repo.seed_agents_for_tests(SEEDED_HERDR_ID, Some(summary(0, 0, 2, 0, &["claude"])));
    });
    h.frame();

    let labels = node_labels(&h.root);
    assert!(labels.iter().any(|l| l == "\u{25cf} 2"), "{labels:?}");
    for gone in ["\u{25c9}", "\u{2713}", "\u{25cb}"] {
        assert!(
            !labels.iter().any(|l| l.starts_with(gone)),
            "no {gone} pill: {labels:?}"
        );
    }
    assert!(!labels.iter().any(|l| l == "needs you"), "{labels:?}");
    assert_eq!(labels.iter().filter(|l| *l == "running").count(), 2);
}

#[test]
fn a_zellij_card_and_an_idle_card_render_no_strip() {
    let _g = serial();
    let _ambient = setup();
    // Even a summary seeded onto the idle card draws nothing: the strip is
    // gated on liveness, like the pane pills.
    let mut h = connected_with(|repo| {
        let some = Some(summary(1, 0, 1, 0, &["claude", "codex"]));
        repo.seed_agents_for_tests("zellij:jumping-peach", some);
    });
    h.frame();

    // The seed's running zellij card carries no agents, so the one strip on
    // screen is the herdr card's.
    let values = node_values(&h.root);
    assert_eq!(values.iter().filter(|v| v.ends_with(" agents")).count(), 1);
    assert!(values.iter().any(|v| v == "4 agents"), "{values:?}");
    assert!(!values.iter().any(|v| v == "2 agents"), "{values:?}");
}

#[test]
fn a_session_without_agents_renders_as_it_always_did() {
    let _g = serial();
    let _ambient = setup();
    let mut with = connected_with(|_| {});
    let scene_with = with.frame();
    let mut without = connected_with(|repo| repo.seed_agents_for_tests(SEEDED_HERDR_ID, None));
    let scene_without = without.frame();

    let labels = node_labels(&without.root);
    assert!(!labels.iter().any(|l| l == "needs you"), "{labels:?}");
    assert!(
        !node_values(&without.root)
            .iter()
            .any(|v| v.contains("agents")),
        "no count text"
    );
    // Same cards, same accents, but the strip's line is gone from the herdr
    // card, so it is shorter.
    assert_eq!(card_backs(&scene_without).len(), SEEDED.len());
    assert_eq!(accent_bars(&scene_without).len(), RUNNING);
    let tallest = |scene: &RecScene| {
        card_backs(scene)
            .iter()
            .map(|(_, size)| size.height)
            .fold(0.0, f64::max)
    };
    assert!(tallest(&scene_with) > tallest(&scene_without));
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
    let (connected_depth, _) = tap_outcome(true);
    let (guarded_depth, guarded_rects) = tap_outcome(false);

    // `/terminal` resolves through the shell page, and only a route inside the
    // shell places that scaffold — so the outer op identifies which route the
    // push resolved to as exactly as any pixel would.
    assert_eq!(connected_depth, 2, "a card tap places the shell");
    // The bounce lands on the route the outer navigator's root already is, so
    // it applies **no** op: pushing there would stack a second copy of that
    // page over the live one. The whole observable is that the frame the tap
    // started on is still the frame.
    assert_eq!(guarded_depth, 1, "…and a bounced one pushes nothing");
    assert_eq!(
        guarded_rects, 0,
        "…leaving the frame it was tapped on untouched"
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

    // Bottom-anchored: the panel's own bottom edge is the viewport's, where a
    // dialog's panel floats clear of it.
    let (origin, size) = sheet.panel();
    assert!(
        (origin.y + size.height - support::H).abs() < EPSILON,
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
fn the_rename_row_leaves_the_sheet_mounted_until_its_exit_completes_before_the_dialog_lands() {
    // The chain pin: `sheet_action`'s Rename/Kill rows close through
    // `SheetHandle::close_then` precisely because an unchained close-then-push
    // would land the dialog *before* the sheet's own staged exit completes —
    // racing its pop. One frame after the tap is where that race would show
    // up: an unchained push lands synchronously (before any frame at all), so
    // depth would already read the sheet-plus-dialog's `3` here; chained, the
    // sheet is still mid-exit and the dialog has not landed yet, so depth is
    // still the sheet's own `2`. Fail-then-pass verified by reasoning:
    // reverting `sheet_action`'s Rename arm to a bare
    // `sheet.close(); actions.open_rename_dialog(&id);` would push the dialog
    // in the same rebuild the tap fires in, failing the assertion below.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let sheet = open_manage_sheet(&mut h, SEEDED[0]);
    let sheet_depth = h.depth();
    let rename = sheet.content_rows()[1];
    let before = h.frame();
    h.tap(center(rename));

    h.frame();
    assert_eq!(
        h.depth(),
        sheet_depth,
        "the sheet is still mounted one frame after Rename — playing its \
         exit, the dialog not yet pushed"
    );

    // Settling carries the chain to completion: the sheet pops, then the
    // dialog it chained lands on top of the popped page — one page traded
    // for another, so the depth the sheet itself sat at is unchanged.
    let after = h.settle();
    assert_eq!(
        h.depth(),
        sheet_depth,
        "…and the dialog lands once the exit finishes, in the sheet's own place"
    );
    let dialog = Chrome::diff(&before, &after);
    dialog.content_child();
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

/// `ui::dialogs::name_input_dialog`'s field composes through `glyph_dialog`'s
/// `.content()` slot, so it gets the framework's dead-space blur: a `Down` on
/// the dialog's own dead panel space ends the field's IME session without
/// closing the dialog, and a second tap re-engages it. The same pin as
/// `tests/settings.rs`'s (New Token dialog); `panel.origin + (2, 2)` sits
/// inside `PANEL_PADDING`, above every child.
#[test]
fn a_dead_space_down_blurs_the_focused_name_field_without_closing_the_dialog() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let dialog = open_dialog_by_value(&mut h, "new session");
    assert_eq!(h.depth(), 2, "the dialog is pushed as its own page");

    h.tap(center(dialog.content_child()));
    assert!(
        h.root.ime_state().is_some(),
        "precondition: the field owns a live IME session"
    );

    let (panel_origin, _) = dialog.panel();
    let dead = Point::new(panel_origin.x + 2.0, panel_origin.y + 2.0);
    h.tap(dead);

    assert!(
        h.root.ime_state().is_none(),
        "a Down on dead panel space ends the field's IME session — the keyboard retracts"
    );
    assert_eq!(h.depth(), 2, "the dialog itself is still open");

    h.tap(center(dialog.content_child()));
    assert!(
        h.root.ime_state().is_some(),
        "a second tap on the field re-engages it"
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
// 7 — the save-credential offer is NOT this screen's
// ---------------------------------------------------------------------------

/// The offer's title, as this screen's banner used to expose it (`glyph::alert`
/// label = title). Kept as the thing this screen must *not* paint: the same
/// words are the Connect dialog's title now (`tests/connect.rs`' own offer
/// section), and this is the regression tripwire against a second copy of the
/// question growing back here.
const OFFER_TITLE: &str = "Save this server?";

#[test]
fn a_pending_credential_offer_paints_nothing_on_this_screen() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    // A connection that raised an offer, exactly as a first connect does — the
    // signal is app-scoped and outlives the redirect, so it is still pending
    // while this screen renders.
    expect_connection_controller().connect(mock_target(true));
    h.await_sessions();

    assert_eq!(
        expect_connection_controller()
            .credential_offer
            .get_untracked(),
        Some("Mock rig".to_string()),
        "the offer is genuinely pending — this test would pass for the wrong \
         reason otherwise"
    );
    let labels = node_labels(&h.root);
    assert!(
        !labels.iter().any(|l| l == OFFER_TITLE),
        "the offer is answered on Connect, before the redirect that lands here \
         (see `crate::connection`'s credential-offer placement): {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l == "Not now"),
        "…and so is neither of its actions: {labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l == "Save"),
        "…including the one that raised the biometric prompt over this screen: \
         {labels:?}"
    );
}

#[test]
fn this_screen_never_touches_the_pending_offer() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target(true));
    h.await_sessions();
    // Whatever this screen does — load, poll, rebuild — the offer is not its
    // state to consume: only Connect's own dialog (or a teardown) clears it.
    h.frame();
    h.frame();

    assert_eq!(
        expect_connection_controller()
            .credential_offer
            .get_untracked(),
        Some("Mock rig".to_string()),
        "Sessions neither renders nor clears the offer any more"
    );
}

// ---------------------------------------------------------------------------
// 8 — responsive width-clamp pass
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
    // A phone on its side: wide, but its short side keeps it Compact (the
    // 800×600 default surface would be a Tablet window).
    let mut h = mount_with_metrics(Some(support::metrics_at(support::W, PHONE_LANDSCAPE_H)));
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
    // (`widgets::server_strip::DOT`-sized, the one 7×7 rounded fill in this
    // frame) is painted at the strip's own `STRIP_PAD_X` — unaffected by any
    // centering
    // margin — which is exactly the full-bleed proof. The dot sits centered
    // in the heartbeat's own ring slot rather than flush against that
    // padding, so the slot's half-inset comes into it; all three numbers are
    // transcribed from the build, like every other constant here.
    const STRIP_PAD_X: f64 = 16.0;
    const DOT: f64 = 7.0;
    const RING_SLOT: f64 = 20.0;
    let dot = scene
        .rounded
        .iter()
        .find(|(_, size)| (size.width - DOT).abs() < EPSILON && (size.height - DOT).abs() < EPSILON)
        .copied()
        .expect("the strip's success dot paints its own rounded fill");
    assert!(
        (dot.0.x - (STRIP_PAD_X + (RING_SLOT - DOT) / 2.0)).abs() < EPSILON,
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
    let mut h = mount_with_metrics(Some(support::metrics_at(400.0, support::H)));
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

// ---------------------------------------------------------------------------
// 9 — read-only gating (create/rename/kill disable; attach doesn't)
// ---------------------------------------------------------------------------

/// Flipping [`muxr::connection::ConnectionController::is_read_only`] directly
/// is enough to drive this — the mock backend's own `connect` always answers
/// `is_read_only: false` (`connection`'s module docs), so a suite that needs
/// the read-only branch writes the signal itself, exactly the seam a real
/// read-only credential's `Login` response would flip.
#[test]
fn read_only_disables_create_and_the_sheets_rename_and_kill_rows() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    expect_connection_controller().is_read_only.set(true);
    h.frame();

    // The dashed row is still on screen (same label, just dimmed) — a tap
    // opens nothing.
    let before = h.frame();
    h.tap_value("new session");
    let after = h.settle();
    assert_eq!(h.depth(), 1, "a read-only tap opens no create dialog");
    assert!(
        added(&before.rounded, &after.rounded).is_empty(),
        "…and paints no new chrome"
    );

    // The management sheet: Rename…/Kill… are still labelled rows — a
    // `Ghost` button paints no fill at rest (`frust-widgets`' own
    // `StylePaint`), so a disabled row vanishes from `content_rows`'s
    // fill-based geometric scan; locate them by label instead, the same way
    // `open_dialog_by_label` already does for the empty state's own action.
    // Attach is untouched by read-only.
    open_manage_sheet(&mut h, SEEDED[0]);
    let labels = node_labels(&h.root);
    assert!(labels.iter().any(|l| l == "Attach"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "Rename\u{2026}"), "{labels:?}");
    assert!(labels.iter().any(|l| l == "Kill\u{2026}"), "{labels:?}");

    let before = h.frame();
    h.tap_label("Rename\u{2026}");
    let after = h.settle();
    assert!(
        Chrome::diff(&before, &after).rounded.is_empty(),
        "a read-only rename tap opens no dialog"
    );
    assert_eq!(h.depth(), 2, "…and the sheet is still the top page");

    let before = h.frame();
    h.tap_label("Kill\u{2026}");
    let after = h.settle();
    assert!(
        Chrome::diff(&before, &after).rounded.is_empty(),
        "a read-only kill tap opens no confirm dialog"
    );
    assert_eq!(h.depth(), 2);
}

#[test]
fn read_only_leaves_the_empty_states_own_create_action_disabled_too() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    for name in SEEDED {
        kill_through_the_sheet(&mut h, name, true);
        h.pump_until_values("the killed card to disappear", move |values| {
            !values.iter().any(|v| v == name)
        });
    }
    assert!(
        node_labels(&h.root).iter().any(|l| l == "No sessions"),
        "an emptied list shows the empty state"
    );

    expect_connection_controller().is_read_only.set(true);
    h.frame();

    let before = h.frame();
    h.tap_label("+ new session");
    let after = h.settle();
    assert_eq!(h.depth(), 1, "a read-only tap opens no create dialog");
    assert!(added(&before.rounded, &after.rounded).is_empty());
}

// ---------------------------------------------------------------------------
// 10 — poll pile-up and stale-overwrite: at the controller level, no widget
// tree needed, since both hazards live entirely in `SessionsController`'s own
// claim/generation bookkeeping.
// ---------------------------------------------------------------------------

/// A minimal [`Session`], distinguishable only by `name` — the one field
/// either test below reads.
fn stub_session(name: &str) -> Session {
    Session {
        id: name.to_string(),
        name: name.to_string(),
        status: SessionStatus::Running,
        backend: SessionBackend::Zellij,
        age_secs: 0,
        has_bell: false,
        is_current: false,
        layout: LayoutSummary {
            tabs: 1,
            panes: 1,
            third: ThirdMetric::Clients(0),
            preview: Vec::new(),
            tabs_detail: Vec::new(),
            space: None,
        },
        agents: None,
    }
}

/// A [`SessionsRepository`] that only counts its own `list_sessions` calls —
/// the observable [`a_tick_mid_flight_does_not_spawn_a_second_poll_refresh`]
/// asserts on.
struct CountingSessionsRepo {
    calls: AtomicU32,
}

impl CountingSessionsRepo {
    fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for CountingSessionsRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Vec::new())
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }
}

/// A tick arriving while the previous one's poll-triggered refresh is still
/// outstanding is skipped, not stacked on top — the confirmed pile-up defect:
/// without the in-flight claim, an RPC round trip slower than the poll's own
/// period lets every later tick add another full fan-out to the ones still
/// running.
#[tokio::test]
async fn a_tick_mid_flight_does_not_spawn_a_second_poll_refresh() {
    let repo = Arc::new(CountingSessionsRepo::new());
    let controller = SessionsController::new(Arc::clone(&repo) as SharedSessionsRepository);
    controller.refresh().await;
    assert_eq!(repo.calls(), 1, "the first load ran exactly once");

    assert!(
        controller.take_poll_tick(),
        "the first tick claims the in-flight guard"
    );
    assert!(
        !controller.take_poll_tick(),
        "a second tick before the claimed refresh finishes is skipped, not stacked"
    );

    controller.refresh_from_poll().await;
    assert_eq!(
        repo.calls(),
        2,
        "exactly one poll-triggered fan-out ran, not two"
    );

    assert!(
        controller.take_poll_tick(),
        "the claim released once the poll refresh finished, so the next tick is taken again"
    );
}

/// A hand-rolled [`SessionsRepository`] whose two calls race deterministically
/// — `MockSessionsRepository`'s own latency is one fixed constant with no seam
/// to make one call outrun another, so the staleness-guard test below drives
/// this instead. The first call ("older") sleeps long enough that the second
/// ("newer") both starts and finishes first.
struct RaceSessionsRepo {
    calls: AtomicU32,
}

impl RaceSessionsRepo {
    fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for RaceSessionsRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            clean_signals::time::sleep(Duration::from_millis(80)).await;
            Ok(vec![stub_session("stale")])
        } else {
            clean_signals::time::sleep(Duration::from_millis(5)).await;
            Ok(vec![stub_session("fresh")])
        }
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the poll/staleness tests")
    }
}

/// An older, slower refresh that completes *after* a newer, faster one must
/// not clobber it — the confirmed overwrite defect: `sessions` is
/// last-writer-wins, so without the generation guard whichever round trip
/// happens to land last wins regardless of which one was actually requested
/// last.
#[tokio::test]
async fn a_slow_stale_refresh_completing_after_a_newer_one_does_not_clobber_it() {
    let controller = SessionsController::new(Arc::new(RaceSessionsRepo::new()));

    // `join!` polls its two arguments in the order written: the first
    // `refresh` bumps the generation and immediately suspends on its own
    // longer delay, then the second bumps past it and finishes first, on a
    // shorter one — reproducing the race deterministically rather than
    // depending on real network jitter (`src/net/client.rs`'s own
    // `concurrent_reconnects_are_single_flight` test relies on the same
    // left-to-right first-poll ordering).
    tokio::join!(controller.refresh(), controller.refresh());

    let names: Vec<String> = controller
        .sessions
        .get_untracked()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        names,
        vec!["fresh".to_string()],
        "the newer, faster refresh's result wins, not the older, slower one: {names:?}"
    );
}

// ---------------------------------------------------------------------------
// A new connection restarts the screen: the old server's list never lingers
// ---------------------------------------------------------------------------

/// `restart()` drops an in-flight refresh's late reply (the old server's), clears the
/// selection, and the kicked first load is what lands.
#[test]
fn a_restart_ignores_the_old_generations_late_reply_and_clears_the_selection() {
    use frust_reactive::ReactiveRuntime;

    let _g = serial();
    let _ambient = setup();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let controller = Arc::new(SessionsController::new(Arc::new(RaceSessionsRepo::new())));
    controller.select("stale");
    runtime.block_on(async {
        // The first read (call 0) is the slow "stale" one; the restart lands
        // while it is parked, so its reply carries an older generation.
        let late = {
            let controller = Arc::clone(&controller);
            async move { controller.refresh().await }
        };
        let restart = {
            let controller = Arc::clone(&controller);
            async move {
                tokio::task::yield_now().await;
                controller.restart();
            }
        };
        tokio::join!(late, restart);
    });
    assert!(
        controller.sessions.get_untracked().is_empty(),
        "the late old-generation reply was ignored"
    );
    assert_eq!(controller.selected.get_untracked(), None);
    assert!(!controller.loaded.get_untracked(), "the first load is anew");

    let pump = ReactiveRuntime::get().expect("runtime installed by setup()");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while !controller.first_load_settled.get_untracked() {
        assert!(Instant::now() < deadline, "the restarted load never landed");
        pump.pump_local();
        runtime.block_on(async { tokio::time::sleep(Duration::from_millis(5)).await });
    }
    let names: Vec<String> = controller
        .sessions
        .get_untracked()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, vec!["fresh".to_string()]);
}

/// A repository whose list is swapped between connections.
struct SwitchableRepo {
    names: Mutex<Vec<String>>,
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for SwitchableRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        let names = self.names.lock().expect("ok").clone();
        Ok(names.iter().map(|n| stub_session(n)).collect())
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the switch test")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the switch test")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the switch test")
    }
}

#[test]
fn switching_servers_replaces_the_old_hosts_cards_with_the_new_hosts_list() {
    let _g = serial();
    let _ambient = setup();
    let repo = Arc::new(SwitchableRepo {
        names: Mutex::new(vec!["alpha-one".to_string(), "alpha-two".to_string()]),
    });
    let mut h = Harness::mount_over(
        Some(Arc::clone(&repo) as SharedSessionsRepository),
        support::surface(),
    );
    expect_connection_controller().connect(mock_target(false));
    h.pump_until_values("host A's list", |v| v.iter().any(|s| s == "alpha-two"));

    *repo.names.lock().expect("ok") = vec!["beta-one".to_string()];
    let connection = expect_connection_controller();
    connection.disconnect();
    connection.connect(mock_target(false));

    h.pump_until_values("host B's list", |v| v.iter().any(|s| s == "beta-one"));
    let values = node_values(&h.root);
    assert!(
        !values.iter().any(|s| s.starts_with("alpha")),
        "host A's cards are gone: {values:?}"
    );
    assert_eq!(h.card_count(), 1);
}

// ---------------------------------------------------------------------------
// The Tablet detail's agent list is controller state: fetched on a selection
// change and on every poll tick while the detail shows it.
// ---------------------------------------------------------------------------

/// The one live herdr session [`AgentsRepo`] lists.
const AGENTS_SESSION: &str = "herdr:agents";

/// A repository with one live session whose roll-up names two agents, and a
/// scriptable agent list behind it.
struct AgentsRepo {
    agents: Mutex<Vec<SessionAgent>>,
    fail: std::sync::atomic::AtomicBool,
    calls: AtomicU32,
    lists: AtomicU32,
    /// When set, the next agents read parks until [`release`](Self::release) is notified.
    stall: std::sync::atomic::AtomicBool,
    release: tokio::sync::Notify,
}

impl AgentsRepo {
    fn new(agents: Vec<SessionAgent>) -> Self {
        Self {
            agents: Mutex::new(agents),
            fail: std::sync::atomic::AtomicBool::new(false),
            calls: AtomicU32::new(0),
            lists: AtomicU32::new(0),
            stall: std::sync::atomic::AtomicBool::new(false),
            release: tokio::sync::Notify::new(),
        }
    }

    fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    fn lists(&self) -> u32 {
        self.lists.load(Ordering::SeqCst)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for AgentsRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        self.lists.fetch_add(1, Ordering::SeqCst);
        let mut session = stub_session(AGENTS_SESSION);
        session.backend = SessionBackend::Herdr;
        session.agents = Some(summary(1, 0, 1, 0, &["claude"]));
        Ok(vec![session, stub_session("other")])
    }

    async fn list_agents(&self, _session_id: String) -> Result<Vec<SessionAgent>, AppFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.stall.swap(false, Ordering::SeqCst) {
            self.release.notified().await;
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(AppFailure::Network("list_agents refused".to_string()));
        }
        Ok(self.agents.lock().expect("ok").clone())
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the agent-list tests")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the agent-list tests")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the agent-list tests")
    }
}

fn session_agent(pane_id: u32, status: AgentStatus) -> SessionAgent {
    SessionAgent {
        pane_id,
        tab_position: Some(0),
        space_id: None,
        space_name: None,
        tab_name: "work".to_string(),
        kind: "claude".to_string(),
        name: Some(format!("agent{pane_id}")),
        display_kind: None,
        status,
        title: None,
        summary: None,
        change_seq: 1,
    }
}

fn agent_rows(controller: &SessionsController) -> Vec<(u32, AgentStatus)> {
    controller
        .agents
        .get_untracked()
        .iter()
        .map(|agent| (agent.pane_id, agent.status))
        .collect()
}

/// A loaded controller over `repo`, its detail visible, with the agents
/// session selected and its first agent read landed.
async fn selected_with_agents(repo: &Arc<AgentsRepo>) -> Arc<SessionsController> {
    let controller = Arc::new(SessionsController::new(
        Arc::clone(repo) as SharedSessionsRepository
    ));
    controller.set_detail_visible(true);
    controller.refresh().await;
    controller.select(AGENTS_SESSION);
    controller.refresh_agents().await;
    controller
}

/// One poll tick, run to completion.
async fn tick(controller: &SessionsController) {
    assert!(controller.take_poll_tick(), "the poll is armed");
    controller.refresh_from_poll().await;
}

#[tokio::test]
async fn swapping_two_agents_statuses_at_constant_counts_updates_the_rows_on_the_next_tick() {
    let repo = Arc::new(AgentsRepo::new(vec![
        session_agent(1, AgentStatus::Blocked),
        session_agent(2, AgentStatus::Working),
    ]));
    let controller = selected_with_agents(&repo).await;
    assert_eq!(
        agent_rows(&controller),
        [(1, AgentStatus::Blocked), (2, AgentStatus::Working)]
    );

    // The roll-up still reads one blocked, one working: only the rows moved.
    *repo.agents.lock().expect("ok") = vec![
        session_agent(1, AgentStatus::Working),
        session_agent(2, AgentStatus::Blocked),
    ];
    tick(&controller).await;

    assert_eq!(
        agent_rows(&controller),
        [(2, AgentStatus::Blocked), (1, AgentStatus::Working)]
    );
}

#[tokio::test]
async fn a_failed_agent_read_keeps_the_previous_rows_and_the_next_tick_retries() {
    let repo = Arc::new(AgentsRepo::new(vec![session_agent(
        1,
        AgentStatus::Blocked,
    )]));
    let controller = selected_with_agents(&repo).await;

    repo.fail.store(true, Ordering::SeqCst);
    *repo.agents.lock().expect("ok") = vec![session_agent(1, AgentStatus::Idle)];
    tick(&controller).await;
    assert_eq!(agent_rows(&controller), [(1, AgentStatus::Blocked)]);

    repo.fail.store(false, Ordering::SeqCst);
    tick(&controller).await;
    assert_eq!(agent_rows(&controller), [(1, AgentStatus::Idle)]);
}

#[tokio::test]
async fn a_hidden_detail_never_reads_agents_and_a_selection_change_empties_the_list_at_once() {
    let repo = Arc::new(AgentsRepo::new(vec![session_agent(
        1,
        AgentStatus::Blocked,
    )]));
    let controller = selected_with_agents(&repo).await;
    assert_eq!(repo.calls(), 1);

    controller.select("other");
    assert!(
        controller.agents.get_untracked().is_empty(),
        "cleared on the change"
    );
    tick(&controller).await;
    assert_eq!(repo.calls(), 1, "a session without agents is never asked");

    controller.select(AGENTS_SESSION);
    controller.set_detail_visible(false);
    tick(&controller).await;
    assert_eq!(repo.calls(), 1, "a Compact window pays for nothing");
    assert!(controller.agents.get_untracked().is_empty());
}

#[tokio::test]
async fn a_stalled_agents_read_does_not_delay_the_next_list_poll() {
    let repo = Arc::new(AgentsRepo::new(vec![session_agent(
        1,
        AgentStatus::Blocked,
    )]));
    let controller = selected_with_agents(&repo).await;
    let (calls, lists) = (repo.calls(), repo.lists());

    repo.stall.store(true, Ordering::SeqCst);
    assert!(controller.take_poll_tick(), "the poll is armed");
    let first = controller.refresh_from_poll();
    let second = async {
        while repo.calls() == calls {
            tokio::task::yield_now().await;
        }
        assert!(
            controller.take_poll_tick(),
            "the list poll's claim is free while the agents read hangs"
        );
        controller.refresh_from_poll().await;
        assert_eq!(repo.lists(), lists + 2, "the second list refresh ran");
        assert_eq!(
            repo.calls(),
            calls + 1,
            "the hung agents read is single-flight: no second one stacked"
        );
        repo.release.notify_one();
    };
    tokio::join!(first, second);

    tick(&controller).await;
    assert_eq!(
        repo.calls(),
        calls + 2,
        "the claim released: agents read again"
    );
}

#[test]
fn a_visible_edge_with_a_selected_session_fetches_the_agents_without_a_tick() {
    use frust_reactive::ReactiveRuntime;

    let _g = serial();
    let _ambient = setup();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let repo = Arc::new(AgentsRepo::new(vec![session_agent(
        1,
        AgentStatus::Blocked,
    )]));
    let controller = Arc::new(SessionsController::new(
        Arc::clone(&repo) as SharedSessionsRepository
    ));
    runtime.block_on(async {
        controller.refresh().await;
        controller.select(AGENTS_SESSION);
    });
    assert_eq!(repo.calls(), 0, "hidden: nothing read yet");

    controller.set_detail_visible(true);
    let pump = ReactiveRuntime::get().expect("runtime installed by setup()");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while repo.calls() == 0 {
        assert!(Instant::now() < deadline, "the edge never fetched");
        pump.pump_local();
        runtime.block_on(tokio::task::yield_now());
    }
    while agent_rows(&controller).is_empty() {
        assert!(Instant::now() < deadline, "the rows never landed");
        pump.pump_local();
        runtime.block_on(tokio::task::yield_now());
    }
    assert_eq!(agent_rows(&controller), [(1, AgentStatus::Blocked)]);

    controller.set_detail_visible(true);
    assert_eq!(repo.calls(), 1, "a repeated value is not an edge");
    controller.set_detail_visible(false);
    assert!(
        controller.agents.get_untracked().is_empty(),
        "the hidden edge clears the list"
    );
}

// ---------------------------------------------------------------------------
// 11 — the session-poll cadence is read live, not loaded once per mount:
// `resolve_poll_interval` is the one seam both `pages::spawn_cadence_loop`
// instances (the session-list poll and the ping heartbeat) call before every
// sleep, so proving *it* re-reads live proves both loops do — no widget tree
// needed, same "controller level" shape section 10 uses.
// ---------------------------------------------------------------------------

/// A [`PreferencesRepository`] whose every method fails — what
/// [`a_pref_read_failure_falls_back_to_the_default_without_killing_the_loop`]
/// drives [`resolve_poll_interval`] against.
struct FailingPreferencesRepo;

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl PreferencesRepository for FailingPreferencesRepo {
    async fn load_preferences(&self) -> Result<Preferences, AppFailure> {
        Err(AppFailure::Storage("simulated read failure".to_string()))
    }

    async fn save_appearance(
        &self,
        _prefs: AppearancePrefs,
    ) -> Result<AppearancePrefs, AppFailure> {
        unreachable!("not exercised by the cadence tests")
    }

    async fn save_poll_prefs(&self, _prefs: PollPrefs) -> Result<PollPrefs, AppFailure> {
        unreachable!("not exercised by the cadence tests")
    }
}

/// The shipped default (`layout_refresh_secs: 3`) resolves to the 3-second
/// pin every existing device-gate expectation assumes — unchanged by the
/// live-re-read rework.
#[tokio::test]
async fn the_default_pref_resolves_to_the_three_second_pin() {
    let preferences: SharedPreferencesRepository = Arc::new(MockPreferencesRepository::seeded());

    let interval = resolve_poll_interval(&preferences).await;

    assert_eq!(interval, Duration::from_secs(u64::from(POLL_PERIOD_SECS)));
}

/// Changing the pref mid-mount changes the *next* scheduled interval — for
/// both loops, since both re-resolve through this exact seam before every
/// sleep (see the section header).
#[tokio::test]
async fn changing_the_pref_mid_mount_changes_the_next_scheduled_interval_for_both_loops() {
    let preferences: SharedPreferencesRepository = Arc::new(MockPreferencesRepository::seeded());

    let first = resolve_poll_interval(&preferences).await;
    assert_eq!(
        first,
        Duration::from_secs(3),
        "the shipped default, before any settings edit"
    );

    // The settings screen's own picker write — a live edit landing while
    // Sessions is already mounted underneath it.
    preferences
        .save_poll_prefs(PollPrefs {
            layout_refresh_secs: 10,
            bell_notifications: true,
        })
        .await
        .expect("a legal choice-set value is accepted");

    let second = resolve_poll_interval(&preferences).await;
    assert_eq!(
        second,
        Duration::from_secs(10),
        "the next resolve — the one either loop's next scheduled sleep reads — sees the live value"
    );
}

/// A preferences-repository read failure falls back to the fixed default
/// rather than propagating — the loop that scheduled this tick keeps
/// running on the next one instead of dying.
#[tokio::test]
async fn a_pref_read_failure_falls_back_to_the_default_without_killing_the_loop() {
    let preferences: SharedPreferencesRepository = Arc::new(FailingPreferencesRepo);

    let interval = resolve_poll_interval(&preferences).await;

    assert_eq!(interval, Duration::from_secs(u64::from(POLL_PERIOD_SECS)));
}

// ---------------------------------------------------------------------------
// 12 — Tablet master-detail: a card tap selects, the detail pane shows it
// ---------------------------------------------------------------------------

/// Mount at `size` (published as the window's metrics too) over `repository`,
/// then connect.
fn sized(size: Size, repository: Option<SharedSessionsRepository>) -> Harness {
    let metrics = support::metrics_at(size.width, size.height);
    let mut h =
        support::mount_with_metrics(Some(metrics), || Harness::mount_over(repository, size));
    expect_connection_controller().connect(mock_target(false));
    h.frame();
    h
}

fn count_values(h: &Harness, value: &str) -> usize {
    node_values(&h.root).iter().filter(|v| *v == value).count()
}

fn has_label(h: &Harness, label: &str) -> bool {
    node_labels(&h.root).iter().any(|l| l == label)
}

fn has_value(h: &Harness, value: &str) -> bool {
    count_values(h, value) > 0
}

/// A stat card's printed value, read from its semantics description.
fn stat_value(h: &Harness, label: &str) -> Option<String> {
    h.root
        .semantics()
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some(label) && node.description().is_some())
        .and_then(|(_, node)| node.description().map(str::to_string))
}

/// Tap the **last** node printing `value` — the detail pane sits after the
/// list column, so its copy of a repeated title is the later one.
fn tap_last_value(h: &mut Harness, value: &str) {
    let point = *node_centers_by_value(&h.root, value)
        .last()
        .unwrap_or_else(|| panic!("nothing printing {value:?}"));
    h.tap(point);
}

#[test]
fn a_tablet_card_tap_selects_it_and_the_detail_header_names_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    assert!(
        has_value(&h, "loading sessions\u{2026}") && !has_label(&h, "Select a session"),
        "the first load in flight: the column's loading line, skeletons in the detail"
    );
    h.await_sessions();
    assert!(has_label(&h, "Select a session"), "nothing selected yet");
    assert_eq!(count_values(&h, SEEDED[1]), 1, "only the card names it");

    h.tap_value(SEEDED[1]);
    h.settle();

    assert_eq!(h.depth(), 1, "a Tablet tap selects, it never attaches");
    assert_eq!(count_values(&h, SEEDED[1]), 2, "the header names it too");
    let labels = node_labels(&h.root);
    assert!(
        labels.iter().any(|l| l.contains("attach")),
        "the header's actions carry attach: {labels:?}"
    );
    assert!(!has_label(&h, "Select a session"));
    // The second seeded card is the resurrectible one.
    assert!(has_label(&h, "\u{2192} attach to resurrect"), "{labels:?}");
    assert!(has_value(&h, "saved, not running"));
    let values = node_values(&h.root);
    assert!(
        !values
            .iter()
            .any(|v| v.starts_with("tabs \u{2192} panes") || v.starts_with("active tab")),
        "a resurrectible session draws no layout card: {values:?}"
    );
}

#[test]
fn the_tabs_to_panes_card_opens_the_focused_tab_on_its_pane_pills() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();

    h.tap_value(SEEDED[0]);
    h.settle();

    assert!(has_label(&h, "\u{2192} attach"));
    assert!(has_value(&h, "tabs \u{2192} panes"));
    assert!(has_value(&h, "active tab \u{b7} main"));
    for tab in ["main", "build", "edit", "logs"] {
        assert!(has_value(&h, tab), "every tab is listed: {tab}");
    }
    assert!(has_label(&h, "focused tab"));
    assert!(has_label(&h, "float"), "the floating pane carries its tag");
    assert!(has_value(&h, "scratch"), "the focused tab's pane pills");
    assert!(
        !has_value(&h, "cargo watch") && !has_value(&h, "journalctl"),
        "only the focused tab is open"
    );
    // The counts are the layout summary's, not a wire zero.
    assert_eq!(stat_value(&h, "tabs").as_deref(), Some("4"));
    assert_eq!(stat_value(&h, "panes").as_deref(), Some("9"));
    assert_eq!(stat_value(&h, "clients").as_deref(), Some("0"));
    assert_eq!(stat_value(&h, "age").as_deref(), Some("3 hours ago"));

    // A pane pill pushes the terminal route carrying its tab and pane id.
    tap_last_value(&mut h, "scratch");
    h.settle();
    assert_eq!(h.depth(), 2, "a pane pill pushes the terminal route");
    let nav = muxr::routes::app_nav();
    let param = |key: &str| nav.param(key);
    assert_eq!(
        param(muxr::routes::TERMINAL_SESSION_QUERY_KEY).as_deref(),
        Some(SEEDED_DEV_ID)
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_TAB_QUERY_KEY).as_deref(),
        Some("0")
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_PANE_QUERY_KEY).as_deref(),
        Some("2")
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_SPACE_QUERY_KEY),
        None,
        "a pane pill names no space"
    );
}

#[test]
fn the_tablet_detail_lists_the_seeded_agents_in_priority_order_and_a_row_tap_deep_links() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();

    h.tap_value(SEEDED[2]);
    h.settle();
    h.pump_until("the agent list", |h| has_value(h, "AI AGENTS \u{b7} 4"));

    let values = node_values(&h.root);
    let at = |wanted: &str| values.iter().position(|v| v == wanted);
    let order: Vec<_> = [
        "reviewer \u{b7} claude",
        "planner \u{b7} codex",
        "scout \u{b7} gemini",
        "notes \u{b7} claude",
    ]
    .map(|row| at(row).unwrap_or_else(|| panic!("no row {row:?} in {values:?}")))
    .into();
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "blocked, done, working, idle: {values:?}"
    );
    assert!(
        at("AI AGENTS \u{b7} 4") < at("tabs \u{2192} panes \u{b7} space home"),
        "the agents sit above the tabs card"
    );
    assert!(has_value(&h, "home \u{203a} work \u{203a} p0"));

    tap_last_value(&mut h, "reviewer \u{b7} claude");
    h.settle();
    assert_eq!(h.depth(), 2, "an agent row pushes the terminal route");
    let nav = muxr::routes::app_nav();
    let param = |key: &str| nav.param(key);
    assert_eq!(
        param(muxr::routes::TERMINAL_SESSION_QUERY_KEY).as_deref(),
        Some(SEEDED_HERDR_ID)
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_SPACE_QUERY_KEY).as_deref(),
        Some("home")
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_TAB_QUERY_KEY).as_deref(),
        Some("0")
    );
    assert_eq!(
        param(muxr::routes::TERMINAL_PANE_QUERY_KEY).as_deref(),
        Some("0")
    );
}

#[test]
fn a_session_without_agents_draws_no_agent_section() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();

    h.tap_value(SEEDED[0]);
    h.settle();

    assert!(
        !node_values(&h.root)
            .iter()
            .any(|v| v.starts_with("AI AGENTS"))
    );
}

#[test]
fn the_herdr_detail_names_its_space_on_the_collapsed_band() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(COLLAPSED_TABLET, None);
    h.await_sessions();

    h.tap_value(SEEDED[2]);
    h.settle();

    assert_eq!(h.depth(), 1);
    assert!(has_value(&h, "space \u{b7} home \u{b7} tab \u{b7} work"));
    assert!(has_value(&h, "tabs \u{2192} panes \u{b7} space home"));
    assert_eq!(stat_value(&h, "spaces").as_deref(), Some("5"));
}

/// Fails its first `list_sessions`, then answers one session — and counts.
struct FailFirstRepo {
    calls: AtomicU32,
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for FailFirstRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(AppFailure::Network("first load".to_string()));
        }
        Ok(vec![stub_session("alpha")])
    }

    async fn create_session(&self, _params: CreateSessionParams) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the retry test")
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the retry test")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the retry test")
    }
}

#[test]
fn a_failed_first_load_offers_a_retry_that_reruns_it() {
    let _g = serial();
    let _ambient = setup();
    let repo = Arc::new(FailFirstRepo {
        calls: AtomicU32::new(0),
    });
    let mut h = sized(
        FULL_TABLET,
        Some(Arc::clone(&repo) as SharedSessionsRepository),
    );
    h.pump_until("the failed state", |h| {
        has_label(h, "Couldn't load sessions")
    });
    assert!(has_label(&h, "Retry"));
    assert!(
        has_label(&h, "Select a session"),
        "the detail shows no selection"
    );
    assert_eq!(repo.calls.load(Ordering::SeqCst), 1);

    h.tap_label("Retry");
    h.pump_until_values("the retried load", |values| {
        values.iter().any(|v| v == "alpha")
    });

    assert_eq!(
        repo.calls.load(Ordering::SeqCst),
        2,
        "Retry re-ran the load"
    );
    assert!(!has_label(&h, "Couldn't load sessions"));
}

/// Ed's iPad gate (2026-10-01): the selected card painted as a solid amber
/// block because its translucent `amber_faint` fill sat straight over the
/// accent-filled outer box. The fill is now the palette's opaque `amber_wash`,
/// so the 2px strip is all the solid amber a selected card shows.
#[test]
fn a_selected_tablet_card_fills_with_the_opaque_amber_wash_not_a_translucent_tint() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();
    h.tap_value(SEEDED[1]);
    let scene = h.settle();
    let colors = muxr::ui::palette::Palette::from_theme(&frust_glyph::baseline());

    let fills = &scene.rounded_fills;
    let area = |s: &Size| s.width * s.height;
    // The accent box is the largest amber rounded rect (the attach button is amber too, but small).
    let (accent_idx, accent_origin, accent_size) = fills
        .iter()
        .enumerate()
        .filter(|(_, (_, _, _, c))| *c == colors.amber)
        .map(|(i, (o, s, _, _))| (i, *o, *s))
        .max_by(|a, b| area(&a.2).partial_cmp(&area(&b.2)).unwrap())
        .expect("the selected card's accent box");
    let inset_like = |o: &Point, s: &Size| {
        (o.x - (accent_origin.x + 2.0)).abs() < EPSILON
            && (o.y - accent_origin.y).abs() < EPSILON
            && (s.height - accent_size.height).abs() < EPSILON
    };
    let wash_idx = fills
        .iter()
        .position(|(o, s, _, c)| *c == colors.amber_wash && inset_like(o, s))
        .expect("the opaque amber wash inset by the strip");
    assert!(accent_idx < wash_idx, "the wash paints over the accent box");
    assert!(
        !fills
            .iter()
            .any(|(o, s, _, c)| *c == colors.amber_faint && inset_like(o, s)),
        "no translucent tint rides directly on the accent box any more"
    );
}

#[test]
fn read_only_on_tablet_chips_the_strip_and_leaves_only_attach_working() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();
    expect_connection_controller().is_read_only.set(true);
    h.frame();
    assert!(has_label(&h, "read-only"), "the strip's read-only chip");

    h.tap_value(SEEDED[0]);
    h.settle();
    let toasts = muxr::ui::toasts::app_toasts();
    toasts.clear();
    for action in ["rename", "kill"] {
        h.tap_label(action);
        h.settle();
        assert_eq!(h.depth(), 1, "a read-only {action} opens nothing");
    }
    let raised = toasts.pending_untracked();
    assert_eq!(raised.len(), 2, "each refusal says why: {raised:?}");
    assert!(raised.iter().all(|t| t.starts_with("Read-only session")));

    h.tap_label("\u{2192} attach");
    h.settle();
    assert_eq!(h.depth(), 2, "attach is never read-only-gated");
}

#[test]
fn the_detail_headers_rename_opens_the_shared_dialog() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();
    h.tap_value(SEEDED[0]);
    h.settle();

    h.tap_label("rename");
    h.settle();

    assert_eq!(h.depth(), 2, "the rename dialog is up");
}

#[test]
fn renaming_the_selected_session_from_the_detail_keeps_the_detail_on_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(FULL_TABLET, None);
    h.await_sessions();
    h.tap_value(SEEDED[0]);
    h.settle();

    let before = h.frame();
    h.tap_label("rename");
    let after = h.settle();
    let dialog = Chrome::diff(&before, &after);
    let field = dialog.content_child();
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
    h.settle();

    assert!(
        !has_label(&h, "Select a session"),
        "the rename renamed the session's id too; the selection followed it"
    );
    assert_eq!(
        count_values(&h, "dev-2"),
        2,
        "the list card and the detail header both show the renamed session"
    );
}

#[test]
fn read_only_adds_the_strip_chip_on_compact_too() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    assert!(!has_label(&h, "read-only"));

    expect_connection_controller().is_read_only.set(true);
    h.frame();

    assert!(has_label(&h, "read-only"));
}

#[test]
fn a_phone_card_tap_still_attaches_and_marks_this_device() {
    let _g = serial();
    let _ambient = setup();
    let mut h = sized(PHONE, None);
    h.await_sessions();
    assert!(
        !has_label(&h, "Select a session"),
        "no detail pane on a phone"
    );
    assert_eq!(count_values(&h, "\u{25cf} this device"), 1);

    h.tap_value(SEEDED[0]);
    h.settle();

    assert_eq!(h.depth(), 2, "a phone tap attaches");
}

// ---------------------------------------------------------------------------
// 13 — the new-session dialog's backend choice
// ---------------------------------------------------------------------------

/// A repository that serves one card and records every create it is asked for.
struct RecordingCreateRepo {
    created: Mutex<Vec<CreateSessionParams>>,
}

#[cfg_attr(not(target_arch = "wasm32"), clean_signals::async_trait)]
#[cfg_attr(target_arch = "wasm32", clean_signals::async_trait(?Send))]
impl SessionsRepository for RecordingCreateRepo {
    async fn list_sessions(&self) -> Result<Vec<Session>, AppFailure> {
        let mut all = vec![stub_session("alpha")];
        all.extend(
            self.created
                .lock()
                .unwrap()
                .iter()
                .map(|params| stub_session(&params.name)),
        );
        Ok(all)
    }

    async fn create_session(&self, params: CreateSessionParams) -> Result<Session, AppFailure> {
        let session = stub_session(&params.name);
        self.created.lock().unwrap().push(params);
        Ok(session)
    }

    async fn rename_session(&self, _request: RenameRequest) -> Result<Session, AppFailure> {
        unreachable!("not exercised by the backend-choice tests")
    }

    async fn kill_session(&self, _id: String) -> Result<(), AppFailure> {
        unreachable!("not exercised by the backend-choice tests")
    }
}

/// Mount over a recording repository, connect offering `backends`, and wait
/// for the one card to land.
fn offering(backends: Vec<ServerBackend>) -> (Harness, Arc<RecordingCreateRepo>) {
    let repo = Arc::new(RecordingCreateRepo {
        created: Mutex::new(Vec::new()),
    });
    let mut h = sized(
        FULL_TABLET,
        Some(Arc::clone(&repo) as SharedSessionsRepository),
    );
    let connection = expect_connection_controller();
    let mut info = connection
        .server_info
        .get_untracked()
        .expect("the mock connect published server info");
    info.available_backends = backends;
    connection.server_info.set(Some(info));
    h.pump_until_values("the seeded card", |values| {
        values.iter().any(|v| v == "alpha")
    });
    (h, repo)
}

/// Type `name` into the open dialog's field and submit with Enter.
fn create_named(h: &mut Harness, dialog: &Chrome, name: &str) {
    h.tap(center(dialog.content_child()));
    h.type_text(name);
    h.press(NamedKey::Enter);
    h.flush();
    h.pump_until_values("the created card", |values| {
        values.iter().any(|v| v == name)
    });
}

#[test]
fn two_offered_backends_show_the_choice_and_herdr_reaches_the_repository() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, repo) = offering(vec![ServerBackend::Zellij, ServerBackend::Herdr]);

    let dialog = open_dialog_by_value(&mut h, "new session");
    assert!(
        has_label(&h, "zellij") && has_label(&h, "herdr"),
        "the segmented control is mounted"
    );

    // The segment nodes carry no bounds of their own, and the control packs
    // its segments to the left: the selected pill (the first segment) is
    // painted as the panel's last rounded rect, so "herdr" sits just past it.
    let (pill_origin, pill_size) = *dialog.rounded.last().expect("the selected segment's pill");
    let y = node_centers_by_label(&h.root, "herdr")[0].y;
    h.tap(Point::new(pill_origin.x + pill_size.width + 20.0, y));
    h.settle();
    create_named(&mut h, &dialog, "on-herdr");

    let created = repo.created.lock().unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].name, "on-herdr");
    assert_eq!(created[0].backend, SessionBackend::Herdr);
}

#[test]
fn two_offered_backends_default_to_zellij() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, repo) = offering(vec![ServerBackend::Herdr, ServerBackend::Zellij]);

    let dialog = open_dialog_by_value(&mut h, "new session");
    create_named(&mut h, &dialog, "untouched");

    assert_eq!(
        repo.created.lock().unwrap()[0].backend,
        SessionBackend::Zellij
    );
}

#[test]
fn a_lone_backend_mounts_no_choice_and_creates_on_it() {
    let _g = serial();
    let _ambient = setup();
    let (mut h, repo) = offering(vec![ServerBackend::Zellij]);

    let dialog = open_dialog_by_value(&mut h, "new session");
    assert!(!has_label(&h, "herdr"), "no segmented control");
    create_named(&mut h, &dialog, "plain");

    assert_eq!(
        repo.created.lock().unwrap()[0].backend,
        SessionBackend::Zellij
    );
}

#[test]
fn a_blank_name_creates_nothing_with_or_without_the_choice() {
    let _g = serial();
    let _ambient = setup();
    for backends in [
        vec![ServerBackend::Zellij],
        vec![ServerBackend::Zellij, ServerBackend::Herdr],
    ] {
        let (mut h, repo) = offering(backends);
        let dialog = open_dialog_by_value(&mut h, "new session");
        h.tap(center(dialog.content_child()));
        h.type_text("   ");
        h.press(NamedKey::Enter);
        h.tap_label("Create");
        h.flush();
        assert_eq!(h.depth(), 2, "a blank name leaves the dialog open");
        assert!(repo.created.lock().unwrap().is_empty());
    }
}

#[test]
fn the_seeded_herdr_summary_is_the_rollup_of_the_terminal_mocks_default_agents() {
    use muxr::features::terminal::data::repositories::mock_terminal_repository::default_agents_for_preview;
    let rollup = AgentSummary::from_statuses(
        default_agents_for_preview()
            .iter()
            .map(|agent| (agent.status, agent.kind.as_str())),
    );
    let sessions = MockSessionsRepository::seeded().snapshot();
    let herdr = sessions
        .iter()
        .find(|session| session.id == SEEDED_HERDR_ID)
        .expect("herdr");
    assert_eq!(herdr.agents.as_ref(), Some(&rollup));
}
