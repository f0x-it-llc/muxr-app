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
//!
//! # The placeholder-shaping suite
//!
//! The last section drives no widgets at all: it shapes the two fields' real
//! placeholder strings with the fields' real [`TextStyle`] through a bare
//! [`TextContext`], and reads the per-glyph advances back off the resulting
//! glyph runs. That is the deliberate reproduction of a device defect
//! (`zelli.example.com: 5 0 0 5 1`) whose cause is a font stack resolving to
//! nothing — see `pieces::field_font_family` for the full mechanism.
//!
//! A bare `TextContext` is exactly what a `TextInputWidget` owns, so this is
//! the field's own shaping environment and not an approximation of it.

use std::any::Any;
use std::collections::BTreeMap;

use frust::Theme;
use frust::authoring::text::{FamilyName, FontFamily, GenericSlot, TextStyle};
use frust::{
    Align, Alignment, AnyView, Component, GetUntracked, NavigatorController, SizedBox, Stack, any,
    navigator,
};
use frust_core::accesskit::{Node, Rect, Role, Toggled};
use frust_core::{FrameTime, NamedKey, PaintScene, RenderRoot};
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{Point, Size};
use peniko::Color;

use muxr::connection::expect_connection_controller;
use muxr::features::connect::presentation::pages::connect_page;
use muxr::features::connect::presentation::pieces::{
    CONNECT_LABEL, PINNED_CAPTION, SERVER_URL_PLACEHOLDER, SUBTITLE, TOKEN_PLACEHOLDER,
    field_text_style, heading,
};
use muxr::routes::install_app_router;
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{RecScene, char_key, named_key, serial, settle_sized, setup, tap};

/// The test surface: wide enough for the CTA to sit clear of the edges, and
/// tall enough for the whole screen (~830px of pieces) to lay out unclipped —
/// see the module docs' local-helper note.
const SURFACE: Size = Size::new(800.0, 1200.0);

/// A point in the top-left corner: scrim on the QR sheet (whose panel is
/// bottom-anchored), and nothing at all on the screen itself.
const SCRIM_POINT: Point = Point::new(4.0, 4.0);

/// The URL the happy path types — the mock's own placeholder.
const VALID_URL: &str = SERVER_URL_PLACEHOLDER;
/// …and one that cannot be a dial target (no `:port`).
const INVALID_URL: &str = "not-an-endpoint";

/// `AppFailure::Network`'s user-facing message — what a rejected form toasts.
const NETWORK_TOAST: &str = "Connection problem — please try again.";

/// The trust selector's **two** rows (`pieces::trust_option_label`) — the
/// mock's own pair. `TrustMode::Pinned` is deliberately not one of them; see
/// `pieces::trust_selector`.
const SYSTEM_TRUST: &str = "Public domain (CA)";
const INSECURE_TRUST: &str = "Trust anyway";
/// The chip the insecure row carries (`glyph::badge` → `Role::Status`).
const INSECURE_CHIP: &str = "insecure";

/// The QR affordance's accessible name. Transcribed rather than imported (the
/// CTA's is `pieces::CONNECT_LABEL`, which *is* the run it paints and so has to
/// be the same string): this one is the button's *name*, and a test that
/// imported it could not catch it changing.
///
/// It used to be `▣` — the mark was the label, because the affordance was a
/// text run. That is the defect this round closed: the glyph is in none of the
/// bundled faces, so Android and iOS each drew a different mark for the app's
/// primary pairing affordance (and a screen reader read out the name of a
/// character). It is a vector now, named in words.
const QR_LABEL: &str = "Pair by QR code";

/// `.connect{padding:30px 22px}` — the layer's horizontal inset, transcribed
/// (the constant itself is private to `presentation::pages`). What the CTA
/// spans the width *between*.
const LAYER_PAD_X: f64 = 22.0;
/// The platform minimum tap target, in logical px — the floor the full-bleed
/// CTA has to clear.
const MIN_TAP_TARGET: f64 = 44.0;

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
    /// transition (and past an overlay's own enter/exit staging), at
    /// [`SURFACE`]: seed the clock, complete the transition during paint, then
    /// finalize it on the following rebuild. Returns the last frame's paint
    /// record.
    fn settle(&mut self) -> RecScene {
        settle_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
        )
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

    /// Every trust row's label, in tree order — the selector's whole offer.
    fn trust_rows(&self) -> Vec<String> {
        self.root
            .semantics()
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == Role::RadioButton)
            .filter_map(|(_, node)| node.label().map(str::to_string))
            .collect()
    }

    /// Whether any node in the tree carries `value` — how a plain `text` run
    /// (the pinned caption) is read back, since `text` reports its content as
    /// a `Role::Label` node's *value*.
    fn shows_value(&self, value: &str) -> bool {
        self.root
            .semantics()
            .nodes
            .iter()
            .any(|(_, node)| node.value() == Some(value))
    }

    /// Whether a `Role::Status` node (a `glyph::badge`) labelled `label` is on
    /// screen.
    fn shows_chip(&self, label: &str) -> bool {
        self.root
            .semantics()
            .nodes
            .iter()
            .any(|(_, node)| node.role() == Role::Status && node.label() == Some(label))
    }

    /// The bounds of the first node matching `pred` — the geometry read-back
    /// for chrome whose *size* is the assertion (the full-bleed CTA).
    fn bounds(&self, what: &str, pred: impl Fn(&Node) -> bool) -> Rect {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| pred(node))
            .and_then(|(_, node)| node.bounds())
            .unwrap_or_else(|| panic!("no bounds for {what}"))
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
fn the_selector_offers_the_mocks_two_rows_and_flags_the_insecure_one() {
    let _g = serial();
    let _ambient = setup();
    let harness = Harness::mount();

    assert_eq!(
        harness.trust_rows(),
        vec![SYSTEM_TRUST.to_string(), INSECURE_TRUST.to_string()],
        "the selector offers exactly the design's two rows, safest first — \
         `TrustMode::Pinned` is never hand-picked (see pieces::trust_selector)"
    );
    assert!(
        harness.shows_chip(INSECURE_CHIP),
        "the posture that skips certificate validation carries its warn chip"
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
    assert!(!harness.trust_selected(INSECURE_TRUST));

    harness.tap_node("the trust-anyway row", |node| {
        node.role() == Role::RadioButton && node.label() == Some(INSECURE_TRUST)
    });

    // The row is a controlled component like the `Radio` it replaced — it
    // never self-selects, so the row reading as selected after a rebuild means
    // the controller's signal moved.
    assert!(harness.trust_selected(INSECURE_TRUST));
    assert!(
        !harness.trust_selected(SYSTEM_TRUST),
        "the selection is exclusive"
    );
    assert!(
        harness.shows_chip(INSECURE_CHIP),
        "and the chip stays with the row it flags"
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
    // The posture it carried is one the selector deliberately does not offer
    // as a row, so the selector must render *honestly* rather than pretend:
    // neither row selected, and the caption naming where the posture came
    // from (see pieces::trust_selector's pinned-state note).
    assert!(
        !harness.trust_selected(SYSTEM_TRUST) && !harness.trust_selected(INSECURE_TRUST),
        "a pinned posture must not light a phantom selection"
    );
    assert!(
        harness.shows_value(PINNED_CAPTION),
        "…and says so instead: {:?}",
        harness.trust_rows()
    );
}

#[test]
fn switching_away_from_a_pinned_server_leaves_the_pinned_state() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    harness.tap_node("the pinned saved-server row", |node| {
        node.role() == Role::Label && node.value() == Some(PINNED_ENDPOINT)
    });
    harness.tap_node("the public-CA row", |node| {
        node.role() == Role::RadioButton && node.label() == Some(SYSTEM_TRUST)
    });

    assert!(
        harness.trust_selected(SYSTEM_TRUST),
        "tapping a row out of the pinned state simply selects that row"
    );
    assert!(
        !harness.shows_value(PINNED_CAPTION),
        "and the caption goes with the state it explained"
    );
}

// ---------------------------------------------------------------------------
// The connect flow
// ---------------------------------------------------------------------------

#[test]
fn the_cta_spans_the_layer_and_clears_the_minimum_tap_target() {
    let _g = serial();
    let _ambient = setup();
    let harness = Harness::mount();

    // `.btn-primary` is block-level: it fills the layer between its two 22px
    // paddings rather than hugging its label (Ed's round-2 device gate).
    let cta = harness.bounds("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });
    let layer = SURFACE.width - 2.0 * LAYER_PAD_X;
    assert!(
        (cta.width() - layer).abs() < 0.5,
        "the CTA spans {:.1}px of a {layer:.1}px layer",
        cta.width()
    );
    assert!(
        cta.height() >= MIN_TAP_TARGET,
        "the CTA is {:.1}px tall, under the {MIN_TAP_TARGET}px minimum",
        cta.height()
    );

    // …and it is still the *only* full-bleed button: the QR affordance keeps
    // its own natural size, so "the CTA" is unambiguous above.
    let fab = harness.bounds("the QR affordance", |node| {
        node.role() == Role::Button && node.label() == Some(QR_LABEL)
    });
    assert!(fab.width() < layer / 2.0, "the FAB did not stretch too");
}

/// The mark the QR affordance used to set as a *text run*, and which each
/// mobile platform's font-fallback chain resolved differently (or, on iPhone,
/// not at all). Nothing on this screen may paint it again.
const BANNED_GLYPH: &str = "\u{25a3}";

#[test]
fn the_qr_affordance_paints_a_vector_rather_than_a_font_glyph() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    let fab = harness.bounds("the QR affordance", |node| {
        node.role() == Role::Button && node.label() == Some(QR_LABEL)
    });
    let scene = harness.settle();

    // A `frust::icon` paints exactly one **filled path**, and nothing else on
    // Connect paints one at all: every other piece is rounded rects (swatches,
    // panels, the CTA bar) and stroked borders. So "the affordance is a
    // vector" is literally "there is a filled path, and it is inside the FAB".
    assert_eq!(
        scene.paths.len(),
        1,
        "the scan mark is the screen's one vector: {:?}",
        scene.paths
    );
    let (origin, size) = scene.paths[0];
    assert!(
        origin.x >= fab.x0 - 0.5
            && origin.y >= fab.y0 - 0.5
            && origin.x + size.width <= fab.x1 + 0.5
            && origin.y + size.height <= fab.y1 + 0.5,
        "the mark paints inside the FAB's own box: {origin:?}/{size:?} in \
         {fab:?}"
    );
    assert!(
        size.width > 0.0 && size.height > 0.0,
        "…and it is a real shape, not a collapsed path: {size:?}"
    );

    // The other half: no run anywhere on the screen carries the old mark, so
    // the glyph did not simply move somewhere else on the way past.
    let values: Vec<String> = harness
        .root
        .semantics()
        .nodes
        .iter()
        .filter_map(|(_, node)| node.value().map(str::to_string))
        .collect();
    assert!(
        !values.iter().any(|v| v.contains(BANNED_GLYPH)),
        "no text run may carry {BANNED_GLYPH:?} any more: {values:?}"
    );
    let labels: Vec<String> = harness
        .root
        .semantics()
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().map(str::to_string))
        .collect();
    assert!(
        !labels.iter().any(|l| l.contains(BANNED_GLYPH)),
        "…nor may any accessible name: {labels:?}"
    );
}

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

// ---------------------------------------------------------------------------
// The heading's centered subtitle
// ---------------------------------------------------------------------------

/// A phone-width surface for the heading alone — narrow enough that the
/// subtitle actually wraps, which is the whole point (at [`SURFACE`]'s 800px
/// it fits on one line and centering a single line proves nothing about the
/// second one).
const NARROW: Size = Size::new(260.0, 400.0);

/// Glyph size of the subtitle's own type role (`body_small` under Glyph =
/// caption/11), used to pick its runs out of a frame that also paints the
/// 15px title.
const SUBTITLE_SIZE: f32 = 11.0;

/// A paint target that records where each glyph landed: `(font_size, x, y)` in
/// absolute logical px, one entry per glyph.
#[derive(Default)]
struct GlyphScene {
    glyphs: Vec<(f32, f64, f64)>,
}

impl PaintScene for GlyphScene {
    fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
    fn draw_text(&mut self, _origin: Point, _text: &str) {}
    fn draw_glyph_run(&mut self, run: GlyphRun) {
        // A run's glyph positions are run-local; its transform places the run.
        let offset = run.transform.translation();
        for glyph in &run.glyphs {
            self.glyphs.push((
                run.font_size,
                offset.x + f64::from(glyph.x),
                offset.y + f64::from(glyph.y),
            ));
        }
    }
}

/// The subtitle's laid-out lines as `(leftmost x, rightmost x)` pairs, plus the
/// box the paragraph was given — the heading piece rendered alone at
/// [`NARROW`].
///
/// Lines are grouped by baseline: the paragraph draws one run per line, each at
/// its own y.
fn subtitle_lines() -> (Vec<(f64, f64)>, Rect) {
    let theme = Theme::glyph_baseline();
    let mut logic = move |_state: &mut ()| heading::<()>(&theme);
    let mut root: RenderRoot<(), AnyView<()>> = RenderRoot::new();
    let mut state = ();
    let mut tcx = TextContext::new();
    root.rebuild(&mut logic, &mut state);
    root.layout_with_text(NARROW, &mut tcx as &mut dyn Any);
    let mut scene = GlyphScene::default();
    root.paint(&mut scene, FrameTime::ZERO);

    let mut by_line: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
    for (size, x, y) in scene.glyphs {
        if (size - SUBTITLE_SIZE).abs() > f32::EPSILON {
            continue;
        }
        let line = by_line.entry(y.round() as i64).or_insert((x, x));
        line.0 = line.0.min(x);
        line.1 = line.1.max(x);
    }

    let box_ = root
        .semantics()
        .nodes
        .iter()
        .find(|(_, node)| node.value() == Some(SUBTITLE))
        .and_then(|(_, node)| node.bounds())
        .expect("the subtitle contributes one Role::Label node carrying its run");
    (by_line.into_values().collect(), box_)
}

#[test]
fn the_subtitle_centers_every_line_it_wraps_onto() {
    // The device defect: `.sub` is `text-align:center`, but `TextView` has no
    // paragraph alignment and `TextContext::layout` hardcodes
    // `parley::Alignment::Start`, so under the old `Align(CENTER, text(..))`
    // treatment only the *block* was centered — the second line hugged the
    // leading edge. See `chrome::centered_paragraph`.
    let (lines, box_) = subtitle_lines();
    assert!(
        lines.len() >= 2,
        "the subtitle must actually wrap at {NARROW:?} for this to mean \
         anything, got {} line(s)",
        lines.len()
    );

    for (index, (first, last)) in lines.iter().enumerate() {
        let left = first - box_.x0;
        let right = box_.x1 - last;
        // `last` is the final glyph's *pen* position, so `right` runs one
        // advance long — hence a tolerance of one em rather than a hairline.
        // A start-aligned line would read `left == 0` against a `right` of the
        // whole leftover (~100px here), which this separates cleanly.
        assert!(
            (left - right).abs() <= f64::from(SUBTITLE_SIZE),
            "line {index} sits {left:.1}px from the left edge and {right:.1}px \
             from the right — it is not centered"
        );
    }
}

// ---------------------------------------------------------------------------
// Placeholder shaping — the digit-spacing defect
// ---------------------------------------------------------------------------

/// How far apart two advances may be and still count as "the same width", in
/// logical px. Generous: this is separating ~7px letters from a ~13px full-em
/// digit, not measuring hinting.
const ADVANCE_EPSILON: f64 = 0.75;

/// Every consecutive glyph advance in `text` shaped at `style`, flattened
/// across runs, in visual order.
///
/// The last entry is the final glyph's own advance, taken from the laid-out
/// width — a run-splitting fallback would otherwise hide inside the last
/// glyph, which has no successor to be measured against.
fn advances(text: &str, style: &TextStyle) -> Vec<f64> {
    // A bare context is precisely what `TextInputWidget` builds for itself
    // (`TextInputView::build` → `TextContext::new()`), and nothing ever
    // registers the app's fonts into it — see the module docs.
    let mut cx = TextContext::new();
    let layout = cx.layout(text, style, None);
    let mut xs: Vec<f64> = layout
        .to_scene_runs(Point::ZERO)
        .iter()
        .flat_map(|run| run.glyphs.iter().map(|glyph| f64::from(glyph.x)))
        .collect();
    xs.push(layout.size().width);
    xs.windows(2).map(|pair| pair[1] - pair[0]).collect()
}

/// The advances of just the characters of `text` that satisfy `pick`.
fn advances_where(text: &str, style: &TextStyle, pick: impl Fn(char) -> bool) -> Vec<f64> {
    text.chars()
        .zip(advances(text, style))
        .filter(|(c, _)| pick(*c))
        .map(|(_, advance)| advance)
        .collect()
}

/// The style both fields actually shape with.
fn field_style() -> TextStyle {
    field_text_style(&Theme::glyph_baseline(), Color::WHITE)
}

#[test]
fn the_server_url_placeholder_never_advances_its_digits_a_full_em() {
    // The device defect, stated as an inequality: the digits of
    // `zelli.example.com:50051` rendered one full em apart (13.0px at the
    // 13px body role) while the letters around them stayed ~7px, which reads
    // as `zelli.example.com: 5 0 0 5 1`. A digit advancing its own font size
    // is the signature of the string having been split across a fallback font
    // that the primary stack failed to cover.
    let style = field_style();
    let em = f64::from(style.size);
    let digits = advances_where(SERVER_URL_PLACEHOLDER, &style, |c| c.is_ascii_digit());

    assert_eq!(digits.len(), 5, "50051 is five digits");
    for advance in &digits {
        assert!(
            *advance < em - ADVANCE_EPSILON,
            "a digit advanced {advance:.3}px at a {em}px font size — the \
             placeholder's font stack resolved to nothing and parley fell \
             back per cluster (see pieces::field_font_family)"
        );
    }
}

#[test]
fn the_server_url_placeholder_shapes_as_one_run_with_its_letters() {
    // The positive half: digits and letters come from the *same* font, so
    // their advances are drawn from one set of metrics. This is what makes
    // the run a single legible string rather than two interleaved ones.
    let style = field_style();
    let digits = advances_where(SERVER_URL_PLACEHOLDER, &style, |c| c.is_ascii_digit());
    let widest_letter = advances_where(SERVER_URL_PLACEHOLDER, &style, |c| c.is_ascii_alphabetic())
        .into_iter()
        .fold(0.0_f64, f64::max);

    for advance in digits {
        assert!(
            advance <= widest_letter + ADVANCE_EPSILON,
            "a digit advanced {advance:.3}px against a widest letter of \
             {widest_letter:.3}px — the digits are on a different font"
        );
    }
}

#[test]
fn the_token_placeholder_stays_the_control_it_was_on_device() {
    // The all-letter placeholder rendered correctly on the device that showed
    // the defect. Keeping it asserted means a regression that broke *both*
    // fields could not be mistaken for the digit-only one.
    let style = field_style();
    let widths = advances(TOKEN_PLACEHOLDER, &style);
    let em = f64::from(style.size);
    assert_eq!(widths.len(), TOKEN_PLACEHOLDER.chars().count());
    for advance in widths {
        assert!(advance < em, "{advance:.3}px at a {em}px font size");
    }
}

#[test]
fn a_stack_that_resolves_to_nothing_is_what_blows_the_digits_up() {
    // The counter-example that gives the three tests above their teeth: the
    // *same* string, shaped with a stack that cannot resolve, splits into more
    // than one run and advances its digits a full em. Without this, the
    // assertions above would pass on any machine whose generic `monospace`
    // happens to resolve — which is every machine except the one that showed
    // the bug.
    let broken = TextStyle {
        family: FontFamily::Named(vec!["No Such Family 1234".to_string()]),
        ..field_style()
    };
    let em = f64::from(broken.size);
    let digits = advances_where(SERVER_URL_PLACEHOLDER, &broken, |c| c.is_ascii_digit());
    assert!(
        digits
            .iter()
            .all(|advance| *advance >= em - ADVANCE_EPSILON),
        "expected the unresolvable stack to advance digits a full {em}px em, \
         got {digits:?} — if this stops holding, parley's per-cluster fallback \
         changed and `field_font_family`'s workaround should be re-measured"
    );

    // …and the fix is one appended, resolvable generic, nothing more.
    let rescued = TextStyle {
        family: FontFamily::NamedWithGeneric(vec![
            FamilyName::Named("No Such Family 1234".to_string()),
            FamilyName::Generic(GenericSlot::SansSerif),
        ]),
        ..field_style()
    };
    let digits = advances_where(SERVER_URL_PLACEHOLDER, &rescued, |c| c.is_ascii_digit());
    assert!(
        digits.iter().all(|advance| *advance < em - ADVANCE_EPSILON),
        "a resolvable terminal generic must collapse the fallback split, got \
         {digits:?}"
    );
}
