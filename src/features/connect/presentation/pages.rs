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
//! Transcribed from `docs/splash.html`'s `.ui-layer` rule: a top-anchored
//! column, `padding:30px 22px`, `gap:20px`, children stretched to the layer
//! width. The page paints its own `--bg-void` background — the shell clears
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
//! The screen builds its own [`Router`] over `crate::routes::build_routes` —
//! the app's real route table — driving the very same [`NavigatorController`]
//! the app's router drives, and pushes the location through it. Two things force
//! this shape:
//!
//! - The composition root (`src/lib.rs`) publishes its `Router` nowhere; a route
//!   builder receives only the [`NavigatorController`], which has no
//!   location-based API. A `Router` is a *stateless resolver* over a route table
//!   (every navigation-stack fact lives on the controller), so a second one over
//!   the same controller and the same table navigates identically.
//! - Resolving a sibling feature's page builder directly would import
//!   `features::sessions::presentation`, which the cross-feature rule forbids.
//!
//! **The session guard is deliberately not re-attached here.**
//! `crate::routes::session_guard` reads the app-scoped connection controller out
//! of reactive *context*, and a press handler runs outside any ambient reactive
//! owner (`frust-shell-desktop` dispatches events outside
//! `ReactiveRuntime::with_owner`), so consulting it from here would panic on
//! desktop. It also has nothing to decide: this push happens in the same call
//! that flipped `connected` to `true`. The app's own router keeps the guard for
//! every other way into `/sessions`.
//!
//! Publishing the composition root's `Router` through context (from `src/lib.rs`)
//! would delete this whole seam; that file is not this task's to edit.

use std::rc::Rc;
use std::sync::Arc;

use clean_signals_frust::use_controller;
use frust::authoring::Color;
use frust::authoring::text::TextStyle;
use frust::{
    Align, Alignment, AnyView, Axis, Component, CrossAxisAlignment, EdgeInsets, FlexView, Padding,
    Router, SizedBox, Stack, any, button, component, inflexible, safe_area, scroll_view, text,
    use_context,
};

use super::chrome::panel;
use super::controller::ConnectController;
use super::pieces::{Live, active_theme, connect_pieces_with};
use crate::AppState;
use crate::connection::expect_connection_controller;
use crate::failure::AppFailure;
use crate::features::connect::di::SavedServers;
use crate::ui::backdrop::page_backdrop;
use crate::ui::sheet::show_sheet;

/// `.ui-layer{padding:30px 22px}` — horizontal.
const LAYER_PAD_X: f64 = 22.0;
/// `.ui-layer{padding:30px 22px}` — vertical.
const LAYER_PAD_Y: f64 = 30.0;
/// `.ui-layer{gap:20px}` — between consecutive pieces.
const LAYER_GAP: f64 = 20.0;
/// `.fab{right:20px;bottom:20px}` — the QR affordance's inset from the screen's
/// safe area.
const FAB_MARGIN: f64 = 20.0;
/// `.fab` renders `▣` — the mock's QR/pair glyph, reused as the button's label.
const FAB_GLYPH: &str = "▣";
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
/// half — two navigation handles and the controller handle. Every observed
/// value (the form's fields, the saved list, the validation flag) lives in
/// [`ConnectController`]'s signals instead, so a write wakes a rebuild.
pub struct ConnectState {
    /// The hosted view model, disposed with this component (`use_controller`).
    controller: Arc<ConnectController>,
    /// The app's route table over the live navigator — see the module docs'
    /// navigation note. `Rc` because a press closure captures it.
    router: Rc<Router<AppState>>,
    /// The raw navigator, for `crate::ui::sheet::show_sheet` (which pushes a
    /// transparent page rather than resolving a location).
    nav: frust::NavigatorController<AppState>,
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
        let controller = use_controller::<ConnectController, AppFailure>(move || {
            ConnectController::new(repository, connection)
        });

        ConnectState {
            controller,
            router: Rc::new(Router::with_controller(
                &self.nav,
                crate::routes::build_routes(self.nav.clone()),
            )),
            nav: self.nav.clone(),
        }
    }

    fn build(&self, state: &mut ConnectState) -> AnyView<ConnectState> {
        let theme = active_theme();
        let live = Live {
            controller: Arc::clone(&state.controller),
            on_connect: {
                let controller = Arc::clone(&state.controller);
                let router = Rc::clone(&state.router);
                Rc::new(move || controller.connect(&router))
            },
        };
        any(Stack(vec![
            any(page_backdrop::<ConnectState>()),
            any(scroll_view(connect_layout(connect_pieces_with(
                &theme,
                Some(&live),
            )))),
            qr_affordance(&state.nav),
        ]))
    }
}

/// `.fab`: the amber QR/pair affordance, pinned to the screen's bottom-trailing
/// safe area.
///
/// A real [`button`] rather than hand-painted chrome, so the press feedback and
/// the accessibility role come from the framework; its single-glyph label makes
/// `ButtonWidget`'s leading-pinned label (FINDINGS #15) invisible here.
fn qr_affordance(nav: &frust::NavigatorController<AppState>) -> AnyView<ConnectState> {
    let nav = nav.clone();
    any(safe_area(Align(
        Alignment::BOTTOM_RIGHT,
        Padding(
            EdgeInsets::all(FAB_MARGIN),
            button(FAB_GLYPH, move |_state: &mut ConnectState| {
                show_sheet(&nav, qr_sheet_body)
            }),
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
            text(FAB_GLYPH).style(inked(&theme.type_scale.headline_medium, scheme.primary)),
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
