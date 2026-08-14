//! Headless behavior tests for the real Connect screen (`src/features/connect`)
//! — no GPU, no window: a `RenderRoot` over the screen itself, driven through
//! `tests/support`.
//!
//! # The harness shape, and why it is not just `MuxrApp`
//!
//! `NavigatorWidget` contributes no `Widget::semantics` and forwards none of its
//! pages', so **a screen mounted inside a navigator is invisible to the
//! accessibility tree** — the route-level harness `tests/routes.rs` uses can only
//! count glyph runs. This suite instead mounts a `Stack` of
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
//! [`muxr::routes::install_app_router`] over that controller — as both the
//! route navigator and the root overlay host, where production has two
//! (`muxr::AppState::nav` and `AppState::overlays`, the QR sheet being a page of
//! the latter) — re-pointing the app's one path-pushing handle at it: the screen
//! therefore pushes through the real route table and the real session guard, and
//! this harness observes the result as a depth change.
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
//! nothing — see `widgets::fields`' `field_font_family` for the mechanism.
//!
//! A bare `TextContext` is exactly what a `TextInputWidget` owns, so this is
//! the field's own shaping environment and not an approximation of it.

use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use frust::Theme;
use frust::authoring::text::{FamilyName, FontFamily, GenericSlot, TextStyle};
use frust::{
    Align, Alignment, AnyView, Component, Get, GetUntracked, NavigatorController, Set, SizedBox,
    Stack, WindowMetrics, any, navigator, provide_context,
};
use frust_core::accesskit::{Node, Rect, Role, Toggled};
use frust_core::{FrameTime, NamedKey, PaintScene, RenderRoot, WindowInsets};
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{Point, Size};
use peniko::Color;

use muxr::connection::{
    ConnectTarget, ConnectionController, TrustMode, expect_connection_controller,
};
use muxr::features::connect::data::repositories::MockCredentialService;
use muxr::features::connect::data::repositories::mock_saved_servers::MockSavedServersRepository;
use muxr::features::connect::di::{expect_saved_servers, mock_credentials};
use muxr::features::connect::domain::entities::{
    CredentialStatus, PairingPayload, SavedServer, epoch_millis,
};
use muxr::features::connect::domain::repositories::SavedServersRepository;
use muxr::features::connect::domain::use_cases::parse_pairing::parse_pairing_uri;
use muxr::features::connect::presentation::controller::{ConnectController, CredentialOffer};
use muxr::features::connect::presentation::pages::{connect_layout, connect_page};
use muxr::features::connect::presentation::widgets::badge::{SUBTITLE, heading};
use muxr::features::connect::presentation::widgets::cta::CONNECT_LABEL;
use muxr::features::connect::presentation::widgets::fields::{
    SERVER_URL_PLACEHOLDER, TOKEN_PLACEHOLDER, field_text_style,
};
use muxr::features::connect::presentation::widgets::pairing::{
    PAIRING_CAPTION, PINNED_CONNECT_LABEL, READ_ONLY_CHIP,
};
use muxr::features::connect::presentation::widgets::qr_sheet::{NO_CAMERA, qr_sheet_body};
use muxr::features::connect::presentation::widgets::trust::{PAIRED_CAPTION, PINNED_CAPTION};
use muxr::features::connect::presentation::widgets::{Live, connect_pieces, connect_pieces_with};
use muxr::features::settings::data::repositories::MockAuthTokensRepository;
use muxr::routes::{app_nav, install_app_router};
use muxr::ui::palette::Palette;
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, char_key, frame_sized, named_key, node_labels, serial, settle_sized, setup,
    tap,
};

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

/// The trust selector's **two** rows (`widgets::trust`'s `trust_option_label`) — the
/// mock's own pair. `TrustMode::Pinned` is deliberately not one of them; see
/// `widgets::trust::trust_selector`.
const SYSTEM_TRUST: &str = "Public domain (CA)";
const INSECURE_TRUST: &str = "Trust anyway";
/// The chip the insecure row carries (`glyph::badge` → `Role::Status`).
const INSECURE_CHIP: &str = "insecure";

/// The QR affordance's accessible name. Transcribed rather than imported (the
/// CTA's is `widgets::cta::CONNECT_LABEL`, which *is* the run it paints and so has to
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
/// The content column's max width past `crate::responsive::WIDE_BREAKPOINT_PX`
/// (the responsive width-clamp pass), transcribed (the constant itself is
/// private to `presentation::pages`).
const CONNECT_CLAMP_WIDTH: f64 = 520.0;

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

/// [`Harness::mount`], first publishing `metrics` into context when `Some` —
/// the shape `muxr`'s pages read via `use_context::<WindowMetrics>()` for the
/// responsive width-clamp pass. `None` leaves context unset,
/// exactly the state every other test in this suite mounts under (no shell
/// has published yet), so this helper is purely additive.
fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Harness {
    if let Some(metrics) = metrics {
        provide_context(metrics);
    }
    Harness::mount()
}

/// A [`WindowMetrics`] whose size is [`SURFACE`] itself — a shell reporting
/// the same window this harness actually paints at, so a clamp's centering
/// math is checkable against real recorded geometry.
fn metrics_at(size: Size) -> WindowMetrics {
    WindowMetrics::new(size, 1.0, WindowInsets::default())
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
        Self::mount_seeded(|| {})
    }

    /// [`Harness::mount`] with `seed` run in between: after `MuxrApp::init` has
    /// published this feature's stores, and **before** the screen's own
    /// controller reads them.
    ///
    /// The window matters — `init` constructs fresh repositories every time it
    /// runs, so a row written before it is discarded, and the screen's
    /// controller loads its list once at construction, so a row written after
    /// the mount is invisible until something reloads.
    fn mount_seeded(seed: impl FnOnce()) -> Self {
        let state = MuxrApp.init();
        seed();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        // The screen resolves `routes::app_nav()` in its own `init`; this makes
        // that handle the app's real router over *this* navigator, guard and
        // all (see the module docs).
        let _router = install_app_router(&nav, &nav, expect_connection_controller());
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

    /// Drive the UI-thread task queue — and this fixture's own settled frame —
    /// until `done`, or fail.
    ///
    /// [`pump_until_done`]'s twin for the cases that also have to keep
    /// *rendering*: `support::pump_until` frames at its own default surface,
    /// where every read in this suite depends on [`SURFACE`] (see the module
    /// docs' surface note).
    fn pump_until(&mut self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let runtime =
            frust_reactive::ReactiveRuntime::get().expect("setup() installed the runtime");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done(self) {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            runtime.pump_local();
            std::thread::sleep(std::time::Duration::from_millis(1));
            self.settle();
        }
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

    /// Every `TextInput`/`PasswordInput` node's current value, in tree order
    /// (0 = server URL, 1 = login token). The token field obscures (#20
    /// adopted), so its node reports as `Role::PasswordInput` rather than
    /// `Role::TextInput` and its value is the *masked* mirror — an empty
    /// value masks to itself, so this still reads `""` for an untyped token.
    fn field_values(&self) -> Vec<String> {
        self.root
            .semantics()
            .nodes
            .iter()
            .filter(|(_, node)| matches!(node.role(), Role::TextInput | Role::PasswordInput))
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
    /// `chrome::switch_semantics` contributes over the real `frust::switch`
    /// (the widget's own semantics carry no label, so the wrapper still
    /// supplies the one this node reads).
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
fn the_token_field_masks_its_glyphs_but_stays_editable() {
    // The login token field obscures. `TextInputWidget`
    // reports `Role::PasswordInput` (not `TextInput`) and publishes the
    // *masked* mirror as its accessible value — the harness's only window
    // onto rendered text content (`a recording PaintScene sees glyph runs,
    // not strings` — the module docs above), so this is the mask's
    // observable proof: the real text never surfaces as a readable value.
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    harness.tap_node("the login token field", |node| {
        node.role() == Role::PasswordInput
    });
    harness.type_text("s3cr3t");

    let masked = harness
        .root
        .semantics()
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::PasswordInput)
        .and_then(|(_, node)| node.value().map(str::to_string))
        .expect("the token field contributes a PasswordInput node");
    assert_eq!(
        masked,
        "\u{2022}".repeat("s3cr3t".chars().count()),
        "the reported value is one mask bullet per typed character, not the \
         real text"
    );

    // …and the round trip still lands on the controller: retyping the same
    // masked field must not have silently made it read-only.
    assert_eq!(
        harness.field_values(),
        vec![String::new(), masked],
        "the live token field stays editable while obscured"
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
         `TrustMode::Pinned` is never hand-picked (see widgets::trust::trust_selector)"
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
    // from (see widgets::trust::trust_selector's pinned-state note).
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
    // paddings rather than hugging its label.
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

// ---------------------------------------------------------------------------
// Responsive width-clamp pass
// ---------------------------------------------------------------------------

#[test]
fn wide_metrics_clamp_and_center_the_content_column() {
    let _g = serial();
    let _ambient = setup();
    let harness = mount_with_metrics(Some(metrics_at(SURFACE)));

    // The CTA is the layer's own full-bleed piece (see the test above), so its
    // box is the cheapest honest read on where the *clamped* layer itself
    // sits: centered, `CONNECT_CLAMP_WIDTH` wide, minus the layer's own
    // `LAYER_PAD_X` padding on each side.
    let cta = harness.bounds("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });
    let margin = (SURFACE.width - CONNECT_CLAMP_WIDTH) / 2.0;
    assert!(
        (cta.x0 - (margin + LAYER_PAD_X)).abs() < 0.5,
        "the layer centers within the surface: CTA left edge at {:.1}, expected {:.1}",
        cta.x0,
        margin + LAYER_PAD_X
    );
    assert!(
        (cta.width() - (CONNECT_CLAMP_WIDTH - 2.0 * LAYER_PAD_X)).abs() < 0.5,
        "the CTA spans the clamped layer, not the whole surface: {:.1}",
        cta.width()
    );
}

#[test]
fn narrow_metrics_leave_the_layout_unclamped_like_no_metrics_at_all() {
    let _g = serial();
    let _ambient = setup();
    // Explicit narrow metrics, even though the harness still paints at the
    // full-width `SURFACE` — proving the branch keys on the *published*
    // width, not the paint surface, and lands exactly where the no-metrics
    // baseline (`the_cta_spans_the_layer_and_clears_the_minimum_tap_target`)
    // already does.
    let harness = mount_with_metrics(Some(metrics_at(Size::new(400.0, SURFACE.height))));

    let cta = harness.bounds("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });
    let layer = SURFACE.width - 2.0 * LAYER_PAD_X;
    assert!(
        (cta.width() - layer).abs() < 0.5,
        "narrow metrics take the same unclamped branch as no metrics: {:.1}",
        cta.width()
    );
    assert!((cta.x0 - LAYER_PAD_X).abs() < 0.5, "{:?}", cta);
}

/// The CTA's box in a standalone mount of the **splash's** assembly path:
/// `connect_layout(connect_pieces(..))` inside a `Stack`, which is exactly what
/// `crate::splash_overlay` hands the sequence and how the sequence hosts it
/// (its own wrapper is a paint-time entrance and changes no geometry). The
/// `Stack` matters: it hands its children a *bounded* height, where the live
/// screen's `scroll_view` hands an unbounded one — the difference
/// `pages::CLAMP_ALIGNMENT` exists for. Laid out at [`SURFACE`], under whatever
/// `WindowMetrics` the caller has already provided.
fn splash_cta_bounds(saved: &[SavedServer]) -> Rect {
    let theme = Theme::glyph_baseline();
    let saved = saved.to_vec();
    let mut logic = move |_state: &mut ()| {
        any(Stack(vec![connect_layout(connect_pieces::<()>(
            &theme, &saved,
        ))]))
    };
    let mut root: RenderRoot<(), AnyView<()>> = RenderRoot::new();
    let mut state = ();
    let mut tcx = TextContext::new();
    frame_sized(&mut root, &mut logic, &mut state, &mut tcx, SURFACE);
    root.semantics()
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Button && node.label() == Some(CONNECT_LABEL))
        .and_then(|(_, node)| node.bounds())
        .expect("the splash's assembly paints the CTA")
}

#[test]
fn the_splash_assembly_clamps_exactly_like_the_live_screen() {
    let _g = serial();
    let _ambient = setup();
    // The width jump: the clamp used to live in the screen's own `build`, so
    // the splash assembled edge to edge on any window at or past the
    // breakpoint and the whole form snapped inward at the handoff. It is
    // `connect_layout`'s now — the one function both renderings assemble
    // through — so this asserts the two land on the same horizontal box rather
    // than that each happens to satisfy the same arithmetic.
    let harness = mount_with_metrics(Some(metrics_at(SURFACE)));
    let live = harness.bounds("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });
    let splash = splash_cta_bounds(&expect_saved_servers().list().expect("the mock lists"));

    assert!(
        (splash.x0 - live.x0).abs() < EPSILON && (splash.x1 - live.x1).abs() < EPSILON,
        "the splash's assembly and the live screen must occupy the same \
         horizontal box on a wide window: splash {splash:?} vs live {live:?}"
    );
    // …and it really is the clamped one, not both being unclamped.
    let margin = (SURFACE.width - CONNECT_CLAMP_WIDTH) / 2.0;
    assert!(
        (splash.x0 - (margin + LAYER_PAD_X)).abs() < EPSILON,
        "the splash's own copy is centered inside the clamp: {splash:?}"
    );

    // The clamp centers **horizontally only**: the assembly is hosted by a
    // height-bounded `Stack`, so a vertically-centered alignment would drop the
    // whole column down the screen and trade the width jump for a taller one.
    // Wide and narrow must therefore agree on where the column starts.
    provide_context(metrics_at(Size::new(400.0, SURFACE.height)));
    let narrow = splash_cta_bounds(&[]);
    assert!(
        (splash.y0 - narrow.y0).abs() < EPSILON,
        "clamping must not move the column vertically: wide {splash:?} vs \
         narrow {narrow:?}"
    );
}

#[test]
fn narrow_metrics_leave_the_splash_assembly_full_bleed_too() {
    let _g = serial();
    let _ambient = setup();
    provide_context(metrics_at(Size::new(400.0, SURFACE.height)));

    // The other half of the branch: below the breakpoint the splash spans the
    // layer exactly as the live screen does (the shared function takes one
    // branch for both), so the handoff has nothing to move on a phone either.
    let splash = splash_cta_bounds(&[]);
    let layer = SURFACE.width - 2.0 * LAYER_PAD_X;
    assert!(
        (splash.width() - layer).abs() < EPSILON && (splash.x0 - LAYER_PAD_X).abs() < EPSILON,
        "narrow metrics leave the splash unclamped: {splash:?}"
    );
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

    // A `frust::icon` paints exactly one **filled path** each, and nothing
    // else on Connect paints one at all: every other piece is rounded rects
    // (swatches, panels, the CTA bar) and stroked borders. So "the affordance
    // is a vector" is literally "there is a filled path inside the FAB" — one
    // of exactly as many paths as this screen has vector marks: the FAB, plus
    // one delete affordance per saved row.
    let rows = expect_saved_servers()
        .list()
        .expect("the mock store never fails")
        .len();
    assert_eq!(
        scene.paths.len(),
        rows + 1,
        "every vector on the screen is the FAB's mark or a row's delete mark: {:?}",
        scene.paths
    );
    let (origin, size) = *scene
        .paths
        .iter()
        .find(|(origin, size)| {
            origin.x >= fab.x0 - 0.5
                && origin.y >= fab.y0 - 0.5
                && origin.x + size.width <= fab.x1 + 0.5
                && origin.y + size.height <= fab.y1 + 0.5
        })
        .unwrap_or_else(|| {
            panic!(
                "the scan mark paints inside the FAB's own box: {:?} in {fab:?}",
                scene.paths
            )
        });
    assert!(
        size.width > 0.0 && size.height > 0.0,
        "…and it is a real shape, not a collapsed path: {origin:?}/{size:?}"
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
    // A manual connect with remember on always raises the save offer now — a
    // saved row is no longer a reason to suppress it (see the offer section
    // below). This case is about the *connect*, so the question is answered
    // "not now" and the held redirect goes through on the spot.
    harness.tap_node("the offer's Not now action", |node| {
        node.role() == Role::Button && node.label() == Some(OFFER_NOT_NOW)
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
// Token hygiene (review F5): clear on connect/disconnect, zeroizing holder
// ---------------------------------------------------------------------------

/// A [`ConnectTarget`] mirroring `crate::connection`'s own test fixture —
/// this suite drives [`muxr::connection::ConnectionController::connect`]
/// directly for the disconnect-edge test below, bypassing
/// [`ConnectController::connect`] entirely so the two controllers' edges
/// stay observably distinct.
fn mock_target() -> ConnectTarget {
    ConnectTarget {
        host: "mock.example".to_string(),
        port: 7777,
        trust_mode: TrustMode::NoVerification,
        label: "Mock rig".to_string(),
        offer_credential: false,
        token: zeroize::Zeroizing::new(String::new()),
        remember: false,
        fingerprint: None,
    }
}

#[test]
fn a_successful_connect_clears_the_login_token() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    type_url(&mut harness, VALID_URL);
    harness.tap_node("the login token field", |node| {
        node.role() == Role::PasswordInput
    });
    harness.type_text("s3cr3t");
    assert_ne!(
        harness.field_values()[1],
        String::new(),
        "the token landed on the controller before connecting"
    );

    harness.tap_node("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });

    assert!(
        expect_connection_controller().connected.get_untracked(),
        "the connect itself still went through"
    );
    assert_eq!(
        harness.field_values()[1],
        String::new(),
        "the token field is scrubbed the instant a connection opens with it \
         (`ConnectController::connect`'s `Ok` branch)"
    );
}

#[test]
fn a_disconnect_via_the_connection_controller_clears_the_login_token() {
    // Drives `ConnectionController::connect`/`disconnect` directly rather than
    // through `ConnectController::connect` — the task's own carve-out (the
    // real trigger is Settings' Disconnect button, which this headless Connect
    // suite has no reason to mount): the true→false edge is what
    // `ConnectScreen::build` watches, regardless of what flips it.
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    harness.tap_node("the login token field", |node| {
        node.role() == Role::PasswordInput
    });
    harness.type_text("leftover");
    assert_ne!(
        harness.field_values()[1],
        String::new(),
        "the token landed before any connection was opened"
    );

    let connection = expect_connection_controller();
    connection.connect(mock_target());
    harness.settle();
    assert_ne!(
        harness.field_values()[1],
        String::new(),
        "connecting through the connection controller directly (never through \
         `ConnectController::connect`) must not, by itself, touch the token — \
         only the screen's own connect edge does that"
    );

    connection.disconnect();
    harness.settle();

    assert_eq!(
        harness.field_values()[1],
        String::new(),
        "the disconnect edge (`connection.connected` true→false) scrubs the \
         token even though `ConnectController::connect` never ran — \
         `ConnectScreen` stays mounted+reconciled under whatever covers it, \
         and its own `build` is what watches this edge"
    );
}

#[test]
fn set_token_then_clear_token_round_trips_to_an_empty_buffer() {
    // A controller-level check of the two setters themselves, independent of
    // any mounted screen: `set_token` wraps in a fresh `Zeroizing<String>`,
    // and `clear_token` (both edges above call it) always lands on an empty
    // one rather than leaving whatever was typed.
    let _g = serial();
    let _ambient = setup();
    let connection = muxr::connection::provide_connection_controller();
    let controller = controller_over(
        Arc::new(MockSavedServersRepository::seeded()),
        Arc::new(MockCredentialService::new()),
        connection,
    );

    controller.set_token("s3cr3t".to_string());
    assert_eq!(controller.token.get_untracked().as_str(), "s3cr3t");

    controller.clear_token();
    assert_eq!(
        controller.token.get_untracked().as_str(),
        "",
        "clearing after setting leaves an empty buffer, not the old one"
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
        "a remembered, not-yet-saved rig offers its credential, labelled by its \
         own host"
    );
    assert!(
        offer_dialog_up(&harness),
        "…and the offer is asked here rather than carried to Sessions (see \
         the offer section below)"
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
    assert!(
        !offer_dialog_up(&harness),
        "…so no dialog held the redirect: {:?}",
        node_labels(&harness.root)
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
    // The sheet claims focus on any press inside it, so `Escape` has a chain
    // to travel.
    harness.tap(Point::new(SURFACE.width / 2.0, SURFACE.height - 40.0));
    harness
        .root
        .event(&mut harness.state, &named_key(NamedKey::Escape));
    harness.settle();
    harness.settle();

    assert_eq!(harness.nav.depth(), 1);
}

// ---------------------------------------------------------------------------
// QR pairing: the scan sheet, the prefill, and the pinned CTA
// ---------------------------------------------------------------------------

/// A `muxr://pair` v2 `tm=pin` code for [`PAIRED_ENDPOINT`], `ro` as given.
///
/// Built by running the *real* parser over a real URI rather than by hand:
/// this is the shape the camera actually hands the form, so a change to either
/// half shows up here rather than in production.
fn scanned_payload(read_only: bool) -> PairingPayload {
    let ro = u8::from(read_only);
    let uri = format!(
        "muxr://pair?v=2&h=192.168.1.100&p=50051&t={PAIRED_TOKEN_B64}&ro={ro}&n=Home%20Rig\
         &tm=pin&fp={PAIRED_FINGERPRINT}"
    );
    parse_pairing_uri(&uri).expect("the fixture URI is a well-formed pairing code")
}

/// The endpoint [`scanned_payload`] pairs with — deliberately **not** one of
/// the seeded saved servers, so nothing about the prefill can come from the
/// saved list.
const PAIRED_ENDPOINT: &str = "192.168.1.100:50051";
/// The fixture's token, base64url-encoded with no padding (`s3cr3t-token`).
const PAIRED_TOKEN_B64: &str = "czNjcjN0LXRva2Vu";
/// …and in the clear, which is what the form must end up holding.
const PAIRED_TOKEN: &str = "s3cr3t-token";
/// The fixture's pinned SHA-256, 64 lowercase hex characters.
const PAIRED_FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The boxed piece-logic closure the form fixture below drives.
type FormLogic = Box<dyn FnMut(&mut ()) -> AnyView<()>>;

/// The eight Connect pieces mounted **live over a controller the test owns** —
/// the same `Some(&Live)` composition the screen builds, minus the navigator.
///
/// Why not [`Harness`]: the screen hosts its controller through
/// `use_controller`, so a test that mounts the screen cannot reach the very
/// controller a scan would apply to. The pieces take their `Live` wiring as a
/// parameter (that is what the splash/route split exists for), so composing
/// them here is the *supported* way to drive a form whose controller the test
/// also holds — and it is the same code path the route mounts.
///
/// The repository is deliberately **empty**: the seeded store carries a
/// read-only row of its own, and this fixture asserts on how many `read-only`
/// chips are on screen.
struct Form {
    root: RenderRoot<(), AnyView<()>>,
    logic: FormLogic,
    state: (),
    tcx: TextContext,
}

impl Form {
    /// Mount the pieces over `controller`, wiring the pinned CTA to
    /// `on_connect_pinned`.
    fn mount(controller: &Arc<ConnectController>, on_connect_pinned: Rc<dyn Fn()>) -> Self {
        let theme = Theme::glyph_baseline();
        let live = Live {
            controller: Arc::clone(controller),
            on_connect: Rc::new(|| {}),
            on_connect_pinned,
            // This fixture's store is empty, so no saved row is ever painted
            // and nothing can reach either of these (the screen's own wiring is
            // exercised through `Harness` instead).
            on_saved_tap: Rc::new(|_index| {}),
            on_saved_delete: Rc::new(|_index| {}),
        };
        let logic: FormLogic = Box::new(move |_state: &mut ()| {
            // Piece 7's rows are an input to the builder now (the splash passes
            // its own snapshot of the same repository); the screen's own source
            // is this tracked read, so the fixture takes it too.
            let saved = live.controller.saved.get();
            connect_layout(connect_pieces_with::<()>(&theme, Some(&live), &saved))
        });
        let mut form = Form {
            root: RenderRoot::new(),
            logic,
            state: (),
            tcx: TextContext::new(),
        };
        form.frame();
        form
    }

    /// One frame at [`SURFACE`].
    fn frame(&mut self) {
        frame_sized(
            &mut self.root,
            &mut self.logic,
            &mut self.state,
            &mut self.tcx,
            SURFACE,
        );
    }

    /// Both fields' values, in tree order (0 = server URL, 1 = the masked
    /// login token) — [`Harness::field_values`]' twin over this fixture.
    fn field_values(&self) -> Vec<String> {
        self.root
            .semantics()
            .nodes
            .iter()
            .filter(|(_, node)| matches!(node.role(), Role::TextInput | Role::PasswordInput))
            .filter_map(|(_, node)| node.value().map(str::to_string))
            .collect()
    }

    /// Whether any node carries `value` — how a plain `text` run is read back.
    fn shows_value(&self, value: &str) -> bool {
        self.root
            .semantics()
            .nodes
            .iter()
            .any(|(_, node)| node.value() == Some(value))
    }

    /// How many `Role::Status` chips (`glyph::badge`) carry `label`.
    fn chips(&self, label: &str) -> usize {
        self.root
            .semantics()
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == Role::Status && node.label() == Some(label))
            .count()
    }

    /// The center of the button labelled `label`, or `None` when it is not on
    /// screen.
    fn button(&self, label: &str) -> Option<Point> {
        self.root.semantics().nodes.iter().find_map(|(_, node)| {
            if node.role() != Role::Button || node.label() != Some(label) {
                return None;
            }
            let bounds = node.bounds()?;
            Some(Point::new(
                (bounds.x0 + bounds.x1) / 2.0,
                (bounds.y0 + bounds.y1) / 2.0,
            ))
        })
    }

    /// Tap the button labelled `label` and render the frame after it.
    fn tap_button(&mut self, label: &str) {
        let point = self
            .button(label)
            .unwrap_or_else(|| panic!("no button labelled {label:?}"));
        tap(&mut self.root, &mut self.state, point);
        self.frame();
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
}

/// A controller over an **empty** saved-server store and the app-scoped
/// (mock-backed) connection — see [`Form`]'s note on why the store is empty.
fn pairing_controller() -> Arc<ConnectController> {
    let connection = muxr::connection::provide_connection_controller();
    controller_over(
        Arc::new(MockSavedServersRepository::with_servers(Vec::new())),
        Arc::new(MockCredentialService::new()),
        connection,
    )
}

/// A [`ConnectController`] over explicit stores — the same four dependencies
/// the screen's own `init` resolves, with the token store always the seeded
/// mock (a save mints through it, and every assertion about a revoke reads it
/// back through `MockAuthTokensRepository::snapshot`).
fn controller_over(
    servers: Arc<MockSavedServersRepository>,
    credentials: Arc<MockCredentialService>,
    connection: Arc<ConnectionController>,
) -> Arc<ConnectController> {
    controller_with_tokens(
        servers,
        credentials,
        Arc::new(MockAuthTokensRepository::seeded()),
        connection,
    )
}

/// [`controller_over`] with the token store handed in, for the flows that
/// assert on what was minted or revoked.
fn controller_with_tokens(
    servers: Arc<MockSavedServersRepository>,
    credentials: Arc<MockCredentialService>,
    tokens: Arc<MockAuthTokensRepository>,
    connection: Arc<ConnectionController>,
) -> Arc<ConnectController> {
    Arc::new(ConnectController::new(
        servers,
        credentials,
        tokens,
        connection,
    ))
}

#[test]
fn the_qr_sheet_shows_the_no_camera_state_off_device() {
    // The sheet's own body, mounted directly rather than as a pushed page: a
    // navigator contributes no semantics for its pages, so the
    // sheet is unreadable while it is one. This is the desktop/headless path —
    // no camera backend, so the sheet says so instead of framing a dead
    // viewfinder.
    let _g = serial();
    let _ambient = setup();
    let mut state = MuxrApp.init();
    let controller = pairing_controller();
    let mut logic: Logic =
        Box::new(move |_state| qr_sheet_body(Rc::new(|| {}), Arc::clone(&controller)));
    let mut root: RenderRoot<AppState, AnyView<AppState>> = RenderRoot::new();
    let mut tcx = TextContext::new();
    let scene = frame_sized(&mut root, &mut logic, &mut state, &mut tcx, SURFACE);

    let values: Vec<String> = root
        .semantics()
        .nodes
        .iter()
        .filter_map(|(_, node)| node.value().map(str::to_string))
        .collect();
    assert!(
        values.iter().any(|value| value == NO_CAMERA),
        "the sheet explains why there is no viewfinder: {values:?}"
    );
    assert!(
        !root
            .semantics()
            .nodes
            .iter()
            .any(|(_, node)| node.role() == Role::Button),
        "no retry is offered where retrying cannot change the answer"
    );
    // …and it still draws the scan-frame placeholder, as a vector.
    assert_eq!(
        scene.paths.len(),
        1,
        "the placeholder viewfinder's mark is the sheet's one vector: {:?}",
        scene.paths
    );
}

#[test]
fn a_scanned_payload_prefills_the_form_and_raises_the_pinned_cta() {
    let _g = serial();
    let _ambient = setup();
    let controller = pairing_controller();
    let mut form = Form::mount(&controller, Rc::new(|| {}));

    assert_eq!(
        form.button(PINNED_CONNECT_LABEL),
        None,
        "nothing scanned yet, so there is nothing to connect from"
    );

    controller.apply_scanned_payload(scanned_payload(true));
    form.frame();

    assert_eq!(
        form.field_values(),
        vec![
            PAIRED_ENDPOINT.to_string(),
            "\u{2022}".repeat(PAIRED_TOKEN.chars().count()),
        ],
        "the scan prefilled both fields — the token masked, like any other"
    );
    assert_eq!(
        controller.token.get_untracked().as_str(),
        PAIRED_TOKEN,
        "…and the real token is what landed behind the mask"
    );
    // The posture came with the code, so the selector renders it the one
    // honest way it can: neither manual row selected, and the caption naming
    // the QR (not the saved list) as its source.
    assert!(!form.trust_selected(SYSTEM_TRUST) && !form.trust_selected(INSECURE_TRUST));
    assert!(
        form.shows_value(PAIRED_CAPTION),
        "{PAIRED_CAPTION:?} missing"
    );
    assert!(
        !form.shows_value(PINNED_CAPTION),
        "the saved-server caption must not stand in for a scanned pin"
    );
    // The explicit action — a scan prefills, only this connects.
    assert!(
        form.button(PINNED_CONNECT_LABEL).is_some(),
        "the pinned CTA appears with the pairing"
    );
    assert!(form.shows_value(PAIRING_CAPTION));
    assert!(
        form.button(CONNECT_LABEL).is_some(),
        "…beside the ordinary CTA, which never goes away"
    );
    assert_eq!(
        form.chips(READ_ONLY_CHIP),
        1,
        "a ro=1 code surfaces its read-only chip on the form"
    );
}

#[test]
fn a_read_write_pairing_shows_no_read_only_chip() {
    let _g = serial();
    let _ambient = setup();
    let controller = pairing_controller();
    let mut form = Form::mount(&controller, Rc::new(|| {}));

    controller.apply_scanned_payload(scanned_payload(false));
    form.frame();

    assert!(form.button(PINNED_CONNECT_LABEL).is_some());
    assert_eq!(
        form.chips(READ_ONLY_CHIP),
        0,
        "the chip is a fact about the credential, not decoration on the banner"
    );
}

#[test]
fn the_pinned_cta_connects_with_the_codes_own_trust_and_token() {
    let _g = serial();
    let _ambient = setup();
    let _app = MuxrApp.init();
    let connection = expect_connection_controller();
    // The screen pushes `/sessions` through the app's real table and guard;
    // this fixture installs the same router over a navigator of its own so the
    // push resolves rather than panicking on a missing handle.
    let nav_controller: NavigatorController<AppState> = NavigatorController::new();
    let _router = install_app_router(&nav_controller, &nav_controller, Arc::clone(&connection));

    let controller = controller_over(
        Arc::new(MockSavedServersRepository::with_servers(Vec::new())),
        Arc::new(MockCredentialService::new()),
        Arc::clone(&connection),
    );
    let pinned = {
        let controller = Arc::clone(&controller);
        Rc::new(move || controller.connect_from_pairing(&app_nav()))
    };
    let mut form = Form::mount(&controller, pinned);

    controller.apply_scanned_payload(scanned_payload(false));
    form.frame();
    form.tap_button(PINNED_CONNECT_LABEL);

    assert!(
        connection.connected.get_untracked(),
        "the mock connection opened"
    );
    let info = connection
        .server_info
        .get_untracked()
        .expect("a connected controller carries its server facts");
    assert_eq!(
        info.endpoint, PAIRED_ENDPOINT,
        "the scanned endpoint was dialed"
    );
    assert_eq!(
        info.trust_mode,
        TrustMode::Pinned,
        "…under the posture the code pinned, not the selector's default"
    );
    assert_eq!(
        connection.credential_offer.get_untracked(),
        None,
        "the manual save offer must not fire for a scanned credential"
    );
    assert_eq!(
        controller.pending_pairing.get_untracked(),
        None,
        "a live session consumes the pairing"
    );
    assert_eq!(
        controller.token.get_untracked().as_str(),
        "",
        "…and the token's job ended with the connection it opened"
    );
    assert_eq!(
        form.button(PINNED_CONNECT_LABEL),
        None,
        "so the pinned CTA goes with it"
    );
}

#[test]
fn a_failed_pinned_connect_keeps_the_form_filled_and_the_cta_up() {
    // The mock backend never refuses, so a connect that settles nothing is
    // staged with the busy flag instead: the CTA renders `.loading` (its press
    // suppressed) over a controller whose `connect_with` would ignore a second
    // dial anyway. Either way nothing settles — which is exactly the shape a
    // refused dial leaves behind, and what this asserts survives it: the token
    // still typed, the payload still stashed, the CTA still there for the
    // one-tap retry.
    let _g = serial();
    let _ambient = setup();
    let connection = muxr::connection::provide_connection_controller();
    let nav_controller: NavigatorController<AppState> = NavigatorController::new();
    let _router = install_app_router(&nav_controller, &nav_controller, Arc::clone(&connection));
    connection.connecting.set(true);
    let controller = controller_over(
        Arc::new(MockSavedServersRepository::with_servers(Vec::new())),
        Arc::new(MockCredentialService::new()),
        Arc::clone(&connection),
    );
    let pinned = {
        let controller = Arc::clone(&controller);
        Rc::new(move || controller.connect_from_pairing(&app_nav()))
    };
    let mut form = Form::mount(&controller, pinned);

    controller.apply_scanned_payload(scanned_payload(false));
    form.frame();
    form.tap_button(PINNED_CONNECT_LABEL);

    assert!(!connection.connected.get_untracked(), "nothing opened");
    assert_eq!(
        controller.token.get_untracked().as_str(),
        PAIRED_TOKEN,
        "the token survives a connect that did not happen"
    );
    assert!(
        controller.pending_pairing.get_untracked().is_some(),
        "…and so does the pairing"
    );
    assert!(
        form.button(PINNED_CONNECT_LABEL).is_some(),
        "so a retry is one tap"
    );
}

#[test]
fn editing_the_endpoint_away_from_the_scan_drops_the_pairing() {
    let _g = serial();
    let _ambient = setup();
    let controller = pairing_controller();
    let mut form = Form::mount(&controller, Rc::new(|| {}));

    controller.apply_scanned_payload(scanned_payload(false));
    form.frame();
    assert!(form.button(PINNED_CONNECT_LABEL).is_some());

    // Retyping the very same address is not an edit away from it.
    controller.set_url(PAIRED_ENDPOINT.to_string());
    form.frame();
    assert!(
        form.button(PINNED_CONNECT_LABEL).is_some(),
        "the pairing still names what the field says"
    );

    controller.set_url("other.example:50051".to_string());
    form.frame();

    assert_eq!(
        controller.pending_pairing.get_untracked(),
        None,
        "the pin belongs to the scanned address, not to whatever is typed next"
    );
    assert_eq!(
        form.button(PINNED_CONNECT_LABEL),
        None,
        "…so the CTA that would have used it is gone"
    );
    assert!(
        form.trust_selected(SYSTEM_TRUST),
        "and the posture falls back to the manual default rather than staying \
         pinned with no pin behind it"
    );
    assert!(!form.shows_value(PAIRED_CAPTION));
}

#[test]
fn editing_the_token_drops_the_pairing_too() {
    // The pinned CTA sends the *payload's* token; a hand-typed one would make
    // the form say one thing and the button do another.
    let _g = serial();
    let _ambient = setup();
    let controller = pairing_controller();
    let mut form = Form::mount(&controller, Rc::new(|| {}));

    controller.apply_scanned_payload(scanned_payload(false));
    form.frame();
    assert!(form.button(PINNED_CONNECT_LABEL).is_some());

    controller.set_token("typed-by-hand".to_string());
    form.frame();

    assert_eq!(controller.pending_pairing.get_untracked(), None);
    assert_eq!(form.button(PINNED_CONNECT_LABEL), None);
    assert_eq!(
        form.field_values()[0],
        PAIRED_ENDPOINT,
        "only the pairing is dropped — the prefilled endpoint stays typed"
    );
}

#[test]
fn the_ordinary_cta_still_connects_while_a_matching_pairing_is_stashed() {
    // The fail-closed trap this avoids: a prefill leaves the form on
    // `TrustMode::Pinned`, which the manual path carries no fingerprint for —
    // so without the pairing's own pin riding through `validated_target`, the
    // ordinary CTA would refuse every scan's endpoint before dialing.
    let _g = serial();
    let _ambient = setup();
    let connection = muxr::connection::provide_connection_controller();
    let controller = controller_over(
        Arc::new(MockSavedServersRepository::with_servers(Vec::new())),
        Arc::new(MockCredentialService::new()),
        connection,
    );

    controller.apply_scanned_payload(scanned_payload(false));
    let target = controller
        .validated_target()
        .expect("the prefilled endpoint is dialable");

    assert_eq!(target.trust_mode, TrustMode::Pinned);
    assert_eq!(target.fingerprint.as_deref(), Some(PAIRED_FINGERPRINT));
    assert!(
        !target.offer_credential,
        "a scanned credential's save is the pairing flow's own step, never the \
         manual offer"
    );
}

// ---------------------------------------------------------------------------
// Credentials: the gate's outcome table, and the four flows around it
// ---------------------------------------------------------------------------

/// The endpoint every credential case operates on — deliberately not one of the
/// seeded rows, so the fixture's own row is the only thing under test.
const CRED_ENDPOINT: &str = "cred.example:50051";
/// …and the secret stored for it.
const CRED_TOKEN: &str = "stored-s3cr3t";
/// [`credential_row`]'s delete affordance's accessible name — transcribed, not
/// imported, for the reason [`QR_LABEL`] is. It names the *server*, since every
/// row carries one of these buttons.
const DELETE_CRED_LABEL: &str = "Delete Cred rig";
/// The delete confirmation's title and confirm action, likewise transcribed.
const REMOVE_TITLE: &str = "Remove server?";
const REMOVE_CONFIRM: &str = "Remove";

/// A saved row for [`CRED_ENDPOINT`], with or without a stored credential.
fn credential_row(has_credential: bool, token_name: Option<&str>) -> SavedServer {
    SavedServer {
        host: "cred.example".to_string(),
        port: 50051,
        trust_mode: TrustMode::System,
        label: "Cred rig".to_string(),
        last_used_ms: 1_700_000_000_000,
        read_only: false,
        has_credential,
        token_name: token_name.map(str::to_string),
        fingerprint: None,
    }
}

/// The four handles a credential flow is driven and read through.
struct Creds {
    controller: Arc<ConnectController>,
    credentials: Arc<MockCredentialService>,
    servers: Arc<MockSavedServersRepository>,
    tokens: Arc<MockAuthTokensRepository>,
    connection: Arc<ConnectionController>,
    /// The app's real route table over this fixture's own navigator — held
    /// because `routes::app_nav()` resolves through it and a dropped router
    /// would take the handle's table with it. Type-erased because
    /// `frust::Router` is not part of this app's own public surface (only the
    /// composition root ever names one).
    _router: Rc<dyn Any>,
}

/// A controller over a saved-server store holding `rows`, with the app's real
/// router installed so a recovered connect can push `/sessions`.
fn credential_fixture(rows: Vec<SavedServer>) -> Creds {
    let connection = muxr::connection::provide_connection_controller();
    let nav: NavigatorController<AppState> = NavigatorController::new();
    let router = install_app_router(&nav, &nav, Arc::clone(&connection));
    let servers = Arc::new(MockSavedServersRepository::with_servers(rows));
    let credentials = Arc::new(MockCredentialService::new());
    let tokens = Arc::new(MockAuthTokensRepository::seeded());
    let controller = controller_with_tokens(
        Arc::clone(&servers),
        Arc::clone(&credentials),
        Arc::clone(&tokens),
        Arc::clone(&connection),
    );
    Creds {
        controller,
        credentials,
        servers,
        tokens,
        connection,
        _router: router,
    }
}

/// Drive the UI-thread task queue until `done` answers `true`, or fail.
///
/// The credential flows are `spawn_local` chains over repositories that
/// deliberately sleep (`MockAuthTokensRepository`'s own simulated latency), so a
/// single pump is not enough — this is `support::pump_until`'s loop without a
/// `RenderRoot`, since these cases drive a controller rather than a screen.
fn pump_until_done(what: &str, mut done: impl FnMut() -> bool) {
    let runtime = frust_reactive::ReactiveRuntime::get().expect("setup() installed the runtime");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        runtime.pump_local();
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// The saved row for [`CRED_ENDPOINT`] as the store currently holds it.
fn stored_row(servers: &MockSavedServersRepository) -> SavedServer {
    servers
        .list()
        .expect("the mock store never fails")
        .into_iter()
        .find(|server| server.endpoint() == CRED_ENDPOINT)
        .expect("the fixture's row")
}

// --- Flow 3: unlock, and the whole outcome table -----------------------------

#[test]
fn unlocking_a_saved_row_connects_with_the_recovered_token() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(vec![credential_row(true, Some("token_2"))]);
    creds.credentials.seed(CRED_ENDPOINT, CRED_TOKEN);

    creds
        .controller
        .recover_and_connect(stored_row(&creds.servers), &app_nav());
    pump_until_done("the recovered connect", || {
        creds.connection.connected.get_untracked()
    });

    assert_eq!(
        creds.credentials.prompts(),
        vec!["Unlock Cred rig".to_string()],
        "exactly one prompt, naming the rig rather than anything secret"
    );
    let info = creds
        .connection
        .server_info
        .get_untracked()
        .expect("a connected controller carries its facts");
    assert_eq!(info.endpoint, CRED_ENDPOINT, "the row's own endpoint");
    assert_eq!(
        info.trust_mode,
        TrustMode::System,
        "…under the posture the row stored"
    );
    assert_eq!(
        creds.connection.credential_offer.get_untracked(),
        None,
        "a credential this connect just read is never offered for saving"
    );
    assert!(
        stored_row(&creds.servers).has_credential,
        "the login upsert carries the credential fields forward rather than \
         wiping the very credential it just used"
    );
}

#[test]
fn a_canceled_unlock_is_silent_and_leaves_the_form_alone() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(vec![credential_row(true, None)]);
    creds.credentials.seed(CRED_ENDPOINT, CRED_TOKEN);
    creds.credentials.set_gate(CredentialStatus::Canceled);

    creds
        .controller
        .recover_and_connect(stored_row(&creds.servers), &app_nav());
    pump_until_done("the canceled unlock", || {
        !creds.controller.unlocking.get_untracked()
    });

    assert!(
        !creds.connection.connected.get_untracked(),
        "nothing opened"
    );
    assert_eq!(
        creds.controller.url.get_untracked(),
        String::new(),
        "a cancel prefills nothing — the user dismissed their own prompt"
    );
}

#[test]
fn every_refused_unlock_reports_its_own_reason_and_prefills_the_form() {
    // The whole outcome table, in one process: each branch says its own thing
    // and leaves the manual form filled in, since the manual login is the only
    // path left once the gate refuses (there is no plain-storage fallback).
    let _g = serial();
    let _ambient = setup();
    for status in [
        CredentialStatus::NotEnrolled,
        CredentialStatus::NoDeviceCredential,
        CredentialStatus::LockedOut,
        CredentialStatus::Unavailable,
        CredentialStatus::Failed,
    ] {
        let creds = credential_fixture(vec![credential_row(true, None)]);
        creds.credentials.seed(CRED_ENDPOINT, CRED_TOKEN);
        creds.credentials.set_gate(status);

        creds
            .controller
            .recover_and_connect(stored_row(&creds.servers), &app_nav());
        pump_until_done("the refused unlock", || {
            !creds.controller.unlocking.get_untracked()
        });

        assert!(
            !creds.connection.connected.get_untracked(),
            "{status:?} must not connect"
        );
        assert_eq!(
            creds.controller.url.get_untracked(),
            CRED_ENDPOINT,
            "{status:?} falls back to the manual form for that same server"
        );
        assert!(
            status.message().is_some(),
            "{status:?} has something to say to the user"
        );
        assert!(
            stored_row(&creds.servers).has_credential,
            "{status:?} is a refusal, not a loss — the credential is still there"
        );
    }
}

#[test]
fn a_gate_that_passes_over_an_empty_store_reports_no_saved_credential() {
    let _g = serial();
    let _ambient = setup();
    // The row claims a credential the secure store does not hold — a torn
    // pair, which is exactly the not-found branch.
    let creds = credential_fixture(vec![credential_row(true, None)]);

    creds
        .controller
        .recover_and_connect(stored_row(&creds.servers), &app_nav());
    pump_until_done("the empty-store unlock", || {
        !creds.controller.unlocking.get_untracked()
    });

    assert!(!creds.connection.connected.get_untracked());
    assert_eq!(
        creds.controller.url.get_untracked(),
        CRED_ENDPOINT,
        "the manual form is prefilled for that server"
    );
    assert_eq!(
        CredentialStatus::NotFound.message(),
        Some("No saved credential found. Please log in manually.")
    );
}

#[test]
fn an_invalidated_key_forgets_the_credential_it_can_no_longer_read() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(vec![credential_row(true, Some("token_2"))]);
    creds.credentials.seed(CRED_ENDPOINT, CRED_TOKEN);
    creds.credentials.set_gate(CredentialStatus::KeyInvalidated);

    creds
        .controller
        .recover_and_connect(stored_row(&creds.servers), &app_nav());
    pump_until_done("the invalidated unlock", || {
        !stored_row(&creds.servers).has_credential
    });

    assert!(!creds.connection.connected.get_untracked());
    let row = stored_row(&creds.servers);
    assert!(
        !row.has_credential && row.token_name.is_none(),
        "an enrollment change makes the secret unreadable forever, so the row \
         must stop claiming one: {row:?}"
    );
    assert_eq!(
        creds.credentials.stored(CRED_ENDPOINT),
        None,
        "…and the dead entry is dropped, the plugin's own recovery for it"
    );
    assert_eq!(
        creds.controller.url.get_untracked(),
        CRED_ENDPOINT,
        "the manual form is still prefilled"
    );
}

#[test]
fn a_saved_row_with_no_credential_refills_the_form_without_raising_a_prompt() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(vec![credential_row(false, None)]);

    creds.controller.saved_row_tapped(0, &app_nav());

    assert!(
        creds.credentials.prompts().is_empty(),
        "there is nothing to unlock, so nothing may prompt"
    );
    assert_eq!(creds.controller.url.get_untracked(), CRED_ENDPOINT);
    assert!(!creds.connection.connected.get_untracked());
}

// --- Flow 1: the manual save (the offer's accept) ----------------------------

/// Connect the fixture's connection to [`CRED_ENDPOINT`], as a login would,
/// so an accepted offer has a live session to save for.
///
/// The offer prompt is a **recorder**, not a dialog: these cases drive the save
/// itself (mint, gate, rotate) through the connection's own accept, so what
/// matters here is only that the held redirect stayed held — the screen-level
/// suite below is where the dialog and the navigation it resolves are driven
/// end to end. Each recorded entry is the offer's label and whether it asked to
/// *replace* a credential this device already holds.
fn connect_fixture(creds: &Creds) -> Rc<RefCell<Vec<(String, bool)>>> {
    let offered: Rc<RefCell<Vec<(String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
    let record = Rc::clone(&offered);
    creds.controller.set_url(CRED_ENDPOINT.to_string());
    creds.controller.connect(
        &app_nav(),
        Rc::new(move |offer: CredentialOffer| {
            record.borrow_mut().push((offer.label, offer.replacing))
        }),
    );
    assert!(
        creds.connection.connected.get_untracked(),
        "the mock backend settles a connect before `connect` returns"
    );
    offered
}

#[test]
fn accepting_the_offer_mints_a_token_stores_it_and_records_it_on_the_row() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(Vec::new());
    let offered = connect_fixture(&creds);
    assert_eq!(
        offered.borrow().as_slice(),
        [("cred.example".to_string(), false)],
        "the connect handed its offer to the screen's prompt rather than \
         redirecting past it (the held redirect) — as a first-time save, since \
         this device holds nothing for that rig yet"
    );
    creds
        .connection
        .set_credential_saver(creds.controller.credential_saver());
    creds
        .connection
        .credential_offer
        .set(Some("Cred rig".to_string()));

    creds.connection.accept_credential_offer();
    pump_until_done("the credential save", || {
        stored_row(&creds.servers).has_credential
    });

    let row = stored_row(&creds.servers);
    let token_name = row.token_name.clone().expect("the minted token's name");
    assert!(
        token_name.starts_with("muxr-mobile-"),
        "a token minted for this device says so: {token_name}"
    );
    assert!(
        creds
            .tokens
            .snapshot()
            .iter()
            .any(|token| token.name == token_name),
        "…and it exists on the server"
    );
    let stored = creds
        .credentials
        .stored(CRED_ENDPOINT)
        .expect("the minted secret is what was stored");
    assert!(
        stored.starts_with("mock-secret-"),
        "the *freshly minted* secret is stored, never the form's own token: {stored}"
    );
    assert_eq!(
        creds.connection.credential_offer.get_untracked(),
        None,
        "the offer is consumed"
    );
}

#[test]
fn a_denied_gate_revokes_the_token_it_just_minted() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(Vec::new());
    connect_fixture(&creds);
    creds
        .connection
        .set_credential_saver(creds.controller.credential_saver());
    creds.credentials.set_gate(CredentialStatus::Canceled);
    let before = creds.tokens.snapshot().len();
    creds
        .connection
        .credential_offer
        .set(Some("Cred rig".to_string()));

    creds.connection.accept_credential_offer();
    pump_until_done("the denied save", || {
        creds.tokens.snapshot().len() == before && !creds.credentials.prompts().is_empty()
    });

    assert_eq!(
        creds.tokens.snapshot().len(),
        before,
        "a token nobody can hold must not be left orphaned on the server"
    );
    assert_eq!(
        creds.credentials.stored(CRED_ENDPOINT),
        None,
        "and nothing was stored"
    );
    assert!(
        !stored_row(&creds.servers).has_credential,
        "…so the row still claims no credential"
    );
}

#[test]
fn re_saving_rotates_the_previous_token_only_after_the_new_one_is_stored() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(Vec::new());
    connect_fixture(&creds);
    creds
        .connection
        .set_credential_saver(creds.controller.credential_saver());

    creds
        .connection
        .credential_offer
        .set(Some("Cred rig".to_string()));
    creds.connection.accept_credential_offer();
    pump_until_done("the first save", || {
        stored_row(&creds.servers).has_credential
    });
    let first = stored_row(&creds.servers)
        .token_name
        .expect("the first minted token");

    creds
        .connection
        .credential_offer
        .set(Some("Cred rig".to_string()));
    creds.connection.accept_credential_offer();
    // Waiting on the *revoke* rather than on the row: the rotation's whole
    // contract is that the old token outlives the new credential's write, so
    // the row changing first is exactly what this must not mistake for done.
    let revoked = |creds: &Creds| {
        !creds
            .tokens
            .snapshot()
            .iter()
            .any(|token| token.name == first)
    };
    pump_until_done("the second save", || revoked(&creds));

    let names: Vec<String> = creds
        .tokens
        .snapshot()
        .into_iter()
        .map(|token| token.name)
        .collect();
    assert!(
        !names.contains(&first),
        "the previous token is revoked once the new credential is persisted: {names:?}"
    );
    let second = stored_row(&creds.servers)
        .token_name
        .expect("the second minted token");
    assert!(names.contains(&second), "…and the new one survives");
}

#[test]
fn a_read_only_session_never_mints_a_credential() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(Vec::new());
    connect_fixture(&creds);
    creds
        .connection
        .set_credential_saver(creds.controller.credential_saver());
    creds.connection.is_read_only.set(true);
    let before = creds.tokens.snapshot().len();
    creds
        .connection
        .credential_offer
        .set(Some("Cred rig".to_string()));

    creds.connection.accept_credential_offer();

    assert_eq!(
        creds.connection.credential_offer.get_untracked(),
        None,
        "the offer is still consumed"
    );
    assert_eq!(creds.tokens.snapshot().len(), before, "…but nothing minted");
    assert!(creds.credentials.prompts().is_empty(), "…and nothing gated");
}

// --- Flow 2: the QR save -----------------------------------------------------

#[test]
fn a_successful_pairing_stores_the_scanned_token_behind_one_prompt() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(Vec::new());

    creds
        .controller
        .apply_scanned_payload(scanned_payload(false));
    creds.controller.connect_from_pairing(&app_nav());
    pump_until_done("the paired credential save", || {
        creds.credentials.stored(PAIRED_ENDPOINT).is_some()
    });

    assert_eq!(
        creds.credentials.stored(PAIRED_ENDPOINT).as_deref(),
        Some(PAIRED_TOKEN),
        "the QR's own token is the credential — nothing is minted for it"
    );
    assert_eq!(
        creds.credentials.prompts().len(),
        1,
        "exactly one prompt per successful pairing tap"
    );
    let row = creds
        .servers
        .list()
        .expect("the mock store never fails")
        .into_iter()
        .find(|server| server.endpoint() == PAIRED_ENDPOINT)
        .expect("the login upsert wrote the paired rig's row");
    assert!(row.has_credential);
    assert_eq!(
        row.token_name, None,
        "muxrctl minted that token, so this device has no name to revoke it by"
    );
    assert_eq!(row.trust_mode, TrustMode::Pinned);
    assert_eq!(row.fingerprint.as_deref(), Some(PAIRED_FINGERPRINT));
}

#[test]
fn a_denied_pairing_gate_leaves_the_session_live_and_the_row_unsaved() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(Vec::new());
    creds.credentials.set_gate(CredentialStatus::Canceled);

    creds
        .controller
        .apply_scanned_payload(scanned_payload(false));
    creds.controller.connect_from_pairing(&app_nav());
    pump_until_done("the denied pairing gate", || {
        !creds.credentials.prompts().is_empty()
    });

    assert!(
        creds.connection.connected.get_untracked(),
        "the session opened before the prompt and is unaffected by it"
    );
    assert_eq!(creds.credentials.stored(PAIRED_ENDPOINT), None);
    let row = creds
        .servers
        .list()
        .expect("the mock store never fails")
        .into_iter()
        .find(|server| server.endpoint() == PAIRED_ENDPOINT)
        .expect("the login upsert still wrote the row");
    assert!(!row.has_credential);
}

// --- Flow 5: the login upsert ------------------------------------------------

#[test]
fn a_successful_connect_writes_the_servers_metadata_row() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(Vec::new());

    connect_fixture(&creds);

    let row = stored_row(&creds.servers);
    assert_eq!(
        row.label, "cred.example",
        "a new rig is labelled by its host"
    );
    assert_eq!(row.trust_mode, TrustMode::System);
    assert!(!row.read_only);
    assert!(row.last_used_ms > 0, "the login stamped its recency");
    assert!(!row.has_credential, "…and claims no credential yet");
    assert_eq!(
        creds.controller.saved.get_untracked().len(),
        1,
        "the screen's own list is refreshed with it"
    );
}

// --- Flow 4: forgetting a saved server (this device only) --------------------

/// Every saved row the fixture's store currently holds.
fn fixture_rows(servers: &MockSavedServersRepository) -> Vec<SavedServer> {
    servers.list().expect("the mock store never fails")
}

#[test]
fn forgetting_a_saved_server_drops_its_credential_its_row_and_nothing_else() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(vec![credential_row(true, Some("token_2"))]);
    creds.credentials.seed(CRED_ENDPOINT, CRED_TOKEN);
    let tokens_before: Vec<String> = creds
        .tokens
        .snapshot()
        .into_iter()
        .map(|token| token.name)
        .collect();
    // The form is prefilled from the very row about to go — typed (or
    // prefilled) text belongs to the user and must survive the row it came
    // from.
    creds.controller.select_saved(0);
    creds.controller.set_token("typed-by-hand".to_string());

    creds
        .controller
        .forget_saved_server(stored_row(&creds.servers));
    pump_until_done("the local delete", || {
        fixture_rows(&creds.servers).is_empty()
    });

    assert_eq!(
        creds.credentials.stored(CRED_ENDPOINT),
        None,
        "the secret goes with the row it was filed under"
    );
    assert!(
        creds.credentials.prompts().is_empty(),
        "…through the one credential operation that runs no gate: {:?}",
        creds.credentials.prompts()
    );
    assert_eq!(
        creds.controller.saved.get_untracked().len(),
        0,
        "…and the screen's own list is refreshed rather than left stale"
    );
    let tokens_after: Vec<String> = creds
        .tokens
        .snapshot()
        .into_iter()
        .map(|token| token.name)
        .collect();
    assert_eq!(
        tokens_after, tokens_before,
        "this delete is local: the server-side token behind the credential is \
         untouched (revoking one is `muxrctl`'s job, and Settings' own flow)"
    );
    assert_eq!(
        creds.controller.url.get_untracked(),
        CRED_ENDPOINT,
        "the form keeps the address the user was about to connect with"
    );
    assert_eq!(
        creds.controller.token.get_untracked().as_str(),
        "typed-by-hand",
        "…and the token they typed into it"
    );
}

#[test]
fn forgetting_the_currently_connected_server_leaves_the_live_session_alone() {
    // A local delete clears local storage and nothing else: the session was
    // opened with a token the server still honours, so there is nothing about
    // dropping this device's copy that should end it.
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(vec![credential_row(true, Some("token_2"))]);
    creds.credentials.seed(CRED_ENDPOINT, CRED_TOKEN);
    connect_fixture(&creds);
    assert!(creds.connection.connected.get_untracked());

    creds
        .controller
        .forget_saved_server(stored_row(&creds.servers));
    pump_until_done("the local delete", || {
        fixture_rows(&creds.servers).is_empty()
    });

    assert!(
        creds.connection.connected.get_untracked(),
        "the connection is untouched by a delete that only clears storage"
    );
    assert!(
        creds.connection.server_info.get_untracked().is_some(),
        "…facts and all"
    );
}

#[test]
fn a_failed_credential_delete_leaves_the_row_and_its_secret_exactly_as_they_were() {
    // The ordering contract: the secret first, the row only if the secret went
    // — a row removed ahead of a failed delete would strand a secret filed
    // under the very endpoint that stopped being listed.
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(vec![credential_row(true, Some("token_2"))]);
    creds.credentials.seed(CRED_ENDPOINT, CRED_TOKEN);
    creds.credentials.set_delete_failure(true);
    app_toasts().clear();

    creds
        .controller
        .forget_saved_server(stored_row(&creds.servers));
    pump_until_done("the refused delete", || {
        !app_toasts().pending_untracked().is_empty()
    });

    assert_eq!(
        fixture_rows(&creds.servers).len(),
        1,
        "the row survives a delete that could not clear its secret"
    );
    assert_eq!(
        creds.credentials.stored(CRED_ENDPOINT).as_deref(),
        Some(CRED_TOKEN),
        "…and so does the secret itself"
    );
    assert!(
        stored_row(&creds.servers).has_credential,
        "…so the row still tells the truth about it"
    );
}

#[test]
fn a_saved_credential_login_never_re_raises_the_save_offer() {
    // The non-goal of widening `validated_target`'s offer term: the unlock
    // path builds its own target with the offer switched off and never comes
    // through `validated_target` at all, so a login *with* a stored credential
    // must still never be asked to store one.
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(vec![credential_row(true, Some("token_2"))]);
    creds.credentials.seed(CRED_ENDPOINT, CRED_TOKEN);

    creds
        .controller
        .recover_and_connect(stored_row(&creds.servers), &app_nav());
    pump_until_done("the recovered connect", || {
        creds.connection.connected.get_untracked()
    });

    assert_eq!(
        creds.connection.credential_offer.get_untracked(),
        None,
        "a credential this connect just read is never offered back"
    );
    assert_eq!(
        creds.credentials.prompts(),
        vec!["Unlock Cred rig".to_string()],
        "…and the unlock's own prompt is the only one raised"
    );
}

#[test]
fn accepting_the_offer_at_a_known_server_replaces_its_credential_and_keeps_one_row() {
    let _g = serial();
    let _ambient = setup();
    // A row that already holds a credential, whose token has (as far as this
    // device can tell) stopped working — the case a suppressed offer used to
    // make unrecoverable.
    let creds = credential_fixture(vec![credential_row(true, Some("token_2"))]);
    creds.credentials.seed(CRED_ENDPOINT, "stale-token");

    let offered = connect_fixture(&creds);
    assert_eq!(
        offered.borrow().as_slice(),
        [("Cred rig".to_string(), true)],
        "a manual connect at a known server raises the offer, labelled by the \
         row's own stored label — and as a *replacement*, since a credential \
         for it is already stored"
    );
    creds
        .connection
        .set_credential_saver(creds.controller.credential_saver());
    creds
        .connection
        .credential_offer
        .set(Some("Cred rig".to_string()));

    creds.connection.accept_credential_offer();
    // Waiting on the *rotation* rather than on the stored secret: the replaced
    // token is revoked only once the new credential is persisted, so the store
    // changing first is exactly what this must not mistake for done.
    pump_until_done("the replacement save", || {
        !creds
            .tokens
            .snapshot()
            .iter()
            .any(|token| token.name == "token_2")
    });

    let rows = fixture_rows(&creds.servers);
    assert_eq!(
        rows.iter()
            .filter(|server| server.endpoint() == CRED_ENDPOINT)
            .count(),
        1,
        "the save upserts the row it already had rather than adding a second: {rows:?}"
    );
    let row = stored_row(&creds.servers);
    assert!(row.has_credential);
    let token_name = row.token_name.clone().expect("the minted token's name");
    assert_ne!(
        token_name, "token_2",
        "the row records the new token, not the one it replaced"
    );
    assert_eq!(
        creds.credentials.stored(CRED_ENDPOINT),
        Some(format!("mock-secret-{token_name}")),
        "…and the store holds the freshly minted secret, keyed by the same \
         endpoint, so the stale one is genuinely overwritten"
    );
    let names: Vec<String> = creds
        .tokens
        .snapshot()
        .into_iter()
        .map(|token| token.name)
        .collect();
    assert!(
        !names.contains(&"token_2".to_string()),
        "…and the replaced token is rotated out once the new one is stored: {names:?}"
    );
}

// --- The screen, end to end --------------------------------------------------

#[test]
fn tapping_a_credential_row_on_the_real_screen_unlocks_and_lands_on_sessions() {
    let _g = serial();
    let _ambient = setup();
    // The app's own DI stores, seeded in `mount_seeded`'s window: this is the
    // production wiring (the screen resolves both through `connect::di`), so
    // the row, the prompt and the redirect are all the real ones.
    let mut harness = Harness::mount_seeded(|| {
        // Stamped now, so it sorts ahead of the seeded rows and lays out inside
        // the fixture's surface — the list is most-recently-used first.
        let mut row = credential_row(true, None);
        row.last_used_ms = epoch_millis();
        expect_saved_servers()
            .save(row)
            .expect("the mock store never fails");
        mock_credentials()
            .expect("tests/support opted this process into the mock")
            .seed(CRED_ENDPOINT, CRED_TOKEN);
    });
    let credentials = mock_credentials().expect("the mock service is published");
    let before = harness.nav.depth();
    harness.tap_node("the saved row", |node| node.value() == Some(CRED_ENDPOINT));
    pump_until_done("the recovered connect", || {
        expect_connection_controller().connected.get_untracked()
    });
    harness.settle();

    assert_eq!(
        credentials.prompts().len(),
        1,
        "the tap raised exactly one prompt"
    );
    assert!(harness.nav.depth() > before, "…and landed on /sessions");
}

// ---------------------------------------------------------------------------
// The saved row's delete affordance (local only)
// ---------------------------------------------------------------------------

/// Mount the real screen with [`credential_row`] saved and its secret seeded —
/// the app's own DI stores, in `mount_seeded`'s window, so the row, the
/// affordance and both stores are the production wiring.
///
/// Stamped now, so the row sorts ahead of the seeded ones and lays out inside
/// the fixture's surface — the list is most-recently-used first.
fn mount_with_credential_row() -> Harness {
    Harness::mount_seeded(|| {
        let mut row = credential_row(true, Some("token_2"));
        row.last_used_ms = epoch_millis();
        expect_saved_servers()
            .save(row)
            .expect("the mock store never fails");
        mock_credentials()
            .expect("tests/support opted this process into the mock")
            .seed(CRED_ENDPOINT, CRED_TOKEN);
    })
}

/// Whether the app's own store still holds a row for [`CRED_ENDPOINT`].
fn cred_row_is_stored() -> bool {
    expect_saved_servers()
        .list()
        .expect("the mock store never fails")
        .iter()
        .any(|server| server.endpoint() == CRED_ENDPOINT)
}

#[test]
fn the_delete_affordance_confirms_first_and_never_fires_the_rows_own_tap() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_with_credential_row();
    let credentials = mock_credentials().expect("the mock service is published");
    let before = harness.nav.depth();

    harness.tap_node("the row's delete affordance", |node| {
        node.role() == Role::Button && node.label() == Some(DELETE_CRED_LABEL)
    });

    assert_eq!(
        harness.nav.depth(),
        before + 1,
        "the confirmation is pushed as its own page, ahead of any deletion"
    );
    assert!(
        shows_text(&harness, REMOVE_TITLE),
        "…and it is the remove confirmation, not some other dialog: {:?}",
        node_labels(&harness.root)
    );
    assert!(
        credentials.prompts().is_empty(),
        "a local delete raises no biometric prompt at any point"
    );
    assert!(
        !expect_connection_controller().connected.get_untracked(),
        "…and the row's own tap never fired: the affordance consumes only its \
         own press, so pressing it does not also unlock and connect"
    );

    // A scrim tap is `confirm_dialog`'s cancel route (it pops with an empty
    // result), so nothing at all should have been touched.
    harness.tap(SCRIM_POINT);
    harness.settle();

    assert_eq!(harness.nav.depth(), before, "the dialog closed");
    assert!(cred_row_is_stored(), "a cancelled delete keeps the row");
    assert_eq!(
        credentials.stored(CRED_ENDPOINT).as_deref(),
        Some(CRED_TOKEN),
        "…and its stored credential"
    );
}

#[test]
fn confirming_the_delete_clears_the_row_and_its_credential_from_this_device() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_with_credential_row();
    let credentials = mock_credentials().expect("the mock service is published");
    let before = harness.nav.depth();

    harness.tap_node("the row's delete affordance", |node| {
        node.role() == Role::Button && node.label() == Some(DELETE_CRED_LABEL)
    });
    harness.tap_node("the confirmation's Remove action", |node| {
        node.role() == Role::Button && node.label() == Some(REMOVE_CONFIRM)
    });
    harness.pump_until("the local delete", |_harness| !cred_row_is_stored());

    assert_eq!(
        credentials.stored(CRED_ENDPOINT),
        None,
        "the credential goes with the row it was filed under"
    );
    assert!(
        !harness.shows_value(CRED_ENDPOINT),
        "…and the list refreshes in place rather than painting a row the store \
         no longer holds: {:?}",
        node_labels(&harness.root)
    );
    assert!(
        !shows_text(&harness, DELETE_CRED_LABEL),
        "…affordance included"
    );
    assert_eq!(
        harness.nav.depth(),
        before,
        "a delete navigates nowhere: the dialog popped and nothing was pushed"
    );
    assert!(
        credentials.prompts().is_empty(),
        "nothing was gated: {:?}",
        credentials.prompts()
    );
    assert!(
        !expect_connection_controller().connected.get_untracked(),
        "…and nothing connected"
    );
}

#[test]
fn every_saved_row_carries_its_own_named_delete_affordance() {
    let _g = serial();
    let _ambient = setup();
    let harness = Harness::mount();

    // The seeded store's three rigs, each addressable by its own name — a bare
    // "Delete" repeated three times would leave a screen reader with no way to
    // tell which row it is about to remove.
    let deletes: Vec<String> = harness
        .root
        .semantics()
        .nodes
        .iter()
        .filter(|(_, node)| node.role() == Role::Button)
        .filter_map(|(_, node)| node.label().map(str::to_string))
        .filter(|label| label.starts_with("Delete "))
        .collect();
    assert_eq!(
        deletes,
        vec![
            "Delete Home rig".to_string(),
            "Delete zelli".to_string(),
            "Delete herdr staging".to_string(),
        ],
        "one per seeded row, in list order, each naming its own server"
    );
}

// ---------------------------------------------------------------------------
// The no-verification confirm gate
// ---------------------------------------------------------------------------

/// [`credential_row`] with its posture overridden and stamped now, so it sorts
/// ahead of the seeded rows and lays out inside the fixture's surface — the
/// gate tests' one fixture, parametrized over the posture under test.
fn gated_row(trust_mode: TrustMode) -> SavedServer {
    let mut row = credential_row(true, None);
    row.trust_mode = trust_mode;
    row.last_used_ms = epoch_millis();
    if trust_mode == TrustMode::Pinned {
        row.fingerprint = Some(PAIRED_FINGERPRINT.to_string());
    }
    row
}

/// Mount the real screen with one [`gated_row`] saved and its secret seeded —
/// every gate test's fixture.
fn mount_with_gated_row(trust_mode: TrustMode) -> Harness {
    Harness::mount_seeded(|| {
        expect_saved_servers()
            .save(gated_row(trust_mode))
            .expect("the mock store never fails");
        mock_credentials()
            .expect("tests/support opted this process into the mock")
            .seed(CRED_ENDPOINT, CRED_TOKEN);
    })
}

#[test]
fn a_no_verification_saved_row_gates_behind_a_confirmation_before_any_unlock() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_with_gated_row(TrustMode::NoVerification);
    let credentials = mock_credentials().expect("the mock service is published");
    let before = harness.nav.depth();

    harness.tap_node("the no-verification saved row", |node| {
        node.value() == Some(CRED_ENDPOINT)
    });

    assert_eq!(
        harness.nav.depth(),
        before + 1,
        "the confirm dialog is pushed as its own page, ahead of any unlock"
    );
    assert_eq!(
        credentials.prompts().len(),
        0,
        "the biometric prompt must not fire until the dialog is confirmed"
    );
    assert!(
        !expect_connection_controller().connected.get_untracked(),
        "nothing has connected yet"
    );
}

#[test]
fn canceling_the_no_verification_gate_fires_no_prompt_and_leaves_the_form_untouched() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_with_gated_row(TrustMode::NoVerification);
    let credentials = mock_credentials().expect("the mock service is published");
    let before = harness.nav.depth();

    harness.tap_node("the no-verification saved row", |node| {
        node.value() == Some(CRED_ENDPOINT)
    });
    // A scrim tap is `confirm_dialog`'s cancel route: it pops with an empty
    // result, which reads as "cancelled" (see `ui::dialogs`' module docs), so
    // `ConnectController::recover_and_connect` is never called at all.
    harness.tap(SCRIM_POINT);
    harness.settle();

    assert_eq!(harness.nav.depth(), before, "the dialog closed");
    assert_eq!(
        credentials.prompts().len(),
        0,
        "a canceled gate must never raise the biometric prompt — the whole \
         point of gating before the unlock rather than after it"
    );
    assert!(
        !expect_connection_controller().connected.get_untracked(),
        "nothing connected"
    );
    assert_eq!(
        harness.field_values().first().map(String::as_str),
        Some(""),
        "cancel backs out silently: the manual form is untouched, not prefilled"
    );
}

#[test]
fn confirming_the_no_verification_gate_unlocks_and_connects() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_with_gated_row(TrustMode::NoVerification);
    let credentials = mock_credentials().expect("the mock service is published");
    let before = harness.nav.depth();

    harness.tap_node("the no-verification saved row", |node| {
        node.value() == Some(CRED_ENDPOINT)
    });
    assert_eq!(harness.nav.depth(), before + 1, "the confirm dialog opened");

    harness.tap_node("the dialog's confirm action", |node| {
        node.role() == Role::Button && node.label() == Some("Connect anyway")
    });
    pump_until_done("the recovered connect", || {
        expect_connection_controller().connected.get_untracked()
    });
    harness.settle();

    assert_eq!(
        credentials.prompts().len(),
        1,
        "confirming raised exactly one biometric prompt — the same mock \
         semantics as an ungated row"
    );
    assert!(harness.nav.depth() > before, "…and landed on /sessions");
}

#[test]
fn a_verified_saved_rows_credential_flow_is_never_gated() {
    let _g = serial();
    let _ambient = setup();
    // Sequential, not interleaved (`tests/settings.rs`'s own pattern): each
    // harness re-provides the app-scoped connection controller, so only the
    // most recently mounted one is live.
    for trust_mode in [TrustMode::System, TrustMode::Pinned] {
        let mut harness = mount_with_gated_row(trust_mode);
        let credentials = mock_credentials().expect("the mock service is published");
        let before = harness.nav.depth();

        harness.tap_node("the saved row", |node| node.value() == Some(CRED_ENDPOINT));
        pump_until_done("the recovered connect", || {
            expect_connection_controller().connected.get_untracked()
        });
        harness.settle();

        assert_eq!(
            credentials.prompts().len(),
            1,
            "{trust_mode:?}: exactly one prompt — no confirm dialog interposed"
        );
        assert_eq!(
            harness.nav.depth(),
            before + 1,
            "{trust_mode:?}: the tap went straight to /sessions, no dialog page"
        );
    }
}

// ---------------------------------------------------------------------------
// The save-credential offer, answered on Connect
//
// The whole point of the move is an *ordering*: the dialog and its biometric
// prompt happen on this screen, before the redirect to `/sessions` — so every
// case below reads the navigator's depth at two moments (right after the
// answer, and once the save has settled) rather than only at the end.
// ---------------------------------------------------------------------------

/// The offer dialog's title, its accept and its decline — transcribed, not
/// imported, for the reason [`QR_LABEL`] is: these are the words a user reads,
/// and a test that imported them could not catch them changing.
const OFFER_TITLE: &str = "Save this server?";
const OFFER_SAVE: &str = "Save";
const OFFER_NOT_NOW: &str = "Not now";
/// …and the same dialog's title and body when the server already holds a
/// stored credential, which makes the question a replacement rather than a
/// first save. Likewise transcribed.
const REPLACE_TITLE: &str = "Replace saved credential?";
const REPLACE_BODY: &str =
    "Replace the saved credential for Cred rig with a fresh one for this device?";

/// [`CredentialStatus::Failed`]'s own sentence — what a refused gate is
/// reported with (`CredentialStatus::message`).
const GATE_FAILED_TOAST: &str = "Biometric authentication failed. Please log in manually.";

/// Type `url` into the real screen and press the CTA, with the toast queue
/// cleared first so only what the connect itself raises is left.
fn connect_through_the_cta(harness: &mut Harness, url: &str) {
    type_url(harness, url);
    app_toasts().clear();
    harness.tap_node("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });
}

/// Whether `text` is on screen as some node's label or value — a pushed
/// dialog's *title* is the only thing that names which page is up, since the
/// depth alone cannot tell a dialog from the `/sessions` push it is holding
/// back.
fn shows_text(harness: &Harness, text: &str) -> bool {
    harness
        .root
        .semantics()
        .nodes
        .iter()
        .any(|(_, node)| node.label() == Some(text) || node.value() == Some(text))
}

/// Whether the offer dialog is the page on screen, in **either** of its two
/// wordings (a first-time save, or a replacement of a credential already
/// stored).
fn offer_dialog_up(harness: &Harness) -> bool {
    shows_text(harness, OFFER_TITLE) || shows_text(harness, REPLACE_TITLE)
}

/// The saved row for `endpoint` as the app's own store holds it.
fn saved_row(endpoint: &str) -> SavedServer {
    expect_saved_servers()
        .list()
        .expect("the mock store never fails")
        .into_iter()
        .find(|server| server.endpoint() == endpoint)
        .unwrap_or_else(|| panic!("no saved row for {endpoint}"))
}

#[test]
fn a_first_connect_asks_on_connect_and_lands_only_once_the_save_has_settled() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();
    let credentials = mock_credentials().expect("tests/support opted this process into the mock");

    connect_through_the_cta(&mut harness, UNKNOWN_URL);

    let connection = expect_connection_controller();
    assert!(
        connection.connected.get_untracked(),
        "the connection itself opened"
    );
    assert!(
        offer_dialog_up(&harness),
        "the offer is asked here, on Connect — not on the screen the redirect \
         would have landed on: {:?}",
        node_labels(&harness.root)
    );
    assert!(
        credentials.prompts().is_empty(),
        "…and nothing is gated until it is answered"
    );

    harness.tap_node("the offer's Save action", |node| {
        node.role() == Role::Button && node.label() == Some(OFFER_SAVE)
    });

    assert_eq!(
        harness.nav.depth(),
        1,
        "the dialog closed and *nothing* was pushed: the redirect is waiting on \
         the save the tap just started"
    );
    harness.pump_until("the credential save", |harness| harness.nav.depth() > 1);

    assert_eq!(
        credentials.prompts().len(),
        1,
        "exactly one biometric prompt, raised on Connect: {:?}",
        credentials.prompts()
    );
    assert_eq!(
        harness.nav.depth(),
        2,
        "…and only a settled save pushed /sessions"
    );
    assert!(
        saved_row(UNKNOWN_URL).has_credential,
        "the row records the credential the save stored"
    );
    assert!(
        credentials.stored(UNKNOWN_URL).is_some(),
        "…and the secure store holds it"
    );
    assert_eq!(
        connection.credential_offer.get_untracked(),
        None,
        "the offer is consumed"
    );
}

#[test]
fn the_manual_cta_reports_busy_between_accepting_the_offer_and_the_save_settling() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    connect_through_the_cta(&mut harness, UNKNOWN_URL);
    assert!(offer_dialog_up(&harness));

    harness.tap_node("the offer's Save action", |node| {
        node.role() == Role::Button && node.label() == Some(OFFER_SAVE)
    });
    assert_eq!(
        harness.nav.depth(),
        1,
        "the dialog closed and the redirect is still held on the save"
    );

    // The mint → biometric → store → revoke chain is running right now, with
    // nothing else on screen to say so — the manual CTA is the only thing
    // between the user and a re-tap, so it must report busy *here*, not just
    // once the save has already settled (`frust::button`'s own contract:
    // `.loading(true)` reports disabled).
    let cta_disabled = harness
        .root
        .semantics()
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Button && node.label() == Some(CONNECT_LABEL))
        .map(|(_, node)| node.is_disabled());
    assert_eq!(
        cta_disabled,
        Some(true),
        "the manual CTA must render busy while the offer's save is still \
         running: {:?}",
        node_labels(&harness.root)
    );

    harness.pump_until("the credential save", |harness| harness.nav.depth() > 1);
}

#[test]
fn declining_the_offer_lands_at_once_and_saves_nothing() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();
    let credentials = mock_credentials().expect("tests/support opted this process into the mock");

    connect_through_the_cta(&mut harness, UNKNOWN_URL);
    assert!(offer_dialog_up(&harness));

    harness.tap_node("the offer's Not now action", |node| {
        node.role() == Role::Button && node.label() == Some(OFFER_NOT_NOW)
    });

    assert!(!offer_dialog_up(&harness), "the dialog closed");
    assert_eq!(
        harness.nav.depth(),
        2,
        "a decline has nothing to wait for: the held redirect goes through on \
         the spot"
    );
    assert!(
        credentials.prompts().is_empty(),
        "declining never reaches the gate: {:?}",
        credentials.prompts()
    );
    assert_eq!(credentials.stored(UNKNOWN_URL), None, "…and stores nothing");
    assert!(
        !saved_row(UNKNOWN_URL).has_credential,
        "…so the row the login upsert wrote still claims no credential"
    );
    assert_eq!(
        expect_connection_controller()
            .credential_offer
            .get_untracked(),
        None,
        "the offer is consumed either way"
    );
}

#[test]
fn dismissing_the_offer_unanswered_lands_too() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();
    let credentials = mock_credentials().expect("tests/support opted this process into the mock");

    connect_through_the_cta(&mut harness, UNKNOWN_URL);
    assert!(offer_dialog_up(&harness));

    // A scrim tap pops the dialog with an empty result — the same route
    // `Escape` and an Android back press take. The held redirect must read it
    // as "not now" rather than as no answer at all: the connection is already
    // open, and a dialog that fell off the screen cannot be what leaves the
    // user parked on the form.
    harness.tap(SCRIM_POINT);
    harness.settle();

    assert!(!offer_dialog_up(&harness), "the dialog closed");
    assert_eq!(
        harness.nav.depth(),
        2,
        "…and the redirect it was holding went through"
    );
    assert!(credentials.prompts().is_empty(), "nothing was gated");
    assert!(
        !saved_row(UNKNOWN_URL).has_credential,
        "…and nothing was saved"
    );
    assert_eq!(
        expect_connection_controller()
            .credential_offer
            .get_untracked(),
        None,
        "the offer is consumed"
    );
}

#[test]
fn a_denied_gate_says_why_and_still_lands_on_sessions() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();
    let credentials = mock_credentials().expect("tests/support opted this process into the mock");
    credentials.set_gate(CredentialStatus::Failed);

    connect_through_the_cta(&mut harness, UNKNOWN_URL);
    harness.tap_node("the offer's Save action", |node| {
        node.role() == Role::Button && node.label() == Some(OFFER_SAVE)
    });
    harness.pump_until("the denied save", |harness| harness.nav.depth() > 1);

    assert_eq!(
        credentials.prompts().len(),
        1,
        "the gate was asked exactly once"
    );
    assert!(
        app_toasts()
            .pending_untracked()
            .iter()
            .any(|message| message == GATE_FAILED_TOAST),
        "a refused gate says so: {:?}",
        app_toasts().pending_untracked()
    );
    assert!(
        !saved_row(UNKNOWN_URL).has_credential,
        "nothing was stored, so the row claims no credential"
    );
    assert_eq!(
        harness.nav.depth(),
        2,
        "…and the user is still taken to the session they just opened — a \
         declined credential is not a reason to strand them on Connect"
    );
}

#[test]
fn a_repeat_connect_to_a_known_server_still_asks() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();
    let credentials = mock_credentials().expect("tests/support opted this process into the mock");

    // `VALID_URL` is one of the seeded rows, so this is a second connect to a
    // server the app already knows — and a typed token at a known server is
    // exactly the case a suppressed offer used to strand: nothing could ever
    // replace the stored credential once its token was revoked server-side.
    connect_through_the_cta(&mut harness, VALID_URL);

    assert!(
        offer_dialog_up(&harness),
        "an already-saved row is no longer a reason to withhold the offer: {:?}",
        node_labels(&harness.root)
    );
    assert!(
        shows_text(&harness, OFFER_TITLE),
        "…and with nothing stored for it yet, the question is still a plain \
         first-time save: {:?}",
        node_labels(&harness.root)
    );
    assert!(
        credentials.prompts().is_empty(),
        "nothing is gated until the question is answered"
    );

    harness.tap_node("the offer's Not now action", |node| {
        node.role() == Role::Button && node.label() == Some(OFFER_NOT_NOW)
    });
    assert_eq!(
        harness.nav.depth(),
        2,
        "…and declining lands on /sessions exactly as the connect always did"
    );
}

#[test]
fn a_manual_connect_at_a_server_with_a_stored_credential_asks_to_replace_it() {
    let _g = serial();
    let _ambient = setup();
    // The app's own stores, seeded in `mount_seeded`'s window: a saved row that
    // already holds a credential, which is what turns the offer's wording from
    // a save into a replacement.
    let mut harness = Harness::mount_seeded(|| {
        let mut row = credential_row(true, Some("token_2"));
        row.last_used_ms = epoch_millis();
        expect_saved_servers()
            .save(row)
            .expect("the mock store never fails");
        mock_credentials()
            .expect("tests/support opted this process into the mock")
            .seed(CRED_ENDPOINT, CRED_TOKEN);
    });

    connect_through_the_cta(&mut harness, CRED_ENDPOINT);

    assert!(
        shows_text(&harness, REPLACE_TITLE),
        "the dialog reads as an update, not as a first-time save: {:?}",
        node_labels(&harness.root)
    );
    assert!(
        shows_text(&harness, REPLACE_BODY),
        "…and its body names the server whose credential is being replaced: {:?}",
        node_labels(&harness.root)
    );
    assert!(
        !shows_text(&harness, OFFER_TITLE),
        "…rather than both wordings at once"
    );
}

// ---------------------------------------------------------------------------
// The splash's static mock fields
// ---------------------------------------------------------------------------

#[test]
fn the_live_screens_fields_stay_enabled() {
    let _g = serial();
    let _ambient = setup();
    let harness = Harness::mount();

    let disabled: Vec<bool> = harness
        .root
        .semantics()
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.role(), Role::TextInput | Role::PasswordInput))
        .map(|(_, node)| node.is_disabled())
        .collect();
    assert_eq!(
        disabled,
        vec![false, false],
        "the live screen's two fields both take input normally"
    );
}

#[test]
fn the_static_splash_mocks_fields_refuse_focus_on_tap() {
    // `connect_pieces` (== `connect_pieces_with(theme, None, saved)`) is
    // exactly what the cold-start splash assembles — no navigator, no
    // controller, and (per the module docs) no callback here reads the state
    // it's handed, so a bare `()` state and a standalone `RenderRoot` reproduce
    // it directly (mirroring `subtitle_lines`'s own standalone-piece pattern
    // below).
    let _g = serial();
    let _ambient = setup();

    let theme = Theme::glyph_baseline();
    let mut logic = move |_state: &mut ()| connect_layout(connect_pieces::<()>(&theme, &[]));
    let mut root: RenderRoot<(), AnyView<()>> = RenderRoot::new();
    let mut state = ();
    let mut tcx = TextContext::new();
    frame_sized(&mut root, &mut logic, &mut state, &mut tcx, SURFACE);

    let fields: Vec<(Role, bool, bool)> = root
        .semantics()
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.role(), Role::TextInput | Role::PasswordInput))
        .map(|(_, node)| (node.role(), node.is_disabled(), node.is_read_only()))
        .collect();
    assert_eq!(fields.len(), 2, "the splash mock still renders both fields");
    assert!(
        fields.iter().all(|(_, _, read_only)| *read_only),
        "the splash's static fields must present as read-only — a stray tap \
         during the staggering entrance must not focus or caret a field with \
         nothing behind it: {fields:?}"
    );
    assert!(
        fields.iter().all(|(_, disabled, _)| !*disabled),
        "…and NOT as disabled: `enabled(false)` is what dimmed the border and \
         placeholder, so the fields popped to full alpha the instant the live \
         screen took over: {fields:?}"
    );

    // A direct tap on either field's center must not move focus onto it —
    // read-only hangs off the *same* focus gate `enabled(false)` did
    // (`TextInputWidget::interactive` is `enabled && !read_only`): a `Down`
    // inside one neither requests focus nor captures the pointer.
    let centers: Vec<Point> = root
        .semantics()
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.role(), Role::TextInput | Role::PasswordInput))
        .filter_map(|(_, node)| node.bounds())
        .map(|b| Point::new((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0))
        .collect();
    assert_eq!(centers.len(), 2, "both fields carry bounds to tap");
    for point in centers {
        tap(&mut root, &mut state, point);
    }
    frame_sized(&mut root, &mut logic, &mut state, &mut tcx, SURFACE);
    assert!(
        root.semantics().focus.is_none(),
        "a read-only field must not become the tree's focus node after a \
         direct tap"
    );
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
    let colors = Palette::from_theme(&theme);
    let mut logic = move |_state: &mut ()| heading::<()>(&theme, &colors);
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
             back per cluster (see widgets::fields' field_font_family)"
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
