//! Headless coverage of the shell's app-bar adapter (`routes::route_meta`) and the page
//! transition on navigation, driven through the whole app and read off the accessibility tree.
//! `Harness::depth` reads only the outer navigator; in-shell pushes land on the inner one.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use frust::{AnyView, Component, GetUntracked, SizedBox, Theme, any, spawn_local};
use frust_core::accesskit::Role;
use frust_core::{InputEvent, NamedKey, PointerPhase, RenderRoot, ScrollDelta};
use frust_reactive::TrackedScope;
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::{ConnectTarget, TrustMode, expect_connection_controller};
use muxr::features::connect::di::{expect_keyring, expect_saved_servers, mock_credentials};
use muxr::features::connect::domain::entities::{SavedServer, epoch_millis};
use muxr::features::connect::domain::keyring::Keyring;
use muxr::features::connect::presentation::pages::ADD_SERVER_HEADING;
use muxr::features::connect::presentation::widgets::cta::CONNECT_LABEL;
use muxr::features::pro::di::{expect_pro_controller, mock_entitlement};
use muxr::features::pro::domain::entities::Entitlement;
use muxr::features::pro::presentation::NOT_NOW_LABEL;
use muxr::routes::{ADD_SERVER_LOCATION, add_server_location, app_nav, sessions_back_signal};
use muxr::ui::dialogs::confirm_dialog;
use muxr::ui::sheet::{SheetHandle, show_sheet};
use muxr::{AppState, MuxrApp};
use zeroize::Zeroizing;

mod support;
use support::{
    EPSILON, RecScene, added, center, frame, frame_at, largest, ms, named_key,
    node_centers_by_label, node_centers_by_value, node_labels, node_roles, node_values, pointer,
    pump_frames, pump_until, serial, settle, setup, smallest, tap,
};

/// The mock connection every bar-titled test runs behind (the session guard
/// bounces `/sessions`/`/terminal` otherwise).
const HOST: &str = "mock.example";
const PORT: u16 = 7777;

const BRAND: &str = "muxr";
const BACK: &str = "Back";
const GEAR: &str = "Settings";

const BANNED_GLYPHS: [&str; 2] = ["\u{2039}", "\u{2699}"];

const PRE_SPLIT_MS: f64 = 24.0;

const POST_SPLIT_MS: f64 = 60.0;

/// A mock connect target — only `connected` flipping and the `host:port`
/// endpoint matter here.
fn mock_target() -> ConnectTarget {
    ConnectTarget {
        host: HOST.to_string(),
        port: PORT,
        trust_mode: TrustMode::System,
        label: "Mock rig".to_string(),
        offer_credential: false,
        token: zeroize::Zeroizing::new(String::new()),
        remember: false,
        fingerprint: None,
    }
}

fn theme(reduce_motion: bool) -> Theme {
    let mut theme = frust_glyph::baseline();
    theme.motion.reduce_motion = reduce_motion;
    theme
}

fn spend_cold_start() {
    static SPENT: OnceLock<()> = OnceLock::new();
    SPENT.get_or_init(|| {
        let mut throwaway = MuxrApp.init();
        let _ = MuxrApp.build(&mut throwaway);
    });
}

type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

struct Harness {
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    clock: f64,
    scope: TrackedScope,
}

impl Harness {
    fn mount() -> Self {
        Self::themed(theme(false))
    }

    fn themed(theme: Theme) -> Self {
        spend_cold_start();
        let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
        root.set_theme(Box::new(theme));
        let mut harness = Harness {
            root,
            logic: Box::new(|state: &mut AppState| AnyView::new(MuxrApp.build(state))),
            state: MuxrApp.init(),
            tcx: TextContext::new(),
            clock: support::SETTLE_MS,
            scope: TrackedScope::new(),
        };
        harness.frame();
        harness
    }

    fn on_sessions() -> Self {
        let mut harness = Harness::mount();
        expect_connection_controller().connect(mock_target());
        harness.push("/sessions");
        harness
    }

    fn frame(&mut self) -> RecScene {
        frame(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
        )
    }

    fn frame_at(&mut self, at_ms: f64) -> RecScene {
        frame_at(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            ms(at_ms),
        )
    }

    fn settle(&mut self) -> RecScene {
        settle(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
        )
    }

    /// Push `location` through the app's real handle — the seam that carries
    /// the route transition and records the bar mirror — and settle it.
    fn push(&mut self, location: &str) {
        app_nav().push(location);
        self.settle();
    }

    fn requested_frame(&mut self) -> (RecScene, bool) {
        self.clock += FRAME_MS;
        let root = &mut self.root;
        let logic = &mut self.logic;
        let state = &mut self.state;
        let scope = &self.scope;
        scope.track(|| root.rebuild(logic, state));
        let tcx: &mut dyn Any = &mut self.tcx;
        self.root.layout_with_text(support::surface(), tcx);
        let mut scene = RecScene::default();
        let outcome = self.root.paint(&mut scene, ms(self.clock));
        let converging = outcome.needs_frame && !outcome.needs_frame_paced_only;
        let more = converging || self.scope.is_dirty();
        (scene, more)
    }

    fn park(&mut self) -> RecScene {
        for _ in 0..PARK_FRAME_CAP {
            let (scene, more) = self.requested_frame();
            if !more {
                return scene;
            }
        }
        panic!("the app never stopped asking for frames ({PARK_FRAME_CAP} of them)");
    }

    fn dispatch(&mut self, event: &InputEvent) {
        if self.root.event(&mut self.state, event).needs_redraw {
            self.park();
        }
    }

    fn tap_parked(&mut self, point: Point) {
        self.dispatch(&pointer(PointerPhase::Down, point));
        self.dispatch(&pointer(PointerPhase::Up, point));
    }

    fn values(&self) -> Vec<String> {
        node_values(&self.root)
    }

    fn labels(&self) -> Vec<String> {
        node_labels(&self.root)
    }

    fn has_bar(&self) -> bool {
        node_roles(&self.root).contains(&Role::TitleBar)
    }

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
        tap(&mut self.root, &mut self.state, point);
    }

    /// The **route** navigator's page-stack depth — never moved by an overlay
    /// any more (see the module docs' two-navigator note).
    fn depth(&self) -> usize {
        self.state.nav.controller().depth()
    }

    fn overlay_depth(&self) -> usize {
        self.state.overlays.depth()
    }
}

const REST_DEPTH: usize = 1;

fn assert_titled(h: &Harness, title: &str) {
    assert!(h.has_bar(), "the shell bar is mounted");
    assert!(
        h.labels().iter().any(|l| l == title),
        "the bar titles {title:?}: {:?}",
        h.labels()
    );
}

#[test]
fn connect_carries_no_bar_at_all() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::mount();

    assert!(!h.has_bar(), "roles on screen: {:?}", node_roles(&h.root));
    assert!(
        !h.values().iter().any(|v| v == BRAND),
        "…and no brand wordmark with it: {:?}",
        h.values()
    );
}

#[test]
fn sessions_wears_the_brand_the_connected_host_and_the_gear() {
    let _g = serial();
    let _ambient = setup();
    let h = Harness::on_sessions();

    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "the stack's first titled route wears the brand: {:?}",
        h.values()
    );
    assert!(
        h.labels().iter().any(|l| l == GEAR),
        "…and the ⚙ that reaches /settings: {:?}",
        h.labels()
    );
    assert!(
        !h.labels().iter().any(|l| l == BACK),
        "…but no back button at the bottom of the stack: {:?}",
        h.labels()
    );
}

fn round_trip_through(route: &str, title: &str) {
    let mut h = Harness::on_sessions();

    h.push(route);
    assert_titled(&h, title);
    assert!(
        h.labels().iter().any(|l| l == BACK),
        "a pushed route wears the ‹: {:?}",
        h.labels()
    );
    assert!(
        !h.values().iter().any(|v| v == BRAND),
        "…in place of the brand, not beside it: {:?}",
        h.values()
    );
    assert!(
        !h.labels().iter().any(|l| l == GEAR),
        "…and drops /sessions' ⚙: {:?}",
        h.labels()
    );

    h.tap_label(BACK);
    h.settle();

    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "…and the brand came back with it: {:?}",
        h.values()
    );
    assert!(
        !h.labels().iter().any(|l| l == BACK),
        "…leaving no back button behind: {:?}",
        h.labels()
    );
}

#[test]
fn settings_is_titled_and_backed_and_pops_home() {
    let _g = serial();
    let _ambient = setup();
    round_trip_through("/settings", "server settings");
}

#[test]
fn the_terminal_route_is_titled_and_backed_and_pops_home() {
    let _g = serial();
    let _ambient = setup();
    round_trip_through("/terminal?session=s-1", "terminal");
}

#[test]
fn the_bars_gear_reaches_settings() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    h.tap_label(GEAR);
    h.settle();

    assert_titled(&h, "server settings");
}

fn open_overlay(h: &mut Harness) {
    h.state
        .overlays
        .push_transparent(|| any(SizedBox::<AppState>(Some(10.0), Some(10.0))));
    h.settle();
}

fn close_overlay(h: &mut Harness) {
    h.state.overlays.pop();
    h.settle();
}

#[test]
fn a_transparent_overlay_over_sessions_never_retitles_the_bar() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    let covered_depth = h.depth();

    open_overlay(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the overlay is its own page on the host"
    );
    assert_eq!(
        h.depth(),
        covered_depth,
        "…and the route stack under it is untouched"
    );

    close_overlay(&mut h);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "…and dismissing it pops only it"
    );
    assert_eq!(h.depth(), covered_depth, "the route stack, still untouched");
    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "…leaving the covered route's own bar, brand and all: {:?}",
        h.values()
    );
}

#[test]
fn the_gear_does_nothing_under_an_open_overlay_and_works_again_after() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    // Where the ⚙ lives while the bar is live — recorded *before* the modal,
    // because a modal bar publishes no node to find it by.
    let gear = node_centers_by_label(&h.root, GEAR)
        .first()
        .copied()
        .expect("the settings affordance is on screen");

    open_overlay(&mut h);
    assert!(
        !h.has_bar(),
        "a modal bar publishes no node: an affordance that routes nowhere is \
         worse than none, and a screen reader must not be offered a way round \
         the modality: {:?}",
        node_roles(&h.root)
    );

    // The modality hole this closes: before the root host, this press pushed
    // /settings out from under an open sheet.
    tap(&mut h.root, &mut h.state, gear);
    h.settle();

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the overlay is still the only thing up"
    );
    assert!(
        !h.values().iter().any(|v| v == "server settings"),
        "…and nothing retitled: {:?}",
        h.values()
    );

    close_overlay(&mut h);
    assert!(h.has_bar(), "the bar is live again");
    h.tap_label(GEAR);
    h.settle();
    assert_titled(&h, "server settings");
}

const SHEET_BODY: f64 = 100.0;
const SHEET_SCRIM_POINT: Point = Point::new(4.0, 200.0);
/// How far [`Harness::requested_frame`] advances the clock — one 60Hz frame,
/// the cadence a desktop `request_redraw` actually produces.
const FRAME_MS: f64 = 16.0;
const PARK_FRAME_CAP: usize = 600;

fn open_real_sheet(h: &mut Harness) -> (Point, Size) {
    let before = h.park();
    show_sheet(&h.state.overlays, |_sheet| {
        any(SizedBox::<AppState>(Some(SHEET_BODY), Some(SHEET_BODY)))
    });
    let open = h.park();
    assert!(
        !h.has_bar(),
        "a sheet dims the bar and takes its input: {:?}",
        node_roles(&h.root)
    );
    largest(&added(&before.paths, &open.paths)).expect("the sheet paints its own panel")
}

#[test]
fn a_sheet_dismissed_by_its_scrim_gives_the_bar_back_before_the_loop_parks() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    let covered_depth = h.depth();

    open_real_sheet(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the sheet is its own page on the host"
    );
    assert_eq!(h.depth(), covered_depth, "…over an untouched route stack");

    h.tap_parked(SHEET_SCRIM_POINT);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the scrim tap popped the sheet"
    );
    assert_eq!(h.depth(), covered_depth, "…and only the sheet");
    assert!(
        h.has_bar(),
        "…and the bar is lit and addressable again with no further input: {:?}",
        node_roles(&h.root)
    );
    h.tap_label(GEAR);
    h.settle();
    assert_titled(&h, "server settings");
}

#[test]
fn escape_on_a_sheet_gives_the_bar_back_before_the_loop_parks() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    let covered_depth = h.depth();

    let panel = open_real_sheet(&mut h);
    h.tap_parked(center(panel));
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "a panel press keeps it open"
    );

    h.dispatch(&named_key(NamedKey::Escape));

    assert_eq!(h.overlay_depth(), REST_DEPTH, "Escape dismissed the sheet");
    assert_eq!(h.depth(), covered_depth, "…and left the route stack alone");
    assert!(
        h.has_bar(),
        "…and the bar came back with it: {:?}",
        node_roles(&h.root)
    );
}

#[test]
fn a_dialog_action_gives_the_bar_back_before_the_loop_parks() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    let before = h.park();
    let covered_depth = h.depth();

    confirm_dialog(
        &h.state.overlays,
        "Kill session?",
        "Any attached client is disconnected immediately.",
        "Kill",
        |_state: &mut AppState| {},
    );
    let open = h.park();
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the dialog is its own page on the host"
    );
    assert_eq!(h.depth(), covered_depth, "…over an untouched route stack");
    assert!(!h.has_bar(), "…and the bar is inert under it");

    let action = smallest(&added(&before.strokes, &open.strokes))
        .expect("the danger action strokes its border");
    h.tap_parked(center(action));

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the action closed the dialog"
    );
    assert_eq!(h.depth(), covered_depth, "…and only the dialog");
    assert!(
        h.has_bar(),
        "…and the bar is live again with no further input: {:?}",
        node_roles(&h.root)
    );
}

#[test]
fn an_android_back_press_dismisses_the_overlay_and_leaves_the_route_alone() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    let covered_depth = h.depth();

    open_real_sheet(&mut h);
    assert_eq!(h.overlay_depth(), REST_DEPTH + 1, "the sheet is up");

    frust::push_back_press();
    h.settle();

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the back press dismissed the overlay"
    );
    assert_eq!(
        h.depth(),
        covered_depth,
        "…and NOT the route under it (the #44 back bug)"
    );
    assert!(
        h.has_bar(),
        "…and the bar is live again: {:?}",
        node_roles(&h.root)
    );
    h.tap_label(GEAR);
    h.settle();
    assert_titled(&h, "server settings");
}

fn open_sheet_with_handle(h: &mut Harness) -> SheetHandle {
    let captured: Rc<RefCell<Option<SheetHandle>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&captured);
    show_sheet(&h.state.overlays, move |sheet| {
        *sink.borrow_mut() = Some(sheet.clone());
        any(SizedBox::<AppState>(Some(SHEET_BODY), Some(SHEET_BODY)))
    });
    h.park();
    let handle = captured.borrow().clone();
    handle.expect("the sheet's body builder ran and minted a handle")
}

const IDLE_MOVE: Point = Point::new(4.0, 400.0);

#[test]
fn a_route_pushed_as_a_sheet_closes_keeps_its_bar_entry_under_the_next_overlay() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();

    let sheet = open_sheet_with_handle(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the sheet is its own page on the host"
    );

    sheet.close();
    app_nav().push("/settings");
    h.park();

    assert_eq!(h.overlay_depth(), REST_DEPTH, "the sheet's page went");
    assert_titled(&h, "server settings");

    h.dispatch(&pointer(PointerPhase::Move, IDLE_MOVE));

    // Stale-bar scenario, asserted end to end: an overlay
    // opening and closing over `/settings` must not evict its bar entry.
    let overlay = open_sheet_with_handle(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the overlay is its own page"
    );
    assert!(
        !h.has_bar(),
        "…and it dims the bar over the route: {:?}",
        node_roles(&h.root)
    );

    overlay.close();
    h.park();

    assert_eq!(h.overlay_depth(), REST_DEPTH, "the overlay popped");
    assert_titled(&h, "server settings");
    assert!(
        h.labels().iter().any(|l| l == BACK),
        "…back affordance and all — the route's own entry survived: {:?}",
        h.labels()
    );
    assert!(
        !h.values().iter().any(|v| v == HOST),
        "…and the bar did not fall back to the route underneath: {:?}",
        h.values()
    );
}

const TERMINAL_MENU: &str = "Terminal menu";

#[test]
fn the_terminal_menu_button_keeps_its_name_on_a_tablet() {
    let _g = serial();
    let _ambient = setup();
    let metrics = Some(support::metrics_at(1024.0, 720.0));
    let mut h = support::mount_with_metrics(metrics, Harness::mount);
    expect_connection_controller().connect(mock_target());
    h.push("/sessions");
    h.push("/terminal?session=s-1");
    h.park();

    assert!(
        h.labels().iter().any(|l| l == TERMINAL_MENU),
        "{:?}",
        h.labels()
    );
}

#[test]
fn the_terminal_menu_button_opens_its_side_sheet_directly_and_gives_the_bar_back_after_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();

    h.push("/terminal?session=s-1");
    let covered_depth = h.depth();
    h.park();
    assert_titled(&h, "terminal");

    h.tap_label(TERMINAL_MENU);
    h.park();
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the side sheet is its own page on the host — above the bar it \
         anchors to, rather than clipped under it"
    );
    assert_eq!(h.depth(), covered_depth, "…over an untouched route stack");
    assert!(
        !h.has_bar(),
        "…and it takes the bar it anchors to out of the tree with it: {:?}",
        node_roles(&h.root)
    );

    h.tap_parked(SHEET_SCRIM_POINT);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the scrim tap popped the side sheet"
    );
    assert_eq!(h.depth(), covered_depth, "…and the route stayed put");
    assert!(
        h.has_bar(),
        "…and the bar is lit and addressable again with no further input: {:?}",
        node_roles(&h.root)
    );
    assert_titled(&h, "terminal");
    h.tap_label(BACK);
    h.settle();
    assert_titled(&h, HOST);
}

#[test]
fn the_bar_carries_no_font_fallback_glyph_any_more() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    for glyph in BANNED_GLYPHS {
        assert!(
            !h.labels().iter().any(|l| l.contains(glyph)),
            "{glyph:?} survives as a label: {:?}",
            h.labels()
        );
        assert!(
            !h.values().iter().any(|v| v.contains(glyph)),
            "{glyph:?} survives as a run: {:?}",
            h.values()
        );
    }

    h.push("/settings");
    for glyph in BANNED_GLYPHS {
        assert!(
            !h.labels().iter().any(|l| l.contains(glyph)),
            "{glyph:?} survives on a pushed route: {:?}",
            h.labels()
        );
    }
    assert!(
        h.labels().iter().any(|l| l == BACK),
        "…the back affordance is still there, named: {:?}",
        h.labels()
    );
}

#[test]
fn settings_back_sessions_back_connect_retitles_the_bar_at_every_step() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "/sessions wears the brand"
    );

    h.push("/settings");
    assert_titled(&h, "server settings");
    assert!(h.labels().iter().any(|l| l == BACK));

    // back → sessions — an in-shell pop, same observer, no mirror to desync.
    h.tap_label(BACK);
    h.settle();
    assert_titled(&h, HOST);
    assert!(
        h.values().iter().any(|v| v == BRAND),
        "the brand is back too"
    );

    press_back(&mut h);
    assert!(
        showing_confirmation(&h),
        "the disconnect confirmation is up"
    );
    let action = confirmation_action(&h, DISCONNECT_ACTION);
    h.tap_parked(action);
    h.settle();
    h.park();

    assert!(
        !h.has_bar(),
        "connect is bar-less, the last step of the round trip: {:?}",
        node_roles(&h.root)
    );
}

const DISCONNECT_TITLE: &str = "Disconnect from server?";
const DISCONNECT_ACTION: &str = "Disconnect";
const CANCEL_ACTION: &str = "Cancel";

fn connected() -> bool {
    expect_connection_controller().connected.get_untracked()
}

fn showing_confirmation(h: &Harness) -> bool {
    h.values().iter().any(|v| v == DISCONNECT_TITLE)
        && h.values().iter().any(|v| v.contains(HOST))
        && h.labels().iter().any(|l| l == DISCONNECT_ACTION)
}

fn press_back(h: &mut Harness) {
    frust::push_back_press();
    h.settle();
    h.park();
}

/// Where a tap must land to take the confirmation's `label` action.
fn confirmation_action(h: &Harness, label: &str) -> Point {
    node_centers_by_label(&h.root, label)
        .first()
        .copied()
        .unwrap_or_else(|| {
            panic!(
                "the confirmation carries no {label:?} action: {:?}",
                h.labels()
            )
        })
}

#[test]
fn back_on_sessions_asks_instead_of_popping_and_bumps_the_cell_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    let route_depth = h.depth();
    let generation = sessions_back_signal().get();

    press_back(&mut h);

    assert_eq!(
        h.depth(),
        route_depth,
        "the route stack is untouched — the page was asked, not popped"
    );
    assert_eq!(
        sessions_back_signal().get(),
        generation + 1,
        "…the navigator bumped the page's dismiss cell exactly once"
    );
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "…and the screen turned that into one confirmation"
    );
    assert!(
        showing_confirmation(&h),
        "…the disconnect confirmation, naming the host: {:?} / {:?}",
        h.values(),
        h.labels()
    );
    assert!(connected(), "nothing is disconnected until it is confirmed");
    assert!(
        !h.has_bar(),
        "…and the app under the confirmation is inert, bar included: {:?}",
        node_roles(&h.root)
    );
}

#[test]
fn a_bump_that_lands_while_the_confirmation_is_up_is_swallowed() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();

    press_back(&mut h);
    assert_eq!(h.overlay_depth(), REST_DEPTH + 1, "the confirmation is up");

    let signal = sessions_back_signal();
    signal.set(signal.get().wrapping_add(1));
    h.park();

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "still exactly one confirmation, not two stacked"
    );
    assert!(showing_confirmation(&h), "…and it is still the same one");

    press_back(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the dismissal left no queued second confirmation: {:?}",
        h.values()
    );
    assert!(connected(), "…and nothing was disconnected");
}

#[test]
fn a_second_back_press_cancels_the_confirmation_and_stays_connected() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    let route_depth = h.depth();

    press_back(&mut h);
    assert_eq!(h.overlay_depth(), REST_DEPTH + 1, "the confirmation is up");

    press_back(&mut h);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "the second press dismissed the confirmation"
    );
    assert_eq!(h.depth(), route_depth, "…and popped no route with it");
    assert!(connected(), "…leaving the connection alone");
    assert_titled(&h, HOST);

    press_back(&mut h);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the next press asks again"
    );
    assert!(showing_confirmation(&h));

    let cancel = confirmation_action(&h, CANCEL_ACTION);
    h.tap_parked(cancel);
    h.settle();
    h.park();

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "Cancel dismissed the confirmation"
    );
    assert_eq!(h.depth(), route_depth, "…popping no route");
    assert!(connected(), "…and disconnecting nothing");
    assert_titled(&h, HOST);
}

#[test]
fn confirming_the_disconnect_drops_the_connection_and_returns_to_connect() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();

    press_back(&mut h);
    let action = confirmation_action(&h, DISCONNECT_ACTION);

    h.tap_parked(action);
    h.settle();
    h.park();

    assert!(!connected(), "the confirmed answer disconnected");
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "…the confirmation closed with it"
    );
    assert_eq!(h.depth(), 1, "…and `/sessions` popped back to Connect");
    assert!(
        !h.has_bar(),
        "…which is the bar-less route: {:?}",
        node_roles(&h.root)
    );
}

#[test]
fn back_on_the_terminal_route_still_pops_to_sessions_with_no_question_asked() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();

    h.push("/terminal?session=s-1");
    assert_titled(&h, "terminal");
    let generation = sessions_back_signal().get();

    // Only `/sessions` carries `DismissAnimated`; every other route keeps the
    // default `Pop`, so this press is the plain (in-shell) pop it always was.
    press_back(&mut h);

    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH,
        "…asking nothing on the way: {:?}",
        h.values()
    );
    assert_eq!(
        sessions_back_signal().get(),
        generation,
        "…and never touching the `/sessions` cell"
    );
    assert!(connected(), "…nor the connection");
    assert_titled(&h, HOST);
}

fn unlock_pro(h: &mut Harness) {
    mock_entitlement()
        .expect("tests/support opted this process into the mock")
        .set_entitlement(Entitlement::Pro);
    let pro = expect_pro_controller();
    let refreshing = Arc::clone(&pro);
    spawn_local(async move { refreshing.refresh().await });
    let scene = h.frame();
    pump_until(
        &mut h.root,
        &mut h.logic,
        &mut h.state,
        &mut h.tcx,
        scene,
        || pro.is_pro_untracked(),
    );
}

fn on_add_server_page() -> Harness {
    let mut h = Harness::on_sessions();
    unlock_pro(&mut h);
    h.park();
    assert_eq!(h.depth(), 2, "Connect plus the placed shell page");
    h.push(ADD_SERVER_LOCATION);
    h.park();
    h
}

#[test]
fn the_add_server_page_covers_the_placed_shell_without_a_bar_or_a_guard_bounce() {
    let _g = serial();
    let _ambient = setup();
    let h = on_add_server_page();

    assert_eq!(h.depth(), 3, "pushed over the shell page, which stays");
    assert!(
        h.values().iter().any(|v| v == ADD_SERVER_HEADING),
        "the add-server form is on screen: {:?}",
        h.values()
    );
    assert!(
        h.labels().iter().any(|l| l == BACK),
        "…with its own back chevron: {:?}",
        h.labels()
    );
    assert!(
        !h.has_bar(),
        "…and no shell bar over it: {:?}",
        node_roles(&h.root)
    );
    assert!(connected(), "the live connection is untouched");
    assert_eq!(h.overlay_depth(), REST_DEPTH, "Pro: no paywall first");
}

#[test]
fn back_on_the_add_server_page_pops_to_sessions_with_the_shell_still_placed() {
    let _g = serial();
    let _ambient = setup();
    let mut h = on_add_server_page();
    let generation = sessions_back_signal().get();

    press_back(&mut h);

    assert_eq!(h.depth(), 2, "only the add page came off");
    assert!(
        h.has_bar(),
        "the shell's bar is back: {:?}",
        node_roles(&h.root)
    );
    assert_titled(&h, HOST);
    assert_eq!(
        sessions_back_signal().get(),
        generation,
        "the press never reached the Sessions page beneath"
    );
    assert_eq!(h.overlay_depth(), REST_DEPTH, "…and asked nothing");
    assert!(connected(), "the guard never fired: still connected");
}

#[test]
fn the_add_server_pages_own_chevron_pops_it_the_same_way() {
    let _g = serial();
    let _ambient = setup();
    let mut h = on_add_server_page();

    h.tap_label(BACK);
    h.settle();
    h.park();

    assert_eq!(h.depth(), 2, "the shell page stays placed");
    assert_titled(&h, HOST);
}

#[test]
fn a_free_install_meets_the_paywall_first_and_not_now_pops_the_add_page() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();

    h.push(ADD_SERVER_LOCATION);
    h.park();
    assert_eq!(h.depth(), 3);
    assert_eq!(
        h.overlay_depth(),
        REST_DEPTH + 1,
        "the seeded saved rows put a new server past the free tier"
    );

    let not_now = node_centers_by_label(&h.root, NOT_NOW_LABEL)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the paywall offers Not now: {:?}", h.labels()));
    h.tap_parked(not_now);
    h.settle();
    h.park();
    h.settle();
    h.park();

    assert_eq!(h.overlay_depth(), REST_DEPTH, "the paywall closed");
    assert_eq!(h.depth(), 2, "…and took the add page with it");
    assert_titled(&h, HOST);
    assert!(connected());
}

const SAVED_HOST: &str = "saved.example";
const SAVED_ENDPOINT: &str = "saved.example:50051";
const SAVED_LABEL: &str = "Saved rig";
const SAVED_TOKEN: &str = "saved-s3cr3t";
const REMEMBER_LABEL: &str = "Remember for 4 weeks";

fn connected_endpoint() -> Option<String> {
    expect_connection_controller()
        .server_info
        .get_untracked()
        .map(|info| info.endpoint)
}

fn pump_until_true(h: &mut Harness, done: impl FnMut() -> bool) {
    let scene = h.frame();
    pump_until(
        &mut h.root,
        &mut h.logic,
        &mut h.state,
        &mut h.tcx,
        scene,
        done,
    );
}

fn on_settings_with_a_saved_server(unlocked: bool) -> Harness {
    let mut h = Harness::on_sessions();
    unlock_pro(&mut h);
    expect_saved_servers()
        .save(SavedServer {
            host: SAVED_HOST.to_string(),
            port: 50051,
            trust_mode: TrustMode::System,
            label: SAVED_LABEL.to_string(),
            last_used_ms: epoch_millis(),
            read_only: false,
            has_credential: true,
            token_name: None,
            fingerprint: None,
        })
        .expect("the mock store never fails");
    mock_credentials()
        .expect("tests/support opted this process into the mock")
        .seed_vault(&[(SAVED_ENDPOINT, SAVED_TOKEN)]);
    if unlocked {
        let mut keyring = Keyring::new();
        keyring.insert(SAVED_ENDPOINT, Zeroizing::new(SAVED_TOKEN.to_string()));
        expect_keyring().publish(keyring);
    }
    h.push("/settings");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !h.values().iter().any(|v| v == SAVED_LABEL) {
        assert!(
            std::time::Instant::now() < deadline,
            "the saved row never rendered: {:?}",
            h.values()
        );
        pump_frames(&mut h.root, &mut h.logic, &mut h.state, &mut h.tcx, 1);
    }
    h.park();
    assert_titled(&h, "server settings");
    h
}

fn saved_row_point(h: &mut Harness) -> Point {
    let label_at = |h: &Harness| {
        node_centers_by_value(&h.root, SAVED_LABEL)
            .first()
            .copied()
            .unwrap_or_else(|| panic!("no saved row on screen: {:?}", h.values()))
    };
    for _ in 0..40 {
        let point = label_at(h);
        if point.y < support::H - 60.0 {
            return point;
        }
        h.root.event(
            &mut h.state,
            &InputEvent::Scroll {
                position: Point::new(support::W / 2.0, support::H / 2.0),
                delta: ScrollDelta::Lines(0.0, 3.0),
            },
        );
        h.settle();
    }
    panic!("the saved row never scrolled into view");
}

#[test]
fn a_dial_settling_after_back_left_the_add_page_pops_nothing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = on_add_server_page();
    let mark = app_nav().mark_top();

    press_back(&mut h);
    assert_eq!(h.depth(), 2, "back took the add page off");

    app_nav().pop_marked(&mark);
    h.settle();
    h.park();

    assert_eq!(h.depth(), 2, "the shell is still placed");
    assert_titled(&h, HOST);
    assert!(connected(), "…on a live connection");
}

#[test]
fn a_marked_pop_still_takes_its_own_page_off_and_only_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = on_add_server_page();
    let mark = app_nav().mark_top();

    app_nav().pop_marked(&mark);
    app_nav().pop_marked(&mark);
    h.settle();
    h.park();

    assert_eq!(h.depth(), 2, "the add page came off, the shell did not");
    assert_titled(&h, HOST);
}

#[test]
fn a_settings_switch_settling_after_back_lands_on_sessions_still_connected() {
    let _g = serial();
    let _ambient = setup();
    let mut h = on_settings_with_a_saved_server(false);

    let row = saved_row_point(&mut h);
    tap(&mut h.root, &mut h.state, row);
    assert_ne!(connected_endpoint().as_deref(), Some(SAVED_ENDPOINT));
    h.tap_label(BACK);
    h.settle();
    h.park();
    assert_titled(&h, HOST);

    pump_until_true(&mut h, || {
        connected_endpoint().as_deref() == Some(SAVED_ENDPOINT)
    });
    h.settle();
    h.park();

    assert_eq!(h.depth(), 2, "the shell is still placed");
    assert_eq!(h.overlay_depth(), REST_DEPTH, "nothing asked anything");
    assert_titled(&h, SAVED_HOST);
    assert!(connected(), "…on the switched-to server");
}

#[test]
fn a_double_tap_on_a_saved_row_switches_and_pops_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = on_settings_with_a_saved_server(true);
    let epoch = expect_connection_controller()
        .connection_epoch
        .get_untracked();

    let row = saved_row_point(&mut h);
    tap(&mut h.root, &mut h.state, row);
    tap(&mut h.root, &mut h.state, row);
    h.settle();
    h.park();

    assert_eq!(
        expect_connection_controller()
            .connection_epoch
            .get_untracked(),
        epoch + 2,
        "one switch: one teardown, one connect"
    );
    assert_eq!(h.depth(), 2, "one pop: Settings off, the shell kept");
    assert_titled(&h, SAVED_HOST);
    assert!(connected());
}

#[test]
fn a_double_tap_on_the_add_pages_connect_switches_and_pops_once() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    unlock_pro(&mut h);
    h.park();
    h.push(&add_server_location(Some("100.64.0.7:50051")));
    h.park();
    h.tap_label(REMEMBER_LABEL);
    h.park();
    let epoch = expect_connection_controller()
        .connection_epoch
        .get_untracked();

    let cta = node_centers_by_label(&h.root, CONNECT_LABEL)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("no connect CTA: {:?}", h.labels()));
    tap(&mut h.root, &mut h.state, cta);
    tap(&mut h.root, &mut h.state, cta);
    h.settle();
    h.park();

    assert_eq!(
        connected_endpoint().as_deref(),
        Some("100.64.0.7:50051"),
        "the add page switched"
    );
    assert_eq!(
        expect_connection_controller()
            .connection_epoch
            .get_untracked(),
        epoch + 2,
        "…once"
    );
    assert_eq!(h.depth(), 2, "…and popped once, keeping the shell");
    assert_titled(&h, "100.64.0.7");
}

#[test]
fn an_unwind_with_the_add_page_covering_takes_it_and_the_shell_off() {
    let _g = serial();
    let _ambient = setup();
    let mut h = on_add_server_page();
    assert_eq!(h.depth(), 3);

    expect_connection_controller().disconnect();
    app_nav().push("/settings");
    h.settle();
    h.park();

    assert_eq!(h.depth(), 1, "no shell page is left placed");
    assert!(!h.has_bar(), "…so no bar: {:?}", node_roles(&h.root));
    assert_eq!(
        h.state
            .nav
            .controller()
            .route_stack()
            .current_route()
            .map(|location| location.path.clone())
            .as_deref(),
        Some("/"),
        "the outer root, Connect, is what is left"
    );
}

fn leaving_page_x(scene: &RecScene) -> f64 {
    largest(&scene.rounded)
        .unwrap_or_else(|| panic!("the leaving page painted no chrome to track"))
        .0
        .x
}

fn rect_sized(rects: &[(Point, Size)], size: Size) -> Option<(Point, Size)> {
    rects.iter().copied().find(|(_, s)| {
        (s.width - size.width).abs() < EPSILON && (s.height - size.height).abs() < EPSILON
    })
}

fn distinctive_chrome(page: &[(Point, Size)], other: &[(Point, Size)]) -> Option<(Point, Size)> {
    let alone: Vec<(Point, Size)> = page
        .iter()
        .copied()
        .filter(|(_, size)| rect_sized(other, *size).is_none())
        .collect();
    largest(&alone)
}

fn probe_push_transition(reduce_motion: bool) -> (RecScene, RecScene, RecScene) {
    let mut h = Harness::themed(theme(reduce_motion));
    let settled = h.frame();

    expect_connection_controller().connect(mock_target());
    app_nav().push("/settings");
    h.frame_at(0.0);
    let pre_split = h.frame_at(PRE_SPLIT_MS);
    let post_split = h.frame_at(POST_SPLIT_MS);
    (settled, pre_split, post_split)
}

#[test]
fn a_route_push_slides_the_leaving_page_then_reveals_the_entering_one() {
    let _g = serial();
    let _ambient = setup();

    let (settled, pre_split, post_split) = probe_push_transition(false);

    let mut rest = Harness::mount();
    expect_connection_controller().connect(mock_target());
    rest.push("/settings");
    let entering_rest = rest.frame();

    let (settled_pos, settled_size) = distinctive_chrome(&settled.rounded, &entering_rest.rounded)
        .expect("the settled leaving page painted chrome Settings does not");
    let (entering_pos, entering_size) =
        distinctive_chrome(&entering_rest.rounded, &settled.rounded)
            .expect("the settled entering page painted chrome Connect does not");

    let (pre_pos, _) = rect_sized(&pre_split.rounded, settled_size).unwrap_or_else(|| {
        panic!(
            "Connect's {settled_size:?} chrome is gone before the fade split: {:?}",
            pre_split.rounded
        )
    });
    assert!(
        pre_pos.x < settled_pos.x - EPSILON,
        "the leaving page is displaced before the split ({} vs {})",
        pre_pos.x,
        settled_pos.x
    );
    assert!(
        settled_pos.x - pre_pos.x <= 16.0 + EPSILON,
        "…by at most the preset's 16dp travel ({} px)",
        settled_pos.x - pre_pos.x
    );
    assert!(
        rect_sized(&pre_split.rounded, entering_size).is_none(),
        "Settings' {entering_size:?} chrome already paints before the split: {:?}",
        pre_split.rounded
    );

    assert!(
        rect_sized(&post_split.rounded, settled_size).is_none(),
        "Connect's {settled_size:?} chrome still paints past the split: {:?}",
        post_split.rounded
    );
    let (post_pos, _) = rect_sized(&post_split.rounded, entering_size).unwrap_or_else(|| {
        panic!(
            "Settings' {entering_size:?} chrome hasn't painted past the split: {:?}",
            post_split.rounded
        )
    });
    assert!(
        post_pos.x > entering_pos.x + EPSILON,
        "the entering page is displaced past the split ({} vs {})",
        post_pos.x,
        entering_pos.x
    );
    assert!(
        post_pos.x - entering_pos.x <= 16.0 + EPSILON,
        "…by at most the preset's 16dp travel ({} px)",
        post_pos.x - entering_pos.x
    );
}

#[test]
fn reduce_motion_collapses_the_route_slide_to_a_pure_crossfade() {
    let _g = serial();
    let _ambient = setup();

    let (settled, _pre_split, post_split) = probe_push_transition(true);
    let base_x = leaving_page_x(&settled);
    let mid_x = leaving_page_x(&post_split);

    assert!(
        (mid_x - base_x).abs() < EPSILON,
        "reduced motion must not slide the leaving page ({mid_x} vs {base_x})"
    );
}

#[test]
fn the_bar_still_retitles_under_reduce_motion() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::themed(theme(true));
    expect_connection_controller().connect(mock_target());

    h.push("/sessions");
    assert_titled(&h, HOST);
    h.push("/settings");
    assert_titled(&h, "server settings");
    assert!(
        h.labels().iter().any(|l| l == BACK),
        "…back button included: {:?}",
        h.labels()
    );
}

#[test]
fn the_navigator_survives_the_bar_appearing_and_disappearing() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::mount();
    expect_connection_controller().connect(mock_target());

    let start_depth = h.depth();
    h.push("/sessions");
    assert!(h.has_bar());
    assert_eq!(
        start_depth + 1,
        h.depth(),
        "placing the shell is the one outer op"
    );

    h.push("/settings");
    assert_eq!(start_depth + 1, h.depth(), "still the one shell page");

    h.tap_label(BACK);
    h.settle();
    h.state.nav.controller().pop();
    h.settle();

    assert_eq!(
        h.depth(),
        start_depth,
        "back at Connect, the shell page gone with it"
    );
    assert!(!h.has_bar(), "…and the bar collapsed with it");
}

const SERVERS_FAB: &str = "Servers";
const LOCK_CHIP: &str = "Lock keyring";
const ADD_CHIP: &str = "add server";
const CLOSE_CHIP: &str = "Close";

fn switcher() -> Arc<muxr::features::servers::presentation::controller::ServersSwitcherController> {
    muxr::features::servers::di::expect_servers_controller()
}

fn stage_is_open() -> bool {
    switcher().open.get_untracked()
}

fn open_stage(h: &mut Harness) {
    switcher().open();
    h.park();
}

#[test]
fn the_servers_fab_exists_only_on_sessions() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    assert!(
        h.labels().iter().any(|l| l == SERVERS_FAB),
        "sessions carries the FAB: {:?}",
        h.labels()
    );
    let fab_role = h
        .root
        .semantics()
        .nodes
        .iter()
        .find(|(_, node)| node.label() == Some(SERVERS_FAB))
        .map(|(_, node)| node.role());
    assert_eq!(fab_role, Some(Role::Button), "the FAB node is a Button");

    h.push("/settings");
    h.park();
    assert!(
        !h.labels().iter().any(|l| l == SERVERS_FAB),
        "settings does not: {:?}",
        h.labels()
    );

    h.push("/terminal?session=dev");
    h.park();
    assert!(
        !h.labels().iter().any(|l| l == SERVERS_FAB),
        "…nor does the terminal: {:?}",
        h.labels()
    );
}

#[test]
fn the_fab_opens_the_stage_and_the_dots_read_the_strip() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    assert!(!stage_is_open());
    let fab = node_centers_by_label(&h.root, SERVERS_FAB)[0];
    h.tap_parked(fab);
    assert!(stage_is_open(), "a tap on the FAB raised the stage");

    let strip = muxr::features::servers::presentation::controller::peer_endpoints(
        &switcher().faces.get_untracked(),
    )
    .len()
        + 1;
    let dots = format!("page 1 of {strip}");
    assert!(
        h.labels().contains(&dots),
        "the dots' status reads {dots:?} (the live card is first): {:?}",
        h.labels()
    );
}

#[test]
fn the_stage_bar_close_chip_dismisses_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    open_stage(&mut h);
    let close = node_centers_by_label(&h.root, CLOSE_CHIP)[0];
    h.tap_parked(close);
    assert!(!stage_is_open());
}

#[test]
fn back_with_the_stage_open_closes_it_before_any_navigator_pops() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    h.park();
    open_stage(&mut h);
    let generation = sessions_back_signal().get();
    let (route_depth, overlay_depth) = (h.depth(), h.overlay_depth());

    press_back(&mut h);

    assert!(!stage_is_open(), "the press closed the stage");
    assert_eq!(h.depth(), route_depth, "no route popped");
    assert_eq!(h.overlay_depth(), overlay_depth, "no overlay appeared");
    assert_eq!(
        sessions_back_signal().get(),
        generation + 1,
        "the route navigator outranks the fallback and bumped Sessions' cell…"
    );
    assert!(
        !showing_confirmation(&h),
        "…which closed the stage instead of asking about disconnecting"
    );
    assert!(connected());

    press_back(&mut h);
    assert!(
        showing_confirmation(&h),
        "with the stage down, back is Sessions' own again"
    );
}

#[test]
fn the_stage_bars_add_chip_pushes_the_add_server_page_under_pro() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    unlock_pro(&mut h);
    open_stage(&mut h);
    let add = node_centers_by_label(&h.root, ADD_CHIP)[0];

    h.tap_parked(add);
    h.settle();
    h.park();

    assert!(
        h.values().iter().any(|v| v == ADD_SERVER_HEADING),
        "the add form is up: {:?}",
        h.values()
    );
    assert_eq!(h.depth(), 3, "pushed over the placed shell");
    assert!(!stage_is_open(), "the add chip took the stage down with it");

    press_back(&mut h);
    assert_eq!(h.depth(), 2, "the add page popped back to the shell");
    assert!(!stage_is_open(), "the stage does not reappear on pop");
    assert!(
        h.labels().iter().all(|l| !l.starts_with("page ")),
        "no dots: {:?}",
        h.labels()
    );
}

#[test]
fn the_stage_never_outlives_sessions() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    open_stage(&mut h);
    assert!(stage_is_open());

    app_nav().push("/settings");
    h.settle();
    h.park();

    assert!(!stage_is_open(), "leaving /sessions closed the stage");
    assert!(
        h.labels().iter().all(|l| !l.starts_with("page ")),
        "no dots: {:?}",
        h.labels()
    );
}

#[test]
fn the_stage_bars_lock_chip_locks_the_keyring_and_lands_on_connect() {
    let _g = serial();
    let _ambient = setup();
    let mut h = Harness::on_sessions();
    let mut keyring = Keyring::new();
    keyring.insert(SAVED_ENDPOINT, Zeroizing::new(SAVED_TOKEN.to_string()));
    expect_keyring().publish(keyring);
    open_stage(&mut h);
    let lock = node_centers_by_label(&h.root, LOCK_CHIP)[0];

    h.tap_parked(lock);
    h.settle();
    h.park();

    assert!(!expect_keyring().is_unlocked(), "the vault is shut");
    assert!(!connected(), "…and the connection dropped");
    assert!(!stage_is_open());
    assert_eq!(h.depth(), 1, "…landing on Connect");
}

fn stage_over_a_saved_server() -> Harness {
    let mut h = Harness::on_sessions();
    unlock_pro(&mut h);
    expect_saved_servers()
        .save(SavedServer {
            host: SAVED_HOST.to_string(),
            port: 50051,
            trust_mode: TrustMode::System,
            label: SAVED_LABEL.to_string(),
            last_used_ms: epoch_millis(),
            read_only: false,
            has_credential: true,
            token_name: None,
            fingerprint: None,
        })
        .expect("the mock store never fails");
    let mut keyring = Keyring::new();
    keyring.insert(SAVED_ENDPOINT, Zeroizing::new(SAVED_TOKEN.to_string()));
    expect_keyring().publish(keyring);
    open_stage(&mut h);
    h
}

#[test]
fn selecting_a_peer_switches_the_server_and_the_stage_comes_down_on_it() {
    let _g = serial();
    let _ambient = setup();
    let mut h = stage_over_a_saved_server();
    let epoch = expect_connection_controller()
        .connection_epoch
        .get_untracked();
    let endpoints = muxr::features::servers::presentation::controller::peer_endpoints(
        &switcher().faces.get_untracked(),
    );
    let peer = endpoints
        .iter()
        .position(|e| e == SAVED_ENDPOINT)
        .expect("the saved row is a peer card");
    let gate = muxr::features::servers::presentation::controller::SwitchGate::standard(
        &h.state.overlays,
        &expect_pro_controller(),
        &app_nav(),
    );

    switcher().select(peer, &gate);
    pump_until_true(&mut h, || {
        connected_endpoint().as_deref() == Some(SAVED_ENDPOINT) && !stage_is_open()
    });
    h.park();

    assert!(
        expect_connection_controller()
            .connection_epoch
            .get_untracked()
            > epoch,
        "the epoch advanced"
    );
    assert!(switcher().expanding.get_untracked().is_none());
    assert_eq!(h.depth(), 2, "still on the shell page, no navigation");
    assert!(
        h.labels().iter().any(|l| l == SERVERS_FAB),
        "the new server's Sessions is up with its FAB: {:?}",
        h.labels()
    );
}

fn first_frame_shift(reduce_motion: bool) -> f64 {
    let mut h = Harness::themed(theme(reduce_motion));
    expect_connection_controller().connect(mock_target());
    h.push("/sessions");
    h.park();
    switcher().open();
    h.requested_frame();
    let first = node_centers_by_label(&h.root, SERVERS_FAB)[0];
    h.park();
    let settled = node_centers_by_label(&h.root, SERVERS_FAB)[0];
    (first - settled).hypot()
}

#[test]
fn reduce_motion_opens_at_card_scale_on_the_first_frame() {
    let _g = serial();
    let _ambient = setup();
    let moving = first_frame_shift(false);
    let snapped = first_frame_shift(true);
    assert!(
        moving > 1.0,
        "with motion the first frame is mid-flight: {moving}"
    );
    assert!(snapped < 1.0, "reduce-motion snaps the scale: {snapped}");
}
