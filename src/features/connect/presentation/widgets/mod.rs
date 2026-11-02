//! The Connect screen's widgets: the eight staggered pieces, plus the screen's
//! own QR, credential, free-tier-gate and getting-started chrome.
//!
//! [`connect_pieces_with`] returns the eight pieces in stagger order:
//! [`mark::connect_mark`], [`badge::server_badge`], [`badge::heading`],
//! [`fields::server_url_field`], [`fields::token_field`],
//! [`trust::remember_row`], [`cta::connect_button`] (the CTA row, with the
//! [`qr_affordance`] square) and [`saved::saved_address`] (the rows, closed by
//! the [`keyring_dock`]). [`qr_sheet`], [`credential_dialogs`], [`pro_gate`]
//! and [`getting_started`] are screen chrome, never staggered pieces.
//!
//! # The parity contract
//!
//! `Option<&`[`Live`]`>` is a wiring difference and nothing else: `Some(..)`
//! turns a piece into a control, `None` (the splash, assembled before any
//! controller exists) paints the same pixels with no callbacks. Three
//! guarantees hold that true:
//!
//! 1. Piece 8's rows are an input — the screen passes its controller's
//!    signal, the splash [`saved::splash_saved_servers`]' snapshot of the
//!    same repository; piece 7's prominence and the dock's label follow them.
//! 2. The splash's fields are `read_only`, never `enabled(false)`: the same
//!    focus gate without the dimming.
//! 3. The layout is one function per size class, which both callers
//!    assemble through: Compact's [`connect_layout`](layout::connect_layout)
//!    owns the responsive clamp, and the Tablet structure
//!    ([`tablet_panel_pieces`], [`tablet_form_pieces`]) goes through
//!    `tablet::tablet_layout` for the screen and the splash alike
//!    ([`tablet::tablet_splash_pieces`], [`tablet::tablet_splash_layout`]);
//!    the page's `splash_assembly` reads the class once per build, as the
//!    screen does, to pick the pair.
//!
//! Accepted differences: the saved rows' pro tag paints only under `Some(..)`,
//! since the splash has no entitlement to read, and the splash's dock is
//! always idle (it probes no biometrics and has no unlock in flight).
//!
//! Every piece is `<State>`-generic (the splash composes under
//! [`crate::AppState`], the screen under its own `ConnectState`), reads no
//! state it is handed, and takes the [`Palette`] resolved once per build.

pub mod badge;
pub mod credential_dialogs;
pub mod cta;
pub mod fields;
pub mod getting_started;
pub mod keyring_dock;
pub mod layout;
pub mod mark;
pub mod pairing;
pub mod pro_gate;
pub mod qr_affordance;
pub mod qr_sheet;
pub mod saved;
pub mod tablet;
pub mod trust;

use std::rc::Rc;
use std::sync::Arc;

use frust::{
    AnyView, Axis, CrossAxisAlignment, FlexView, SizedBox, Theme, View, ViewSeq, any, column,
    container, inflexible,
};

use self::cta::Prominence;
use super::controller::ConnectController;
use crate::features::connect::domain::entities::SavedServer;
use crate::ui::palette::Palette;
use crate::ui::spacing;

/// What a piece needs to be a control rather than a picture: the screen's
/// hosted controller, plus the actions it cannot express itself.
///
/// Built fresh on every `ConnectScreen::build` and passed down by reference.
/// Actions are `Rc<dyn Fn(..)>` — they navigate or raise an overlay, so
/// cannot live on the `Send + Sync` controller; the screen owns and closes
/// over them here.
pub struct Live {
    pub controller: Arc<ConnectController>,
    /// Validates, connects, and navigates — raising the save-credential offer
    /// first where there is one.
    pub on_connect: Rc<dyn Fn()>,
    /// Connects with the scanned pairing's own credentials. Reached only from
    /// the pinned CTA, which renders only while one is stashed.
    pub on_connect_pinned: Rc<dyn Fn()>,
    /// A saved row tapped, by index — refills the form, or unlocks its stored
    /// credential and connects. Wired only while the dock is unavailable.
    pub on_saved_tap: Rc<dyn Fn(usize)>,
    /// A saved row's delete pressed, by index: confirm, then drop this
    /// device's copy (nothing is deleted on the server).
    pub on_saved_delete: Rc<dyn Fn(usize)>,
    /// The dock: the verdict, one keyring prompt, then the most recent row.
    pub on_unlock: Rc<dyn Fn()>,
    /// The failed dock's reset: confirm, then drop every saved credential.
    pub on_reset_credentials: Rc<dyn Fn()>,
    /// The QR square: the free-tier gate, then the camera permission.
    pub on_scan: Rc<dyn Fn()>,
}

/// A `gap`-tall spacer between stacked pieces/rows.
fn v_gap<S: 'static>(gap: f64) -> impl View<S> {
    SizedBox::<S>(None, Some(gap))
}

/// A `gap`-wide spacer between row items.
fn h_gap<S: 'static>(gap: f64) -> impl View<S> {
    SizedBox::<S>(Some(gap), None)
}

/// A vertical stack whose children each fill the available width — the CSS
/// block-flow the mock's blocks assume.
fn block_column<S: 'static, M>(children: impl ViewSeq<S, M>) -> FlexView<S> {
    column()
        .children(children)
        .cross_axis(CrossAxisAlignment::Stretch)
}

/// A vertical stack that shrink-wraps to its widest child and centers each
/// one, rather than spanning the layer (stretching would turn the mark's
/// tile into a full-width bar).
fn hugging_column<S: 'static, M>(children: impl ViewSeq<S, M>) -> FlexView<S> {
    column()
        .children(children)
        .cross_axis(CrossAxisAlignment::Center)
}

/// A horizontal row with its items vertically centered.
fn centered_row<S: 'static>(children: Vec<frust::FlexChild<S>>) -> FlexView<S> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

/// A full-width piece backing in the page tone — invisible over the route's
/// backdrop, while giving the splash something opaque behind its assembling text.
fn backed<S: 'static, V: View<S>>(child: V, colors: &Palette) -> impl View<S> + use<S, V> {
    container(child).fill(colors.page)
}

/// `live` is `Some(..)` for the screen and `None` for the splash (the parity
/// contract); `saved` is piece 7's rows.
pub fn connect_pieces_with<S: 'static>(
    theme: &Theme,
    live: Option<&Live>,
    saved: &[SavedServer],
) -> Vec<AnyView<S>> // erasure: keep accumulator into Vec<AnyView>
{
    let colors = Palette::from_theme(theme);
    vec![
        any(mark::connect_mark(theme, &colors)),
        any(badge::server_badge(theme, &colors)),
        badge::heading(theme, &colors),
        any(fields::server_url_field(theme, &colors, live)),
        any(fields::token_field(theme, &colors, live)),
        any(trust::remember_row(theme, &colors, live)),
        any(cta::connect_button(
            theme,
            &colors,
            live,
            Prominence::for_saved(saved),
        )),
        any(saved::saved_address(theme, &colors, saved, live)),
    ]
}

/// The Tablet left panel, top to bottom: brand row, headline, the inline
/// getting-started card, and the saved list. `live` is `None` for the splash
/// (the parity contract).
pub fn tablet_panel_pieces<S: 'static>(
    theme: &Theme,
    live: Option<&Live>,
    saved: &[SavedServer],
) -> Vec<AnyView<S>> // erasure: keep accumulator into Vec<AnyView>
{
    let colors = Palette::from_theme(theme);
    vec![
        any(centered_row::<S>(vec![
            inflexible(mark::connect_mark(theme, &colors)),
            inflexible(h_gap::<S>(spacing::LG)),
            inflexible(badge::server_badge(theme, &colors)),
        ])),
        any(getting_started::tablet_headline(theme, &colors)),
        any(getting_started::getting_started_card(theme, &colors)),
        any(saved::tablet_saved_list(theme, &colors, saved, live)),
    ]
}

/// The Tablet form column: the phone's form pieces, then the action block
/// with `secondary` beside the CTA, then — when `folded` carries the list
/// (the collapsed band) — the saved list under it. `live` is `None` for the
/// splash (the parity contract).
pub fn tablet_form_pieces<S: 'static>(
    theme: &Theme,
    live: Option<&Live>,
    prominence: Prominence,
    folded: Option<&[SavedServer]>,
    secondary: impl View<S>,
) -> Vec<AnyView<S>> // erasure: keep accumulator into Vec<AnyView>
{
    let colors = Palette::from_theme(theme);
    let mut pieces = vec![
        badge::heading(theme, &colors),
        any(fields::server_url_field(theme, &colors, live)),
        any(fields::token_field(theme, &colors, live)),
        any(trust::remember_row(theme, &colors, live)),
        any(cta::connect_actions(
            theme, &colors, live, prominence, secondary,
        )),
    ];
    if let Some(saved) = folded {
        pieces.push(any(saved::tablet_saved_list(theme, &colors, saved, live)));
    }
    pieces
}

/// The eight pieces with no live wiring — the shared splash/route seam.
pub fn connect_pieces<S: 'static>(theme: &Theme, saved: &[SavedServer]) -> Vec<AnyView<S>> {
    connect_pieces_with(theme, None, saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use crate::ui::splash::spec;

    #[test]
    fn there_is_one_piece_per_source_stagger() {
        let theme = frust_glyph::baseline();
        assert_eq!(
            connect_pieces::<AppState>(&theme, &[]).len(),
            spec::PIECE_DELAYS_MS.len(),
            "the piece list and the `data-delay` table are the same eight \
             elements, in the same order"
        );
    }
}
