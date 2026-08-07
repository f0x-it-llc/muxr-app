//! The `/` route's page: the Connect screen.
//!
//! [`connect_page`] is what the router mounts — a one-line adapter returning
//! `any(component(ConnectScreen))`, per `docs/ARCHITECTURE.md`'s "Building a
//! Screen" charter: the route seam speaks [`AnyView<AppState>`], the screen
//! itself is a `Component` with its own [`ConnectState`].
//!
//! [`connect_layout`] is the arrangement half, split out because the cold-start
//! splash ([`crate::ui::splash::sequence`]) lays out the *same* eight pieces
//! through the *same* function — each wrapped in its spring-drop entrance — so
//! the frame the splash finishes on and the frame the route paints are the same
//! picture. Both callers hand the layout a `Vec` of already-built pieces (see
//! [`super::pieces`]); the layout itself is pure arrangement and never builds
//! one, which is what keeps the two trees honest.
//!
//! Transcribed from `docs/redesign.html`'s `.connect` rule (and
//! `docs/splash.html`'s `.ui-layer`, which is the same box): a top-anchored
//! column, `padding:30px 22px`, `gap:22px`, children stretched to the layer
//! width. The page paints its own `--bg-base` background — the shell clears
//! to `ColorScheme::surface` (Glyph's card tone), not the page tone.
//!
//! # The screen's three layers
//!
//! `docs/redesign.html`'s connect block is a `.scroll` over the page, with an
//! absolutely-positioned `.fab` on top. This page mirrors that as a `Stack`:
//! backdrop, scrolled layout, QR affordance. The **scroll is the screen's, not
//! the layout's** — the splash assembles into a fixed layer with nothing to
//! scroll, and wrapping [`connect_layout`] itself would put the splash's
//! staggering pieces inside a scrollable viewport for no reason. Likewise the
//! FAB: it is screen chrome (like the shell's toast layer), not one of the eight
//! staggered pieces, so it appears at the handoff rather than assembling.
//!
//! # Navigating to `/sessions`
//!
//! Through [`crate::routes::Nav`], the app's one path-pushing handle: the
//! screen resolves it with [`app_nav`](crate::routes::app_nav) in
//! [`Component::init`] — where every app-scoped dependency is resolved — keeps
//! it in [`ConnectState`], and lends it to
//! [`ConnectController::connect`] from the CTA's press handler. That push runs
//! the app's real route table **and** its real session guard, exactly as a deep
//! link to `/sessions` would; the guard is capture-based (see
//! `crate::routes::session_guard_with`), so it is safe to consult from a press
//! handler's owner-less call stack.
//!
//! Two things this screen deliberately does **not** do: build a `Router` of its
//! own (there is one `Router` in this app, the composition root's), and call a
//! sibling feature's page builder directly (the cross-feature rule forbids
//! importing `features::sessions::presentation`).
//!
//! The raw [`frust::NavigatorController`] is still held beside the handle: a
//! sheet is a pushed page *builder*, not a path, so
//! `crate::ui::sheet::show_sheet` needs the controller itself. That controller
//! is the **root overlay host**'s (`crate::AppState::overlays`), not the route
//! navigator's: the QR sheet is a page above every piece of app chrome, which
//! is what keeps the floating top bar from staying tappable under it, and what
//! puts the sheet's own scrim over it (FINDINGS #44).
//!
//! # The no-verification confirm gate
//!
//! A saved row stored under [`crate::connection::TrustMode::NoVerification`]
//! carries a real stored credential that
//! [`ConnectController::recover_and_connect`] would otherwise replay, silently,
//! over a connection that authenticates nothing. [`handle_saved_row_tap`] is
//! the gate: it consults
//! [`ConnectController::saved_row_needs_confirmation`] and, only for that one
//! combination (a stored credential **and** [`TrustMode::NoVerification`]),
//! raises [`crate::ui::dialogs::confirm_dialog`] on the **overlay host's**
//! navigator (never the route one — this is a dialog, not a push) before
//! [`ConnectController::saved_row_tapped`] runs at all. Every other row (no
//! stored credential, or `System`/`Pinned`) is untouched.
//!
//! **Ordering: before the unlock, not after.** The gate fires ahead of the
//! biometric prompt rather than after a recovered-but-unconfirmed token, which
//! is the cheaper honest ordering — a "no" answer here costs the user nothing
//! more than the tap they just made, where gating after the unlock would have
//! already spent a system prompt (and, on some platforms, a lockout counter)
//! on a connect they were about to decline anyway.
//!
//! **Why the manual form carries no such gate.** Selecting "Trust anyway" in
//! [`super::pieces::trust_selector`] *is* the explicit consent for a manual
//! connect — the user is looking at the radio row's own warning chip and
//! choosing it. A saved-row tap has no equivalent moment: the row was chosen
//! for its endpoint, not its posture, so the posture needs its own prompt.
//!
//! # The QR sheet
//!
//! [`qr_sheet_body`] is a `Component` of its own ([`QrScanSheet`]), not a plain
//! builder, because it owns a **camera session** — and therefore a lifecycle:
//! open on mount, tear down on every way out (a scrim tap, `Escape`, an Android
//! back press). That is what `Component::init`'s `on_cleanup` gives it, and why
//! the session is never a `Drop` impl (`docs/CODE_STANDARDS.md`'s State &
//! Reactivity rule). The wiring follows `examples/glyph-catalog`'s camera page,
//! the sanctioned `frust-camera` integration precedent, point for point:
//! `use_task` + `spawn_blocking` for the blocking permission call, an
//! `Arc<Mutex<Option<Arc<CameraSession>>>>` cell (an `Arc` because `on_cleanup`
//! takes a `Send + Sync` closure), the session opened once per grant from
//! `build`, and **no signal write from inside the detection callback**, which
//! runs on a plugin-owned thread.
//!
//! The hand-off is therefore explicit: `on_detect` copies the raw value into a
//! plain mutex cell, and a UI-thread poll loop ([`spawn_detection_pump`], the
//! `spawn_local` + cancel-flag shape `presentation::term`'s feed pump already
//! uses) is what parses it, applies it to the form and closes the sheet. First
//! parseable payload wins: the loop returns, so every later detection is
//! ignored — and an *unparseable* one is ignored silently, exactly as the
//! Flutter sheet does, since a QR that is simply not a pairing code is not a
//! failure worth interrupting a scan for. No scanned text is ever formatted
//! into a message, a label or a log line: the raw value is a bearer token in
//! the clear, and it lives in a [`Zeroizing`] holder from the callback until
//! the parse consumes it.
//!
//! **The camera is a phone/tablet affordance.** [`open_camera`] and
//! [`request_camera_permission`] are gated to Android and iOS: every other
//! target renders the sheet's unavailable state instead. That is not a
//! stand-in for the plugin's own desktop fail-soft — it is the product line the
//! Flutter client already draws (macOS is not a muxr target and deliberately
//! carries no camera entitlement), and it is what keeps a desktop preview, and
//! every headless test on a developer's Mac, from asking AVFoundation for a
//! capture device it has no usage-description string to justify.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use clean_signals_frust::use_controller;
use frust::authoring::text::TextStyle;
use frust::authoring::{Color, Size};
use frust::{
    Align, Alignment, AnyView, AsyncValue, Axis, ButtonStyle, Component, CrossAxisAlignment,
    EdgeInsets, FlexView, Get, GetUntracked, IconSource, Padding, SizedBox, Stack, Theme, UseTask,
    WindowMetrics, any, button, component, icon, inflexible, on_cleanup, platform_view, safe_area,
    scroll_view, spawn_local, text, use_context,
};
use frust_camera::barcode::{Barcode, BarcodeFormat};
use frust_camera::{
    BarcodeStreamOptions, CameraError, CameraSession, DetectionPolicy, PermissionStatus,
};
use zeroize::Zeroizing;

use super::chrome::panel;
use super::controller::ConnectController;
use super::pieces::{Live, active_theme, connect_pieces_with};
use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::{expect_credential_service, expect_saved_servers};
use crate::features::connect::domain::use_cases::parse_pairing::parse_pairing_uri;
use crate::features::settings::di::expect_auth_tokens_repository;
use crate::responsive::WIDE_BREAKPOINT_PX;
use crate::routes::{Nav, app_nav};
use crate::ui::backdrop::page_backdrop;
use crate::ui::dialogs::confirm_dialog;
use crate::ui::sheet::show_sheet;
use crate::ui::topbar::icon_button;

/// `.connect{padding:30px 22px}` (`.ui-layer`'s too) — horizontal.
const LAYER_PAD_X: f64 = 22.0;
/// `.connect{padding:30px 22px}` (`.ui-layer`'s too) — vertical.
const LAYER_PAD_Y: f64 = 30.0;
/// `.connect{gap:22px}` — between consecutive pieces.
///
/// The one number the two mocks disagree on: `docs/splash.html`'s older
/// `.ui-layer` says `gap:20px`, `docs/redesign.html`'s `.connect` says 22.
/// The redesign is the screen's visual ground truth, and this constant is the
/// *shared* seam — the splash lays its staggering copies out through this very
/// function — so both renderings move together and the handoff stays a single
/// picture. Forking them is the one thing this seam exists to prevent.
const LAYER_GAP: f64 = 22.0;
/// `.fab{right:20px;bottom:20px}` — the QR affordance's inset from the screen's
/// safe area.
const FAB_MARGIN: f64 = 20.0;
/// The content column's max width past [`WIDE_BREAKPOINT_PX`] (FINDINGS #21's
/// conservative width-clamp pass) — a tablet/landscape/desktop window centers
/// the form rather than stretching it edge to edge. The CTA and every field
/// still stretch full-width *within* this clamp (`connect_layout`'s own
/// stretch cross-axis is untouched); only the box around them narrows. No
/// design mock exists for a structural two-pane Connect, so this is a plain
/// clamp, not a layout switch — see `crate::responsive`'s module docs.
const CONNECT_CLAMP_WIDTH: f64 = 520.0;
/// The QR affordance's accessible name — what a screen reader announces, and
/// what `tests/connect.rs` addresses the button by.
pub const FAB_LABEL: &str = "Pair by QR code";
/// The FAB's box, in logical px. The mock's `.fab` is a 46px disc; this is the
/// same box, which also clears the 44px platform minimum tap target.
const FAB_BOX: f64 = 46.0;
/// The scan mark drawn inside it, in logical px.
const FAB_MARK: f64 = 22.0;
/// …and the same mark, larger, in the sheet's placeholder viewfinder.
const VIEWFINDER_MARK: f64 = 44.0;

/// `.fab` renders `▣` — the mock's QR/scan mark. **As a vector, never that
/// glyph.**
///
/// The FAB used to set `▣` (`U+25A3`, "white square containing black small
/// square") as a button label. It is in none of the bundled Glyph faces, so
/// each platform resolved it through its own fallback chain and each drew a
/// *different mark* — Ed's round-4 device gate caught Android and iOS
/// disagreeing about what the app's primary pairing affordance even looks
/// like. A font fallback chain is not something an app can specify, so the
/// only fix is to stop asking for one (see `crate::ui::topbar`'s "Chrome icons
/// are vectors" rule).
///
/// **Why an app-supplied source.** The vendored Material Symbols starter set
/// (`frust::icons`) carries no QR/scan/viewfinder glyph at all — the closest
/// is `image`, which reads as a photo. So this is authored here against the
/// same 24×24 design box every generated entry uses, through the documented
/// app-supply seam (`IconSource`/`IconData::from_path`), reproducing the
/// mock's motif exactly: four corner brackets — the universal scan-frame —
/// around a solid centre square, which is `▣`'s own "square containing a
/// square" read at any size.
///
/// Authored as one `d` string of axis-aligned rectangles: eight bracket arms
/// (two per corner, `BRACKET`-long and `STROKE`-thick, inset `INSET` from the
/// box) plus the centre square. Every number below is the source of one
/// subpath, and the unit test at the bottom of this module re-derives the
/// bracket geometry from them so a typo cannot pass as art.
const SCAN_MARK: IconSource = IconSource {
    d: concat!(
        // top-left corner: across, then down
        "M4 4h7v2.5H4Z M4 4h2.5v7H4Z ",
        // top-right
        "M13 4h7v2.5h-7Z M17.5 4H20v7h-2.5Z ",
        // bottom-left
        "M4 17.5h2.5V20H4Z M4 13h2.5v7H4Z ",
        // bottom-right
        "M13 17.5h7V20h-7Z M17.5 13H20v7h-2.5Z ",
        // the centre square
        "M9 9h6v6H9Z",
    ),
    design: 24.0,
};

/// [`SCAN_MARK`]'s inset from its design box's edge, its bracket-arm length,
/// and its arm thickness — the three numbers the `d` string above is built
/// from, named so the tripwire below can re-derive it.
const SCAN_INSET: f64 = 4.0;
const SCAN_BRACKET: f64 = 7.0;
const SCAN_STROKE: f64 = 2.5;

// The mark reads as a scan *frame* only while its corner brackets stay
// corners: an edge's two arms must leave a gap between them, and an arm must
// be thinner than it is long. Checked at compile time, since the `d` string
// above is authored by hand from exactly these numbers (the module's own test
// re-derives the far arms' coordinates from them, and parses the result).
const _: () = {
    assert!(2.0 * SCAN_BRACKET < 24.0 - 2.0 * SCAN_INSET);
    assert!(SCAN_STROKE < SCAN_BRACKET);
};
/// The QR sheet's viewfinder square, in logical px. An original constant: the
/// mock has no camera screen to transcribe, and this is the largest square that
/// still leaves the sheet's caption room on a 340px-wide phone.
const VIEWFINDER: f64 = 220.0;
/// Gap between the sheet's viewfinder and the copy under it, in logical px —
/// Glyph's `--space-4` step, matching `crate::ui::dialogs`' own field/action gap.
const SHEET_GAP: f64 = 16.0;

/// Arrange `pieces` exactly as `.ui-layer` does: a safe-area-inset,
/// `LAYER_PAD_*`-padded, top-anchored column with `LAYER_GAP` between
/// consecutive pieces, each stretched to the full layer width.
///
/// Takes the pieces already built so the splash can pass its wrapped copies
/// (identical layout, animated paint) and the screen can pass its live ones.
/// Generic over the state type for the same reason the pieces are — the splash
/// composes under [`AppState`], the screen under its own [`ConnectState`].
pub fn connect_layout<S: 'static>(pieces: Vec<AnyView<S>>) -> AnyView<S> {
    let mut children = Vec::with_capacity(pieces.len() * 2);
    for (index, piece) in pieces.into_iter().enumerate() {
        if index > 0 {
            children.push(inflexible(any(SizedBox::<S>(None, Some(LAYER_GAP)))));
        }
        children.push(inflexible(piece));
    }

    any(safe_area(Padding(
        EdgeInsets::symmetric(LAYER_PAD_X, LAYER_PAD_Y),
        FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch),
    )))
}

/// The Connect screen: the `/` route's page. See the [module docs](self).
pub fn connect_page(overlays: frust::NavigatorController<AppState>) -> AnyView<AppState> {
    any(component(ConnectScreen { overlays }))
}

/// The Connect screen `Component`. Stateless configuration — the overlay-host
/// handle the route builder was given, and nothing else; everything retained
/// lives in [`ConnectState`].
struct ConnectScreen {
    overlays: frust::NavigatorController<AppState>,
}

/// The Connect screen's retained state.
///
/// Per the charter's state-split rule this holds only the **non-reactive**
/// half — three navigation/connection handles, the controller handle, and one
/// plain `bool`. Every *observed* value (the form's fields, the saved list,
/// the validation flag) lives in [`ConnectController`]'s signals instead, so a
/// write wakes a rebuild.
pub struct ConnectState {
    /// The hosted view model, disposed with this component (`use_controller`).
    controller: Arc<ConnectController>,
    /// The app's one path-pushing handle — see the module docs' navigation
    /// note. Clonable (an `Rc` bump), so a press closure captures its own copy.
    app_nav: Nav,
    /// The root overlay host's navigator, for `crate::ui::sheet::show_sheet`
    /// (which pushes a transparent page rather than resolving a location) —
    /// see the module docs' navigation note.
    overlays: frust::NavigatorController<AppState>,
    /// The app-scoped connection handle, held only so [`Component::build`] can
    /// watch [`ConnectionController::connected`]'s true→false edge (a
    /// Disconnect, e.g. Settings') and scrub the login-token field then — see
    /// [`ConnectController::token`]'s doc for the full token-hygiene picture.
    /// `ConnectController::connect` drives this same handle itself; this copy
    /// exists purely to read it back.
    connection: Arc<ConnectionController>,
    /// Whether `connection.connected` read `true` the last time `build` ran —
    /// the previous-observed value the disconnect edge above is detected
    /// against. Seeded from the connected state at mount, so a screen that
    /// somehow mounted already-connected does not misfire a spurious clear on
    /// its first build.
    was_connected: bool,
}

impl Component for ConnectScreen {
    type State = ConnectState;

    fn init(&self) -> ConnectState {
        // Resolve app-scoped dependencies HERE, where a reactive owner is live
        // (`Component::init` runs inside the shell's rebuild, under the owner
        // `ComponentView::build` created). A press handler cannot do this —
        // see `super::controller`'s injection note.
        let repository = expect_saved_servers();
        let credentials = expect_credential_service();
        // The one cross-feature resolve: a saved credential *is* a server-side
        // token, and `settings` owns that store — the same shape the terminal
        // screen resolves `sessions`'/`settings`' own repositories with
        // (`docs/ARCHITECTURE.md`'s cross-feature note).
        let tokens = expect_auth_tokens_repository();
        let connection = expect_connection_controller();
        let was_connected = connection.connected.get_untracked();
        let controller = use_controller::<ConnectController, AppFailure>({
            let connection = Arc::clone(&connection);
            move || ConnectController::new(repository, credentials, tokens, connection)
        });
        // The credential-save seam `crate::connection` calls when the user
        // accepts the offer Sessions raises: installed here, where the flow's
        // dependencies are already resolved, and held weakly by the connection
        // handle (see `ConnectController::credential_saver`).
        connection.set_credential_saver(controller.credential_saver());

        ConnectState {
            controller,
            // Resolved here for the same reason the two above are: `init` is
            // the screen's one dependency-resolution point (`app_nav` reads no
            // reactive context, but a press handler resolving its own handles
            // is the habit this app does not want).
            app_nav: app_nav(),
            overlays: self.overlays.clone(),
            connection,
            was_connected,
        }
    }

    fn build(&self, state: &mut ConnectState) -> AnyView<ConnectState> {
        let theme = active_theme();

        // Login-token hygiene (review F5), the disconnect half: `connect()`
        // clears the token on its own edge, but a Disconnect (Settings' flow
        // calls `ConnectionController::disconnect`) can flip `connected` back
        // to `false` while this screen stays mounted+reconciled underneath
        // whatever covers it — the whole-app rebuild model keeps a covered
        // route's own `build` reconciling. A **tracked** read here (not
        // `get_untracked`) is what lets a later write wake this component's
        // own next rebuild, per the reactive-read rule.
        let connected = state.connection.connected.get();
        if state.was_connected && !connected {
            state.controller.clear_token();
            // A stashed pairing goes out on the same edge: the pin it carries
            // belongs to a session that has ended (`connect_controller.dart`
            // clears its pending trust here too).
            state.controller.clear_pending_pairing();
        }
        state.was_connected = connected;

        let live = Live {
            controller: Arc::clone(&state.controller),
            on_connect: {
                let controller = Arc::clone(&state.controller);
                let nav = state.app_nav.clone();
                Rc::new(move || controller.connect(&nav))
            },
            on_connect_pinned: {
                let controller = Arc::clone(&state.controller);
                let nav = state.app_nav.clone();
                Rc::new(move || controller.connect_from_pairing(&nav))
            },
            // A saved row's tap needs the navigator too — reconnecting through
            // a stored credential lands on `/sessions` exactly as a manual
            // connect does — so it is a screen-owned callback like the two
            // above rather than a bare controller method. It also needs the
            // overlay host, for the no-verification confirm gate
            // `handle_saved_row_tap` may raise (see the module docs).
            on_saved_tap: {
                let controller = Arc::clone(&state.controller);
                let overlays = state.overlays.clone();
                let nav = state.app_nav.clone();
                Rc::new(move |index| handle_saved_row_tap(&controller, &overlays, &nav, index))
            },
        };

        // Responsive width-clamp pass (FINDINGS #21): read metrics once,
        // branch on one page constant, never touch layout again — the
        // reference idiom `examples/glyph-catalog/src/pages/responsive.rs`
        // documents. `None` (no shell has published yet, or a bare test
        // harness) takes the same branch as a genuinely narrow window.
        let metrics = use_context::<WindowMetrics>();
        let is_wide = metrics
            .map(|m| m.size.width >= WIDE_BREAKPOINT_PX)
            .unwrap_or(false);

        let content = connect_layout(connect_pieces_with(&theme, Some(&live)));
        let content = if is_wide {
            any(Align(
                Alignment::CENTER,
                SizedBox::<ConnectState>(Some(CONNECT_CLAMP_WIDTH), None).child(content),
            ))
        } else {
            content
        };

        any(Stack(vec![
            any(page_backdrop::<ConnectState>()),
            any(scroll_view(content)),
            qr_affordance(&state.overlays, &state.controller),
        ]))
    }
}

/// A saved row at `index` was tapped. The no-verification confirm gate: for a row
/// [`ConnectController::saved_row_needs_confirmation`] flags (a stored
/// credential under [`crate::connection::TrustMode::NoVerification`]), raise
/// [`confirm_dialog`] on `overlays` and defer
/// [`ConnectController::recover_and_connect`] to its confirm action; every
/// other row runs [`ConnectController::saved_row_tapped`] immediately, exactly
/// as before this gate existed. See the [module docs](self)'s "no-verification
/// confirm gate" section for the ordering and manual-path rationale.
fn handle_saved_row_tap(
    controller: &Arc<ConnectController>,
    overlays: &frust::NavigatorController<AppState>,
    nav: &Nav,
    index: usize,
) {
    let Some(server) = controller.saved_row_needs_confirmation(index) else {
        controller.saved_row_tapped(index, nav);
        return;
    };
    let controller = Arc::clone(controller);
    let nav = nav.clone();
    confirm_dialog(
        overlays,
        "Connect without verification?",
        "This connection can't verify the server, and the saved token will be \
         sent over an unauthenticated channel.",
        "Connect anyway",
        move |_app: &mut AppState| {
            controller.recover_and_connect(server.clone(), &nav);
        },
    );
}

/// `.fab`: the amber QR/pair affordance, pinned to the screen's bottom-trailing
/// safe area.
///
/// An [`icon_button`] rather than [`button`]: the affordance is a **mark**, and
/// `frust::button` takes a `String` label and nothing else, which is exactly
/// how it came to be a font glyph in the first place (see [`SCAN_MARK`]). What
/// that costs against the stock button — its press *scale* (this dips tone
/// instead) and its loading/disabled states — this affordance never used, the
/// same trade `super::pieces::connect_button` already records.
fn qr_affordance(
    overlays: &frust::NavigatorController<AppState>,
    controller: &Arc<ConnectController>,
) -> AnyView<ConnectState> {
    let overlays = overlays.clone();
    let controller = Arc::clone(controller);
    let theme = active_theme();
    let scheme = theme.scheme();
    any(safe_area(Align(
        Alignment::BOTTOM_RIGHT,
        Padding(
            EdgeInsets::all(FAB_MARGIN),
            icon_button(SCAN_MARK, FAB_LABEL, move |_state: &mut ConnectState| {
                let controller = Arc::clone(&controller);
                show_sheet(&overlays, move |sheet| {
                    // The sheet closes itself the moment a payload lands, so
                    // its handle is threaded in as the one-shot closer the
                    // scan pump fires — the same `Rc<dyn Fn()>` shape
                    // `ui::sheet`'s own `on_dismiss` takes, and gated by the
                    // same at-most-one-pop handle every other closer goes
                    // through.
                    let sheet = sheet.clone();
                    qr_sheet_body(
                        Rc::new(move || {
                            sheet.close();
                        }),
                        Arc::clone(&controller),
                    )
                })
            })
            // The mock's amber disc and the ink the design pairs with it — the
            // same `primary_container`/`on_primary_container` pair the CTA
            // resolves (see `super::pieces`' `Palette`).
            .fill(scheme.primary_container)
            .ink(scheme.on_primary_container)
            .icon_size(FAB_MARK)
            .box_size(Size::new(FAB_BOX, FAB_BOX))
            .radius(FAB_BOX / 2.0),
        ),
    )))
}

// ---------------------------------------------------------------------------
// The QR sheet (see the module docs)
// ---------------------------------------------------------------------------

/// The sheet's own title — the same words the affordance that opened it is
/// named by.
const SHEET_TITLE: &str = FAB_LABEL;
/// The live viewfinder's copy — the Flutter sheet's own subtitle.
const SCAN_HINT: &str = "Point the camera at the QR code shown by muxrctl on your server.";
/// …while the platform is still answering the permission request.
const PERMISSION_PENDING: &str = "Requesting camera permission…";
/// …once it has refused. No retry is offered: iOS shows its permission dialog
/// once per install, so a second ask is a no-op and the only way forward is the
/// system settings app (the Flutter sheet draws the same line).
const PERMISSION_DENIED: &str = concat!(
    "Camera permission denied — enable camera access for muxr in your device ",
    "settings, then scan again.",
);
/// …when Android has no foreground activity cached to show the dialog on. A
/// retry is worth offering here: the state is transient by construction.
const PERMISSION_NEEDS_UI: &str =
    "The permission dialog needs muxr in the foreground — try again from here.";
/// …on a build with no camera backend at all: the desktop preview, and every
/// headless test. Pairing by hand is the whole path, so this is a plain
/// instruction rather than an error.
pub const NO_CAMERA: &str =
    "Camera pairing needs a phone or tablet — enter the server URL and token by hand instead.";
/// …when the camera exists but refused to open or to stream. The plugin's own
/// detail is deliberately not shown (`crate::failure`'s rule: internal detail
/// never reaches the user).
const CAMERA_UNAVAILABLE: &str = "Camera unavailable.";
/// The retry action's label — the Flutter sheet's own copy.
const RETRY_LABEL: &str = "try again";
/// The torch toggle's two labels.
const TORCH_ON_LABEL: &str = "torch on";
const TORCH_OFF_LABEL: &str = "torch off";
/// What a screen reader announces over the live preview slot: the platform
/// view carries no frust-visible semantics of its own, so this label is the
/// only signal that region gives (`platform_view`'s documented v1 gap).
const VIEWFINDER_LABEL: &str = "Camera viewfinder";

/// How often the UI thread checks the detection cell the camera's own thread
/// writes into. 100ms is well under the reaction time of pointing a phone at a
/// code and far above the cost of taking an uncontended lock; the scan itself
/// runs at the camera's rate, not this one.
const SCAN_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The QR sheet's body: a live scanning viewfinder, or the copy that explains
/// why there isn't one.
///
/// `close` is the sheet's one-shot dismissal (see [`qr_affordance`]); it is
/// fired exactly once, by the scan pump, when a payload has been applied to
/// `controller`. Every *other* way out of this sheet is the scaffold's own
/// (a scrim tap, `Escape`, an Android back press), and each of them tears this
/// component down, which is what closes the camera.
///
/// `pub` so `tests/connect.rs` can mount the body directly: a navigator's pages
/// contribute no semantics (FINDINGS #23), so the sheet is invisible to the
/// accessibility tree while it is a pushed page.
pub fn qr_sheet_body(close: Rc<dyn Fn()>, controller: Arc<ConnectController>) -> AnyView<AppState> {
    any(component(QrScanSheet { close, controller }))
}

/// The QR sheet `Component`. Stateless configuration; everything retained
/// lives in [`QrSheetState`].
struct QrScanSheet {
    /// See [`qr_sheet_body`].
    close: Rc<dyn Fn()>,
    /// The form a scanned payload is applied to.
    controller: Arc<ConnectController>,
}

/// The QR sheet's retained state.
pub struct QrSheetState {
    /// The camera-permission probe, or `None` on a target with no camera
    /// backend — where the sheet renders [`NO_CAMERA`] immediately rather than
    /// awaiting an answer nothing will give (see the module docs' gating note).
    permission: Option<UseTask<PermissionStatus>>,
    /// The open session. `Arc<Mutex<..>>` rather than an `Rc`/`RefCell` only
    /// because [`on_cleanup`] takes a `Send + Sync` closure — the
    /// `examples/glyph-catalog` camera page's own reason for the same shape.
    session: Arc<Mutex<Option<Arc<CameraSession>>>>,
    /// Where the plugin thread leaves what it decoded.
    detections: Arc<Detections>,
    /// Whether the camera has been opened for the current grant — so a refusal
    /// is not retried on every rebuild.
    opened: bool,
    /// Whether opening the camera, or starting its barcode stream, failed.
    /// A plain flag: the plugin's message is diagnostic, and this sheet shows
    /// the user [`CAMERA_UNAVAILABLE`] instead of it.
    camera_failed: bool,
    /// The torch's last *requested* state. Optimistic by necessity —
    /// `set_torch` is "accepted, not confirmed" (`plugins/camera`'s §5), so
    /// this mirrors the last call this sheet made, never the LED.
    torch_on: bool,
}

/// The one-slot hand-off between the camera's decode thread and the UI thread.
///
/// A plain mutex, never a signal: `on_detect` runs on a plugin-owned thread
/// with the platform's own buffer borrowed, and writing a signal from there is
/// forbidden outright (`plugins/camera`'s stream contract). The held value is
/// [`Zeroizing`] because a pairing QR's text *is* a bearer token — see the
/// module docs.
#[derive(Default)]
struct Detections {
    /// The most recent decoded value, or `None` once the pump has taken it.
    last: Mutex<Option<Zeroizing<String>>>,
}

impl Detections {
    /// Record `raw` as the newest detection, replacing (and scrubbing) any the
    /// pump has not taken yet. Runs on the plugin's thread.
    fn publish(&self, raw: &str) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Zeroizing::new(raw.to_string()));
    }

    /// Take the newest detection, if any. Runs on the UI thread.
    fn take(&self) -> Option<Zeroizing<String>> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl Component for QrScanSheet {
    type State = QrSheetState;

    fn init(&self) -> QrSheetState {
        let session: Arc<Mutex<Option<Arc<CameraSession>>>> = Arc::new(Mutex::new(None));
        {
            // The mandatory teardown: this sheet holds the device's single
            // image-stream claim, and every way out of it (payload applied,
            // scrim, `Escape`, Android back) disposes this component's owner.
            // `CameraSession::close` stops the running barcode stream and takes
            // the torch out with it, so this one call is the whole teardown.
            let cell = Arc::clone(&session);
            on_cleanup(move || {
                let session = cell.lock().unwrap_or_else(PoisonError::into_inner).take();
                if let Some(session) = session {
                    session.close();
                }
            });
        }

        let detections = Arc::new(Detections::default());
        spawn_detection_pump(
            Arc::clone(&detections),
            Arc::clone(&self.controller),
            Rc::clone(&self.close),
        );

        QrSheetState {
            permission: request_camera_permission(),
            session,
            detections,
            opened: false,
            camera_failed: false,
            torch_on: false,
        }
    }

    fn build(&self, state: &mut QrSheetState) -> AnyView<QrSheetState> {
        let theme = active_theme();
        open_camera_once(state);
        match sheet_stage(state) {
            Stage::Live => live_body(state, &theme),
            Stage::Waiting => message_body(&theme, PERMISSION_PENDING, MessageTone::Muted, false),
            Stage::Unavailable(message, tone, retryable) => {
                message_body(&theme, message, tone, retryable)
            }
        }
    }
}

/// What the sheet is showing this frame.
enum Stage {
    /// The permission request is still out.
    Waiting,
    /// A session is open: the live viewfinder.
    Live,
    /// No viewfinder — the copy that says why, its ink, and whether a retry
    /// could plausibly change the answer.
    Unavailable(&'static str, MessageTone, bool),
}

/// Whether a message reads as an ordinary instruction or as a failure.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageTone {
    /// `on_surface_variant` — nothing went wrong (the desktop path, the wait).
    Muted,
    /// `error` — the camera is there and refused.
    Failed,
}

/// Which [`Stage`] `state` is in. Reads the permission task's signal, so the
/// sheet rebuilds when the platform answers.
fn sheet_stage(state: &QrSheetState) -> Stage {
    let Some(permission) = &state.permission else {
        return Stage::Unavailable(NO_CAMERA, MessageTone::Muted, false);
    };
    if state.camera_failed {
        return Stage::Unavailable(CAMERA_UNAVAILABLE, MessageTone::Failed, true);
    }
    if has_session(state) {
        return Stage::Live;
    }
    match permission.signal().get() {
        AsyncValue::Idle | AsyncValue::Loading(_) => Stage::Waiting,
        AsyncValue::Ready(PermissionStatus::Granted) => Stage::Waiting,
        AsyncValue::Ready(PermissionStatus::Pending) => Stage::Waiting,
        AsyncValue::Ready(PermissionStatus::Denied) => {
            Stage::Unavailable(PERMISSION_DENIED, MessageTone::Failed, false)
        }
        AsyncValue::Ready(PermissionStatus::NeedsUi) => {
            Stage::Unavailable(PERMISSION_NEEDS_UI, MessageTone::Failed, true)
        }
        // `PermissionStatus` is `#[non_exhaustive]`: a variant this build has
        // never heard of reads as an unspecified refusal rather than a compile
        // break, and a retry is the only honest offer to make about it.
        AsyncValue::Ready(_) => Stage::Unavailable(CAMERA_UNAVAILABLE, MessageTone::Failed, true),
        AsyncValue::Error(_) => Stage::Unavailable(CAMERA_UNAVAILABLE, MessageTone::Failed, true),
    }
}

/// Whether a session is open right now.
fn has_session(state: &QrSheetState) -> bool {
    state
        .session
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
}

/// Open the camera and start scanning — once per grant, from `build`, the
/// `examples/glyph-catalog` camera page's own idempotent open branch.
fn open_camera_once(state: &mut QrSheetState) {
    let granted = state.permission.as_ref().is_some_and(|permission| {
        matches!(
            permission.signal().get(),
            AsyncValue::Ready(PermissionStatus::Granted)
        )
    });
    if state.opened || !granted {
        return;
    }
    state.opened = true;
    let Ok(session) = open_camera() else {
        state.camera_failed = true;
        return;
    };
    let session = Arc::new(session);
    if start_scan(&session, &state.detections).is_err() {
        // A session that cannot stream is a viewfinder with nothing behind it:
        // let it go rather than hold the device open behind an error state.
        session.close();
        state.camera_failed = true;
        return;
    }
    *state.session.lock().unwrap_or_else(PoisonError::into_inner) = Some(session);
}

/// Start the barcode stream, wiring `on_detect` to `detections` and nothing
/// else.
///
/// [`DetectionPolicy::NoDuplicates`] because a pairing code sits in frame for
/// as long as it takes to focus, and this sheet wants the payload once rather
/// than at the decode rate. QR only: `muxrctl` emits nothing else, and every
/// other format the engine might grow would decode to something this sheet
/// would only reject.
fn start_scan(session: &CameraSession, detections: &Arc<Detections>) -> Result<(), CameraError> {
    let detections = Arc::clone(detections);
    session.start_barcode_stream(
        BarcodeStreamOptions {
            formats: vec![BarcodeFormat::QrCode],
            detection: DetectionPolicy::NoDuplicates,
        },
        move |found: &[Barcode]| {
            // Never empty (the plugin's own contract) — `first` is the
            // one-code-per-frame convenience, not a defensive check. Nothing
            // here parses, allocates a message, or writes a signal: the whole
            // callback is one copy into the hand-off cell.
            if let Some(barcode) = found.first() {
                detections.publish(&barcode.raw_value);
            }
        },
    )
}

/// Poll the detection cell on the **UI thread**, apply the first parseable
/// pairing payload, and close the sheet.
///
/// The `spawn_local` + `on_cleanup` cancel-flag shape
/// `presentation::term`'s feed pump already uses: the loop checks the flag
/// before every read, so a sheet torn down between two wakes never touches the
/// controller again. It returns after the first payload — single-shot, so the
/// detections that keep arriving until the session closes are ignored — and an
/// unparseable value is dropped without a word (see the module docs).
fn spawn_detection_pump(
    detections: Arc<Detections>,
    controller: Arc<ConnectController>,
    close: Rc<dyn Fn()>,
) {
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let cancelled = Arc::clone(&cancelled);
        on_cleanup(move || cancelled.store(true, Ordering::Relaxed));
    }
    spawn_local(async move {
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            if let Some(raw) = detections.take()
                && let Some(payload) = parse_pairing_uri(&raw)
            {
                controller.apply_scanned_payload(payload);
                close();
                return;
            }
            clean_signals::time::sleep(SCAN_POLL_INTERVAL).await;
        }
    });
}

/// The live sheet: the platform preview inside the same framed viewfinder the
/// placeholder draws, the scan hint, and — where the lens has one — the torch.
fn live_body(state: &QrSheetState, theme: &Theme) -> AnyView<QrSheetState> {
    let scheme = theme.scheme();
    let guard = state.session.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(session) = guard.as_ref() else {
        // Unreachable: `Stage::Live` is what a held session means. Rendering
        // the instruction beats an `expect` on a lock a rebuild could in
        // principle race.
        drop(guard);
        return message_body(theme, CAMERA_UNAVAILABLE, MessageTone::Failed, true);
    };
    // Target-gated by the session itself — never a shared literal
    // (`plugins/camera`'s README §2).
    let view_type = session.preview_view_type().to_string();
    let params = session.params_json();
    // Read fresh every build rather than cached: Android answers `false` until
    // CameraX finishes binding, so a control gated on the first answer would
    // never appear (`plugins/camera`'s README §5's re-check note).
    let torch_available = session.torch_available();
    drop(guard);

    let viewfinder = panel(
        SizedBox::<QrSheetState>(Some(VIEWFINDER), Some(VIEWFINDER)).child(
            platform_view(view_type)
                .size(VIEWFINDER, VIEWFINDER)
                .params_json(params)
                .semantics_label(VIEWFINDER_LABEL),
        ),
        scheme.surface_container_lowest,
        theme.shape.medium,
    )
    .border(scheme.outline);

    let mut children = vec![
        inflexible(any(Align(Alignment::CENTER, viewfinder))),
        inflexible(any(SizedBox::<QrSheetState>(None, Some(SHEET_GAP)))),
        inflexible(any(
            text(SHEET_TITLE).style(inked(&theme.type_scale.title_medium, scheme.on_surface))
        )),
        inflexible(any(SizedBox::<QrSheetState>(None, Some(SHEET_GAP / 2.0)))),
        inflexible(any(text(SCAN_HINT).style(inked(
            &theme.type_scale.body_small,
            scheme.on_surface_variant,
        )))),
    ];
    if torch_available {
        children.push(inflexible(any(SizedBox::<QrSheetState>(
            None,
            Some(SHEET_GAP),
        ))));
        children.push(inflexible(any(Align(
            Alignment::CENTER,
            button(torch_label(state.torch_on), toggle_torch).style(ButtonStyle::Secondary),
        ))));
    }

    any(FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch))
}

/// The torch toggle's label at `on`.
fn torch_label(on: bool) -> &'static str {
    if on { TORCH_ON_LABEL } else { TORCH_OFF_LABEL }
}

/// Flip the torch. Non-blocking and callable from any thread, the UI one
/// included (`plugins/camera`'s README §5), so this runs straight from the
/// press handler rather than through `spawn_blocking`.
///
/// The requested state is recorded only when the call was accepted; a refusal
/// leaves the label reading what the LED is actually doing, which is all a
/// caller can honestly claim about a control the platform confirms
/// asynchronously (or, on Apple, not at all).
fn toggle_torch(state: &mut QrSheetState) {
    let session = state
        .session
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let Some(session) = session else {
        return;
    };
    let requested = !state.torch_on;
    if session.set_torch(requested).is_ok() {
        state.torch_on = requested;
    }
}

/// The sheet with no live camera: the placeholder viewfinder over `message`,
/// and a retry where one could help.
fn message_body(
    theme: &Theme,
    message: &str,
    tone: MessageTone,
    retryable: bool,
) -> AnyView<QrSheetState> {
    let scheme = theme.scheme();
    let viewfinder = panel(
        SizedBox::<QrSheetState>(Some(VIEWFINDER), Some(VIEWFINDER)).child(Align(
            Alignment::CENTER,
            // The same vector the FAB draws, at viewfinder size — a scan frame
            // is exactly what a camera placeholder wants, and it keeps this
            // screen free of the font-fallback glyph entirely.
            icon(SCAN_MARK).size(VIEWFINDER_MARK).color(scheme.primary),
        )),
        scheme.surface_container_lowest,
        theme.shape.medium,
    )
    .border(scheme.outline);
    let message_ink = match tone {
        MessageTone::Muted => scheme.on_surface_variant,
        MessageTone::Failed => scheme.error,
    };

    let mut children = vec![
        inflexible(any(Align(Alignment::CENTER, viewfinder))),
        inflexible(any(SizedBox::<QrSheetState>(None, Some(SHEET_GAP)))),
        inflexible(any(
            text(SHEET_TITLE).style(inked(&theme.type_scale.title_medium, scheme.on_surface))
        )),
        inflexible(any(SizedBox::<QrSheetState>(None, Some(SHEET_GAP / 2.0)))),
        inflexible(any(
            text(message).style(inked(&theme.type_scale.body_small, message_ink))
        )),
    ];
    if retryable {
        children.push(inflexible(any(SizedBox::<QrSheetState>(
            None,
            Some(SHEET_GAP),
        ))));
        children.push(inflexible(any(Align(
            Alignment::CENTER,
            button(RETRY_LABEL, retry_camera).style(ButtonStyle::Secondary),
        ))));
    }

    any(FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch))
}

/// Ask again: re-run the permission probe and let `build`'s own open branch —
/// not a second one — reopen the camera on the next rebuild.
fn retry_camera(state: &mut QrSheetState) {
    state.camera_failed = false;
    state.opened = false;
    if let Some(permission) = &state.permission {
        permission.restart();
    }
}

/// Request camera permission off the UI thread, or `None` where this build has
/// no camera backend to ask (see the module docs' gating note).
///
/// `Camera::request_permission` **blocks** until the platform machinery
/// resolves — a system dialog on Android, `requestAccessForMediaType:` on iOS —
/// so it is paired with `spawn_blocking` and flattened into one `Result` for
/// `use_task`'s single-error bound, exactly as the glyph-catalog camera page
/// does.
fn request_camera_permission() -> Option<UseTask<PermissionStatus>> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        Some(frust::use_task(|| async {
            match frust::spawn_blocking(frust_camera::Camera::request_permission).await {
                Ok(result) => result,
                Err(join_error) => Err(CameraError::Platform(join_error.to_string())),
            }
        }))
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        None
    }
}

/// Open the back camera, or refuse on a build with no camera backend.
///
/// The refusal is this app's own product line, not the plugin's fail-soft: see
/// the module docs' gating note for why a desktop build must not reach
/// AVFoundation at all.
fn open_camera() -> Result<CameraSession, CameraError> {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        frust_camera::Camera::open(frust_camera::Lens::Back, frust_camera::Resolution::Auto)
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        Err(CameraError::PlatformNotInitialized)
    }
}

/// `style` re-inked with `color` — the same explicit-ink discipline
/// [`super::pieces`] applies (a `TextStyle` left at its default would paint 16px
/// black regardless of the theme). Duplicated rather than shared because
/// `pieces`' copy is private to that module and this is the only other caller.
fn inked(style: &TextStyle, color: Color) -> TextStyle {
    let mut style = style.clone();
    style.color = color;
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use frust::authoring::{BezPath, Shape};

    /// Geometry tolerance, in design-box units.
    const EPSILON: f64 = 1e-9;

    #[test]
    fn the_scan_mark_parses_into_nine_subpaths() {
        // A malformed `d` is a *panic* at first paint (`IconData::resolve`
        // treats a parse failure as the wiring bug it is), which on device
        // means a crash rather than a wrong-looking FAB. This is the tripwire.
        let path = BezPath::from_svg(SCAN_MARK.d).expect("the scan mark parses");
        assert!(!path.elements().is_empty());
        assert_eq!(
            SCAN_MARK.d.matches('M').count(),
            9,
            "eight bracket arms plus the centre square"
        );
    }

    #[test]
    fn the_scan_mark_fills_its_design_box_symmetrically() {
        // Every corner bracket sits `SCAN_INSET` in from its own edge, so the
        // mark's bounds are that inset on all four sides — which is what makes
        // it read as centred at any drawn size (the FAB's 22px and the
        // viewfinder's 44px).
        let path = BezPath::from_svg(SCAN_MARK.d).expect("the scan mark parses");
        let bbox = path.bounding_box();
        let far = SCAN_MARK.design - SCAN_INSET;
        assert!((bbox.x0 - SCAN_INSET).abs() < EPSILON, "{bbox:?}");
        assert!((bbox.y0 - SCAN_INSET).abs() < EPSILON, "{bbox:?}");
        assert!((bbox.x1 - far).abs() < EPSILON, "{bbox:?}");
        assert!((bbox.y1 - far).abs() < EPSILON, "{bbox:?}");
    }

    #[test]
    fn the_far_brackets_sit_where_the_named_numbers_put_them() {
        // The literal `d` above places its trailing/bottom arms at 13 and 17.5
        // — which is `24 - 4 - 7` and `24 - 4 - 2.5`. The transcription's own
        // tripwire, since the path data is authored by hand. (The "brackets
        // stay corners" half is the `const _` assertion beside the constants.)
        assert!((SCAN_MARK.design - SCAN_INSET - SCAN_BRACKET - 13.0).abs() < EPSILON);
        assert!((SCAN_MARK.design - SCAN_INSET - SCAN_STROKE - 17.5).abs() < EPSILON);
    }

    #[test]
    fn the_fab_clears_the_minimum_tap_target() {
        // The mock's 46px disc is also the platform floor (44px) with room to
        // spare; the mark inside it is decoration and has no floor of its own.
        const MIN_TAP_TARGET: f64 = 44.0;
        const { assert!(FAB_BOX >= MIN_TAP_TARGET) };
        const { assert!(FAB_MARK < FAB_BOX) };
    }
}
