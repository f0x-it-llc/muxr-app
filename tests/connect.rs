//! Headless behavior tests for the real Connect screen (`src/features/connect`)
//! — no GPU, no window: a `RenderRoot` over the screen itself, driven through
//! `tests/support`.
//!
//! `NavigatorWidget` publishes no semantics for its pages, so [`Harness`] mounts
//! a `Stack` of [`connect_page`] (fully addressable) over a [`navigator`] with a
//! zero-size base page: the empty top page falls through to the screen, while a
//! real push (the QR sheet, a landed `/sessions`) still owns input and reads as
//! a depth change. `mount_seeded` re-points the app's router at the fixture's
//! own navigator, so the screen pushes through the real route table and guard.
//!
//! The screen is ~830px tall, so every frame here lays out at [`SURFACE`], not
//! `support`'s default. The placeholder-shaping tests at the bottom shape the
//! fields' real placeholder strings through a bare [`TextContext`], guarding
//! against the device defect they were fixed for (digits shaping a full em
//! apart), which frust 0.6.4's host shaping no longer reproduces.

use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use frust::authoring::text::{FamilyName, FontFamily, GenericSlot, TextStyle};
use frust::authoring::{CornerInset, CornerInsets, WindowInsets};
use frust::{
    Align, Alignment, AnyView, Component, Get, GetUntracked, NavigatorController, Set, SizedBox,
    Stack, WindowMetrics, any, navigator, provide_context,
};
use frust_core::accesskit::{Node, Rect, Role, Toggled};
use frust_core::{FrameTime, NamedKey, PaintScene, PointerPhase, RenderRoot};
use frust_scene::GlyphRun;
use frust_text::TextContext;
use kurbo::{Point, Size};
use peniko::Color;

use muxr::connection::{
    ConnectTarget, ConnectionController, TrustMode, expect_connection_controller,
};
use muxr::failure::AppFailure;
use muxr::features::connect::data::repositories::MockCredentialService;
use muxr::features::connect::data::repositories::mock_saved_servers::MockSavedServersRepository;
use muxr::features::connect::di::{
    KeyringHandle, expect_keyring, expect_saved_servers, mock_credentials,
};
use muxr::features::connect::domain::entities::{
    CredentialStatus, PairingPayload, SavedServer, StoreRequest, UnlockRequest, UnlockedCredential,
    UnlockedKeyring, epoch_millis,
};
use muxr::features::connect::domain::keyring::Keyring;
use muxr::features::connect::domain::repositories::{CredentialService, SavedServersRepository};
use muxr::features::connect::domain::use_cases::parse_pairing::parse_pairing_uri;
use muxr::features::connect::presentation::controller::{
    ConnectController, ConnectRetry, CredentialOffer, GatePrompt, MANUAL_RESET_MESSAGE,
    OfferPrompt, ServerLimitBlock, URL_ERROR_MESSAGE, VAULT_RESET_MESSAGE,
    set_biometrics_available_for_tests,
};
use muxr::features::connect::presentation::pages::{connect_layout, connect_page, splash_assembly};
use muxr::features::connect::presentation::widgets::badge::{SUBTITLE, heading};
use muxr::features::connect::presentation::widgets::cta::CONNECT_LABEL;
use muxr::features::connect::presentation::widgets::fields::{
    SERVER_URL_PLACEHOLDER, TOKEN_PLACEHOLDER, field_text_style,
};
use muxr::features::connect::presentation::widgets::getting_started::GETTING_STARTED_BODY;
use muxr::features::connect::presentation::widgets::pairing::{
    PAIRED_CONNECT_LABEL, PAIRING_CAPTION, PINNED_CONNECT_LABEL, READ_ONLY_CHIP,
};
use muxr::features::connect::presentation::widgets::qr_sheet::{NO_CAMERA, qr_sheet_body};
use muxr::features::connect::presentation::widgets::saved::{NO_SAVED_SERVERS, PRO_TAG};
use muxr::features::connect::presentation::widgets::tablet::{
    tablet_splash_layout, tablet_splash_pieces,
};
use muxr::features::connect::presentation::widgets::trust::{PAIRED_CAPTION, PINNED_CAPTION};
use muxr::features::connect::presentation::widgets::{Live, connect_pieces, connect_pieces_with};
use muxr::features::pro::data::repositories::MockEntitlementRepository;
use muxr::features::pro::di::{
    SharedEntitlementRepository, expect_pro_controller, mock_entitlement,
};
use muxr::features::pro::domain::entities::Entitlement;
use muxr::features::pro::presentation::RESTORE_LABEL;
use muxr::features::pro::presentation::controller::ProController;
use muxr::features::settings::data::repositories::MockAuthTokensRepository;
use muxr::links::set_link_opener_for_tests;
use muxr::routes::{app_nav, install_app_router};
use muxr::ui::palette::Palette;
use muxr::ui::toasts::app_toasts;
use muxr::{AppState, MuxrApp};

mod support;
use support::{
    EPSILON, RecScene, char_key, frame_sized, named_key, node_labels, pointer, serial,
    settle_sized, setup, tap,
};

/// The test surface: wide enough for the CTA to sit clear of the edges, and
/// tall enough for the whole screen (~830px of pieces) to lay out unclipped —
/// see the module docs' local-helper note.
const SURFACE: Size = Size::new(800.0, 1200.0);

/// A point in the top-left corner: scrim on the QR sheet (whose panel is
/// bottom-anchored), and nothing at all on the screen itself.
const SCRIM_POINT: Point = Point::new(4.0, 4.0);

/// How many pump-and-settle rounds [`Harness::flush`] runs — comfortably more
/// than the paywall's spawn → store answer → staged close → completion-frame
/// chain needs (`tests/pro.rs`'s own count).
const FLUSH_ROUNDS: usize = 4;

/// The URL the happy path types — the mock's own placeholder.
const VALID_URL: &str = SERVER_URL_PLACEHOLDER;
/// …and one that still cannot be a dial target: a bare host now parses (its
/// posture's own default port applies), so this keeps its explicit `:port`
/// but makes that port unparseable.
const INVALID_URL: &str = "not-an-endpoint:not-a-port";

/// The trust selector's **two** rows (`widgets::trust`'s `trust_option_label`) — the
/// mock's own pair. `TrustMode::Pinned` is deliberately not one of them; see
/// `widgets::trust::trust_selector`.
const SYSTEM_TRUST: &str = "Public domain (CA)";
const INSECURE_TRUST: &str = "Trust anyway";
/// The chip the insecure row carries (`glyph::badge` → `Role::Status`).
const INSECURE_CHIP: &str = "insecure";

/// The QR affordance's accessible name. Transcribed rather than imported (the
/// CTA's is `widgets::cta::CONNECT_LABEL`, which *is* the run it paints, but
/// this one is the button's *name* — a test that imported it could not catch
/// it changing). See [`BANNED_GLYPH`] for why it is words, not a mark.
const QR_LABEL: &str = "Pair by QR code";
/// The getting-started affordance's accessible name, its dialog's title and
/// the two actions — transcribed for the reason [`QR_LABEL`] is: the words a
/// user reads. The title differs from the button's name on purpose, so a
/// title on screen can only mean the dialog is up.
const INFO_LABEL: &str = "Getting started";
const GETTING_STARTED_TITLE: &str = "Getting started with Muxr";
const OPEN_GUIDE_LABEL: &str = "Open guide";
const CLOSE_LABEL: &str = "Close";
/// Where the guide action lands.
const DOCS_URL: &str = "https://muxr.app/docs.html";

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
/// `docs/splash.html`'s `.qr-square{width:44px}` and `.cta-row{gap:8px}`,
/// transcribed: what the CTA gives up of the layer to the square beside it.
const QR_SQUARE: f64 = 44.0;
const CTA_ROW_GAP: f64 = 8.0;
const SQUARE_SHARE: f64 = QR_SQUARE + CTA_ROW_GAP;

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
/// the shape `muxr`'s pages read for the responsive width-clamp pass. `None`
/// leaves context unset, the state every other test in this suite mounts under.
fn mount_with_metrics(metrics: Option<WindowMetrics>) -> Harness {
    support::mount_with_metrics(metrics, Harness::mount)
}

/// `mount` under `size`'s metrics, every frame then laid out at `size` too —
/// how the size-class cases see the structure a real window of that shape gets.
fn sized(size: Size, mount: impl FnOnce() -> Harness) -> Harness {
    let metrics = support::metrics_at(size.width, size.height);
    let mut harness = support::mount_with_metrics(Some(metrics), mount);
    harness.surface = size;
    harness.settle();
    harness
}

/// How a mounted fixture's install is entitled.
///
/// Every case but the free-tier gate's own mounts [`Tier::Pro`]: the app's
/// seeded store holds three rows, which a *free* install may not connect to at
/// all (`features::pro`'s one-server rule), and every one of those cases is
/// about something else. The gate's own cases mount [`Tier::Free`] over a store
/// they seed themselves ([`mount_limited`]).
#[derive(Clone, Copy)]
enum Tier {
    Free,
    Pro,
}

impl Tier {
    fn entitlement(self) -> Entitlement {
        match self {
            Tier::Free => Entitlement::Free,
            Tier::Pro => Entitlement::Pro,
        }
    }
}

/// Publish `tier` on both halves of the seam, inside [`Harness::mount_seeded`]'s
/// window: the scriptable store, so `MuxrApp::init`'s own launch refresh
/// republishes the same answer, and the live signal, since the first verdict is
/// taken long before that refresh lands.
fn entitle(tier: Tier) {
    mock_entitlement()
        .expect("tests/support opted this process into the scriptable store")
        .set_entitlement(tier.entitlement());
    expect_pro_controller().entitlement.set(tier.entitlement());
}

/// The Connect screen plus a live navigator over the same controller.
struct Harness {
    nav: NavigatorController<AppState>,
    root: RenderRoot<AppState, AnyView<AppState>>,
    logic: Logic,
    state: AppState,
    tcx: TextContext,
    /// What every frame lays out at: [`SURFACE`] unless [`sized`] said otherwise.
    surface: Size,
}

impl Harness {
    /// [`Harness::mount_seeded`] with nothing seeded.
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
        Self::mount_tiered(Tier::Pro, seed)
    }

    /// [`Harness::mount_seeded`] over an install entitled as `tier` — see
    /// [`Tier`] for why every case but the gate's own mounts entitled.
    fn mount_tiered(tier: Tier, seed: impl FnOnce()) -> Self {
        let state = MuxrApp.init();
        entitle(tier);
        seed();
        let nav: NavigatorController<AppState> = NavigatorController::new();
        // Re-points `routes::app_nav()` at this fixture's own navigator, guard
        // and all.
        let _router = install_app_router(&nav, &nav, expect_connection_controller());
        let nav_for_navigator = nav.clone();
        let nav_for_screen = nav.clone();
        let logic: Logic = Box::new(move |_state: &mut AppState| {
            any(Stack(vec![
                any(connect_page(nav_for_screen.clone())),
                any(navigator(&nav_for_navigator, empty_base_page)),
            ]))
        });
        let mut harness = Harness {
            nav,
            root: RenderRoot::new(),
            logic,
            state,
            tcx: TextContext::new(),
            surface: SURFACE,
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
            self.surface,
        )
    }

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

    /// Pump the UI-thread queue and settle the frames [`FLUSH_ROUNDS`] times,
    /// then deliver a pop's result with a harmless `Move`.
    ///
    /// [`Harness::settle`] alone carries a dialog, but not the paywall: its
    /// store lookup, its staged close and the follow-on that close chains are
    /// three separate queued steps (`tests/pro.rs` documents the chain).
    fn flush(&mut self) {
        let runtime =
            frust_reactive::ReactiveRuntime::get().expect("setup() installed the runtime");
        for _ in 0..FLUSH_ROUNDS {
            runtime.pump_local();
            std::thread::sleep(std::time::Duration::from_millis(1));
            self.settle();
        }
        self.root
            .event(&mut self.state, &pointer(PointerPhase::Move, SCRIM_POINT));
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
    /// the real `frust_glyph::toggle` contributes itself (its own
    /// `.label(...)` call is what this node's label reads).
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

/// Focus the server-URL field and type `url` into it. Asserting the value
/// *after* a rebuild is what makes this a round trip through the controller:
/// `TextInput` is controlled, so a rebuild re-applies whatever it stored.
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

#[test]
fn tapping_the_insecure_rows_own_warning_chip_still_selects_it() {
    // The regression a node-center tap on the `Role::RadioButton` node alone
    // cannot prove: `widgets::trust::trust_row` used to swap its full-row
    // press for `frust_glyph::radio`'s own ring+label bounds only, shrinking
    // the row's live tap target away from the chip entirely. The chip's own
    // `Role::Status` node sits *outside* the radio node's bounds, so tapping
    // at the chip's own center exercises exactly the region the review
    // proved untestable via `tap_node`-on-the-radio alone (see
    // `selecting_a_trust_mode_sticks`, which already covers the radio's own
    // bounds and must keep passing unchanged).
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    assert!(!harness.trust_selected(INSECURE_TRUST));

    harness.tap_node("the insecure row's warning chip", |node| {
        node.role() == Role::Status && node.label() == Some(INSECURE_CHIP)
    });

    assert!(
        harness.trust_selected(INSECURE_TRUST),
        "a tap on the chip — outside the radio's own bounds, inside the \
         row's restored full-row press target — still selects the row"
    );
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

    // `.cta-row` fills the layer between its two 22px paddings: the CTA takes
    // all of it but the QR square and the gap before it.
    let cta = harness.bounds("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });
    let layer = SURFACE.width - 2.0 * LAYER_PAD_X;
    assert!(
        (cta.width() - (layer - SQUARE_SHARE)).abs() < 0.5,
        "the CTA spans {:.1}px of a {layer:.1}px layer",
        cta.width()
    );
    assert!(
        cta.height() >= MIN_TAP_TARGET,
        "the CTA is {:.1}px tall, under the {MIN_TAP_TARGET}px minimum",
        cta.height()
    );

    // …and the square keeps its own 44px box at the layer's trailing edge.
    let square = harness.bounds("the QR square", |node| {
        node.role() == Role::Button && node.label() == Some(QR_LABEL)
    });
    assert!(
        (square.width() - QR_SQUARE).abs() < 0.5 && (square.height() - QR_SQUARE).abs() < 0.5,
        "{square:?}"
    );
    assert!(
        (square.x1 - (SURFACE.width - LAYER_PAD_X)).abs() < 0.5
            && (square.x0 - cta.x1 - CTA_ROW_GAP).abs() < 0.5,
        "the square trails the CTA by the row's gap: {square:?} vs {cta:?}"
    );
}

// ---------------------------------------------------------------------------
// Responsive width-clamp pass
// ---------------------------------------------------------------------------

/// Phone-landscape metrics as wide as [`SURFACE`]: past the wide breakpoint
/// but Compact (shortest side under 600), the one shape the clamp serves.
fn compact_wide() -> WindowMetrics {
    support::metrics_at(SURFACE.width, 390.0)
}

#[test]
fn wide_metrics_clamp_and_center_the_content_column() {
    let _g = serial();
    let _ambient = setup();
    let harness = mount_with_metrics(Some(compact_wide()));

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
        (cta.width() - (CONNECT_CLAMP_WIDTH - 2.0 * LAYER_PAD_X - SQUARE_SHARE)).abs() < 0.5,
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
    let harness = mount_with_metrics(Some(support::metrics_at(400.0, SURFACE.height)));

    let cta = harness.bounds("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });
    let layer = SURFACE.width - 2.0 * LAYER_PAD_X;
    assert!(
        (cta.width() - (layer - SQUARE_SHARE)).abs() < 0.5,
        "narrow metrics take the same unclamped branch as no metrics: {:.1}",
        cta.width()
    );
    assert!((cta.x0 - LAYER_PAD_X).abs() < 0.5, "{:?}", cta);
}

/// The CTA's box in a standalone mount of the **splash's** assembly path:
/// `connect_layout(connect_pieces(..))` inside a `Stack`, exactly what
/// `crate::splash_overlay` hands the sequence. The `Stack` matters: it hands a
/// *bounded* height, where the live screen's `scroll_view` hands an unbounded
/// one — the difference `pages::CLAMP_ALIGNMENT` exists for.
fn splash_cta_bounds(saved: &[SavedServer]) -> Rect {
    let theme = frust_glyph::baseline();
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
    // The clamp lives in `connect_layout`, the one function both renderings
    // assemble through, so this asserts the two land on the same horizontal
    // box rather than that each happens to satisfy the same arithmetic.
    let harness = mount_with_metrics(Some(compact_wide()));
    let live = harness.bounds("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });
    let splash = splash_cta_bounds(&expect_saved_servers().list().expect("the mock lists"));

    assert!(
        (splash.x0 - live.x0).abs() < EPSILON && (splash.x1 - live.x1).abs() < EPSILON,
        "the splash's assembly and the live screen must occupy the same \
         horizontal box on a wide window: splash {splash:?} vs live {live:?}"
    );
    assert!(
        (splash.y0 - live.y0).abs() < EPSILON,
        "…and start at the same height — the static trust rows above the CTA \
         are exactly as tall as the live radio rows: splash {splash:?} vs live {live:?}"
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
    provide_context(support::metrics_at(400.0, SURFACE.height));
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
    provide_context(support::metrics_at(400.0, SURFACE.height));

    // The other half of the branch: below the breakpoint the splash spans the
    // layer exactly as the live screen does (the shared function takes one
    // branch for both), so the handoff has nothing to move on a phone either.
    let splash = splash_cta_bounds(&[]);
    let layer = SURFACE.width - 2.0 * LAYER_PAD_X - SQUARE_SHARE;
    assert!(
        (splash.width() - layer).abs() < EPSILON && (splash.x0 - LAYER_PAD_X).abs() < EPSILON,
        "narrow metrics leave the splash unclamped: {splash:?}"
    );
}

/// The CTA's and the saved header's boxes in a standalone mount of the
/// **splash's Tablet** assembly path —
/// `tablet_splash_layout(tablet_splash_pieces(..))` inside a `Stack`, what
/// `crate::splash_overlay` hands the sequence on a Tablet window — laid out at
/// `size` under whatever metrics are already in context.
fn tablet_splash_bounds(size: Size, full: bool, saved: &[SavedServer]) -> (Rect, Rect) {
    let theme = frust_glyph::baseline();
    let saved = saved.to_vec();
    let mut logic = move |_state: &mut ()| {
        any(Stack(vec![tablet_splash_layout(
            &theme,
            full,
            tablet_splash_pieces::<()>(&theme, &saved, full),
        )]))
    };
    let mut root: RenderRoot<(), AnyView<()>> = RenderRoot::new();
    let mut state = ();
    let mut tcx = TextContext::new();
    frame_sized(&mut root, &mut logic, &mut state, &mut tcx, size);
    let semantics = root.semantics();
    let find = |what: &str, pred: fn(&Node) -> bool| {
        semantics
            .nodes
            .iter()
            .find(|(_, node)| pred(node))
            .and_then(|(_, node)| node.bounds())
            .unwrap_or_else(|| panic!("the splash's Tablet assembly paints {what}"))
    };
    (
        find("the CTA", connect_cta),
        find("the saved header", saved_header),
    )
}

/// Whether `a` and `b` are the same box, to [`EPSILON`].
fn same_box(a: Rect, b: Rect) -> bool {
    (a.x0 - b.x0).abs() < EPSILON
        && (a.y0 - b.y0).abs() < EPSILON
        && (a.x1 - b.x1).abs() < EPSILON
        && (a.y1 - b.y1).abs() < EPSILON
}

#[test]
fn the_splash_assembly_follows_the_windows_size_class() {
    let _g = serial();
    let _ambient = setup();
    let theme = frust_glyph::baseline();
    // Nine staggered pieces on the Full band, the eight-piece phone column on
    // a Compact window — the one read that decides which screen the overlay
    // hands off onto.
    provide_context(support::metrics_at(FULL_TABLET.width, FULL_TABLET.height));
    let (tablet, _) = splash_assembly::<()>(&theme, &[]);
    assert_eq!(
        tablet.len(),
        9,
        "the Full band's interleaved panel and form"
    );
    provide_context(compact_wide());
    let (phone, _) = splash_assembly::<()>(&theme, &[]);
    assert_eq!(
        phone.len(),
        8,
        "the phone column, one piece per stagger slot"
    );
}

#[test]
fn the_full_tablet_splash_assembles_the_two_column_screen_in_place() {
    let _g = serial();
    let _ambient = setup();
    // The live screen first: `sized` leaves its metrics in context, so the
    // splash's mount below reads the same window.
    let harness = sized(FULL_TABLET, Harness::mount);
    let live_cta = harness.bounds("the connect CTA", connect_cta);
    let live_header = harness.bounds("the saved header", saved_header);
    let saved = expect_saved_servers().list().expect("the mock lists");

    let (cta, header) = tablet_splash_bounds(FULL_TABLET, true, &saved);
    assert!(
        same_box(cta, live_cta),
        "the splash's CTA lands on the live screen's box: {cta:?} vs {live_cta:?}"
    );
    assert!(
        same_box(header, live_header),
        "…and its saved header too: {header:?} vs {live_header:?}"
    );
    // …and it really is the two-column arrangement, not the phone column
    // happening to agree: the list sits beside the form.
    assert!(
        header.x1 <= cta.x0,
        "the splash assembles the list in the left panel: {header:?} vs {cta:?}"
    );
}

#[test]
fn the_collapsed_tablet_splash_folds_the_list_under_the_button_row_like_the_screen() {
    let _g = serial();
    let _ambient = setup();
    let harness = sized(COLLAPSED_TABLET, Harness::mount);
    let live_cta = harness.bounds("the connect CTA", connect_cta);
    let live_header = harness.bounds("the saved header", saved_header);
    let saved = expect_saved_servers().list().expect("the mock lists");

    let (cta, header) = tablet_splash_bounds(COLLAPSED_TABLET, false, &saved);
    assert!(
        same_box(cta, live_cta),
        "the splash's CTA lands on the live screen's box: {cta:?} vs {live_cta:?}"
    );
    assert!(
        same_box(header, live_header),
        "…and its saved header too: {header:?} vs {live_header:?}"
    );
    assert!(
        cta.y1 <= header.y0,
        "the collapsed band's folded order, button row over the list: {cta:?} vs {header:?}"
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
    // is a vector" is literally "there is a filled path inside the square" —
    // one of exactly as many paths as this screen has vector marks: the
    // square's, the getting-started mark, the dock's biometric mark, plus a
    // lock and a delete mark per saved row.
    let rows = expect_saved_servers()
        .list()
        .expect("the mock store never fails")
        .len();
    assert_eq!(
        scene.paths.len(),
        2 * rows + 3,
        "every vector on the screen is the square's, the getting-started, the dock's or a row's lock/delete mark: {:?}",
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
                "the scan mark paints inside the square's own box: {:?} in {fab:?}",
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

// ---------------------------------------------------------------------------
// The getting-started affordance
// ---------------------------------------------------------------------------

fn info_button(node: &Node) -> bool {
    node.role() == Role::Button && node.label() == Some(INFO_LABEL)
}

#[test]
fn the_getting_started_affordance_sits_in_the_top_trailing_corner() {
    let _g = serial();
    let _ambient = setup();
    let harness = Harness::mount();

    // The top-trailing corner: a store-front visitor's first tap, clear of the
    // form and of the mark at the top of the column.
    let info = harness.bounds("the getting-started affordance", info_button);
    assert!(
        info.x0 > SURFACE.width / 2.0,
        "in the trailing half of the surface: {info:?}"
    );
    assert!(
        info.y1 < SURFACE.height / 4.0,
        "at the top of the surface: {info:?}"
    );
    assert!(
        info.width() >= MIN_TAP_TARGET && info.height() >= MIN_TAP_TARGET,
        "clears the {MIN_TAP_TARGET}px platform minimum tap target: {info:?}"
    );
}

#[test]
fn tapping_the_affordance_raises_the_getting_started_dialog_and_close_dismisses_it() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();
    let before = harness.nav.depth();
    assert!(
        !shows_text(&harness, GETTING_STARTED_TITLE),
        "nothing explains itself until asked"
    );

    harness.tap_node("the getting-started affordance", info_button);
    assert_eq!(
        harness.nav.depth(),
        before + 1,
        "the dialog is pushed as its own page"
    );
    assert!(
        shows_text(&harness, GETTING_STARTED_TITLE),
        "…and it is the orientation dialog"
    );
    assert!(
        shows_text(&harness, OPEN_GUIDE_LABEL),
        "…offering the guide as an action"
    );

    harness.tap_node("the dialog's Close action", |node| {
        node.role() == Role::Button && node.label() == Some(CLOSE_LABEL)
    });
    assert_eq!(
        harness.nav.depth(),
        before,
        "Close pops the dialog and nothing else"
    );
}

#[test]
fn the_guide_action_hands_the_docs_url_to_the_link_opener_and_closes() {
    let _g = serial();
    let _ambient = setup();
    let opened: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&opened);
    set_link_opener_for_tests(move |url| {
        sink.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(url.to_string());
    });
    let mut harness = Harness::mount();
    let before = harness.nav.depth();

    harness.tap_node("the getting-started affordance", info_button);
    harness.tap_node("the dialog's guide action", |node| {
        node.role() == Role::Button && node.label() == Some(OPEN_GUIDE_LABEL)
    });

    assert_eq!(
        *opened.lock().unwrap_or_else(|e| e.into_inner()),
        vec![DOCS_URL.to_string()],
        "exactly one launch, of the setup guide"
    );
    assert_eq!(
        harness.nav.depth(),
        before,
        "…and the dialog closed behind it"
    );
}

#[test]
fn an_invalid_url_shows_the_inline_line_and_never_toasts_or_navigates() {
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

    assert!(
        app_toasts().pending_untracked().is_empty(),
        "a validation reject is the inline line, never a toast"
    );
    assert!(harness.shows_value(URL_ERROR_MESSAGE), "the inline line");
    assert!(
        !harness
            .root
            .semantics()
            .nodes
            .iter()
            .filter(|(_, node)| node.role() != Role::TextInput)
            .any(|(_, node)| node.value().is_some_and(|v| v.contains(INVALID_URL))),
        "…which never echoes what was typed (the field itself holds it, nothing else)"
    );
    assert_eq!(harness.nav.depth(), 1, "nothing was pushed");
    assert!(
        !expect_connection_controller().connected.get_untracked(),
        "and no connection was opened"
    );
}

#[test]
fn editing_the_url_clears_the_inline_line() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = Harness::mount();

    type_url(&mut harness, INVALID_URL);
    harness.tap_node("the connect CTA", |node| {
        node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
    });
    assert!(harness.shows_value(URL_ERROR_MESSAGE));

    harness.tap_node("the server URL field", |node| {
        node.role() == Role::TextInput
    });
    harness.type_text("x");
    assert!(!harness.shows_value(URL_ERROR_MESSAGE), "an edit clears it");
}

#[test]
fn a_rejected_url_then_a_successful_connect_leaves_no_inline_line() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(Vec::new());
    creds.controller.set_url(INVALID_URL.to_string());
    creds
        .controller
        .connect(&app_nav(), Rc::new(|_offer: CredentialOffer| {}), ungated());
    assert!(creds.controller.invalid.get_untracked());
    assert_eq!(
        creds.controller.url_error.get_untracked().as_deref(),
        Some(URL_ERROR_MESSAGE)
    );

    // Written straight to the signal: `set_url` would clear the line itself,
    // and this case is about the dial clearing it.
    creds.controller.url.set(VALID_URL.to_string());
    creds
        .controller
        .connect(&app_nav(), Rc::new(|_offer: CredentialOffer| {}), ungated());

    assert!(creds.connection.connected.get_untracked(), "it dialed");
    assert!(!creds.controller.invalid.get_untracked());
    assert_eq!(creds.controller.url_error.get_untracked(), None);
}

#[test]
fn applying_a_scanned_payload_clears_a_rejected_url() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(Vec::new());
    creds.controller.set_url(INVALID_URL.to_string());
    creds
        .controller
        .connect(&app_nav(), Rc::new(|_offer: CredentialOffer| {}), ungated());
    assert!(creds.controller.url_error.get_untracked().is_some());

    creds
        .controller
        .apply_scanned_payload(scanned_payload(false));

    assert!(!creds.controller.invalid.get_untracked());
    assert_eq!(creds.controller.url_error.get_untracked(), None);
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
// Token hygiene: clear on connect/disconnect, zeroizing holder
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

/// [`scanned_payload`]'s `tm=ca` sibling, for the same [`PAIRED_ENDPOINT`] and
/// token — the demo QR's own shape (`ca`, not `pin`).
fn scanned_payload_ca(read_only: bool) -> PairingPayload {
    let ro = u8::from(read_only);
    let uri = format!(
        "muxr://pair?v=2&h=192.168.1.100&p=50051&t={PAIRED_TOKEN_B64}&ro={ro}&n=Home%20Rig&tm=ca"
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
/// Not [`Harness`]: the screen hosts its controller through `use_controller`,
/// unreachable to a test mounting the screen, so the pieces are composed here
/// instead, over a controller the test also holds. The repository is
/// deliberately **empty**: the seeded store's own read-only row would otherwise
/// double-count this fixture's `read-only` chip assertion.
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
        let theme = frust_glyph::baseline();
        let live = Live {
            controller: Arc::clone(controller),
            on_connect: Rc::new(|| {}),
            on_connect_pinned,
            // This fixture's store is empty, so no saved row is ever painted
            // and nothing can reach either of these (the screen's own wiring is
            // exercised through `Harness` instead).
            on_saved_tap: Rc::new(|_index| {}),
            on_saved_delete: Rc::new(|_index| {}),
            on_unlock: Rc::new(|| {}),
            on_reset_credentials: Rc::new(|| {}),
            on_scan: Rc::new(|| {}),
        };
        let logic: FormLogic = Box::new(move |_state: &mut ()| {
            // Piece 8's rows are an input to the builder now (the splash passes
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

    fn trust_selected(&self, label: &str) -> bool {
        self.root
            .semantics()
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::RadioButton && node.label() == Some(label))
            .map(|(_, node)| node.is_selected() == Some(true))
            .unwrap_or_else(|| panic!("no trust row labelled {label:?}"))
    }

    /// Tap the trust-selector row labelled `label` and render the frame after
    /// it — [`Self::tap_button`]'s twin for a `Role::RadioButton` node.
    fn tap_trust_row(&mut self, label: &str) {
        let point = self
            .root
            .semantics()
            .nodes
            .iter()
            .find_map(|(_, node)| {
                if node.role() != Role::RadioButton || node.label() != Some(label) {
                    return None;
                }
                let bounds = node.bounds()?;
                Some(Point::new(
                    (bounds.x0 + bounds.x1) / 2.0,
                    (bounds.y0 + bounds.y1) / 2.0,
                ))
            })
            .unwrap_or_else(|| panic!("no trust row labelled {label:?}"));
        tap(&mut self.root, &mut self.state, point);
        self.frame();
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

/// A [`GatePrompt`] that fails the test if it is ever raised. Every case
/// outside the free-tier gate's own section connects well inside the one-server
/// rule, so a block reaching this is a regression rather than a state to
/// handle.
fn ungated() -> GatePrompt {
    Rc::new(|block: ServerLimitBlock| {
        panic!(
            "the free-tier gate refused this connect, asking to forget {:?}",
            block
                .to_forget
                .iter()
                .map(SavedServer::endpoint)
                .collect::<Vec<String>>()
        )
    })
}

/// A [`ProController`] over a free-tier store of its own. The fixtures below
/// build their dependencies by hand rather than through `MuxrApp::init`, so
/// there is no app-scoped one to resolve — and free is the tier whose rule
/// there is anything to test.
fn free_pro() -> Arc<ProController> {
    let repository = Arc::new(MockEntitlementRepository::free()) as SharedEntitlementRepository;
    Arc::new(ProController::new(repository))
}

/// A [`ConnectController`] over explicit stores — the same five dependencies
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
        free_pro(),
    )
}

/// [`controller_over`] with the token store and the entitlement handed in, for
/// the flows that assert on what was minted or revoked, and for the free-tier
/// gate's own cases.
fn controller_with_tokens(
    servers: Arc<MockSavedServersRepository>,
    credentials: Arc<MockCredentialService>,
    tokens: Arc<MockAuthTokensRepository>,
    connection: Arc<ConnectionController>,
    pro: Arc<ProController>,
) -> Arc<ConnectController> {
    Arc::new(ConnectController::new(
        servers,
        credentials,
        tokens,
        connection,
        pro,
        KeyringHandle::new(),
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
        Box::new(move |_state| any(qr_sheet_body(Rc::new(|| {}), Arc::clone(&controller))));
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
        Rc::new(move || controller.connect_from_pairing(&app_nav(), ungated()))
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
        Rc::new(move || controller.connect_from_pairing(&app_nav(), ungated()))
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

#[test]
fn choosing_a_trust_mode_by_hand_drops_the_pairing() {
    // The defect this fixes: `select_trust_mode` only moved the radio, while
    // both CTAs kept reading the stashed pairing's own posture underneath
    // it — "Trust anyway" over a `tm=ca` scan never actually dialed
    // insecurely, no matter which row the selector showed selected.
    let _g = serial();
    let _ambient = setup();
    let controller = pairing_controller();
    let mut form = Form::mount(&controller, Rc::new(|| {}));

    controller.apply_scanned_payload(scanned_payload_ca(false));
    form.frame();
    assert!(form.button(PAIRED_CONNECT_LABEL).is_some());

    form.tap_trust_row(INSECURE_TRUST);

    assert_eq!(
        controller.pending_pairing.get_untracked(),
        None,
        "a manual posture that disagrees with the pairing's own drops it"
    );
    assert_eq!(
        controller
            .validated_target()
            .expect("the prefilled endpoint is still dialable")
            .trust_mode,
        TrustMode::NoVerification,
        "…and the chosen posture is what a dial actually uses"
    );
    assert_eq!(
        controller.token.get_untracked().as_str(),
        PAIRED_TOKEN,
        "the scanned token stays typed even though the pairing itself is gone"
    );
    assert_eq!(
        form.button(PAIRED_CONNECT_LABEL),
        None,
        "the paired CTA goes with the pairing it depended on"
    );
}

#[test]
fn re_selecting_the_pairings_own_trust_keeps_it() {
    let _g = serial();
    let _ambient = setup();
    let controller = pairing_controller();
    let mut form = Form::mount(&controller, Rc::new(|| {}));

    controller.apply_scanned_payload(scanned_payload_ca(false));
    form.frame();
    assert!(form.button(PAIRED_CONNECT_LABEL).is_some());

    // `tm=ca` maps onto `TrustMode::System` — already the selected row.
    form.tap_trust_row(SYSTEM_TRUST);

    assert!(
        controller.pending_pairing.get_untracked().is_some(),
        "re-picking the pairing's own posture is not a manual override"
    );
    assert!(
        form.button(PAIRED_CONNECT_LABEL).is_some(),
        "…so the paired CTA stays up"
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
    /// The free-tier entitlement the controller takes every verdict from —
    /// held so a gate case can publish a landed purchase mid-flight.
    pro: Arc<ProController>,
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
    let pro = free_pro();
    let controller = controller_with_tokens(
        Arc::clone(&servers),
        Arc::clone(&credentials),
        Arc::clone(&tokens),
        Arc::clone(&connection),
        Arc::clone(&pro),
    );
    Creds {
        controller,
        credentials,
        servers,
        tokens,
        connection,
        pro,
        _router: router,
    }
}

/// Drive the UI-thread task queue until `done` answers `true`, or fail. The
/// credential flows are `spawn_local` chains over repositories with simulated
/// latency, so a single pump is not enough — `support::pump_until`'s loop
/// without a `RenderRoot`, since these cases drive a controller, not a screen.
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

/// Connect the fixture's connection to [`CRED_ENDPOINT`], as a login would, so
/// an accepted offer has a live session to save for. The offer prompt is a
/// **recorder**, not a dialog — these cases drive the save (mint, gate, rotate)
/// through the connection's own accept; the screen-level suite below drives the
/// dialog and navigation end to end. Each recorded entry is the offer's label
/// and whether it asked to *replace* a credential this device already holds.
fn connect_fixture(creds: &Creds) -> Rc<RefCell<Vec<(String, bool)>>> {
    let offered: Rc<RefCell<Vec<(String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
    let record = Rc::clone(&offered);
    creds.controller.set_url(CRED_ENDPOINT.to_string());
    creds.controller.connect(
        &app_nav(),
        Rc::new(move |offer: CredentialOffer| {
            record.borrow_mut().push((offer.label, offer.replacing))
        }),
        ungated(),
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
    creds.controller.connect_from_pairing(&app_nav(), ungated());
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
    creds.controller.connect_from_pairing(&app_nav(), ungated());
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
/// the app's own DI stores, so the row, the affordance and both stores are the
/// production wiring. Stamped now so the row sorts ahead of the seeded ones
/// (the list is most-recently-used first).
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
// The free-tier server gate (`features::pro`'s one-server rule)
// ---------------------------------------------------------------------------

/// The gate cases' own two endpoints. Neither is a seeded row — every case
/// below replaces the seeded store outright ([`only_saved`]), since three rows
/// block a free install's every connect on their own.
const LIMIT_A: &str = "limit-a.example:50051";
const LIMIT_B: &str = "limit-b.example:50051";
/// Their row labels, which are what the paywall's forget action names.
const LIMIT_A_LABEL: &str = "Limit rig";
const LIMIT_B_LABEL: &str = "Other rig";

/// The paywall's heading and the three actions a blocked connect can take,
/// transcribed rather than imported for the reason [`QR_LABEL`] is: these are
/// the words a user reads, and a test that imported them could not catch them
/// changing. [`PAYWALL_NOT_NOW`] reads the same as the offer dialog's own
/// decline by coincidence, not by sharing anything with it.
const UPGRADE_TITLE: &str = "Muxr Pro";
const UPGRADE_ACTION: &str = "Upgrade \u{b7} $4.99";
const FORGET_LIMIT_A: &str = "Forget Limit rig and connect";
const FORGET_LIMIT_B: &str = "Forget Other rig and connect";
const PAYWALL_NOT_NOW: &str = "Not now";

/// A saved row for `endpoint`, carrying a stored credential when
/// `has_credential` — which is what makes the row's own tap dial rather than
/// refill the form.
fn limit_row(endpoint: &str, label: &str, has_credential: bool) -> SavedServer {
    let (host, port) = endpoint.rsplit_once(':').expect("host:port");
    SavedServer {
        host: host.to_string(),
        port: port.parse().expect("a port"),
        trust_mode: TrustMode::System,
        label: label.to_string(),
        last_used_ms: epoch_millis(),
        read_only: false,
        has_credential,
        token_name: None,
        fingerprint: None,
    }
}

/// [`limit_row`] stamped an hour ago. The list is most-recently-used first, so
/// a row built this way sits *behind* one stamped now — which is what makes the
/// retry's index shift after a forget real rather than incidental.
fn older_limit_row(endpoint: &str, label: &str, has_credential: bool) -> SavedServer {
    SavedServer {
        last_used_ms: epoch_millis() - 60 * 60 * 1000,
        ..limit_row(endpoint, label, has_credential)
    }
}

/// Replace the app's own saved-server store with exactly `rows`, in
/// [`Harness::mount_seeded`]'s window.
fn only_saved(rows: Vec<SavedServer>) {
    let store = expect_saved_servers();
    for existing in store.list().expect("the mock store never fails") {
        store
            .remove(&existing.endpoint())
            .expect("the mock store never fails");
    }
    for row in rows {
        store.save(row).expect("the mock store never fails");
    }
}

/// Mount the real screen as a `tier` install remembering exactly `rows`.
fn mount_limited(tier: Tier, rows: Vec<SavedServer>) -> Harness {
    Harness::mount_tiered(tier, move || only_saved(rows))
}

/// [`mount_limited`] at the free tier's ceiling: one remembered server, its
/// credential seeded, so a forget has both halves to drop.
fn mount_at_the_limit() -> Harness {
    Harness::mount_tiered(Tier::Free, || {
        only_saved(vec![limit_row(LIMIT_A, LIMIT_A_LABEL, true)]);
        mock_credentials()
            .expect("tests/support opted this process into the mock")
            .seed(LIMIT_A, CRED_TOKEN);
    })
}

/// [`mount_at_the_limit`]'s legacy sibling: a free install remembering *two*
/// servers, both credentials seeded — a state a free install can only have
/// arrived at before the rule existed. [`LIMIT_A`] is the older of the two, so
/// it sits second and the retry that re-enters by row has an index shift to
/// survive.
fn mount_over_the_limit() -> Harness {
    Harness::mount_tiered(Tier::Free, || {
        only_saved(vec![
            older_limit_row(LIMIT_A, LIMIT_A_LABEL, true),
            limit_row(LIMIT_B, LIMIT_B_LABEL, true),
        ]);
        let credentials =
            mock_credentials().expect("tests/support opted this process into the mock");
        credentials.seed(LIMIT_A, CRED_TOKEN);
        credentials.seed(LIMIT_B, CRED_TOKEN);
    })
}

/// Every endpoint the app's own store holds, in list order.
fn app_saved_endpoints() -> Vec<String> {
    expect_saved_servers()
        .list()
        .expect("the mock store never fails")
        .iter()
        .map(SavedServer::endpoint)
        .collect()
}

/// …sorted, for the cases where two rows survive and their order is the
/// store's business rather than the assertion's.
fn app_saved_endpoints_sorted() -> Vec<String> {
    let mut endpoints = app_saved_endpoints();
    endpoints.sort();
    endpoints
}

/// A [`GatePrompt`] that records what it refused instead of raising the sheet —
/// the controller-level half of the gate, driven with no overlay host at all
/// ([`connect_fixture`]'s recorder shape).
fn recording_gate(seen: &Rc<RefCell<Vec<ServerLimitBlock>>>) -> GatePrompt {
    let seen = Rc::clone(seen);
    Rc::new(move |block: ServerLimitBlock| seen.borrow_mut().push(block))
}

/// The endpoints each recorded block asked to forget, in order.
fn blocked_endpoints(seen: &Rc<RefCell<Vec<ServerLimitBlock>>>) -> Vec<Vec<String>> {
    seen.borrow()
        .iter()
        .map(|block| block.to_forget.iter().map(SavedServer::endpoint).collect())
        .collect()
}

/// Answer the save offer a retried connect raises with "not now" — these cases
/// are about the dial behind it, and the offer's own behavior is covered above.
fn decline_the_offer(harness: &mut Harness) {
    assert!(
        offer_dialog_up(harness),
        "the dial went through and asked: {:?}",
        node_labels(&harness.root)
    );
    harness.tap_node("the offer's Not now action", |node| {
        node.role() == Role::Button && node.label() == Some(OFFER_NOT_NOW)
    });
}

#[test]
fn a_free_installs_second_server_raises_the_paywall_instead_of_dialing() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_at_the_limit();
    let before = harness.nav.depth();

    connect_through_the_cta(&mut harness, LIMIT_B);
    harness.flush();

    assert!(
        !expect_connection_controller().connected.get_untracked(),
        "the verdict runs ahead of the dial, so nothing was connected"
    );
    assert!(
        shows_text(&harness, UPGRADE_TITLE),
        "the paywall is the whole answer: {:?}",
        node_labels(&harness.root)
    );
    assert!(
        shows_text(&harness, FORGET_LIMIT_A),
        "…naming the one row standing in the way"
    );
    assert_eq!(
        harness.nav.depth(),
        before + 1,
        "the sheet is the only page pushed — nothing landed on /sessions"
    );
    assert_eq!(
        app_saved_endpoints(),
        vec![LIMIT_A.to_string()],
        "and nothing was remembered: the only place a new row is written is a \
         successful login"
    );
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "a refusal is the sheet, never a toast"
    );
}

#[test]
fn forgetting_the_row_in_the_way_dials_and_leaves_only_the_new_server_saved() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_at_the_limit();
    let credentials = mock_credentials().expect("tests/support opted this process into the mock");

    connect_through_the_cta(&mut harness, LIMIT_B);
    harness.flush();
    harness.tap_node("the paywall's forget-and-connect action", |node| {
        node.role() == Role::Button && node.label() == Some(FORGET_LIMIT_A)
    });
    harness.flush();
    decline_the_offer(&mut harness);

    assert!(
        expect_connection_controller().connected.get_untracked(),
        "the same connect the sheet refused went through behind it"
    );
    assert_eq!(harness.nav.depth(), 2, "…and landed on /sessions");
    assert_eq!(
        app_saved_endpoints(),
        vec![LIMIT_B.to_string()],
        "the forgotten row is gone and the dialed one took its place"
    );
    assert_eq!(
        credentials.stored(LIMIT_A),
        None,
        "…secret and all (this device only, as every forget is)"
    );
}

#[test]
fn a_free_install_may_still_reconnect_to_the_one_server_it_remembers() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_at_the_limit();

    connect_through_the_cta(&mut harness, LIMIT_A);
    harness.flush();

    assert!(
        !shows_text(&harness, UPGRADE_TITLE),
        "the row being dialed is the row a free install is allowed to keep"
    );
    decline_the_offer(&mut harness);
    assert!(expect_connection_controller().connected.get_untracked());
    assert_eq!(app_saved_endpoints(), vec![LIMIT_A.to_string()]);
}

#[test]
fn an_entitled_install_connects_to_a_second_server_and_keeps_both() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_limited(Tier::Pro, vec![limit_row(LIMIT_A, LIMIT_A_LABEL, false)]);

    connect_through_the_cta(&mut harness, LIMIT_B);
    harness.flush();

    assert!(
        !shows_text(&harness, UPGRADE_TITLE),
        "Pro lifts the rule rather than raising the bar"
    );
    decline_the_offer(&mut harness);
    assert!(expect_connection_controller().connected.get_untracked());
    assert_eq!(
        app_saved_endpoints_sorted(),
        vec![LIMIT_A.to_string(), LIMIT_B.to_string()]
    );
}

#[test]
fn buying_pro_from_the_paywall_closes_it_and_lets_the_blocked_connect_through() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_at_the_limit();

    connect_through_the_cta(&mut harness, LIMIT_B);
    harness.flush();
    harness.tap_node("the paywall's Upgrade action", |node| {
        node.role() == Role::Button && node.label() == Some(UPGRADE_ACTION)
    });
    harness.flush();
    decline_the_offer(&mut harness);

    assert!(
        !shows_text(&harness, UPGRADE_TITLE),
        "the sheet closed behind the purchase"
    );
    assert_eq!(
        expect_pro_controller().entitlement.get_untracked(),
        Entitlement::Pro
    );
    assert!(expect_connection_controller().connected.get_untracked());
    assert_eq!(
        app_saved_endpoints_sorted(),
        vec![LIMIT_A.to_string(), LIMIT_B.to_string()],
        "nothing was forgotten — the limit was lifted, not met"
    );
}

#[test]
fn not_now_leaves_the_paywall_closed_the_form_typed_and_nothing_dialed() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_at_the_limit();
    let before = harness.nav.depth();

    connect_through_the_cta(&mut harness, LIMIT_B);
    harness.flush();
    harness.tap_node("the paywall's Not now action", |node| {
        node.role() == Role::Button && node.label() == Some(PAYWALL_NOT_NOW)
    });
    harness.flush();

    assert!(!shows_text(&harness, UPGRADE_TITLE), "the sheet is gone");
    assert_eq!(
        harness.nav.depth(),
        before,
        "…and it was the only page there was"
    );
    assert!(!expect_connection_controller().connected.get_untracked());
    assert_eq!(
        harness.field_values().first().map(String::as_str),
        Some(LIMIT_B),
        "the form is exactly as it was typed, one tap from trying again"
    );
    assert_eq!(app_saved_endpoints(), vec![LIMIT_A.to_string()]);
}

#[test]
fn a_two_row_free_install_is_told_which_row_to_forget_before_any_unlock() {
    // The tap is on the row it may keep, so the *other* one is what stands in
    // the way.
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_over_the_limit();
    let credentials = mock_credentials().expect("the mock service is published");

    harness.tap_node("the saved row carrying a credential", |node| {
        node.value() == Some(LIMIT_A)
    });
    harness.flush();

    assert!(
        shows_text(&harness, UPGRADE_TITLE),
        "a saved row's own tap is gated too: {:?}",
        node_labels(&harness.root)
    );
    assert!(
        shows_text(&harness, FORGET_LIMIT_B),
        "the row being connected to is never one of the rows to forget"
    );
    assert!(
        credentials.prompts().is_empty(),
        "the verdict runs ahead of the unlock, so no biometric prompt fired"
    );
    assert!(!expect_connection_controller().connected.get_untracked());
}

#[test]
fn a_saved_row_tapped_mid_dial_raises_no_paywall_and_forgets_nothing() {
    // The mock backend settles a dial before `connect_with` returns, so an
    // in-flight one is staged by holding the very signal a real dial holds
    // (`tests/pro.rs`'s idiom for an overlapping call). Without the busy check
    // ahead of the verdict, this tap would raise a paywall whose
    // forget-and-connect drops a row and its secret for a dial that
    // `recover_and_connect` then refuses — destructive, and connected to
    // nothing.
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_over_the_limit();
    let credentials = mock_credentials().expect("the mock service is published");
    let before = harness.nav.depth();
    expect_connection_controller().connecting.set(true);

    harness.tap_node("the older saved row", |node| node.value() == Some(LIMIT_A));
    harness.flush();

    assert!(
        !shows_text(&harness, UPGRADE_TITLE),
        "a tap the busy guard refuses never reaches the verdict: {:?}",
        node_labels(&harness.root)
    );
    assert_eq!(
        harness.nav.depth(),
        before,
        "…so nothing was pushed over the screen at all"
    );
    assert_eq!(
        app_saved_endpoints_sorted(),
        vec![LIMIT_A.to_string(), LIMIT_B.to_string()],
        "and both rows are still remembered"
    );
    for endpoint in [LIMIT_A, LIMIT_B] {
        assert_eq!(
            credentials.stored(endpoint).as_deref(),
            Some(CRED_TOKEN),
            "…secrets and all"
        );
    }
    assert!(
        credentials.prompts().is_empty(),
        "no unlock was started either"
    );
}

#[test]
fn forgetting_from_a_saved_rows_paywall_dials_that_row_after_the_index_shift() {
    // The retry holds the *row* it was blocked on, not its index: forgetting
    // the row ahead of it moves it from second to first, and a retry that had
    // kept the index would unlock the wrong row or none at all.
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_over_the_limit();
    let credentials = mock_credentials().expect("the mock service is published");

    harness.tap_node("the older saved row", |node| node.value() == Some(LIMIT_A));
    harness.flush();
    assert!(
        shows_text(&harness, FORGET_LIMIT_B),
        "the paywall names the row ahead of the one being connected to: {:?}",
        node_labels(&harness.root)
    );

    harness.tap_node("the paywall's forget-and-connect action", |node| {
        node.role() == Role::Button && node.label() == Some(FORGET_LIMIT_B)
    });
    harness.flush();
    harness.pump_until("the retried row's own connect", |_| {
        expect_connection_controller().connected.get_untracked()
    });

    assert_eq!(
        app_saved_endpoints(),
        vec![LIMIT_A.to_string()],
        "the forgotten row is gone and the tapped one is what dialed"
    );
    assert_eq!(credentials.stored(LIMIT_B), None, "…secret and all");
    assert_eq!(
        credentials.stored(LIMIT_A).as_deref(),
        Some(CRED_TOKEN),
        "while the row that was retried kept its own"
    );
    assert_eq!(
        credentials.prompts().len(),
        1,
        "exactly one unlock ran — the retry's, on the row the tap named"
    );
    assert_eq!(
        harness.nav.depth(),
        2,
        "…and it landed on /sessions (a recovered credential offers no save)"
    );
}

#[test]
fn a_blocked_pairing_dials_nothing_and_connects_once_the_row_is_forgotten() {
    // The controller's own half of the pairing gate: the sheet is covered by
    // the cases above (and `tests/pro.rs`), so this one lends a recorder and
    // runs the two actions that sheet offers.
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(vec![limit_row(LIMIT_A, LIMIT_A_LABEL, false)]);
    let blocks: Rc<RefCell<Vec<ServerLimitBlock>>> = Rc::new(RefCell::new(Vec::new()));
    let gate = recording_gate(&blocks);
    creds
        .controller
        .apply_scanned_payload(scanned_payload(false));

    creds
        .controller
        .connect_from_pairing(&app_nav(), Rc::clone(&gate));

    assert!(
        !creds.connection.connected.get_untracked(),
        "a scan that lands on the limit dials nothing"
    );
    assert_eq!(blocked_endpoints(&blocks), vec![vec![LIMIT_A.to_string()]]);
    assert!(
        matches!(blocks.borrow()[0].retry, ConnectRetry::Pairing),
        "…and the retry re-runs the pairing, not the form"
    );
    assert!(
        creds.controller.pending_pairing.get_untracked().is_some(),
        "the payload that retry dials is still stashed"
    );

    let to_forget = blocks.borrow()[0].to_forget.clone();
    let retry_controller = Arc::clone(&creds.controller);
    creds.controller.forget_saved_servers_then(
        to_forget,
        Box::new(move || retry_controller.connect_from_pairing(&app_nav(), gate)),
    );
    pump_until_done("the forget-and-connect retry", || {
        creds.connection.connected.get_untracked()
    });

    assert_eq!(
        blocks.borrow().len(),
        1,
        "the retry was let through rather than refused again"
    );
    assert_eq!(
        fixture_rows(&creds.servers)
            .iter()
            .map(SavedServer::endpoint)
            .collect::<Vec<String>>(),
        vec![PAIRED_ENDPOINT.to_string()],
        "the forgotten row is gone and the scanned one took its place"
    );
}

#[test]
fn a_blocked_connect_leaves_nothing_in_flight_and_the_unlocked_retry_dials_once() {
    let _g = serial();
    let _ambient = setup();
    let creds = credential_fixture(vec![limit_row(LIMIT_A, LIMIT_A_LABEL, false)]);
    let blocks: Rc<RefCell<Vec<ServerLimitBlock>>> = Rc::new(RefCell::new(Vec::new()));
    let gate = recording_gate(&blocks);
    let offered: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let offer_prompt: OfferPrompt = {
        let record = Rc::clone(&offered);
        Rc::new(move |offer: CredentialOffer| record.borrow_mut().push(offer.label))
    };
    creds.controller.set_url(LIMIT_B.to_string());

    creds
        .controller
        .connect(&app_nav(), Rc::clone(&offer_prompt), Rc::clone(&gate));

    assert_eq!(blocks.borrow().len(), 1, "the connect was refused");
    assert!(!creds.connection.connected.get_untracked());
    assert!(
        offered.borrow().is_empty(),
        "nothing dialed, so nothing was offered to save"
    );
    assert!(
        !creds.controller.busy(),
        "a blocked connect leaves no dial, unlock or unanswered offer behind — \
         the paywall's own retry would otherwise be swallowed by the busy guard"
    );

    // What a landed purchase publishes on the controller the gate reads.
    creds.pro.entitlement.set(Entitlement::Pro);
    creds.controller.connect(&app_nav(), offer_prompt, gate);

    assert_eq!(
        blocks.borrow().len(),
        1,
        "…and the unlocked retry is not refused a second time"
    );
    assert!(creds.connection.connected.get_untracked());
    assert_eq!(offered.borrow().len(), 1, "the retry dialed exactly once");
}

/// The QR affordance's own gate: a free install already remembering a server
/// takes the free-tier verdict at the tap, before the scanner ever mounts.
#[test]
fn a_free_installs_qr_tap_raises_the_paywall_when_a_server_is_already_saved() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_limited(Tier::Free, vec![limit_row(LIMIT_A, LIMIT_A_LABEL, false)]);
    let before = harness.nav.depth();

    harness.tap_node("the QR affordance", |node| {
        node.role() == Role::Button && node.label() == Some(QR_LABEL)
    });
    harness.flush();

    assert!(
        shows_text(&harness, UPGRADE_TITLE),
        "the paywall is the whole answer: {:?}",
        node_labels(&harness.root)
    );
    assert_eq!(
        harness.nav.depth(),
        before + 1,
        "the sheet is the only page pushed"
    );
    assert!(
        !shows_text(&harness, NO_CAMERA),
        "the scanner never mounted behind the paywall"
    );
    assert!(
        !shows_text(&harness, FORGET_LIMIT_A),
        "there is no dial to retry, so no forget-and-connect action is offered"
    );

    harness.tap_node("the paywall's Not now action", |node| {
        node.role() == Role::Button && node.label() == Some(PAYWALL_NOT_NOW)
    });
    harness.flush();

    assert_eq!(
        harness.nav.depth(),
        before,
        "…and it was the only page there was"
    );
}

#[test]
fn a_free_install_remembering_nothing_scans_straight_away() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_limited(Tier::Free, vec![]);

    harness.tap_node("the QR affordance", |node| {
        node.role() == Role::Button && node.label() == Some(QR_LABEL)
    });

    assert_eq!(
        harness.nav.depth(),
        2,
        "the sheet is pushed as its own page"
    );
    assert!(
        shows_text(&harness, NO_CAMERA),
        "nothing stood in the way, so the scanner mounted off-device: {:?}",
        node_labels(&harness.root)
    );
    assert!(!shows_text(&harness, UPGRADE_TITLE));
}

#[test]
fn unlocking_from_the_qr_paywall_lands_in_the_scanner() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_limited(Tier::Free, vec![limit_row(LIMIT_A, LIMIT_A_LABEL, false)]);

    harness.tap_node("the QR affordance", |node| {
        node.role() == Role::Button && node.label() == Some(QR_LABEL)
    });
    harness.flush();
    assert!(
        shows_text(&harness, UPGRADE_TITLE),
        "the paywall is up before the unlock: {:?}",
        node_labels(&harness.root)
    );

    mock_entitlement()
        .expect("tests/support opted this process into the scriptable store")
        .set_entitlement(Entitlement::Pro);
    harness.tap_node("the paywall's Restore action", |node| {
        node.role() == Role::Button && node.label() == Some(RESTORE_LABEL)
    });
    harness.pump_until("the scanner to land after the unlock", |harness| {
        shows_text(harness, NO_CAMERA)
    });

    assert_eq!(
        harness.nav.depth(),
        2,
        "the unlock's own scan is the only push"
    );
    assert!(!shows_text(&harness, UPGRADE_TITLE), "the paywall is gone");
}

// ---------------------------------------------------------------------------
// The Tablet structure: two panels on the full band, folded when collapsed
// ---------------------------------------------------------------------------

/// `docs/tablet.html`'s landscape frame: Tablet, full band.
const FULL_TABLET: Size = Size::new(1024.0, 720.0);
/// Tablet (shortest side 600 or more) on the collapsed band (under 840 wide).
const COLLAPSED_TABLET: Size = Size::new(720.0, 720.0);
/// A phone in portrait: Compact.
const PHONE: Size = Size::new(390.0, 844.0);
/// The saved list's header run, as `widgets::saved` paints it.
const SAVED_HEADER: &str = "SAVED SERVERS";
/// The seeded store's pinned row's label — the Tablet row's primary line.
const PINNED_LABEL: &str = "herdr staging";

fn qr_action(node: &Node) -> bool {
    node.role() == Role::Button && node.label() == Some(QR_LABEL)
}

fn connect_cta(node: &Node) -> bool {
    node.role() == Role::Button && node.label() == Some(CONNECT_LABEL)
}

fn saved_header(node: &Node) -> bool {
    node.value() == Some(SAVED_HEADER)
}

/// How many nodes on screen match `pred`.
fn count(harness: &Harness, pred: impl Fn(&Node) -> bool) -> usize {
    harness
        .root
        .semantics()
        .nodes
        .iter()
        .filter(|(_, node)| pred(node))
        .count()
}

/// The QR action sits in the CTA's own row, trailing it — the in-flow
/// button, not the corner FAB.
fn assert_qr_beside_the_cta(harness: &Harness) {
    assert_eq!(count(harness, qr_action), 1, "one QR action, no FAB");
    let qr = harness.bounds("the QR button", qr_action);
    let cta = harness.bounds("the connect CTA", connect_cta);
    assert!(
        (qr.y0 - cta.y0).abs() < EPSILON && (qr.y1 - cta.y1).abs() < EPSILON,
        "the QR button shares the CTA's row: {qr:?} vs {cta:?}"
    );
    assert!(qr.x0 > cta.x1, "…trailing it: {qr:?} vs {cta:?}");
    assert!(qr.height() >= MIN_TAP_TARGET, "{qr:?}");
}

#[test]
fn the_compact_info_button_clears_a_top_trailing_window_control_corner() {
    let _g = serial();
    let _ambient = setup();
    let plain = sized(PHONE, Harness::mount);
    let flush = plain.bounds("the info button", info_button);

    let corner = CornerInsets {
        top_right: CornerInset::new(100.0, 40.0),
        ..CornerInsets::ZERO
    };
    let insets = WindowInsets::default().with_corner_insets(corner);
    let metrics = WindowMetrics::new(PHONE, 1.0, insets);
    let mut shifted = mount_with_metrics(Some(metrics));
    shifted.surface = PHONE;
    shifted.settle();
    let moved = shifted.bounds("the info button", info_button);

    assert!(
        PHONE.width - moved.x1 >= PHONE.width - flush.x1 + 100.0 - EPSILON,
        "100px further from the trailing edge: {moved:?} vs {flush:?}"
    );
    assert!(
        moved.y0 >= flush.y0 + 40.0 - EPSILON,
        "40px lower: {moved:?} vs {flush:?}"
    );
}

/// `size`'s metrics carrying `corner` as the top-right window-control corner.
fn corner_metrics(size: Size, corner: CornerInsets) -> WindowMetrics {
    WindowMetrics::new(
        size,
        1.0,
        WindowInsets::default().with_corner_insets(corner),
    )
}

const TOP_RIGHT_CORNER: CornerInsets = CornerInsets {
    top_right: CornerInset::new(100.0, 40.0),
    ..CornerInsets::ZERO
};

#[test]
fn a_resize_that_adds_a_corner_moves_the_info_button_live() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = sized(PHONE, Harness::mount);
    let flush = harness.bounds("the info button", info_button);

    support::reprovide_metrics(corner_metrics(PHONE, TOP_RIGHT_CORNER));
    for _ in 0..3 {
        harness.settle();
    }
    let moved = harness.bounds("the info button", info_button);

    assert!(
        moved.x1 <= flush.x1 - 100.0 + EPSILON && moved.y0 >= flush.y0 + 40.0 - EPSILON,
        "the builder re-read the corner on resize: {moved:?} vs {flush:?}"
    );
}

#[test]
fn a_collapsed_tablet_info_button_clears_the_top_trailing_corner() {
    let _g = serial();
    let _ambient = setup();
    let plain = sized(COLLAPSED_TABLET, Harness::mount);
    let flush = plain.bounds("the info button", info_button);

    let mut shifted = mount_with_metrics(Some(corner_metrics(COLLAPSED_TABLET, TOP_RIGHT_CORNER)));
    shifted.surface = COLLAPSED_TABLET;
    shifted.settle();
    let moved = shifted.bounds("the info button", info_button);

    assert!(
        flush.x1 - moved.x1 >= 100.0 - EPSILON && moved.y0 - flush.y0 >= 40.0 - EPSILON,
        "at least the corner's width and height: {moved:?} vs {flush:?}"
    );
}

#[test]
fn a_full_tablet_mounts_no_info_button_even_with_a_corner() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = mount_with_metrics(Some(corner_metrics(FULL_TABLET, TOP_RIGHT_CORNER)));
    harness.surface = FULL_TABLET;
    harness.settle();

    assert_eq!(
        count(&harness, info_button),
        0,
        "the panel carries the copy"
    );
}

#[test]
fn a_top_left_corner_pushes_the_full_panels_brand_tile_clear_of_it() {
    let _g = serial();
    let _ambient = setup();
    let corner = CornerInsets {
        top_left: CornerInset::new(66.0, 43.0),
        ..CornerInsets::ZERO
    };
    let mut harness = mount_with_metrics(Some(corner_metrics(FULL_TABLET, corner)));
    harness.surface = FULL_TABLET;
    let scene = harness.settle();

    let tile = scene
        .rounded_fills
        .iter()
        .find(|(_, size, _, _)| {
            (size.width - 60.0).abs() < EPSILON && (size.height - 60.0).abs() < EPSILON
        })
        .map(|(origin, size, _, _)| (*origin, *size))
        .expect("the panel's 60x60 brand tile is painted");
    let (origin, _) = tile;
    let overlaps = origin.x < 66.0 && origin.y < 43.0;
    assert!(!overlaps, "the tile clears the window control: {tile:?}");
}

#[test]
fn a_credential_less_row_past_the_free_tier_is_tagged_refills_on_tap_and_gates_at_connect() {
    let _g = serial();
    let _ambient = setup();
    let rows = vec![
        limit_row(LIMIT_A, LIMIT_A_LABEL, false),
        older_limit_row(LIMIT_B, LIMIT_B_LABEL, false),
    ];
    let mut harness = sized(FULL_TABLET, || mount_limited(Tier::Free, rows));
    assert_eq!(pro_tags(&harness), 2, "both rows are past the free tier");

    harness.tap_node("the credential-less row", |node| {
        node.role() == Role::Label && node.value() == Some(LIMIT_A_LABEL)
    });
    harness.flush();
    assert!(
        !shows_text(&harness, UPGRADE_TITLE),
        "the tap only refills: there is no credential to dial with"
    );
    assert_eq!(
        harness.field_values().first().map(String::as_str),
        Some(LIMIT_A),
        "the form took the row's endpoint"
    );

    harness.tap_node("the connect CTA", connect_cta);
    harness.flush();
    assert!(
        shows_text(&harness, UPGRADE_TITLE),
        "the paywall fires at Connect: {:?}",
        node_labels(&harness.root)
    );
}

#[test]
fn a_full_tablet_puts_orientation_and_saved_servers_beside_the_form() {
    let _g = serial();
    let _ambient = setup();
    let harness = sized(FULL_TABLET, Harness::mount);

    assert!(
        harness.shows_value(GETTING_STARTED_BODY),
        "the orientation paragraph is inline: {:?}",
        node_labels(&harness.root)
    );
    assert_eq!(
        count(&harness, |node| node.label() == Some(INFO_LABEL)),
        0,
        "…so the corner affordance that raised it is not mounted"
    );
    assert_eq!(count(&harness, saved_header), 1, "one saved-servers header");
    assert_qr_beside_the_cta(&harness);

    let header = harness.bounds("the saved header", saved_header);
    let cta = harness.bounds("the connect CTA", connect_cta);
    assert!(
        header.x1 <= cta.x0,
        "the saved list sits in the left panel, beside the form: {header:?} vs {cta:?}"
    );
}

#[test]
fn a_collapsed_tablet_folds_the_saved_list_under_the_button_row() {
    let _g = serial();
    let _ambient = setup();
    let harness = sized(COLLAPSED_TABLET, Harness::mount);

    assert!(
        !harness.shows_value(GETTING_STARTED_BODY),
        "no left panel, so no paragraph until asked"
    );
    assert_eq!(
        count(&harness, |node| node.label() == Some(INFO_LABEL)),
        1,
        "the info mark keeps the orientation reachable"
    );
    assert_eq!(count(&harness, saved_header), 1, "one saved-servers header");
    assert_qr_beside_the_cta(&harness);

    let cta = harness.bounds("the connect CTA", connect_cta);
    let header = harness.bounds("the saved header", saved_header);
    let row = harness.bounds("the pinned row", |node| node.value() == Some(PINNED_LABEL));
    assert!(
        cta.y1 <= header.y0 && header.y1 <= row.y0,
        "the phone's order: button row, header, rows: {cta:?} / {header:?} / {row:?}"
    );
}

#[test]
fn a_collapsed_tablet_raises_the_getting_started_dialog_from_the_info_mark() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = sized(COLLAPSED_TABLET, Harness::mount);
    let before = harness.nav.depth();

    harness.tap_node("the getting-started affordance", info_button);
    assert_eq!(harness.nav.depth(), before + 1, "the dialog is pushed");
    assert!(shows_text(&harness, GETTING_STARTED_TITLE));
    assert!(harness.shows_value(GETTING_STARTED_BODY));
}

#[test]
fn a_phone_keeps_the_corner_affordances_and_no_inline_paragraph() {
    let _g = serial();
    let _ambient = setup();
    let harness = sized(PHONE, Harness::mount);

    assert!(!harness.shows_value(GETTING_STARTED_BODY));
    assert_eq!(count(&harness, info_button), 1, "the info mark is up");
    assert_eq!(count(&harness, saved_header), 1);
    // The corner FAB is gone: the QR square rides in the CTA's own row.
    assert_qr_beside_the_cta(&harness);
    let square = harness.bounds("the QR square", qr_action);
    assert!((square.width() - QR_SQUARE).abs() < 0.5, "{square:?}");
}

/// How many `pro` tags the screen shows.
fn pro_tags(harness: &Harness) -> usize {
    count(harness, |node| {
        node.role() == Role::Status && node.label() == Some(PRO_TAG)
    })
}

#[test]
fn a_free_install_with_two_rows_tags_both_on_both_classes() {
    let _g = serial();
    let _ambient = setup();
    let rows = || {
        vec![
            limit_row(LIMIT_A, LIMIT_A_LABEL, false),
            older_limit_row(LIMIT_B, LIMIT_B_LABEL, false),
        ]
    };
    for size in [PHONE, FULL_TABLET] {
        let harness = sized(size, || mount_limited(Tier::Free, rows()));
        assert_eq!(pro_tags(&harness), 2, "free at {size:?}");
    }
}

#[test]
fn a_free_install_with_one_row_tags_none_on_both_classes() {
    let _g = serial();
    let _ambient = setup();
    for size in [PHONE, FULL_TABLET] {
        let harness = sized(size, || {
            mount_limited(Tier::Free, vec![limit_row(LIMIT_A, LIMIT_A_LABEL, false)])
        });
        assert_eq!(pro_tags(&harness), 0, "free at {size:?}");
    }
}

#[test]
fn a_pro_install_tags_no_row() {
    let _g = serial();
    let _ambient = setup();
    let rows = vec![
        limit_row(LIMIT_A, LIMIT_A_LABEL, false),
        older_limit_row(LIMIT_B, LIMIT_B_LABEL, false),
    ];
    let harness = sized(FULL_TABLET, || mount_limited(Tier::Pro, rows));
    assert_eq!(pro_tags(&harness), 0);
}

#[test]
fn an_empty_tablet_list_says_so_under_its_header() {
    let _g = serial();
    let _ambient = setup();
    let harness = sized(FULL_TABLET, || mount_limited(Tier::Pro, vec![]));

    assert_eq!(count(&harness, saved_header), 1);
    assert!(
        harness.shows_value(NO_SAVED_SERVERS),
        "{:?}",
        node_labels(&harness.root)
    );
}

#[test]
fn a_tablet_saved_row_leads_with_its_label_and_refills_the_form_like_the_phone_row() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = sized(FULL_TABLET, Harness::mount);

    let meta_prefix = format!("{PINNED_ENDPOINT} · pinned TLS · ");
    assert!(
        harness
            .root
            .semantics()
            .nodes
            .iter()
            .any(|(_, node)| node.value().is_some_and(|v| v.starts_with(&meta_prefix))),
        "the meta line leads with host:port: {:?}",
        node_labels(&harness.root)
    );

    harness.tap_node("the pinned row", |node| {
        node.role() == Role::Label && node.value() == Some(PINNED_LABEL)
    });
    assert_eq!(
        harness.field_values().first().map(String::as_str),
        Some(PINNED_ENDPOINT),
        "the row filled the server-URL field"
    );
    assert!(
        harness.shows_value(PINNED_CAPTION),
        "…and its pinned posture"
    );
}

#[test]
fn a_tablet_rows_delete_target_raises_the_same_remove_confirmation() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = sized(FULL_TABLET, mount_with_credential_row);
    let before = harness.nav.depth();

    let target = harness.bounds("the row's delete target", |node| {
        node.role() == Role::Button && node.label() == Some(DELETE_CRED_LABEL)
    });
    assert!(
        target.width() >= MIN_TAP_TARGET && target.height() >= MIN_TAP_TARGET,
        "{target:?}"
    );
    harness.tap_node("the row's delete target", |node| {
        node.role() == Role::Button && node.label() == Some(DELETE_CRED_LABEL)
    });
    assert_eq!(harness.nav.depth(), before + 1);
    assert!(shows_text(&harness, REMOVE_TITLE));
    assert!(
        !expect_connection_controller().connected.get_untracked(),
        "the row's own tap never fired"
    );
}

#[test]
fn a_tablet_credential_row_unlocks_and_lands_on_sessions() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = sized(FULL_TABLET, mount_with_credential_row);
    let credentials = mock_credentials().expect("the mock service is published");
    let before = harness.nav.depth();

    harness.tap_node("the credential row", |node| {
        node.role() == Role::Label && node.value() == Some("Cred rig")
    });
    pump_until_done("the recovered connect", || {
        expect_connection_controller().connected.get_untracked()
    });
    harness.settle();

    assert_eq!(credentials.prompts().len(), 1, "exactly one prompt");
    assert!(harness.nav.depth() > before, "…and landed on /sessions");
}

#[test]
fn the_tablet_guide_button_hands_the_docs_url_to_the_link_opener() {
    let _g = serial();
    let _ambient = setup();
    let opened: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&opened);
    set_link_opener_for_tests(move |url| {
        sink.lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(url.to_string());
    });
    let mut harness = sized(FULL_TABLET, Harness::mount);
    let before = harness.nav.depth();

    harness.tap_node("the inline guide button", |node| {
        node.role() == Role::Button && node.label() == Some(OPEN_GUIDE_LABEL)
    });

    assert_eq!(
        *opened.lock().unwrap_or_else(|e| e.into_inner()),
        vec![DOCS_URL.to_string()]
    );
    assert_eq!(harness.nav.depth(), before, "no dialog in between");
}

#[test]
fn the_tablet_qr_button_opens_the_scanner_like_the_fab() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = sized(COLLAPSED_TABLET, || mount_limited(Tier::Free, vec![]));

    harness.tap_node("the QR button", qr_action);

    assert_eq!(harness.nav.depth(), 2, "the sheet is pushed");
    assert!(
        shows_text(&harness, NO_CAMERA),
        "{:?}",
        node_labels(&harness.root)
    );
}

#[test]
fn the_tablet_qr_button_takes_the_fabs_free_tier_gate() {
    let _g = serial();
    let _ambient = setup();
    let mut harness = sized(FULL_TABLET, || {
        mount_limited(Tier::Free, vec![limit_row(LIMIT_A, LIMIT_A_LABEL, false)])
    });
    let before = harness.nav.depth();

    harness.tap_node("the QR button", qr_action);
    harness.flush();

    assert!(
        shows_text(&harness, UPGRADE_TITLE),
        "{:?}",
        node_labels(&harness.root)
    );
    assert_eq!(harness.nav.depth(), before + 1);
    assert!(
        !shows_text(&harness, NO_CAMERA),
        "the scanner never mounted"
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
fn the_static_splash_mocks_fields_refuse_editing_on_tap() {
    // `connect_pieces` (== `connect_pieces_with(theme, None, saved)`) is what
    // the cold-start splash assembles — no navigator, no controller — so a
    // bare `()` state and a standalone `RenderRoot` reproduce it directly.
    //
    // frust's read-only contract (`TextInput: a read-only field is focusable
    // and copyable`) means a tap here MAY claim focus now — that alone is not
    // the defect this guards. What must still never happen is a stray tap
    // during the splash's staggering entrance caret-editing or accepting
    // input into a field with nothing behind it: no keystroke may land, and
    // any IME session the field publishes must keep the soft keyboard down.
    let _g = serial();
    let _ambient = setup();

    let theme = frust_glyph::baseline();
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
        "the splash's static fields must present as read-only: {fields:?}"
    );
    assert!(
        fields.iter().all(|(_, disabled, _)| !*disabled),
        "…and NOT as disabled: `enabled(false)` is what dimmed the border and \
         placeholder, so the fields popped to full alpha the instant the live \
         screen took over: {fields:?}"
    );

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
        frame_sized(&mut root, &mut logic, &mut state, &mut tcx, SURFACE);

        // A focused read-only field may publish a keyboard-suppressed IME
        // session (for copy); it must never ask for the keyboard itself.
        if let Some(ime) = root.ime_state() {
            assert!(
                ime.suppress_soft_keyboard,
                "a focused read-only splash field must never raise its \
                 keyboard: {ime:?}"
            );
        }

        // Either way, a stray keystroke must never land on a field with
        // nothing behind it.
        root.event(&mut state, &char_key("x"));
    }
    frame_sized(&mut root, &mut logic, &mut state, &mut tcx, SURFACE);

    let values: Vec<Option<String>> = root
        .semantics()
        .nodes
        .iter()
        .filter(|(_, node)| matches!(node.role(), Role::TextInput | Role::PasswordInput))
        .map(|(_, node)| node.value().map(str::to_string))
        .collect();
    assert!(
        values.iter().all(|value| value.as_deref() == Some("")),
        "a stray tap during the splash's staggering entrance must never let a \
         keystroke reach a field with nothing behind it: {values:?}"
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
    let theme = frust_glyph::baseline();
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
    // `TextContext::layout` hardcodes `parley::Alignment::Start`, so a plain
    // `Align(CENTER, text(..))` only centers the *block* — the second line
    // hugs the leading edge; `badge::heading` centers the subtitle per line
    // instead, via `TextView`'s own `.align(TextAlign::Center)`.
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
    field_text_style(&frust_glyph::baseline(), Color::WHITE)
}

#[test]
fn the_server_url_placeholder_never_advances_its_digits_a_full_em() {
    // A digit advancing its own font size (13px, vs ~7px for the surrounding
    // letters) is the signature of the string having split across a fallback
    // font the primary stack failed to cover — read as
    // `zelli.example.com: 5 0 0 5 1`.
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
    // The positive half: digits and letters share one font, so their
    // advances share one set of metrics rather than interleaving two.
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
fn an_unresolvable_stack_no_longer_splits_digits_per_cluster() {
    // The retired counter-example. It was the negative control for
    // `field_font_family`: on parley 0.11 a stack that resolves to nothing fell
    // back per cluster and advanced every digit a full em. On frust 0.6.4
    // (parley 0.12) it no longer does, so the host no longer reproduces that
    // device defect. The workaround is kept until it is re-measured on the
    // device that showed the defect. If this fails again, parley's fallback has
    // regressed and the workaround is load-bearing once more.
    let broken = TextStyle {
        family: FontFamily::Named(vec!["No Such Family 1234".to_string()]),
        ..field_style()
    };
    let em = f64::from(broken.size);
    let digits = advances_where(SERVER_URL_PLACEHOLDER, &broken, |c| c.is_ascii_digit());
    assert_eq!(
        digits.len(),
        5,
        "the unresolvable stack must shape all five digits of 50051, not an empty measurement"
    );
    assert!(
        digits.iter().all(|advance| *advance < em - ADVANCE_EPSILON),
        "expected the unresolvable stack to leave every digit below a full \
         {em}px em, got {digits:?} — if this fails, parley's per-cluster \
         fallback has regressed and `field_font_family`'s workaround is \
         load-bearing again; re-measure it on the device"
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
    assert_eq!(
        digits.len(),
        5,
        "the rescued stack must shape all five digits of 50051, not an empty measurement"
    );
    assert!(
        digits.iter().all(|advance| *advance < em - ADVANCE_EPSILON),
        "a resolvable terminal generic must collapse the fallback split, got \
         {digits:?}"
    );
}

// ---------------------------------------------------------------------------
// The unlock dock
// ---------------------------------------------------------------------------

const DOCK_A: &str = "dock-a.example:50051";
const DOCK_B: &str = "dock-b.example:50051";
const DOCK_TOKEN_A: &str = "dock-s3cr3t-a";
const DOCK_TOKEN_B: &str = "dock-s3cr3t-b";
/// The dock's face and its prompt's reason, for two rows — transcribed.
const UNLOCK_TWO: &str = "Unlock 2 servers";
const UNVERIFIED_TITLE: &str = "Connect without verification?";
const UNVERIFIED_CONFIRM: &str = "Connect anyway";
const TRY_AGAIN: &str = "Try again";
const RESET_ACTION: &str = "Reset saved credentials";
const RESET_TITLE: &str = "Reset saved credentials?";
const RESET_CONFIRM: &str = "Reset";

/// Opts this test thread's biometric probe in for as long as it is held.
struct Biometrics;

impl Biometrics {
    fn available() -> Self {
        set_biometrics_available_for_tests(Some(true));
        Biometrics
    }
}

impl Drop for Biometrics {
    fn drop(&mut self) {
        set_biometrics_available_for_tests(None);
    }
}

/// [`DOCK_A`] stamped now (the most recent row, under `trust_a`) over
/// [`DOCK_B`] an hour older, both claiming a credential.
fn dock_rows(trust_a: TrustMode) -> Vec<SavedServer> {
    vec![
        SavedServer {
            trust_mode: trust_a,
            ..limit_row(DOCK_A, "Dock A", true)
        },
        older_limit_row(DOCK_B, "Dock B", true),
    ]
}

/// The real screen over exactly `rows`, the credential mock seeded by `seed`.
fn mount_dock(
    tier: Tier,
    rows: Vec<SavedServer>,
    seed: impl FnOnce(&MockCredentialService),
) -> Harness {
    Harness::mount_tiered(tier, move || {
        only_saved(rows);
        seed(&mock_credentials().expect("tests/support opted this process into the mock"));
    })
}

fn seed_both_in_the_vault(credentials: &MockCredentialService) {
    credentials.seed_vault(&[(DOCK_A, DOCK_TOKEN_A), (DOCK_B, DOCK_TOKEN_B)]);
}

fn button_labelled(label: &'static str) -> impl Fn(&Node) -> bool {
    move |node| node.role() == Role::Button && node.label() == Some(label)
}

fn connected() -> bool {
    expect_connection_controller().connected.get_untracked()
}

fn dock_credentials() -> Arc<MockCredentialService> {
    mock_credentials().expect("the mock service is published")
}

/// [`credential_fixture`] for an entitled install whose controller publishes
/// into `keyring`.
fn keyring_fixture(rows: Vec<SavedServer>, keyring: &KeyringHandle) -> Creds {
    let connection = muxr::connection::provide_connection_controller();
    let nav: NavigatorController<AppState> = NavigatorController::new();
    let router = install_app_router(&nav, &nav, Arc::clone(&connection));
    let servers = Arc::new(MockSavedServersRepository::with_servers(rows));
    let credentials = Arc::new(MockCredentialService::new());
    let tokens = Arc::new(MockAuthTokensRepository::seeded());
    let entitled = Arc::new(MockEntitlementRepository::pro()) as SharedEntitlementRepository;
    let pro = Arc::new(ProController::new(entitled));
    let controller = Arc::new(ConnectController::new(
        Arc::clone(&servers) as _,
        Arc::clone(&credentials) as _,
        Arc::clone(&tokens) as _,
        Arc::clone(&connection),
        Arc::clone(&pro),
        keyring.clone(),
    ));
    Creds {
        controller,
        credentials,
        servers,
        tokens,
        connection,
        pro,
        _router: router,
    }
}

/// The dock's press at controller level, with no confirm expected.
fn unlock_through_the_dock(creds: &Creds) {
    creds.controller.unlock_keyring_and_connect(
        &app_nav(),
        ungated(),
        Rc::new(|_server, _dial| panic!("no row here needs the no-verification confirm")),
    );
}

#[test]
fn the_dock_unlocks_every_row_behind_one_prompt_and_lands_on_the_most_recent_one() {
    let _g = serial();
    let _ambient = setup();
    let _bio = Biometrics::available();
    let mut harness = mount_dock(
        Tier::Pro,
        dock_rows(TrustMode::System),
        seed_both_in_the_vault,
    );
    let before = harness.nav.depth();

    harness.tap_node("the dock", button_labelled(UNLOCK_TWO));
    harness.pump_until("the dock's connect", |_| connected());
    harness.settle();

    assert_eq!(dock_credentials().prompts(), vec![UNLOCK_TWO.to_string()]);
    let info = expect_connection_controller()
        .server_info
        .get_untracked()
        .expect("a connected controller carries its facts");
    assert_eq!(info.endpoint, DOCK_A, "the most recent row is the target");
    assert!(harness.nav.depth() > before, "…and it landed on /sessions");
    let keyring = expect_keyring();
    assert!(keyring.is_unlocked(), "the unlocked keyring is published");
    assert_eq!(
        keyring.token_for(DOCK_B).as_deref().map(String::as_str),
        Some(DOCK_TOKEN_B),
        "…holding every row's token, not just the one it dialed"
    );
}

#[test]
fn legacy_entries_migrate_on_the_first_unlock_and_the_next_reads_the_vault_alone() {
    let _g = serial();
    let _ambient = setup();
    let keyring = KeyringHandle::new();
    let creds = keyring_fixture(dock_rows(TrustMode::System), &keyring);
    creds.credentials.seed(DOCK_A, DOCK_TOKEN_A);
    creds.credentials.seed(DOCK_B, DOCK_TOKEN_B);

    unlock_through_the_dock(&creds);
    pump_until_done("the first unlock's connect", || {
        creds.connection.connected.get_untracked()
    });
    assert_eq!(
        creds.credentials.prompts().len(),
        3,
        "one vault read plus one read per legacy entry"
    );
    assert_eq!(
        creds.credentials.vault_endpoints(),
        Some(vec![DOCK_A.to_string(), DOCK_B.to_string()])
    );
    assert_eq!(creds.credentials.legacy(DOCK_B), None, "migrated once");

    creds.connection.lock_and_disconnect(&keyring);
    unlock_through_the_dock(&creds);
    pump_until_done("the second unlock's connect", || {
        creds.connection.connected.get_untracked()
    });
    assert_eq!(
        creds.credentials.prompts().len(),
        4,
        "the next unlock reads the vault alone"
    );
}

#[test]
fn a_free_install_with_two_rows_raises_the_paywall_before_any_prompt() {
    let _g = serial();
    let _ambient = setup();
    let _bio = Biometrics::available();
    let mut harness = mount_dock(
        Tier::Free,
        dock_rows(TrustMode::System),
        seed_both_in_the_vault,
    );

    harness.tap_node("the dock", button_labelled(UNLOCK_TWO));
    harness.flush();

    assert!(
        shows_text(&harness, UPGRADE_TITLE),
        "{:?}",
        node_labels(&harness.root)
    );
    assert!(dock_credentials().prompts().is_empty(), "no prompt at all");
    assert!(!connected());
}

#[test]
fn a_canceled_dock_unlock_is_silent_and_leaves_the_dock_idle() {
    let _g = serial();
    let _ambient = setup();
    let _bio = Biometrics::available();
    let mut harness = mount_dock(Tier::Pro, dock_rows(TrustMode::System), |credentials| {
        seed_both_in_the_vault(credentials);
        credentials.set_gate(CredentialStatus::Canceled);
    });
    app_toasts().clear();

    harness.tap_node("the dock", button_labelled(UNLOCK_TWO));
    harness.pump_until("the canceled unlock to settle", |harness| {
        dock_credentials().prompts().len() == 1 && count(harness, button_labelled(UNLOCK_TWO)) == 1
    });

    assert!(!connected());
    assert!(
        app_toasts().pending_untracked().is_empty(),
        "a cancel says nothing"
    );
    assert!(!expect_keyring().is_unlocked());
    let disabled = harness
        .root
        .semantics()
        .nodes
        .iter()
        .find(|(_, node)| button_labelled(UNLOCK_TWO)(node))
        .map(|(_, node)| node.is_disabled());
    assert_eq!(disabled, Some(false), "the idle dock takes the next tap");
}

#[test]
fn a_no_verification_target_asks_once_after_the_unlock_and_dials_on_confirm() {
    let _g = serial();
    let _ambient = setup();
    let _bio = Biometrics::available();
    let mut harness = mount_dock(
        Tier::Pro,
        dock_rows(TrustMode::NoVerification),
        seed_both_in_the_vault,
    );

    harness.tap_node("the dock", button_labelled(UNLOCK_TWO));
    harness.pump_until("the confirm", |harness| {
        shows_text(harness, UNVERIFIED_TITLE)
    });
    assert_eq!(dock_credentials().prompts().len(), 1);
    assert!(!connected(), "nothing dials before the confirm");

    harness.tap_node("the confirm", button_labelled(UNVERIFIED_CONFIRM));
    harness.pump_until("the confirmed connect", |_| connected());
    assert_eq!(
        dock_credentials().prompts().len(),
        1,
        "asked once, prompted once"
    );
}

#[test]
fn an_unreadable_legacy_row_is_forgotten_while_the_most_recent_one_connects() {
    let _g = serial();
    let _ambient = setup();
    let keyring = KeyringHandle::new();
    let creds = keyring_fixture(dock_rows(TrustMode::System), &keyring);
    creds.credentials.seed_vault(&[(DOCK_A, DOCK_TOKEN_A)]);
    creds.credentials.seed(DOCK_B, DOCK_TOKEN_B);
    // Op 0 reads the vault; op 1 is DOCK_B's legacy read.
    creds
        .credentials
        .set_gate_at(1, CredentialStatus::KeyInvalidated);

    unlock_through_the_dock(&creds);
    pump_until_done("the connect", || creds.connection.connected.get_untracked());

    let row = |endpoint: &str| {
        creds
            .servers
            .list()
            .expect("the mock store never fails")
            .into_iter()
            .find(|server| server.endpoint() == endpoint)
            .expect("both rows stay")
    };
    assert!(
        !row(DOCK_B).has_credential,
        "the unreadable row is forgotten"
    );
    assert_eq!(creds.credentials.legacy(DOCK_B), None);
    assert!(row(DOCK_A).has_credential);
    let info = creds
        .connection
        .server_info
        .get_untracked()
        .expect("connected");
    assert_eq!(info.endpoint, DOCK_A);
}

#[test]
fn a_vault_invalidated_by_re_enrollment_is_reset_and_every_row_cleared() {
    let _g = serial();
    let _ambient = setup();
    let _bio = Biometrics::available();
    let mut harness = mount_dock(Tier::Pro, dock_rows(TrustMode::System), |credentials| {
        seed_both_in_the_vault(credentials);
        credentials.set_gate_at(0, CredentialStatus::KeyInvalidated);
    });

    harness.tap_node("the dock", button_labelled(UNLOCK_TWO));
    harness.pump_until("the reset's copy", |harness| {
        harness.shows_value(VAULT_RESET_MESSAGE)
    });

    let credentials = dock_credentials();
    assert!(credentials.vault_endpoints().unwrap_or_default().is_empty());
    assert!(!saved_row(DOCK_A).has_credential && !saved_row(DOCK_B).has_credential);
    assert_eq!(
        credentials.prompts().len(),
        1,
        "the reset itself is ungated"
    );
    assert_eq!(credentials.writes(), 0);
    assert!(!connected());
    assert_eq!(count(&harness, button_labelled(TRY_AGAIN)), 1);
    assert_eq!(
        count(&harness, button_labelled(RESET_ACTION)),
        0,
        "nothing left to reset"
    );
}

#[test]
fn a_failed_unlock_offers_the_reset_only_behind_its_confirm() {
    let _g = serial();
    let _ambient = setup();
    let _bio = Biometrics::available();
    let mut harness = mount_dock(Tier::Pro, dock_rows(TrustMode::System), |credentials| {
        seed_both_in_the_vault(credentials);
        credentials.set_gate_at(0, CredentialStatus::Failed);
    });
    app_toasts().clear();
    let both = Some(vec![DOCK_A.to_string(), DOCK_B.to_string()]);

    harness.tap_node("the dock", button_labelled(UNLOCK_TWO));
    harness.pump_until("the failed dock", |harness| {
        count(harness, button_labelled(RESET_ACTION)) == 1
    });
    assert_eq!(count(&harness, button_labelled(TRY_AGAIN)), 1);
    assert!(
        app_toasts()
            .pending_untracked()
            .contains(&GATE_FAILED_TOAST.to_string()),
        "the failure toasts as every flow does"
    );
    assert_eq!(
        dock_credentials().vault_endpoints(),
        both,
        "never reset unasked"
    );

    harness.tap_node("the reset action", button_labelled(RESET_ACTION));
    assert!(shows_text(&harness, RESET_TITLE));
    assert_eq!(
        dock_credentials().vault_endpoints(),
        both,
        "…nor before the confirm"
    );

    harness.tap_node("the reset confirm", button_labelled(RESET_CONFIRM));
    harness.pump_until("the reset's copy", |harness| {
        harness.shows_value(MANUAL_RESET_MESSAGE)
    });
    assert!(
        dock_credentials()
            .vault_endpoints()
            .unwrap_or_default()
            .is_empty()
    );
    assert!(!saved_row(DOCK_A).has_credential && !saved_row(DOCK_B).has_credential);
    assert_eq!(dock_credentials().prompts().len(), 1);
}

/// Connect through the form to [`CRED_ENDPOINT`] and accept the save offer.
fn save_through_the_offer(creds: &Creds) {
    connect_fixture(creds);
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
}

#[test]
fn a_form_connect_while_unlocked_saves_with_one_vault_write_into_the_handle() {
    let _g = serial();
    let _ambient = setup();
    let keyring = KeyringHandle::new();
    keyring.publish(Keyring::new());
    let creds = keyring_fixture(Vec::new(), &keyring);

    save_through_the_offer(&creds);

    assert_eq!(creds.credentials.writes(), 1);
    assert!(creds.credentials.prompts().is_empty(), "no read at all");
    let stored = creds
        .credentials
        .stored(CRED_ENDPOINT)
        .expect("in the vault");
    assert_eq!(
        keyring
            .token_for(CRED_ENDPOINT)
            .as_deref()
            .map(String::as_str),
        Some(stored.as_str()),
        "the handle holds the new token"
    );
}

#[test]
fn a_form_connect_while_locked_takes_the_locked_save_and_leaves_the_handle_alone() {
    let _g = serial();
    let _ambient = setup();
    let keyring = KeyringHandle::new();
    let creds = keyring_fixture(Vec::new(), &keyring);

    save_through_the_offer(&creds);

    assert_eq!(
        creds.credentials.prompts().len(),
        1,
        "the read of the read-modify-write"
    );
    assert_eq!(creds.credentials.writes(), 1);
    assert!(creds.credentials.stored(CRED_ENDPOINT).is_some());
    assert!(!keyring.is_unlocked() && keyring.endpoints().is_empty());
}

/// The fill of the rounded rect the CTA paints, found by its size.
fn cta_fill(harness: &mut Harness) -> Color {
    let cta = harness.bounds("the connect CTA", connect_cta);
    let scene = harness.settle();
    scene
        .rounded_fills
        .iter()
        .find(|(_, size, _, _)| {
            (size.width - cta.width()).abs() < 1.0 && (size.height - cta.height()).abs() < 1.0
        })
        .map(|(_, _, _, color)| *color)
        .expect("the CTA paints its fill")
}

#[test]
fn the_cta_steps_down_to_secondary_once_servers_are_saved() {
    let _g = serial();
    let _ambient = setup();
    // The harness paints buttons with their unthemed style constants, so the
    // read is the change of fill between the two styles, not a palette token.
    let mut empty = mount_limited(Tier::Pro, Vec::new());
    let primary = cta_fill(&mut empty);
    assert_eq!(count(&empty, button_labelled(UNLOCK_TWO)), 0, "no dock");
    assert_qr_beside_the_cta(&empty);

    let mut saved = mount_limited(Tier::Pro, dock_rows(TrustMode::System));
    let secondary = cta_fill(&mut saved);
    assert_ne!(primary, secondary, "the CTA steps down beside the dock");
    assert_eq!(count(&saved, button_labelled(UNLOCK_TWO)), 1);
    assert_qr_beside_the_cta(&saved);

    let mut emptied = mount_limited(Tier::Pro, Vec::new());
    assert_eq!(cta_fill(&mut emptied), primary, "…and back up without rows");
}

#[test]
fn rows_are_informational_while_the_dock_can_unlock_and_delete_stays_ungated() {
    let _g = serial();
    let _ambient = setup();
    let _bio = Biometrics::available();
    let mut harness = mount_dock(
        Tier::Pro,
        dock_rows(TrustMode::System),
        seed_both_in_the_vault,
    );

    harness.tap_node("the older row", |node| node.value() == Some(DOCK_B));
    harness.flush();
    assert_eq!(
        harness.field_values().first().map(String::as_str),
        Some(""),
        "the row tap refills nothing"
    );
    assert!(dock_credentials().prompts().is_empty() && !connected());

    harness.tap_node("the older row's delete", button_labelled("Delete Dock B"));
    harness.tap_node("the remove confirm", button_labelled(REMOVE_CONFIRM));
    harness.pump_until("the row to go", |_| {
        app_saved_endpoints() == vec![DOCK_A.to_string()]
    });
    assert!(dock_credentials().prompts().is_empty(), "delete is ungated");
}

#[test]
fn the_tablet_list_closes_on_the_same_dock_with_informational_rows() {
    let _g = serial();
    let _ambient = setup();
    let _bio = Biometrics::available();
    let mut harness = sized(FULL_TABLET, || {
        mount_dock(
            Tier::Pro,
            dock_rows(TrustMode::System),
            seed_both_in_the_vault,
        )
    });

    harness.tap_node("the older tablet row", |node| {
        node.value() == Some("Dock B")
    });
    harness.flush();
    assert!(dock_credentials().prompts().is_empty() && !connected());

    harness.tap_node("the dock", button_labelled(UNLOCK_TWO));
    harness.pump_until("the dock's connect", |_| connected());
    assert_eq!(dock_credentials().prompts(), vec![UNLOCK_TWO.to_string()]);
}

/// A held biometric prompt: `store_in_vault` waits here, after the snapshot
/// was taken, until the test releases it.
#[derive(Default)]
struct HeldPrompt {
    entered: std::sync::atomic::AtomicBool,
    released: std::sync::atomic::AtomicBool,
    waker: Mutex<Option<std::task::Waker>>,
}

impl HeldPrompt {
    fn release(&self) {
        self.released
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(waker) = self.waker.lock().unwrap_or_else(|p| p.into_inner()).take() {
            waker.wake();
        }
    }
}

struct Hold<'a>(&'a HeldPrompt);

impl std::future::Future for Hold<'_> {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        let held = self.0;
        held.entered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if held.released.load(std::sync::atomic::Ordering::SeqCst) {
            return std::task::Poll::Ready(());
        }
        *held.waker.lock().unwrap_or_else(|p| p.into_inner()) = Some(cx.waker().clone());
        std::task::Poll::Pending
    }
}

/// [`MockCredentialService`] whose unlocked save holds on [`HeldPrompt`].
struct HeldVault {
    inner: Arc<MockCredentialService>,
    prompt: Arc<HeldPrompt>,
}

#[clean_signals::async_trait]
impl CredentialService for HeldVault {
    async fn store(&self, request: StoreRequest) -> CredentialStatus {
        self.inner.store(request).await
    }

    async fn unlock(&self, request: UnlockRequest) -> UnlockedCredential {
        self.inner.unlock(request).await
    }

    async fn unlock_all(&self, reason: &str, endpoints: &[String]) -> UnlockedKeyring {
        self.inner.unlock_all(reason, endpoints).await
    }

    async fn store_in_vault(
        &self,
        request: StoreRequest,
        keyring: &mut Keyring,
    ) -> CredentialStatus {
        Hold(&self.prompt).await;
        self.inner.store_in_vault(request, keyring).await
    }

    async fn delete_from_vault(
        &self,
        server_key: String,
        keyring: &mut Keyring,
    ) -> Result<(), AppFailure> {
        self.inner.delete_from_vault(server_key, keyring).await
    }

    async fn delete(&self, server_key: String) -> Result<(), AppFailure> {
        self.inner.delete(server_key).await
    }

    async fn reset_vault(&self) -> Result<(), AppFailure> {
        self.inner.reset_vault().await
    }
}

/// An unlocked install's form-connect save to [`CRED_ENDPOINT`], held at its
/// prompt (after the snapshot) until the returned [`HeldPrompt`] is released.
fn hold_an_unlocked_save(keyring: &KeyringHandle) -> (Creds, Arc<HeldPrompt>) {
    keyring.publish(Keyring::new());
    let prompt = Arc::new(HeldPrompt::default());
    let creds = keyring_fixture(Vec::new(), keyring);
    let held = Arc::new(HeldVault {
        inner: Arc::clone(&creds.credentials),
        prompt: Arc::clone(&prompt),
    });
    let controller = Arc::new(ConnectController::new(
        Arc::clone(&creds.servers) as _,
        held as _,
        Arc::clone(&creds.tokens) as _,
        Arc::clone(&creds.connection),
        Arc::clone(&creds.pro),
        keyring.clone(),
    ));
    let creds = Creds {
        controller,
        ..creds
    };

    connect_fixture(&creds);
    creds
        .connection
        .set_credential_saver(creds.controller.credential_saver());
    creds
        .connection
        .credential_offer
        .set(Some("Cred rig".to_string()));
    creds.connection.accept_credential_offer();
    pump_until_done("the save to reach its prompt", || {
        prompt.entered.load(std::sync::atomic::Ordering::SeqCst)
    });
    (creds, prompt)
}

/// Release `prompt` and drive the held save until the row records it.
fn settle_the_held_save(creds: &Creds, prompt: &HeldPrompt) {
    prompt.release();
    pump_until_done("the save to settle", || {
        stored_row(&creds.servers).has_credential
    });
    assert!(
        creds.credentials.stored(CRED_ENDPOINT).is_some(),
        "the vault write itself went through"
    );
}

#[test]
fn a_lock_during_the_unlocked_save_keeps_the_token_out_of_the_locked_handle() {
    let _g = serial();
    let _ambient = setup();
    let keyring = KeyringHandle::new();
    let (creds, prompt) = hold_an_unlocked_save(&keyring);

    creds.connection.lock_and_disconnect(&keyring);
    settle_the_held_save(&creds, &prompt);

    assert!(
        !keyring.is_unlocked() && keyring.endpoints().is_empty(),
        "a handle locked mid-prompt stays locked and empty: {:?}",
        keyring.endpoints()
    );
}

#[test]
fn another_change_to_the_unlocked_handle_mid_save_still_takes_the_token() {
    let _g = serial();
    let _ambient = setup();
    let keyring = KeyringHandle::new();
    let (creds, prompt) = hold_an_unlocked_save(&keyring);

    keyring.insert(DOCK_B, zeroize::Zeroizing::new(DOCK_TOKEN_B.to_string()));
    settle_the_held_save(&creds, &prompt);

    assert_eq!(
        keyring
            .token_for(CRED_ENDPOINT)
            .as_deref()
            .map(String::as_str),
        creds.credentials.stored(CRED_ENDPOINT).as_deref(),
        "the handle stays in step with the vault it just wrote"
    );
    assert!(
        keyring.token_for(DOCK_B).is_some(),
        "…beside the other change"
    );
}
