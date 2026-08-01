//! Headless behavior coverage for the `/settings` screen
//! (`src/features/settings/**`) — no GPU, no window: a `RenderRoot` over the
//! real page builder, driven through `tests/support`.
//!
//! # The harness, and why the screen is *beside* a navigator rather than in one
//!
//! In production `settings_page` is a navigator page. Mounting it that way here
//! would make every assertion geometric: `NavigatorWidget` contributes no
//! `Widget::semantics` and forwards none of its pages' (the framework gap
//! `tests/ui_substrate.rs` documents), so nothing inside a navigator appears in
//! the accessibility tree at all.
//!
//! So [`Harness`] mounts the screen as the **bottom layer of a `Stack`**, with
//! an otherwise-empty navigator — driven by the *same* `NavigatorController`
//! the screen is handed — stacked over it, exactly as `tests/sessions.rs` does
//! (see that suite's own module docs for why the base page is a full-bleed,
//! childless `SizedBox`). The screen's rows therefore stay reachable by text
//! value, while the dialogs and picker sheets it pushes are exercised end to
//! end through a real navigator and located by painted geometry.
//!
//! # A taller surface than the default
//!
//! Four groups plus the Disconnect action run past 1000px, so every frame here
//! lays out at [`SURFACE`] rather than `support::surface()`'s 800×600 — a row
//! laid out below the fold would be untappable and invisible to a semantics
//! probe. That is also why this file pumps through its own
//! [`Harness::pump_once`] rather than `support::pump_frames`, which frames at
//! the default size: a pump at 600px high would silently re-layout everything
//! the next probe is about to measure. (Inlined here rather than added to
//! `tests/support`, whose additive rights belong to another task in this wave.)
//!
//! # Reading a picker sheet without semantics
//!
//! A sheet is a navigator page, so its rows are invisible to
//! `node_values`. Three painted facts stand in, and between them they pin the
//! whole of `docs/redesign.html`'s screen 6:
//!
//! - **how many rows** — the sheet's `glyph_runs` delta is `options + 1` (the
//!   title);
//! - **which one is selected** — only that row paints a fill, and it is the
//!   one rect as wide as the panel itself (`.pick-row.sel{margin:0 -20px}`
//!   bleeding exactly the sheet's own 20px gutter on each side), plus the one
//!   filled vector path the sheet adds (the `✓`);
//! - **what the rows say** — by picking one and watching the row value on the
//!   screen behind change, which is the round trip that matters anyway.
//!
//! Row *k* is addressed relative to the selected row's measured rect: every
//! `.pick-row` is one line tall, so the pitch is that rect's own height.

use std::time::{Duration, Instant};

use frust::{
    AnyView, Component, GetUntracked, NavigatorController, Resolution, SizedBox, Stack, any,
    navigator,
};
use frust_core::accesskit::Toggled;
use frust_core::{NamedKey, PointerPhase, RenderRoot};
use frust_reactive::ReactiveRuntime;
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::settings::presentation::pages::settings_page;
use muxr::routes::{app_nav, install_app_router};
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, added, by_width, center, char_key, frame_sized, largest, named_key,
    node_center_by_label, node_centers_by_label, node_centers_by_value, node_labels, node_values,
    pointer, serial, settle_sized, setup, smallest, tap,
};

/// The surface every frame here lays out at — tall enough that the whole screen
/// (four groups, the dashed create row and Disconnect) is on it at once. See
/// the [module docs](self).
const SURFACE: Size = Size::new(800.0, 1400.0);

/// A point in the top-left corner — scrim on every overlay this screen pushes
/// (the dialog centers its panel, the sheet anchors its own to the bottom).
const SCRIM_POINT: Point = Point::new(4.0, 4.0);
/// The panel gutter `glyph::dialog` and `ui::sheet` share, transcribed — a
/// full-bleed child of either is laid out at the panel width minus twice this,
/// and it is exactly what `.pick-row.sel` bleeds back out.
const PANEL_PADDING: f64 = 20.0;
/// How long a pumped wait may run before the test fails rather than hangs.
const PUMP_DEADLINE: Duration = Duration::from_secs(5);

/// The seeded token names `data`'s mock repository ships — the redesign's two
/// `.token-item`s, in order.
const SEEDED_TOKENS: [&str; 2] = ["zelli-mobile-100-71-31-57-mr3g6w5g", "token_2"];
/// The four group labels, as the screen prints them (the source's
/// `text-transform:uppercase`, applied in Rust — `text` carries no transform).
const GROUPS: [&str; 4] = [
    "CONNECTION",
    "AUTH TOKENS",
    "TERMINAL APPEARANCE",
    "SESSION POLL",
];

// ---------------------------------------------------------------------------
// Local harness
// ---------------------------------------------------------------------------

/// A mock connect target with the given TLS posture.
fn mock_target(trust_mode: TrustMode) -> ConnectTarget {
    ConnectTarget {
        host: "mock.example".to_string(),
        port: 7777,
        trust_mode,
        label: "Mock rig".to_string(),
        offer_credential: false,
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
        // feature's two repositories.
        let state = MuxrApp.init();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        // Re-point the app's one path-pushing handle (`routes::app_nav`, which
        // the screen resolves in its own `init`) at this fixture's navigator,
        // real route table and real session guard included — which is what
        // makes Disconnect's `go("/")` the app's real navigation.
        let _router = install_app_router(&nav, expect_connection_controller());
        let logic_nav = nav.clone();
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            let screen_nav = logic_nav.clone();
            let overlay_nav = logic_nav.clone();
            any(Stack(vec![
                settings_page(screen_nav),
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

    /// Mount, mock-connect with `trust_mode`, and wait for the first load.
    fn connected_with(trust_mode: TrustMode) -> Self {
        let mut harness = Harness::mount();
        expect_connection_controller().connect(mock_target(trust_mode));
        harness.await_tokens();
        harness
    }

    /// The common case: connected trust-on-first-use, like the redesign's own
    /// mock (its `insecure (plaintext)` row).
    fn connected() -> Self {
        Harness::connected_with(TrustMode::TrustOnFirstUse)
    }

    /// One frame at [`SURFACE`], on the zero clock.
    fn frame(&mut self) -> RecScene {
        frame_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
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
            SURFACE,
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

    /// Pump the UI-thread task queue and rebuild once, **at [`SURFACE`]** — see
    /// the module docs for why `support::pump_frames` is not used here.
    fn pump_once(&mut self) {
        let runtime =
            ReactiveRuntime::get().expect("pump_once: ReactiveRuntime::init must have run");
        runtime.pump_local();
        std::thread::sleep(Duration::from_millis(1));
        self.frame();
    }

    /// Pump until `done` accepts the rendered semantics values, or fail at
    /// [`PUMP_DEADLINE`] rather than hang.
    fn pump_until_values(&mut self, what: &str, done: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + PUMP_DEADLINE;
        while !done(&node_values(&self.root)) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; on screen: {:?}",
                node_values(&self.root)
            );
            self.pump_once();
        }
    }

    /// Wait for the async first load (the screen renders both seeded tokens).
    fn await_tokens(&mut self) {
        self.pump_until_values("the seeded tokens", |values| {
            SEEDED_TOKENS
                .iter()
                .all(|name| values.iter().any(|v| v == name))
        });
    }

    /// A full tap at `point`.
    fn tap(&mut self, point: Point) {
        tap(&mut self.root, &mut self.state, point);
    }

    /// Tap the first node whose printed text is `value` — how a test presses a
    /// `.set-row` (its label run) or the dashed create row.
    fn tap_value(&mut self, value: &str) {
        let point = self.center_of(value);
        self.tap(point);
    }

    /// Tap the first node *labelled* `label` (a button, the bell switch).
    fn tap_label(&mut self, label: &str) {
        let point = node_center_by_label(&self.root, label).unwrap_or_else(|| {
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

    /// The bell switch's announced state — the `Role::Switch` node
    /// `pieces`' own `switch_semantics` wrapper contributes.
    fn bell_state(&self) -> Option<Toggled> {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("bell notifications"))
            .and_then(|(_, node)| node.toggled())
    }

    /// The navigator's page-stack depth (1 = only the harness's base page).
    fn depth(&self) -> usize {
        self.nav.depth()
    }
}

/// The chrome a pushed overlay added to the frame — the screen underneath is
/// diffed out (see [`added`]).
struct Chrome {
    rounded: Vec<(Point, Size)>,
    strokes: Vec<(Point, Size)>,
    paths: Vec<(Point, Size)>,
    runs: usize,
}

impl Chrome {
    fn diff(before: &RecScene, after: &RecScene) -> Self {
        Chrome {
            rounded: added(&before.rounded, &after.rounded),
            strokes: added(&before.strokes, &after.strokes),
            paths: added(&before.paths, &after.paths),
            runs: after.glyph_runs.saturating_sub(before.glyph_runs),
        }
    }

    /// The overlay's own panel — the biggest rect it painted.
    fn panel(&self) -> (Point, Size) {
        largest(&self.rounded).expect("an overlay always paints its own panel")
    }

    /// The overlay's full-content-width child: a dialog's name field.
    fn content_child(&self) -> (Point, Size) {
        let content_w = self.panel().1.width - 2.0 * PANEL_PADDING;
        by_width(&self.rounded, content_w)
            .unwrap_or_else(|| panic!("no full-width child in {:?}", self.rounded))
    }

    /// A picker sheet's **selected** `.pick-row`: the one rect as wide as the
    /// panel itself but not the panel — `.pick-row.sel{margin:0 -20px}` bleeds
    /// the sheet's own gutter back out on both sides, and no other row paints a
    /// fill at all.
    fn selected_pick_row(&self) -> (Point, Size) {
        let (panel_origin, panel_size) = self.panel();
        let mut rows: Vec<(Point, Size)> = self
            .rounded
            .iter()
            .copied()
            .filter(|(origin, size)| {
                (size.width - panel_size.width).abs() < EPSILON && *origin != panel_origin
            })
            .collect();
        rows.sort_by(|a, b| a.0.y.total_cmp(&b.0.y));
        assert_eq!(
            rows.len(),
            1,
            "exactly one picker row fills, and it is the selected one: {rows:?}"
        );
        rows[0]
    }
}

/// Open a picker sheet by tapping the `.set-row` labelled `row`, and return
/// what the sheet itself painted.
fn open_picker(h: &mut Harness, row: &str) -> Chrome {
    let before = h.frame();
    h.tap_value(row);
    let after = h.settle();
    let sheet = Chrome::diff(&before, &after);
    assert_eq!(h.depth(), 2, "the picker is pushed as its own page");
    sheet
}

/// Take option `target` out of an open picker whose selected row measured
/// `selected_row`, given the index that row sits at.
///
/// Every `.pick-row` is one line tall, so the selected row's own height is the
/// pitch — see the [module docs](self).
fn pick(h: &mut Harness, sheet: &Chrome, selected: usize, target: usize) {
    let (origin, size) = sheet.selected_pick_row();
    let offset = target as f64 - selected as f64;
    h.tap(Point::new(
        origin.x + size.width / 2.0,
        origin.y + offset * size.height + size.height / 2.0,
    ));
    h.flush();
}

/// Open a dialog by tapping the node labelled `label`, then return the rects
/// the dialog itself added.
fn open_dialog_by_label(h: &mut Harness, label: &str) -> Chrome {
    let before = h.frame();
    h.tap_label(label);
    let after = h.settle();
    Chrome::diff(&before, &after)
}

/// …and by tapping the node printing `value` (the dashed create row).
fn open_dialog_by_value(h: &mut Harness, value: &str) -> Chrome {
    let before = h.frame();
    h.tap_value(value);
    let after = h.settle();
    Chrome::diff(&before, &after)
}

/// Confirm an open confirmation dialog: its danger action is the smallest
/// border it painted (the panel's own being the other one).
fn confirm(h: &mut Harness, dialog: &Chrome) {
    let action = smallest(&dialog.strokes).expect("the danger action strokes its border");
    h.tap(center(action));
    h.flush();
}

// ---------------------------------------------------------------------------
// 1 — the whole screen renders
// ---------------------------------------------------------------------------

#[test]
fn all_four_groups_and_the_disconnect_action_render() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    for group in GROUPS {
        assert!(
            values.iter().any(|v| v == group),
            "the {group:?} group label renders: {values:?}"
        );
    }
    // One row label per `.set-row` the redesign draws.
    for label in [
        "server url",
        "tls certificate",
        "zellij version",
        "herdr version",
        "font family",
        "font size",
        "cursor style",
        "layout refresh",
        "bell notifications",
    ] {
        assert!(
            values.iter().any(|v| v == label),
            "the {label:?} row renders: {values:?}"
        );
    }
    // The dashed create row and the Disconnect button (a real `frust::button`,
    // so a label rather than a value).
    assert!(values.iter().any(|v| v == "create new token"), "{values:?}");
    let labels = node_labels(&h.root);
    assert!(labels.iter().any(|l| l == "Disconnect"), "{labels:?}");
}

#[test]
fn no_latency_is_printed_anywhere_on_this_screen() {
    // The one deliberate omission from the mock's data set: a live round-trip
    // time belongs beside the session list it gates (`/sessions`' own strip),
    // not repeated here as a second, differently-stale copy.
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    for value in &values {
        assert!(
            !value.contains("ms"),
            "a latency reading leaked onto Settings: {value:?} in {values:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2 — the `connection` group tracks `ServerInfo`
// ---------------------------------------------------------------------------

#[test]
fn the_connection_group_prints_the_live_servers_facts() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    assert!(
        values.iter().any(|v| v == "mock.example:7777"),
        "the endpoint is the connected one: {values:?}"
    );
    assert!(
        values.iter().any(|v| v == "0.44.3"),
        "the zellij row prints the version without the product name: {values:?}"
    );
    assert!(
        values.iter().any(|v| v == "herdr-wire-v14"),
        "…and the herdr wire version: {values:?}"
    );
}

#[test]
fn the_tls_row_warns_only_for_the_posture_that_skips_validation() {
    let _g = serial();
    let _ambient = setup();

    // Sequential, not interleaved: each harness re-provides the app-scoped
    // connection controller, so only the most recently mounted one is live.
    let insecure = Harness::connected_with(TrustMode::TrustOnFirstUse);
    let values = node_values(&insecure.root);
    assert!(
        values.iter().any(|v| v == "insecure (plaintext)"),
        "trust-on-first-use is named by its consequence: {values:?}"
    );
    drop(insecure);

    let validated = Harness::connected_with(TrustMode::System);
    let values = node_values(&validated.root);
    assert!(
        values.iter().any(|v| v == "public CA"),
        "a validated posture reads as one: {values:?}"
    );
    assert!(
        !values.iter().any(|v| v == "insecure (plaintext)"),
        "…and never as the insecure one: {values:?}"
    );
}

#[test]
fn a_disconnected_settings_screen_still_renders_every_row() {
    // `/settings` is deliberately unguarded, so this is reachable — and a
    // blank connection group would be the worse failure.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    h.await_tokens();

    let values = node_values(&h.root);
    assert!(
        values.iter().filter(|v| *v == "not connected").count() >= 2,
        "the endpoint and TLS rows both say so: {values:?}"
    );
    assert!(
        values.iter().any(|v| v == "server url"),
        "every row still renders: {values:?}"
    );
}

#[test]
fn tapping_a_connection_row_says_it_is_not_wired_yet() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    app_toasts().clear();

    h.tap_value("server url");
    h.frame();

    let pending = app_toasts().pending_untracked();
    assert!(
        pending.iter().any(|m| m.contains("Not yet available")),
        "the row is honest rather than dead: {pending:?}"
    );
    assert_eq!(h.depth(), 1, "…and it pushed nothing");
}

// ---------------------------------------------------------------------------
// 3 — auth tokens
// ---------------------------------------------------------------------------

#[test]
fn the_token_group_is_the_redesigns_two_tokens_verbatim() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::connected();

    let values = node_values(&h.root);
    for name in SEEDED_TOKENS {
        assert!(values.iter().any(|v| v == name), "{name:?}: {values:?}");
    }
    for meta in ["created 2026-07-02", "created 2026-07-10"] {
        assert!(values.iter().any(|v| v == meta), "{meta:?}: {values:?}");
    }
    // Both scope chips, which is what the redesign's own note is about
    // ("token scope is now a visible chip on every token, not just some").
    assert!(
        values.iter().any(|v| v == "read-write") && values.iter().any(|v| v == "read-only"),
        "both scope chips render: {values:?}"
    );
    // One `revoke` action per token.
    assert_eq!(
        node_centers_by_label(&h.root, "revoke").len(),
        SEEDED_TOKENS.len()
    );
}

#[test]
fn revoking_a_token_confirms_first_then_removes_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    // A dismissed confirmation revokes nothing.
    open_dialog_by_label(&mut h, "revoke");
    assert_eq!(h.depth(), 2, "the confirmation is its own page");
    h.tap(SCRIM_POINT);
    h.flush();
    for _ in 0..4 {
        h.pump_once();
    }
    assert_eq!(h.depth(), 1, "the scrim tap closed it");
    assert!(
        node_values(&h.root).iter().any(|v| v == SEEDED_TOKENS[0]),
        "…and nothing was revoked"
    );

    // Confirming does.
    let dialog = open_dialog_by_label(&mut h, "revoke");
    confirm(&mut h, &dialog);
    h.pump_until_values("the revoked token to disappear", |values| {
        !values.iter().any(|v| v == SEEDED_TOKENS[0])
    });
    assert_eq!(h.depth(), 1);
    assert!(
        node_values(&h.root).iter().any(|v| v == SEEDED_TOKENS[1]),
        "…and only the one token went"
    );
}

/// Open the dashed create row's dialog, type `name` into its field and submit.
fn mint_token(h: &mut Harness, name: &str) {
    let dialog = open_dialog_by_value(h, "create new token");
    assert_eq!(h.depth(), 2, "the dialog is pushed as its own page");
    h.tap(center(dialog.content_child()));
    h.type_text(name);
    h.press(NamedKey::Enter);
    h.flush();
}

#[test]
fn the_dashed_row_mints_a_token() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    app_toasts().clear();

    mint_token(&mut h, "laptop");

    h.pump_until_values("the minted token", |values| {
        values.iter().any(|v| v == "laptop")
    });
    assert_eq!(h.depth(), 1, "the dialog closed behind it");
    let values = node_values(&h.root);
    // Appended, not replacing: both seeded tokens are still there…
    for name in SEEDED_TOKENS {
        assert!(values.iter().any(|v| v == name), "{name:?}: {values:?}");
    }
    // …and the new one carries the mock's own mint date and full scope.
    assert!(
        values.iter().any(|v| v == "created 2026-07-31"),
        "the minted token is dated today (mock): {values:?}"
    );
    assert_eq!(
        values.iter().filter(|v| *v == "read-write").count(),
        2,
        "a minted token is read-write: {values:?}"
    );
    let pending = app_toasts().pending_untracked();
    assert!(
        pending.iter().any(|m| m == "Created laptop"),
        "…and the mint is confirmed: {pending:?}"
    );
}

#[test]
fn a_rejected_mint_never_claims_to_have_created_anything() {
    // The mock rejects a duplicate name, which is the one reachable failure on
    // this screen — and the confirmation toast must not contradict the failure
    // toast raised for the same call.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    app_toasts().clear();

    mint_token(&mut h, SEEDED_TOKENS[1]);
    // The mock's own 20ms latency outlasts a handful of pumps, so wait for the
    // call to land rather than counting frames — what lands is the *failure*
    // toast the screen's `use_failure_listener` bridge raises.
    let deadline = Instant::now() + PUMP_DEADLINE;
    while app_toasts().pending_untracked().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the rejected mint surfaced nothing at all"
        );
        h.pump_once();
    }

    let pending = app_toasts().pending_untracked();
    assert!(
        !pending.iter().any(|m| m.starts_with("Created")),
        "a rejected mint raises no confirmation: {pending:?}"
    );
    // And the list is untouched: still exactly the two seeded tokens.
    let values = node_values(&h.root);
    assert_eq!(
        values.iter().filter(|v| *v == "created 2026-07-31").count(),
        0,
        "nothing was minted: {values:?}"
    );
}

// ---------------------------------------------------------------------------
// 4 — the picker sheets (screen 6)
// ---------------------------------------------------------------------------

/// Every picker the screen offers: the row that opens it, how many options it
/// lists, which index is selected out of the box, and what picking the last
/// option should leave in the row's value.
const PICKERS: [(&str, usize, usize, &str); 4] = [
    ("font family", 4, 0, "JetBrains Mono"),
    ("font size", 5, 2, "14px"),
    ("cursor style", 3, 0, "bar"),
    ("layout refresh", 4, 1, "every 10s"),
];

#[test]
fn every_picker_opens_with_its_own_options_and_the_stored_one_checked() {
    let _g = serial();
    let _ambient = setup();

    for (row, options, selected, _) in PICKERS {
        let mut h = Harness::connected();
        let sheet = open_picker(&mut h, row);

        assert_eq!(
            sheet.runs,
            options + 1,
            "{row}: one run per option plus the title"
        );
        assert_eq!(
            sheet.paths.len(),
            1,
            "{row}: exactly one row carries the check vector"
        );

        // The selected row fills amber **past** the sheet's own gutter, which
        // is screen 6's whole "full-bleed" look — and it is the row the stored
        // value sits at.
        let (origin, size) = sheet.selected_pick_row();
        let (panel_origin, panel_size) = sheet.panel();
        assert!(
            (origin.x - panel_origin.x).abs() < EPSILON,
            "{row}: the fill starts at the panel's own edge: {origin:?} vs {panel_origin:?}"
        );
        assert!(
            (size.width - panel_size.width).abs() < EPSILON,
            "{row}: …and spans its whole width"
        );
        // Row 0's top edge is the same distance above the selected row for
        // every picker, so a wrong `selected` shows up as a shifted fill.
        let first_row_top = origin.y - selected as f64 * size.height;
        assert!(
            first_row_top > panel_origin.y,
            "{row}: the first row sits inside the panel: {first_row_top} vs {panel_origin:?}"
        );

        // A scrim tap closes it and changes nothing.
        h.tap(SCRIM_POINT);
        h.flush();
        assert_eq!(h.depth(), 1, "{row}: dismissed");
    }
}

#[test]
fn picking_an_option_persists_it_and_updates_the_row() {
    let _g = serial();
    let _ambient = setup();

    for (row, options, selected, expected) in PICKERS {
        let mut h = Harness::connected();
        let sheet = open_picker(&mut h, row);

        pick(&mut h, &sheet, selected, options - 1);

        h.pump_until_values(expected, |values| values.iter().any(|v| v == expected));
        assert_eq!(h.depth(), 1, "{row}: the sheet closed behind the choice");
    }
}

#[test]
fn a_picked_value_is_still_checked_when_the_sheet_is_reopened() {
    // The round trip that proves the choice reached the repository rather than
    // just the row: the reopened sheet's check has moved to the new row.
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    let first = open_picker(&mut h, "cursor style");
    let (before_origin, row_height) = first.selected_pick_row();
    pick(&mut h, &first, 0, 2);
    h.pump_until_values("bar", |values| values.iter().any(|v| v == "bar"));

    let second = open_picker(&mut h, "cursor style");
    let (after_origin, _) = second.selected_pick_row();
    let moved = after_origin.y - before_origin.y;
    assert!(
        (moved - 2.0 * row_height.height).abs() < EPSILON,
        "the check moved two rows down: {moved} vs {}",
        2.0 * row_height.height
    );
}

// ---------------------------------------------------------------------------
// 5 — the bell switch
// ---------------------------------------------------------------------------

#[test]
fn the_bell_row_toggles_its_preference() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();

    assert_eq!(
        h.bell_state(),
        Some(Toggled::True),
        "the mock seeds the bell on, as the redesign draws it"
    );

    h.tap_label("bell notifications");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while h.bell_state() != Some(Toggled::False) {
        assert!(Instant::now() < deadline, "the bell never turned off");
        h.pump_once();
    }

    // …and back, so the row is a toggle rather than a one-way switch.
    h.tap_label("bell notifications");
    let deadline = Instant::now() + PUMP_DEADLINE;
    while h.bell_state() != Some(Toggled::True) {
        assert!(Instant::now() < deadline, "the bell never came back on");
        h.pump_once();
    }
    assert_eq!(h.depth(), 1, "the bell row pushes nothing");
}

// ---------------------------------------------------------------------------
// 6 — Disconnect
// ---------------------------------------------------------------------------

#[test]
fn disconnect_confirms_first_then_deauthenticates_and_lands_on_connect() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::connected();
    assert!(expect_connection_controller().connected.get_untracked());

    // A dismissed confirmation disconnects nothing.
    open_dialog_by_label(&mut h, "Disconnect");
    assert_eq!(h.depth(), 2);
    h.tap(SCRIM_POINT);
    h.flush();
    assert!(
        expect_connection_controller().connected.get_untracked(),
        "cancelling leaves the connection up"
    );

    // Confirming clears the connection and resets the stack to `/`.
    let dialog = open_dialog_by_label(&mut h, "Disconnect");
    confirm(&mut h, &dialog);

    let connection = expect_connection_controller();
    assert!(!connection.connected.get_untracked(), "deauthenticated");
    assert_eq!(connection.server_info.get_untracked(), None);
    // The screen behind, still mounted by the fixture, agrees.
    h.frame();
    assert!(
        node_values(&h.root).iter().any(|v| v == "not connected"),
        "the connection group cleared: {:?}",
        node_values(&h.root)
    );
    // …and the guard now bounces the routes that need a session, which is what
    // "landed on Connect" means to the router.
    match app_nav().resolve("/sessions") {
        Resolution::Matched { location, .. } => assert_eq!(
            location.path, "/",
            "an unauthenticated /sessions is bounced to Connect"
        ),
        Resolution::Error { .. } => panic!("/sessions is a real route"),
    }
}
