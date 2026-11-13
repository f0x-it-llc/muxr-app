//! The Connect screen's eight staggered pieces and its chrome. Under `None` for [`Live`]
//! (the splash) a piece paints the same pixels with no callbacks; splash fields are
//! `read_only`, never `enabled(false)`, so they keep the focus gate without the dimming.

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

/// Actions are `Rc<dyn Fn(..)>`: they navigate or raise an overlay, so cannot live on the
/// `Send + Sync` controller.
pub struct Live {
    pub controller: Arc<ConnectController>,
    pub on_connect: Rc<dyn Fn()>,
    /// Reached only from the pinned CTA, which renders only while a pairing is stashed.
    pub on_connect_pinned: Rc<dyn Fn()>,
    /// Wired only while the dock is unavailable.
    pub on_saved_tap: Rc<dyn Fn(usize)>,
    /// Drops this device's copy only; nothing is deleted on the server.
    pub on_saved_delete: Rc<dyn Fn(usize)>,
    pub on_unlock: Rc<dyn Fn()>,
    pub on_reset_credentials: Rc<dyn Fn()>,
    pub on_scan: Rc<dyn Fn()>,
}

fn v_gap<S: 'static>(gap: f64) -> impl View<S> {
    SizedBox::<S>(None, Some(gap))
}

fn h_gap<S: 'static>(gap: f64) -> impl View<S> {
    SizedBox::<S>(Some(gap), None)
}

/// The CSS block flow the mock's blocks assume.
fn block_column<S: 'static, M>(children: impl ViewSeq<S, M>) -> FlexView<S> {
    column()
        .children(children)
        .cross_axis(CrossAxisAlignment::Stretch)
}

/// Shrink-wraps: stretching would turn the mark's tile into a full-width bar.
fn hugging_column<S: 'static, M>(children: impl ViewSeq<S, M>) -> FlexView<S> {
    column()
        .children(children)
        .cross_axis(CrossAxisAlignment::Center)
}

fn centered_row<S: 'static>(children: Vec<frust::FlexChild<S>>) -> FlexView<S> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

/// Invisible over the backdrop, but opaque behind the splash's assembling text.
fn backed<S: 'static, V: View<S>>(child: V, colors: &Palette) -> impl View<S> + use<S, V> {
    container(child).fill(colors.page)
}

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
            inflexible(badge::server_badge(theme, &colors)),
        ])
        .gap(spacing::LG)),
        any(getting_started::tablet_headline(theme, &colors)),
        any(getting_started::getting_started_card(theme, &colors)),
        any(saved::tablet_saved_list(theme, &colors, saved, live)),
    ]
}

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
