//! The stashed-pairing chrome piece 6's CTA and the trust selector share: what
//! a scanned QR contributes to the form while it waits for an explicit connect.

use frust::{AnyView, Theme, With, any, flexible, inflexible, text};
use frust_glyph::{BadgeVariant, badge};

use super::{Live, h_gap, row};
use crate::features::connect::domain::entities::PairingTrust;
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

/// The pinned CTA's label — the **explicit action** a scanned pairing waits for (a
/// scan prefills; only this connects). `pub` for `tests/connect.rs`.
pub const PINNED_CONNECT_LABEL: &str = "Connect now (pinned)";
/// …and the same CTA for a `tm=ca` pairing, which carries no pin to name. The
/// action is the same one; only the word that would be a lie changes.
pub const PAIRED_CONNECT_LABEL: &str = "Connect now (paired)";
/// The caption over the pinned CTA, naming where the credentials came from. `pub`
/// — `tests/connect.rs` reads this exact run.
pub const PAIRING_CAPTION: &str = "scanned pairing — the code's own token and trust";
/// The chip a `ro=1` pairing (or a stored read-only credential) carries — one
/// spelling for both, since it is the same fact.
pub const READ_ONLY_CHIP: &str = "read-only";

/// What a stashed pairing contributes to the form's chrome — deliberately **not**
/// the payload itself, since a piece rebuilds at signal rate and copying the
/// scanned token out to read two booleans is a copy nobody needs.
#[derive(Clone, Copy)]
pub(super) struct PairingChrome {
    /// Whether the code's credential is read-only (`ro=1`).
    read_only: bool,
    /// Whether the code pinned a certificate (`tm=pin`/v1) rather than asking
    /// for ordinary CA validation.
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

/// Read the live pairing's chrome, tracked, or `None` when nothing is stashed
/// (and always `None` for the splash, which has no controller to read).
pub(super) fn pairing_chrome(live: Option<&Live>) -> Option<PairingChrome> {
    live?.controller.pending_pairing.with(|pending| {
        pending.as_ref().map(|payload| PairingChrome {
            read_only: payload.read_only,
            pinned: matches!(payload.trust, PairingTrust::Pin { .. }),
        })
    })
}

/// The caption over the pinned CTA, plus — for a `ro=1` code — the same read-only
/// chip a stored read-only credential's row carries.
pub(super) fn pairing_banner<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    pairing: PairingChrome,
) -> AnyView<S> {
    let mut items = vec![flexible(
        1,
        any(text(PAIRING_CAPTION).style(inked(&theme.type_scale.label_small, colors.fg_dim))),
    )];
    if pairing.read_only {
        items.push(inflexible(h_gap::<S>(spacing::MD)));
        items.push(inflexible(any(badge(
            READ_ONLY_CHIP,
            BadgeVariant::Neutral,
        ))));
    }
    any(row::<S>(items))
}
