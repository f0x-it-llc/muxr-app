//! Headless behavior tests for the real Connect screen (`src/features/connect`)
//! — no GPU, no window: a `RenderRoot` over the screen itself, driven through
//! `tests/support`.
//!
//! # The harness shape, and why it is not just `MuxrApp`
//!
//! `NavigatorWidget` contributes no `Widget::semantics` and forwards none of its
//! pages' (FINDINGS #23), so **a screen mounted inside a navigator is invisible
//! to the accessibility tree** — the route-level harness `tests/routes.rs` uses
//! can only count glyph runs. This suite instead mounts a `Stack` of
//!
//! 1. [`connect_page`] — the real route builder, so the whole
//!    `Component`/`use_controller`/pieces stack is under test, *outside* the
//!    navigator and therefore fully addressable by role/label/value; and
//! 2. a [`navigator`] over the same [`NavigatorController`] the screen pushes
//!    through, whose base page is a deliberately **zero-size** child.
//!
//! The navigator sits on top, so anything it *does* have on its stack (the QR
//! sheet, a pushed `/sessions`) owns input, while its empty base page ignores
//! every pointer and lets the screen underneath receive it (`route_event`
//! walks a `Stack` topmost-first and only stops on `Handled`). That gives one
//! fixture that can both read semantics and observe real navigation
//! (`NavigatorController::depth` is only published by a mounted navigator).
//!
//! `MuxrApp::init` still runs, unbuilt: it is the real composition root, and
//! what publishes `features::connect::di`'s repository plus the app-scoped
//! `ConnectionController` the screen resolves in its own `init`. Not *building*
//! it is deliberate — the cold-start splash claim is spent on a launch's first
//! build, so this suite never plays a splash. Because the fixture drives its
//! **own** navigator (see above), it then calls
//! [`muxr::routes::install_app_router`] over that controller, re-pointing the
//! app's one path-pushing handle at it: the screen therefore pushes through the
//! real route table and the real session guard, and this harness observes the
//! result as a depth change.
//!
//! # Surface size
//!
//! The Connect screen is ~830px tall, so at `support`'s default 800×600 its
//! last saved-server row would fall outside the viewport and be untappable —
//! every frame here goes through `support`'s `*_sized` helpers at [`SURFACE`]
//! instead.
//!
//! # What is asserted where
//!
//! Route *identity* after a successful connect (`/sessions` resolving to the
//! Sessions page rather than the error page) is `tests/routes.rs`'s job — it
//! can assert `Router::resolve` directly. Here a push is observed as a depth
//! change plus the connection facts that came with it.

use frust::{
    Align, Alignment, AnyView, Component, GetUntracked, NavigatorController, SizedBox, Stack, any,
    navigator,
};
use frust_core::accesskit::{Node, Role, Toggled};
use frust_core::{NamedKey, RenderRoot};
use frust_text::TextContext;
use kurbo::{Point, Size};

use muxr::connection::expect_connection_controller;
use muxr::features::connect::presentation::pages::connect_page;
use muxr::routes::install_app_router;
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{char_key, named_key, serial, settle_sized, setup, tap};

/// The test surface: wide enough for the CTA to sit clear of the edges, and
/// tall enough for the whole screen (~830px of pieces) to lay out unclipped —
/// see the module docs' local-helper note.
const SURFACE: Size = Size::new(800.0, 1200.0);

/// A point in the top-left corner: scrim on the QR sheet (whose panel is
/// bottom-anchored), and nothing at all on the screen itself.
const SCRIM_POINT: Point = Point::new(4.0, 4.0);

/// The URL the happy path types — the mock's own placeholder.
const VALID_URL: &str = "zelli.example.com:50051";
/// …and one that cannot be a dial target (no `:port`).
const INVALID_URL: &str = "not-an-endpoint";

/// `AppFailure::Network`'s user-facing message — what a rejected form toasts.
const NETWORK_TOAST: &str = "Connection problem — please try again.";

/// The trust-selector rows' labels (`pieces::trust_option_label`).
const SYSTEM_TRUST: &str = "Public domain (CA)";
const PINNED_TRUST: &str = "Pinned certificate";

/// The CTA's and the QR affordance's labels.
const CONNECT_LABEL: &str = "→ connect";
const QR_LABEL: &str = "▣";

/// The seeded store's third row — pinned and read-only, so tapping it changes
/// *both* the URL and the trust mode away from their defaults.
const PINNED_ENDPOINT: &str = "herdr.internal:7443";

/// An endpoint the seeded store does **not** hold, so a connect to it is
/// eligible for a credential offer (unlike [`VALID_URL`], which is row 1).
const UNKNOWN_URL: &str = "fresh.example:50051";
/// [`UNKNOWN_URL`]'s host — the label a new rig's credential offer carries.
const UNKNOWN_HOST: &str = "fresh.example";

/// The boxed app-logic closure `RenderRoot::rebuild` drives.
type Logic = Box<dyn FnMut(&mut AppState) -> AnyView<AppState>>;

/// The navigator's base page: full-bleed for layout (so a pushed sheet gets the
/// whole viewport), zero-size for hit-testing (so every pointer event falls
/// through to the screen below). `Align` fills each bounded axis while leaving
/// its child at its own size — see the module docs.
fn empty_base_page() -> AnyView<AppState> {
    any(Align(
        Alignment::CENTER,
        SizedBox::<AppState>(Some(0.0), Some(0.0)),
    ))
}

/// The Connect screen plus a live navigator over the same controller.
struct Harness {
    nav: NavigatorController<AppState>,
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
}

impl Harness {
    /// Run the real composition root's `init` (DI + the app-scoped connection
    /// controller), re-point the app's nav handle at this fixture's own
    /// navigator, then mount the screen and render its first settled frame.
    fn mount() -> Self {
        let state = MuxrApp.init();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        // The screen resolves `routes::app_nav()` in its own `init`; this makes
        // that handle the app's real router over *this* navigator, guard and
        // all (see the module docs).
        let _router = install_app_router(&nav, expect_connection_controller());
        let nav_for_navigator = nav.clone();
        let nav_for_screen = nav.clone();
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            any(Stack(vec![
                connect_page(nav_for_screen.clone()),
                any(navigator(&nav_for_navigator, empty_base_page)),
            ]))
        });
        let mut harness = Harness {
            nav,
            root: RenderRoot::new(),
            logic,
            state,
            tcx: TextContext::new(),
        };
        harness.settle();
        harness
    }

    /// Drive a pushed/popped page past the navigator's input-blocking
    /// transition, at [`SURFACE`]: seed the clock, complete the transition
    /// during paint, then finalize it on the following rebuild.
    fn settle(&mut self) {
        settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
        );
    }

    /// A full tap at `point`.
    fn tap(&mut self, point: Point) {
        tap(&mut self.root, &mut self.state, point);
    }

    /// Tap the first node matching `pred`, panicking with `what` if the tree
    /// carries none.
    fn tap_node(&mut self, what: &str, pred: impl Fn(&Node) -> bool) {
        let point = self
            .center(&pred)
            .unwrap_or_else(|| panic!("nothing to tap for {what}"));
        self.tap(point);
        self.settle();
    }

    /// The center of the first node matching `pred`.
    fn center(&self, pred: &impl Fn(&Node) -> bool) -> Option<Point> {
        self.root.semantics().nodes.iter().find_map(|(_, node)| {
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

    /// Type `text` into whatever holds focus, one already-resolved character
    /// event at a time (the shape a platform delivers).
    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.root.event(&mut self.state, &char_key(&c.to_string()));
        }
        self.settle();
    }

    /// Every `TextInput` node's current value, in tree order (0 = server URL,
    /// 1 = login token).
    fn field_values(&self) -> Vec<String> {
        self.root
            .semantics()
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == Role::TextInput)
            .filter_map(|(_, node)| node.value().map(str::to_string))
            .collect()
    }

    /// Whether the trust row labelled `label` reads as selected.
    fn trust_selected(&self, label: &str) -> bool {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::RadioButton && node.label() == Some(label))
            .map(|(_, node)| node.is_selected() == Some(true))
            .unwrap_or_else(|| panic!("no trust row labelled {label:?}"))
    }

    /// Whether the remember toggle reads as on — the `Role::Switch` node
    /// `chrome::switch_semantics` contributes over the composed art.
    fn remember_on(&self) -> bool {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::Switch)
            .map(|(_, node)| node.toggled() == Some(Toggled::True))
            .expect("the remember toggle contributes a Switch node")
    }
}

/// Focus the server-URL field and type `url` into it.
///
/// Asserting on the value *after* a rebuild is what makes this a round-trip
/// through the controller rather than a widget-local check: `TextInput` is a
/// controlled component, so a rebuild re-applies the view's value — text that
/// the controller never stored would be wiped by the very next frame.
fn type_url(harness: &mut Harness, url: &str) {
    harness.tap_node("the server URL field", |node| {
        node.role() == Role::TextInput
    });
    harness.type_text(url);
}

// ---------------------------------------------------------------------------
// The form's fields
// ---------------------------------------------------------------------------

#[test]
fn typing_a_server_url_survives_the_rebuild_it_wakes() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    assert_eq!(
        harness.field_values(),
        vec![String::new(), String::new()],
        "both fields start empty"
    );

    type_url(&mut harness, VALID_URL);

    assert_eq!(
        harness.field_values(),
        vec![VALID_URL.to_string(), String::new()],
        "the URL landed on the controller's signal, and only that field changed"
    );
}

#[test]
fn selecting_a_trust_mode_sticks() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    assert!(
        harness.trust_selected(SYSTEM_TRUST),
        "the form starts on the safe posture"
    );
    assert!(!harness.trust_selected(PINNED_TRUST));

    harness.tap_node("the pinned-certificate row", |node| {
        node.role() == Role::RadioButton && node.label() == Some(PINNED_TRUST)
    });

    // `Radio` is a controlled component — it never self-selects, so the row
    // reading as selected after a rebuild means the controller's signal moved.
    assert!(harness.trust_selected(PINNED_TRUST));
    assert!(
        !harness.trust_selected(SYSTEM_TRUST),
        "the selection is exclusive"
    );
}

// ---------------------------------------------------------------------------
// The saved-server list
// ---------------------------------------------------------------------------

#[test]
fn tapping_a_saved_server_refills_the_url_and_its_trust_mode() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    harness.tap_node("the pinned saved-server row", |node| {
        node.role() == Role::Label && node.value() == Some(PINNED_ENDPOINT)
    });

    assert_eq!(
        harness.field_values().first().map(String::as_str),
        Some(PINNED_ENDPOINT),
        "the row filled the server-URL field"
    );
    assert!(
        harness.trust_selected(PINNED_TRUST),
        "…and carried its own trust posture with it"
    );
}

// ---------------------------------------------------------------------------
// The connect flow
// ---------------------------------------------------------------------------

#[test]
fn an_invalid_url_toasts_and_never_navigates() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    type_url(&mut harness, INVALID_URL);
    // Anything the mount itself may have queued is not this assertion's
    // business (the queue only ever grows — see `ui::toasts`' queue policy).
    app_toasts().clear();

    harness.tap_node("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });

    assert_eq!(
        app_toasts().pending_untracked(),
        vec![NETWORK_TOAST.to_string()],
        "the rejection surfaced as the failure's user message, not its detail"
    );
    assert!(
        !app_toasts().pending_untracked()[0].contains(INVALID_URL),
        "…and never echoes what was typed"
    );
    assert_eq!(harness.nav.depth(), 1, "nothing was pushed");
    assert!(
        !expect_connection_controller().connected.get_untracked(),
        "and no connection was opened"
    );
}

#[test]
fn a_valid_url_connects_and_lands_on_sessions() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    type_url(&mut harness, VALID_URL);
    app_toasts().clear();

    harness.tap_node("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });

    let connection = expect_connection_controller();
    assert!(
        connection.connected.get_untracked(),
        "the mock connection opened"
    );
    let info = connection
        .server_info
        .get_untracked()
        .expect("a connected controller carries its server facts");
    assert_eq!(
        info.endpoint, VALID_URL,
        "the form's own endpoint was dialed"
    );
    assert_eq!(
        info.tls_label, "System trust",
        "…under the form's own trust posture"
    );
    assert_eq!(harness.nav.depth(), 2, "and the screen pushed a page");
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "a clean connect raises nothing"
    );
}

// ---------------------------------------------------------------------------
// The remember toggle
// ---------------------------------------------------------------------------

#[test]
fn remembering_a_new_server_offers_its_credential() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    assert!(harness.remember_on(), "the form starts remembering");
    type_url(&mut harness, UNKNOWN_URL);
    harness.tap_node("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });

    assert_eq!(
        expect_connection_controller()
            .credential_offer
            .get_untracked(),
        Some(UNKNOWN_HOST.to_string()),
        "a remembered, not-yet-saved rig offers its credential to Sessions"
    );
}

#[test]
fn toggling_remember_off_suppresses_the_credential_offer() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    harness.tap_node("the remember toggle", |node| node.role() == Role::Switch);
    assert!(
        !harness.remember_on(),
        "the toggle is a control, not art: the press moved the controller's \
         signal and the rebuild reflected it"
    );

    type_url(&mut harness, UNKNOWN_URL);
    harness.tap_node("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });

    assert!(
        expect_connection_controller().connected.get_untracked(),
        "the connect itself still went through"
    );
    assert_eq!(
        expect_connection_controller()
            .credential_offer
            .get_untracked(),
        None,
        "…but nothing was offered for saving"
    );
}

// ---------------------------------------------------------------------------
// The QR sheet
// ---------------------------------------------------------------------------

#[test]
fn the_qr_sheet_opens_and_dismisses() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    harness.tap_node("the QR affordance", |node| {
        node.role() == Role::Button && node.label() == Some(QR_LABEL)
    });
    assert_eq!(
        harness.nav.depth(),
        2,
        "the sheet is pushed as its own page"
    );

    // A scrim tap: the sheet is the navigator's top page and the navigator sits
    // above the screen, so the press reaches the sheet's modal barrier rather
    // than the form underneath it.
    harness.tap(SCRIM_POINT);
    harness.settle();
    harness.settle();

    assert_eq!(harness.nav.depth(), 1, "a scrim tap dismisses it");
    assert!(
        !expect_connection_controller().connected.get_untracked(),
        "and the placeholder sheet connects nothing"
    );
}

#[test]
fn the_qr_sheet_also_dismisses_on_escape() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    harness.tap_node("the QR affordance", |node| {
        node.role() == Role::Button && node.label() == Some(QR_LABEL)
    });
    // The scaffold claims focus on the press that opened it, so `Escape` has a
    // chain to travel (`ui::sheet`'s dismissal contract).
    harness.tap(Point::new(SURFACE.width / 2.0, SURFACE.height - 40.0));
    harness
        .root
        .event(&mut harness.state, &named_key(NamedKey::Escape));
    harness.settle();
    harness.settle();

    assert_eq!(harness.nav.depth(), 1);
}
