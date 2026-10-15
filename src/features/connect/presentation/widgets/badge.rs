//! Pieces 1 and 2 — `.connect-badge` and `.heading`.

use frust::authoring::text::TextAlign;
use frust::{Align, Alignment, AnyView, EdgeInsets, Padding, Theme, View, any, container, text};

use super::{backed, block_column, hugging_column, v_gap};
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

/// `.connect .sub` — the heading's explanatory paragraph. `pub` because
/// `tests/connect.rs`'s line-centering suite reads the very lines this string
/// wraps into; a copy there could drift.
pub const SUBTITLE: &str = "Enter credentials to attach to a remote zellij or herdr session.";

/// Piece 1 — a centered, bordered two-line box naming the daemon and the
/// multiplexers it fronts. Pure decoration — no [`Live`](super::Live).
pub fn server_badge<S: 'static>(theme: &Theme, colors: &Palette) -> impl View<S> {
    // Hugging, not stretched: the lines center against each other, not the layer.
    let lines = hugging_column::<S, _>((
        text("muxrd server").style(inked(&theme.type_scale.body_small, colors.fg_muted)),
        v_gap::<S>(spacing::MICRO),
        text("zellij · herdr").style(inked(&theme.type_scale.label_small, colors.fg_dim)),
    ));
    let box_ = container(Padding(
        EdgeInsets::symmetric(spacing::BASE, spacing::SM),
        lines,
    ))
    .fill(colors.page)
    .radius(theme.shape.small)
    .border(colors.outline, HAIRLINE);
    Align(Alignment::CENTER, box_)
}

/// Piece 2 — the screen's title and its explanation, both centered. Pure
/// copy — no [`Live`](super::Live).
///
/// The title centers as a block ([`Align`]); the subtitle wraps on a phone
/// and centers per line via `TextView`'s own `.align(TextAlign::Center)`, now
/// that `text` carries a paragraph alignment.
pub fn heading<S: 'static>(theme: &Theme, colors: &Palette) -> AnyView<S> // erasure: keep accumulator into Vec<AnyView>
{
    any(backed(
        block_column::<S, _>((
            Align(
                Alignment::CENTER,
                text("Connect to server").style(inked(&theme.type_scale.title_medium, colors.fg)),
            ),
            v_gap::<S>(spacing::XS),
            text(SUBTITLE)
                .style(inked(&theme.type_scale.body_small, colors.fg_muted))
                .align(TextAlign::Center),
        )),
        colors,
    ))
}
