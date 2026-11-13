//! What a scanned QR contributes to the form while it waits for an explicit connect.

use frust::{Theme, View, With, flexible, inflexible, text};
use frust_glyph::{BadgeVariant, badge};

use super::{Live, centered_row, h_gap};
use crate::features::connect::domain::entities::PairingTrust;
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

/// A scan only prefills; only this connects. `pub` for `tests/connect.rs`.
pub const PINNED_CONNECT_LABEL: &str = "Connect now (pinned)";
pub const PAIRED_CONNECT_LABEL: &str = "Connect now (paired)";
/// `pub`: `tests/connect.rs` reads this exact run.
pub const PAIRING_CAPTION: &str = "scanned pairing — the code's own token and trust";
pub const READ_ONLY_CHIP: &str = "read-only";

/// Not the payload itself: a piece rebuilds at signal rate, so the token is not copied out.
#[derive(Clone, Copy)]
pub(super) struct PairingChrome {
    read_only: bool,
    pinned: bool,
}

impl PairingChrome {
    pub(super) fn connect_label(self) -> &'static str {
        if self.pinned {
            PINNED_CONNECT_LABEL
        } else {
            PAIRED_CONNECT_LABEL
        }
    }
}

pub(super) fn pairing_chrome(live: Option<&Live>) -> Option<PairingChrome> {
    live?.controller.pending_pairing.with(|pending| {
        pending.as_ref().map(|payload| PairingChrome {
            read_only: payload.read_only,
            pinned: matches!(payload.trust, PairingTrust::Pin { .. }),
        })
    })
}

pub(super) fn pairing_banner<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    pairing: PairingChrome,
) -> impl View<S> {
    let mut items = vec![flexible(
        1,
        text(PAIRING_CAPTION).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
    )];
    if pairing.read_only {
        items.push(inflexible(h_gap::<S>(spacing::MD)));
        items.push(inflexible(badge(READ_ONLY_CHIP, BadgeVariant::Neutral)));
    }
    centered_row::<S>(items)
}
