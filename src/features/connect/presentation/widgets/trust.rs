//! Piece 5 — `.remember-row` plus the `.radio-opt` trust selector under it.

use std::sync::Arc;

use frust::authoring::{Color, Size};
use frust::{
    AnyView, EdgeInsets, GestureDetector, Get, Padding, Theme, any, flexible, inflexible, text,
};
use frust_glyph::{BadgeVariant, badge, toggle};

use super::super::chrome::{radio_semantics, swatch};
use super::pairing::pairing_chrome;
use super::{Live, backed, column, h_gap, row, v_gap};
use crate::connection::TrustMode;
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

const RADIO_PAD_Y: f64 = 9.0;
/// Ring to its label.
const RADIO_GAP: f64 = 9.0;
/// The ring's outer box.
const RADIO_RING: f64 = 16.0;
const RADIO_RING_BORDER: f64 = 1.5;
/// The selected dot's inset inside the ring; the dot itself is
/// `RADIO_RING - 2 * RADIO_DOT_INSET` wide.
const RADIO_DOT_INSET: f64 = 3.0;

/// The remember row's copy — also the toggle's accessibility label, so the
/// switch announces the thing the row is about rather than "switch".
const REMEMBER_LABEL: &str = "Remember for 4 weeks";

/// The manually selectable [`TrustMode`]s, safest first. See [`trust_selector`]
/// for why `Pinned` is absent.
const TRUST_OPTIONS: [TrustMode; 2] = [TrustMode::System, TrustMode::NoVerification];

/// What the selector says instead of a selection while the form carries
/// [`TrustMode::Pinned`] from a saved server. `pub` — `tests/connect.rs`
/// asserts on this exact run.
pub const PINNED_CAPTION: &str = "pinned certificate — from the saved server";

/// [`PINNED_CAPTION`]'s sibling for the other source of a pin: a scanned
/// pairing code.
pub const PAIRED_CAPTION: &str = "pinned certificate — from the scanned QR code";

/// Piece 5 — the remember toggle and the trust-mode selector, sharing one
/// piece because the stagger table has exactly eight slots.
pub fn remember_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
) -> AnyView<S> {
    let remembered = live.is_none_or(|live| live.controller.remember.get());
    let controller = live.map(|live| Arc::clone(&live.controller));
    // A controlled component: the toggle reports the requested value it never
    // self-applies; the controller's flip stays the single source of truth.
    let toggle_view = any(
        toggle::<S, _>(remembered, move |_state: &mut S, _requested: bool| {
            if let Some(controller) = &controller {
                controller.toggle_remember();
            }
        })
        .label(REMEMBER_LABEL),
    );
    backed(
        column::<S>(vec![
            any(row::<S>(vec![
                // The flexible label takes every pixel the toggle doesn't,
                // pinning the toggle to the trailing edge.
                flexible(
                    1,
                    any(text(REMEMBER_LABEL)
                        .style(inked(&theme.type_scale.body_small, colors.fg_muted))),
                ),
                inflexible(toggle_view),
            ])),
            v_gap::<S>(spacing::SHEET),
            trust_selector(theme, colors, live),
        ]),
        colors,
    )
}

/// Two rows over a three-way enum, the riskier one carrying its warning chip,
/// plus a caption standing in for a selection the user cannot make by hand.
///
/// Only `System` and `NoVerification` are a manual choice.
/// [`TrustMode::Pinned`] arrives with the thing that pinned it — a QR pairing
/// or a saved server — never from a user picking it against a certificate
/// they don't have yet, so offering it as a row would be an affordance with
/// nothing behind it. When neither row owns the current mode, both read
/// unselected and a caption names where the pin came from.
///
/// Hand-composed rather than `frust::radio`: `RadioWidget` shapes its label
/// at `TextStyle::default()` and fixes its ring/dot metrics, neither
/// reachable from the design's row; `chrome::radio_semantics` contributes
/// the `Role::RadioButton` node the composition would otherwise lose.
fn trust_selector<S: 'static>(theme: &Theme, colors: &Palette, live: Option<&Live>) -> AnyView<S> {
    let selected = live.map_or(TrustMode::System, |live| live.controller.trust_mode.get());
    let mut rows: Vec<AnyView<S>> = TRUST_OPTIONS
        .iter()
        .map(|&mode| trust_row(theme, colors, mode, selected == mode, live))
        .collect();
    if !TRUST_OPTIONS.contains(&selected) {
        let caption = if pairing_chrome(live).is_some() {
            PAIRED_CAPTION
        } else {
            PINNED_CAPTION
        };
        rows.push(any(
            text(caption).style(inked(&theme.type_scale.label_small, colors.fg_dim))
        ));
    }
    any(column::<S>(rows))
}

/// This mode's label. [`TrustMode::Pinned`] is never a row; its arm is kept
/// total so a fourth posture cannot be added without naming it.
fn trust_option_label(mode: TrustMode) -> &'static str {
    match mode {
        TrustMode::System => "Public domain (CA)",
        TrustMode::NoVerification => "Trust anyway",
        TrustMode::Pinned => "Pinned certificate",
    }
}

/// The ring, the label, and — on the risky posture — a trailing warning
/// chip, pressable across its whole width.
fn trust_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    mode: TrustMode,
    selected: bool,
    live: Option<&Live>,
) -> AnyView<S> {
    let label = trust_option_label(mode);
    let option = row::<S>(vec![
        inflexible(radio_dot::<S>(colors, selected)),
        inflexible(h_gap::<S>(RADIO_GAP)),
        inflexible(any(
            text(label).style(inked(&theme.type_scale.body_medium, colors.fg))
        )),
    ]);
    // The `Role::RadioButton` node covers the ring and label only, so the
    // chip keeps its own `Role::Status` node — a leaf radio around the whole
    // row would swallow it.
    let option: AnyView<S> = match live {
        None => any(option),
        Some(_) => any(radio_semantics(option, label, selected)),
    };
    // The flexible option pushes the chip to the trailing edge.
    let mut items = vec![flexible(1, option)];
    // Only on the posture that actually skips certificate validation.
    // `BadgeVariant::Warning` paints no border where the mock's chip asks
    // for one, and the app cannot supply it.
    if mode == TrustMode::NoVerification {
        items.push(inflexible(h_gap::<S>(spacing::MD)));
        items.push(inflexible(any(badge("insecure", BadgeVariant::Warning))));
    }
    let body = Padding(EdgeInsets::symmetric(0.0, RADIO_PAD_Y), row::<S>(items));
    match live {
        // Splash: static art with no recognizer and no node — nothing to operate.
        None => any(body),
        Some(live) => {
            let controller = Arc::clone(&live.controller);
            // Pressable across the whole row, chip included.
            any(GestureDetector(body)
                .on_tap(move |_state: &mut S| controller.select_trust_mode(mode)))
        }
    }
}

/// A 16px ring whose selected state re-inks it amber and drops an inset amber
/// dot into it.
fn radio_dot<S: 'static>(colors: &Palette, selected: bool) -> AnyView<S> {
    let ring_size = Size::new(RADIO_RING, RADIO_RING);
    let ring_ink = if selected {
        colors.amber
    } else {
        colors.outline
    };
    let ring = swatch(ring_size, Color::TRANSPARENT, RADIO_RING / 2.0)
        .border(ring_ink)
        .border_width(RADIO_RING_BORDER);
    if !selected {
        return any(ring);
    }
    let dot = RADIO_RING - 2.0 * RADIO_DOT_INSET;
    any(frust::Stack(vec![
        any(ring),
        any(Padding(
            EdgeInsets::all(RADIO_DOT_INSET),
            swatch(Size::new(dot, dot), colors.amber, dot / 2.0),
        )),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_TRUST_MODES: [TrustMode; 3] = [
        TrustMode::System,
        TrustMode::NoVerification,
        TrustMode::Pinned,
    ];

    #[test]
    fn every_trust_mode_has_its_own_option_label() {
        let mut options: Vec<&str> = ALL_TRUST_MODES
            .iter()
            .copied()
            .map(trust_option_label)
            .collect();
        options.sort_unstable();
        options.dedup();
        assert_eq!(options.len(), ALL_TRUST_MODES.len());
    }

    #[test]
    fn the_selector_offers_the_two_manual_postures_and_never_pinned() {
        assert_eq!(TRUST_OPTIONS.len(), 2, "the mock draws exactly two rows");
        assert!(
            !TRUST_OPTIONS.contains(&TrustMode::Pinned),
            "pinned is not a manual choice"
        );
        assert_eq!(trust_option_label(TRUST_OPTIONS[0]), "Public domain (CA)");
        assert_eq!(trust_option_label(TRUST_OPTIONS[1]), "Trust anyway");
        assert!(!PINNED_CAPTION.is_empty());
    }

    #[test]
    fn the_selected_radio_dot_stays_inside_its_ring() {
        // A wider inset than the ring's own radius would be a negative-size layout panic.
        const { assert!(2.0 * RADIO_DOT_INSET < RADIO_RING) };
        const { assert!(RADIO_RING_BORDER < RADIO_RING / 2.0) };
    }
}
