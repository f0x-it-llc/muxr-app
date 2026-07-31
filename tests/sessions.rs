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
//! stays reachable by semantics label (buttons, badges, alerts) and by text
//! value (every run), while the dialogs/sheet/route pushes the screen makes are
//! still exercised **end to end** through a real navigator — located, like
//! `ui_substrate.rs`'s, by painted geometry.
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
//! [`attach_pushes_the_terminal_route_and_the_guard_still_applies`] exercises).
//!
//! # Async
//!
//! Every repository call is real async work spawned onto the UI-thread queue
//! (see `data`'s mock latency), so a mutation is only visible after pumping —
//! [`Harness::pump_until_values`] is the deadline-bounded wait every test uses
//! instead of a fixed frame count.

use std::time::{Duration, Instant};

use frust::{AnyView, Component, NavigatorController, SizedBox, Stack, any, navigator};
use frust_core::{NamedKey, PointerPhase, RenderRoot};
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::sessions::presentation::pages::sessions_page;
use muxr::routes::install_app_router;
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, by_width, center, char_key, frame, largest, named_key,
    node_centers_by_label, node_labels, node_values, pointer, pump_frames, serial, settle, setup,
    smallest, tap,
};

/// A point in the top-left corner — scrim on every overlay this screen pushes
/// (the dialog centers its panel, the sheet anchors its own to the bottom).
const SCRIM_POINT: Point = Point::new(4.0, 4.0);
/// The panel gutter `glyph::dialog` and `ui::sheet` share, transcribed — a
/// full-bleed child of either is laid out at the panel width minus twice this.
const PANEL_PADDING: f64 = 20.0;
/// `glyph::list`'s leading glyph box edge, transcribed: one 32×32 rounded rect
/// per picker row, which is how a test counts the rows a sheet listed.
const LIST_GLYPH_BOX: f64 = 32.0;
/// How long a pumped wait may run before the test fails rather than hangs.
const PUMP_DEADLINE: Duration = Duration::from_secs(5);

/// The seeded session names `data`'s mock repository ships, in order.
const SEEDED: [&str; 4] = ["dev", "jumping-peach", "herdr", "minefit-api"];

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

    /// Tap the `index`-th node labelled `label` (0-based, in pass order).
    fn tap_label_at(&mut self, label: &str, index: usize) {
        let centers = node_centers_by_label(&self.root, label);
        let point = *centers.get(index).unwrap_or_else(|| {
            panic!(
                "no node #{index} labelled {label:?}; labels on screen: {:?}",
                node_labels(&self.root)
            )
        });
        self.tap(point);
    }

    /// Tap the first node labelled `label`.
    fn tap_label(&mut self, label: &str) {
        self.tap_label_at(label, 0);
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

    /// How many session cards are on screen — counted by their per-card
    /// Attach affordance, so the count never depends on which *names* a test
    /// happens to have created.
    fn card_count(&self) -> usize {
        node_centers_by_label(&self.root, "Attach").len()
    }

    /// The navigator's page-stack depth (1 = only the harness's empty base
    /// page).
    fn depth(&self) -> usize {
        self.nav.depth()
    }
}

/// Open a dialog by tapping `label`, then return the rects the dialog itself
/// added to the frame.
fn open_dialog(h: &mut Harness, label: &str) -> DialogChrome {
    let before = h.frame();
    h.tap_label(label);
    let after = h.settle();
    DialogChrome {
        rounded: added(&before.rounded, &after.rounded),
        strokes: added(&before.strokes, &after.strokes),
    }
}

/// The chrome a pushed overlay added to the frame — the screen underneath is
/// diffed out (see [`added`]).
struct DialogChrome {
    rounded: Vec<(Point, Size)>,
    strokes: Vec<(Point, Size)>,
}

impl DialogChrome {
    /// The overlay's own panel — the biggest rect it painted.
    fn panel(&self) -> (Point, Size) {
        largest(&self.rounded).expect("an overlay always paints its own panel")
    }

    /// The overlay's full-content-width child: a dialog's name field, a
    /// sheet's body (see [`PANEL_PADDING`]).
    fn content_child(&self) -> (Point, Size) {
        let content_w = self.panel().1.width - 2.0 * PANEL_PADDING;
        by_width(&self.rounded, content_w)
            .unwrap_or_else(|| panic!("no full-width child in {:?}", self.rounded))
    }
}

// ---------------------------------------------------------------------------
// 1 — the list
// ---------------------------------------------------------------------------

#[test]
fn the_seeded_sessions_render_as_cards() {
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
    // The layout summary each card carries (the redesign's `.sess-meta` row).
    assert!(
        values
            .iter()
            .any(|v| v.contains("9 panes") && v.contains("4 tabs")),
        "a card prints its tab/pane counts: {values:?}"
    );
    assert!(
        values.iter().any(|v| v.contains("created 2026-07-02")),
        "…and its creation label: {values:?}"
    );

    let labels = node_labels(&h.root);
    assert_eq!(
        node_centers_by_label(&h.root, "Attach").len(),
        SEEDED.len(),
        "every card carries its own attach affordance: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == "running") && labels.iter().any(|l| l == "resurrectible"),
        "both status chips are reachable from the seed: {labels:?}"
    );
    // The server-info strip, read off the mock connection.
    assert!(
        values.iter().any(|v| v == "mock.example:7777"),
        "the strip prints the endpoint: {values:?}"
    );
    // The TLS posture is a *badge* (a `Role::Status` node, hence a label), so
    // its hue can be variant-coded off `ServerInfo::trust_mode` — the mock
    // connects trust-on-first-use, the one posture that skips validation, and
    // the redesign's `.warn-badge` is what it earns.
    assert!(
        labels.iter().any(|l| l == "Trust on first use"),
        "…and the TLS posture badge: {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == "42 ms"),
        "…and the latency badge: {labels:?}"
    );
}

// ---------------------------------------------------------------------------
// 2 — create
// ---------------------------------------------------------------------------

#[test]
fn creating_a_session_round_trips_into_a_new_card() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    assert_eq!(h.card_count(), SEEDED.len());

    // The app bar's `+`, not the list's "+ new session" row: the seeded list is
    // taller than this 800×600 viewport, so that row is below the fold (it is
    // the same action either way — see `pages::open_create_dialog`'s callers,
    // and the empty-state test, which taps the row itself).
    let dialog = open_dialog(&mut h, "+");
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

// ---------------------------------------------------------------------------
// 3 — rename
// ---------------------------------------------------------------------------

#[test]
fn renaming_a_session_updates_its_card() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let dialog = open_dialog(&mut h, "Rename");
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

// ---------------------------------------------------------------------------
// 4 — kill
// ---------------------------------------------------------------------------

#[test]
fn killing_a_session_confirms_first_then_removes_the_card() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    // A dismissed confirmation kills nothing.
    open_dialog(&mut h, "Kill");
    h.tap(SCRIM_POINT);
    h.flush();
    pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 4);
    assert_eq!(h.depth(), 1, "the scrim tap closed the dialog");
    assert_eq!(h.card_count(), SEEDED.len(), "…and nothing was killed");

    // Confirming does. The danger action is the smallest border the dialog
    // painted (the panel's own being the other one).
    let dialog = open_dialog(&mut h, "Kill");
    let confirm = smallest(&dialog.strokes).expect("the danger action strokes its border");
    h.tap(center(confirm));
    h.flush();

    h.pump_until_values("the killed card to disappear", |values| {
        !values.iter().any(|v| v == "dev")
    });
    assert_eq!(h.depth(), 1);
    assert_eq!(h.card_count(), SEEDED.len() - 1);
}

// ---------------------------------------------------------------------------
// 5 — the empty state
// ---------------------------------------------------------------------------

#[test]
fn killing_every_session_shows_the_empty_state() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    for remaining in (1..=SEEDED.len()).rev() {
        let dialog = open_dialog(&mut h, "Kill");
        let confirm = smallest(&dialog.strokes).expect("the danger action strokes its border");
        h.tap(center(confirm));
        h.flush();
        h.pump_until("one fewer card", move |h| h.card_count() == remaining - 1);
    }

    let labels = node_labels(&h.root);
    assert!(
        labels.iter().any(|l| l == "No sessions"),
        "an emptied list shows the empty state: {labels:?}"
    );
    assert!(
        node_centers_by_label(&h.root, "Attach").is_empty(),
        "…and no cards"
    );

    // The empty state's own create action is now the only one in the list
    // area, and (unlike the seeded list's row) is on screen — so it is also
    // the one place this suite exercises the "+ new session" affordance.
    let dialog = open_dialog(&mut h, "+ new session");
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
// 6 — the save-credential offer
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
// 7 — the picker sheet
// ---------------------------------------------------------------------------

#[test]
fn the_picker_sheet_opens_and_lists_every_session() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let sheet = open_dialog(&mut h, "Switch");
    assert_eq!(h.depth(), 2, "the sheet is pushed as its own page");

    // One `glyph_list` row paints one 32×32 leading glyph box.
    let rows = row_boxes(&sheet.rounded);
    assert_eq!(
        rows.len(),
        SEEDED.len(),
        "the sheet lists every session: {:?}",
        sheet.rounded
    );

    // Bottom-anchored: the panel's own bottom edge overshoots the viewport by
    // one corner radius (`ui::sheet`'s top-rounded-only trick).
    let (origin, size) = sheet.panel();
    assert!(
        origin.y + size.height > support::H,
        "the sheet anchors to the bottom edge: {origin:?}/{size:?}"
    );

    h.tap(SCRIM_POINT);
    h.flush();
    assert_eq!(h.depth(), 1, "a scrim tap dismisses it");

    // Selecting a row takes the *same* attach path a card's Attach button
    // does: the sheet pops and the terminal route lands in its place, in one
    // rebuild (`open_picker` enqueues both ops together).
    let sheet = open_dialog(&mut h, "Switch");
    let first_row = row_boxes(&sheet.rounded)[0];
    h.tap(center(first_row));
    let after = h.settle();
    assert_eq!(
        h.depth(),
        2,
        "the sheet popped and /terminal took its place"
    );
    assert!(
        row_boxes(&after.rounded).is_empty(),
        "…so the sheet's own rows are gone: {:?}",
        after.rounded
    );
}

/// The 32×32 leading glyph boxes a `glyph_list` paints — one per row, which is
/// how a test counts (and addresses) the picker sheet's rows.
fn row_boxes(rects: &[(Point, Size)]) -> Vec<(Point, Size)> {
    rects
        .iter()
        .copied()
        .filter(|(_, size)| {
            (size.width - LIST_GLYPH_BOX).abs() < EPSILON
                && (size.height - LIST_GLYPH_BOX).abs() < EPSILON
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 8 — attach
// ---------------------------------------------------------------------------

/// Tap the first card's Attach affordance and report the resulting page-stack
/// depth plus the rects the pushed page added to the frame.
fn attach_outcome(connected: bool) -> (usize, usize) {
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target(false));
    h.await_sessions();
    if !connected {
        expect_connection_controller().disconnect();
        h.frame();
    }

    let before = h.frame();
    h.tap_label("Attach");
    let after = h.settle();
    (h.depth(), added(&before.rounded, &after.rounded).len())
}

#[test]
fn attach_pushes_the_terminal_route_and_the_guard_still_applies() {
    let _g = serial();
    let _ambient = setup();

    // Sequential, not interleaved: each harness re-provides the app-scoped
    // connection controller, so only the most recently mounted one is the live
    // handle the route guard reads.
    let (connected_depth, connected_rects) = attach_outcome(true);
    let (guarded_depth, guarded_rects) = attach_outcome(false);

    assert_eq!(connected_depth, 2, "attaching pushes a page");
    assert_eq!(guarded_depth, 2, "…and so does the bounce");
    // The terminal placeholder is an app bar over centered text and paints no
    // rounded chrome at all; the Connect screen the session guard bounces a
    // disconnected attach to paints its eight pieces (mark tile, badge, two
    // fields, the CTA). That gap is what identifies *which* route the push
    // resolved to, without either count being hardcoded.
    assert!(
        connected_rects < guarded_rects,
        "a connected attach lands on /terminal ({connected_rects} rects) while a \
         disconnected one is bounced to Connect ({guarded_rects} rects)"
    );
}
