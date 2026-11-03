//! Piece 7 — the CTA row: the connect button (primary until servers are
//! saved, when the unlock dock takes that role and it steps down to
//! secondary) with the QR square beside it, and the pinned pairing CTA above.

use std::rc::Rc;

use frust::{Alignment, ButtonStyle, Either, SizedBox, Theme, View, button, flexible, inflexible};

use super::pairing::{pairing_banner, pairing_chrome};
use super::qr_affordance::qr_square;
use super::{Live, block_column, centered_row, h_gap, v_gap};
use crate::features::connect::domain::entities::SavedServer;
use crate::ui::palette::Palette;
use crate::ui::spacing;
use crate::ui::splash::spec::CTA_ROW_GAP;

/// The CTA's label. `pub` because `tests/connect.rs` locates the CTA by its
/// accessibility label.
pub const CONNECT_LABEL: &str = "→ connect";

/// The platform minimum tap target: `ButtonWidget`'s own vertical padding
/// lands short of it at this label's size, so a [`SizedBox`] forces the floor.
const CTA_HEIGHT: f64 = 44.0;

/// How loudly the CTA speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prominence {
    Primary,
    /// Saved servers exist, so the unlock dock is the primary action.
    Secondary,
}

impl Prominence {
    pub fn for_saved(saved: &[SavedServer]) -> Self {
        if saved.is_empty() {
            Prominence::Primary
        } else {
            Prominence::Secondary
        }
    }

    fn style(self) -> ButtonStyle {
        match self {
            Prominence::Primary => ButtonStyle::Primary,
            Prominence::Secondary => ButtonStyle::Secondary,
        }
    }
}

/// Piece 7 on Compact: `[cta flex][gap][QR square]`, firing
/// [`Live::on_connect`] and [`Live::on_scan`], plus the pairing pair above
/// while a scanned pairing is stashed. `.label_alignment(Alignment::CENTER)`
/// centers the label on both axes (the stretch default centers only
/// horizontally).
///
/// Known gap: `ButtonView` has no per-instance color/text-style override, so
/// the label shapes at `TextStyle::default()` rather than the mock's role.
///
/// The pairing rows live here rather than as a ninth piece because the
/// stagger table has exactly eight slots; the splash never sees them.
pub fn connect_button<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
    prominence: Prominence,
) -> impl View<S> {
    let on_scan = live.map(|live| Rc::clone(&live.on_scan));
    let square = qr_square::<S>(colors, theme, on_scan);
    connect_block(theme, colors, live, prominence, square, CTA_ROW_GAP)
}

/// The Tablet form's action block: the same pairing rows over a button row,
/// the CTA taking the free width beside `secondary` at the CTA's own height.
pub fn connect_actions<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
    prominence: Prominence,
    secondary: impl View<S>,
) -> impl View<S> {
    connect_block(theme, colors, live, prominence, secondary, spacing::MD)
}

fn connect_block<S: 'static, V: View<S>>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
    prominence: Prominence,
    secondary: V,
    gap: f64,
) -> impl View<S> {
    // Shared by every CTA this piece renders; the splash reads `false`.
    let busy = live.is_some_and(|live| live.controller.busy());
    let on_connect = live.map(|live| Rc::clone(&live.on_connect));
    let cta = SizedBox::<S>(None, Some(CTA_HEIGHT)).child(
        button(CONNECT_LABEL, move |_state: &mut S| {
            if let Some(on_connect) = &on_connect {
                on_connect();
            }
        })
        .style(prominence.style())
        .label_alignment(Alignment::CENTER)
        // `busy` spans the offer's held redirect too, since the save chain runs
        // with the form still looking live and this CTA the only guard against a re-tap.
        .loading(busy),
    );
    let cta = centered_row::<S>(vec![
        flexible(1, cta),
        inflexible(h_gap::<S>(gap)),
        inflexible(SizedBox::<S>(None, Some(CTA_HEIGHT)).child(secondary)),
    ]);
    let Some(pairing) = pairing_chrome(live) else {
        return Either::Left(cta);
    };
    let on_connect_pinned = live.map(|live| Rc::clone(&live.on_connect_pinned));
    Either::Right(block_column::<S, _>((
        pairing_banner(theme, colors, pairing),
        v_gap::<S>(spacing::MD),
        SizedBox::<S>(None, Some(CTA_HEIGHT)).child(
            button(pairing.connect_label(), move |_state: &mut S| {
                if let Some(on_connect_pinned) = &on_connect_pinned {
                    on_connect_pinned();
                }
            })
            .label_alignment(Alignment::CENTER)
            // Same connection-controller signal the manual CTA reads.
            .loading(busy),
        ),
        v_gap::<S>(spacing::MD),
        cta,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cta_clears_the_minimum_tap_target() {
        const { assert!(CTA_HEIGHT >= 44.0) };
    }

    #[test]
    fn saved_servers_demote_the_cta() {
        assert_eq!(Prominence::for_saved(&[]), Prominence::Primary);
        let row = SavedServer {
            host: "rig".to_string(),
            port: 1,
            trust_mode: crate::connection::TrustMode::System,
            label: "rig".to_string(),
            last_used_ms: 0,
            read_only: false,
            has_credential: false,
            token_name: None,
            fingerprint: None,
        };
        assert_eq!(Prominence::for_saved(&[row]), Prominence::Secondary);
    }
}
