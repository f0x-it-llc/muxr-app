//! The stashed-pairing chrome piece 6's CTA and the trust selector share: what
//! a scanned QR contributes to the form while it waits for an explicit connect.

use frust::{Theme, View, With, flexible, inflexible, text};
use frust_glyph::{BadgeVariant, badge};

use super::{Live, centered_row, h_gap};
use crate::features::connect::domain::entities::PairingTrust;
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

/// The explicit action a scanned pairing waits for (a scan prefills; only
/// this connects). `pub` for `tests/connect.rs`.
pub const PINNED_CONNECT_LABEL: &str = "Connect now (pinned)";
/// The same CTA for a `tm=ca` pairing, which carries no pin to name.
pub const PAIRED_CONNECT_LABEL: &str = "Connect now (paired)";
/// The caption over the pinned CTA. `pub` — `tests/connect.rs` reads this exact run.
pub const PAIRING_CAPTION: &str = "scanned pairing — the code's own token and trust";
/// The chip a `ro=1` pairing (or a stored read-only credential) carries — one
/// spelling for both.
pub const READ_ONLY_CHIP: &str = "read-only";

/// What a stashed pairing contributes to the form's chrome — not the payload
/// itself, since a piece rebuilds at signal rate and copying the scanned
/// token out to read two booleans is a copy nobody needs.
#[derive(Clone, Copy)]
pub(super) struct PairingChrome {
    /// The code's credential is read-only (`ro=1`).
    read_only: bool,
    /// The code pinned a certificate (`tm=pin`/v1) rather than asking for
    /// ordinary CA validation.
    pinned: bool,
}

impl PairingChrome {
    /// This pairing's CTA label — see [`PINNED_CONNECT_LABEL`].
    pub(super) fn connect_label(self) -> &'static str {
        if self.pinned {
            PINNED_CONNECT_LABEL
        } else {
            PAIRED_CONNECT_LABEL
        }
    }
}

/// `None` when nothing is stashed, and always for the splash (no controller to read).
pub(super) fn pairing_chrome(live: Option<&Live>) -> Option<PairingChrome> {
    live?.controller.pending_pairing.with(|pending| {
        pending.as_ref().map(|payload| PairingChrome {
            read_only: payload.read_only,
            pinned: matches!(payload.trust, PairingTrust::Pin { .. }),
        })
    })
}

/// The caption over the pinned CTA, plus — for a `ro=1` code — the read-only chip.
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
