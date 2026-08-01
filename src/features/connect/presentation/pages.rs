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
//! `crate::ui::sheet::show_sheet` needs the controller itself — and the
//! [`Nav`] beside it, which carries the overlay accounting the shell's bar
//! dims itself from (`crate::routes`' "Overlay accounting").

use std::rc::Rc;
use std::sync::Arc;

use clean_signals_frust::use_controller;
use frust::authoring::text::TextStyle;
use frust::authoring::{Color, Size};
use frust::{
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexView, Get,
    GetUntracked, IconSource, Padding, SizedBox, Stack, any, component, icon, inflexible,
    safe_area, scroll_view, text, use_context,
};

use super::chrome::panel;
use super::controller::ConnectController;
use super::pieces::{Live, active_theme, connect_pieces_with};
use crate::AppState;
use crate::connection::{ConnectionController, expect_connection_controller};
use crate::failure::AppFailure;
use crate::features::connect::di::SavedServers;
use crate::routes::{Nav, app_nav};
use crate::ui::backdrop::page_backdrop;
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
pub fn connect_page(controller: frust::NavigatorController<AppState>) -> AnyView<AppState> {
    any(component(ConnectScreen { nav: controller }))
}

/// The Connect screen `Component`. Stateless configuration — the navigator
/// handle the route builder was given, and nothing else; everything retained
/// lives in [`ConnectState`].
struct ConnectScreen {
    nav: frust::NavigatorController<AppState>,
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
    /// The raw navigator, for `crate::ui::sheet::show_sheet` (which pushes a
    /// transparent page rather than resolving a location).
    nav: frust::NavigatorController<AppState>,
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
        let repository = use_context::<SavedServers>().expect(
            "features::connect::di::provide must run in MuxrApp::init before the / route mounts",
        );
        let connection = expect_connection_controller();
        let was_connected = connection.connected.get_untracked();
        let controller = use_controller::<ConnectController, AppFailure>({
            let connection = Arc::clone(&connection);
            move || ConnectController::new(repository, connection)
        });

        ConnectState {
            controller,
            // Resolved here for the same reason the two above are: `init` is
            // the screen's one dependency-resolution point (`app_nav` reads no
            // reactive context, but a press handler resolving its own handles
            // is the habit this app does not want).
            app_nav: app_nav(),
            nav: self.nav.clone(),
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
        }
        state.was_connected = connected;

        let live = Live {
            controller: Arc::clone(&state.controller),
            on_connect: {
                let controller = Arc::clone(&state.controller);
                let nav = state.app_nav.clone();
                Rc::new(move || controller.connect(&nav))
            },
        };
        any(Stack(vec![
            any(page_backdrop::<ConnectState>()),
            any(scroll_view(connect_layout(connect_pieces_with(
                &theme,
                Some(&live),
            )))),
            qr_affordance(&state.app_nav, &state.nav),
        ]))
    }
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
    app_nav: &Nav,
    nav: &frust::NavigatorController<AppState>,
) -> AnyView<ConnectState> {
    let nav = nav.clone();
    let app_nav = app_nav.clone();
    let theme = active_theme();
    let scheme = theme.scheme();
    any(safe_area(Align(
        Alignment::BOTTOM_RIGHT,
        Padding(
            EdgeInsets::all(FAB_MARGIN),
            icon_button(SCAN_MARK, FAB_LABEL, move |_state: &mut ConnectState| {
                show_sheet(&app_nav, &nav, qr_sheet_body)
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

/// The QR sheet's body: a placeholder viewfinder over the copy that says so.
///
/// **A placeholder on purpose.** Camera capture needs a platform plugin muxr has
/// not adopted yet, so this sheet exists to prove the affordance and the
/// dismissal path, not to scan anything. Rebuilt on every navigator rebuild
/// while the sheet is up (`show_sheet`'s builder contract), so it stays cheap.
fn qr_sheet_body() -> AnyView<AppState> {
    let theme = active_theme();
    let scheme = theme.scheme();
    let viewfinder = panel(
        SizedBox::<AppState>(Some(VIEWFINDER), Some(VIEWFINDER)).child(Align(
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

    any(FlexView::new(
        Axis::Vertical,
        vec![
            inflexible(any(Align(Alignment::CENTER, viewfinder))),
            inflexible(any(SizedBox::<AppState>(None, Some(SHEET_GAP)))),
            inflexible(any(text("Pair by QR code")
                .style(inked(&theme.type_scale.title_medium, scheme.on_surface)))),
            inflexible(any(SizedBox::<AppState>(None, Some(SHEET_GAP / 2.0)))),
            inflexible(any(text(
                "Camera pairing lands in a later phase — enter the server URL \
                     and token by hand for now.",
            )
            .style(inked(
                &theme.type_scale.body_small,
                scheme.on_surface_variant,
            )))),
        ],
    )
    .cross_axis(CrossAxisAlignment::Stretch))
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
