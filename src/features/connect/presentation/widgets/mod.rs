//! The Connect screen's widgets: the eight staggered pieces, plus the screen's
//! own QR and credential chrome.
//!
//! [`connect_pieces_with`] returns the eight `.piece` elements of
//! `docs/splash.html`'s `.ui-layer` in source order — the order
//! `crate::ui::splash::spec::PIECE_DELAYS_MS` staggers them by:
//! [`mark::connect_mark`], [`badge::server_badge`], [`badge::heading`],
//! [`fields::server_url_field`], [`fields::token_field`],
//! [`trust::remember_row`], [`cta::connect_button`], [`saved::saved_address`].
//! [`qr_affordance`], [`qr_sheet`] and [`credential_dialogs`] are screen chrome,
//! never staggered pieces.
//!
//! # The parity contract
//!
//! `Option<&`[`Live`]`>` is a **wiring** difference and nothing else: `Some(..)`
//! turns a piece into a control, `None` (the splash, which assembles this screen
//! before any controller exists) paints the same pixels with no callbacks, at every
//! window size. Three structural guarantees hold that true:
//!
//! 1. Piece 7's rows are an input, so neither caller can invent a row the other
//!    does not have — the screen passes its controller's signal, the splash
//!    [`saved::splash_saved_servers`]' snapshot of the same repository.
//! 2. The splash's fields are `read_only`, never `enabled(false)`: the same focus
//!    gate without the dimming that used to pop at the handoff.
//! 3. The responsive clamp lives in
//!    [`connect_layout`](super::pages::connect_layout), the one function both
//!    callers assemble through.
//!
//! Every piece is `<State>`-generic (the splash composes under [`crate::AppState`],
//! the screen under its own `ConnectState`), reads no state it is handed, and takes
//! the [`Palette`] resolved once per build rather than resolving its own.
//!
//! Tokens: the Glyph theme's roles are canonical where they and the mock's CSS
//! disagree, and `docs/redesign.html` wins over the older `docs/splash.html`. Both
//! fields and the trust rows render at `body_medium` (13) rather than the source's
//! 12px, and the CTA at `label_large_emphasized`.

pub mod badge;
pub mod credential_dialogs;
pub mod cta;
pub mod fields;
pub mod mark;
pub mod pairing;
pub mod qr_affordance;
pub mod qr_sheet;
pub mod saved;
pub mod trust;

use std::rc::Rc;
use std::sync::Arc;

use frust::{AnyView, Axis, CrossAxisAlignment, FlexView, SizedBox, Theme, any, inflexible};

use super::chrome::panel;
use super::controller::ConnectController;
use crate::features::connect::domain::entities::SavedServer;
use crate::ui::palette::Palette;

/// What a piece needs to be a control rather than a picture: the screen's hosted
/// controller, plus the four actions it cannot express itself.
///
/// Built fresh on every `ConnectScreen::build` and passed down by reference. Each
/// action is a state-free `Rc<dyn Fn(..)>` because it navigates or raises an
/// overlay, and both handles are `Rc`-backed — they cannot live on the
/// `Send + Sync` controller, so the screen owns them and closes over them here.
pub struct Live {
    /// The screen's form/view model.
    pub controller: Arc<ConnectController>,
    /// Validate, connect, and navigate — raising the save-credential offer first
    /// where there is one, since that answer is what the held redirect resolves on.
    pub on_connect: Rc<dyn Fn()>,
    /// Connect with the **scanned pairing's** own credentials, and navigate.
    /// Reached only from the pinned CTA, which renders only while one is stashed.
    pub on_connect_pinned: Rc<dyn Fn()>,
    /// A saved row was tapped, by index — refill the form, or unlock its stored
    /// credential and connect.
    pub on_saved_tap: Rc<dyn Fn(usize)>,
    /// A saved row's delete affordance was pressed, by index: confirm, then drop
    /// **this device's** copy (nothing is deleted on the server).
    pub on_saved_delete: Rc<dyn Fn(usize)>,
}

/// A `gap`-tall spacer between stacked pieces/rows.
fn v_gap<S: 'static>(gap: f64) -> AnyView<S> {
    any(SizedBox::<S>(None, Some(gap)))
}

/// A `gap`-wide spacer between row items.
fn h_gap<S: 'static>(gap: f64) -> AnyView<S> {
    any(SizedBox::<S>(Some(gap), None))
}

/// A vertical stack whose children each fill the available width — the CSS
/// block-flow the mock's blocks assume.
fn column<S: 'static>(children: Vec<AnyView<S>>) -> FlexView<S> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Stretch)
}

/// A vertical stack that **shrink-wraps** to its widest child and centers each one
/// — the `margin:0 auto` blocks, whose boxes must hug their content rather than
/// span the layer (stretching one turns the mark's tile into a full-width bar).
fn hugging_column<S: 'static>(children: Vec<AnyView<S>>) -> FlexView<S> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Center)
}

/// A horizontal row with its items vertically centered.
fn row<S: 'static>(children: Vec<frust::FlexChild<S>>) -> FlexView<S> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

/// A full-width piece backing in the page tone — the same role
/// `crate::ui::backdrop::page_backdrop` paints, so it is invisible over the
/// route's backdrop while still giving the splash something opaque behind its
/// assembling text.
fn backed<S: 'static>(child: impl frust::View<S>, colors: &Palette) -> AnyView<S> {
    any(panel(child, colors.page, 0.0))
}

/// The eight pieces, in `docs/splash.html`'s source order. `live` is `Some(..)`
/// for the screen and `None` for the splash (the parity contract); `saved` is
/// piece 7's rows, which both callers read from the same repository.
pub fn connect_pieces_with<S: 'static>(
    theme: &Theme,
    live: Option<&Live>,
    saved: &[SavedServer],
) -> Vec<AnyView<S>> {
    let colors = Palette::from_theme(theme);
    vec![
        mark::connect_mark(theme, &colors),
        badge::server_badge(theme, &colors),
        badge::heading(theme, &colors),
        fields::server_url_field(theme, &colors, live),
        fields::token_field(theme, &colors, live),
        trust::remember_row(theme, &colors, live),
        cta::connect_button(theme, &colors, live),
        saved::saved_address(theme, &colors, saved, live),
    ]
}

/// The eight pieces with **no** live wiring — the shared splash/route seam
/// (`crate::splash_overlay` and `tests/sequence.rs` both call exactly this).
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
        let theme = Theme::glyph_baseline();
        assert_eq!(
            connect_pieces::<AppState>(&theme, &[]).len(),
            spec::PIECE_DELAYS_MS.len(),
            "the piece list and the `data-delay` table are the same eight \
             elements, in the same order"
        );
    }
}
