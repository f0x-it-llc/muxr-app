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

/// `.radio-opt{padding:9px 0}`.
const RADIO_PAD_Y: f64 = 9.0;
/// `.radio-opt{gap:9px}` — the ring to its label.
const RADIO_GAP: f64 = 9.0;
/// `.radio-dot{width:16px;height:16px}` — the ring's outer box.
const RADIO_RING: f64 = 16.0;
/// `.radio-dot{border:1.5px solid}` — the mock's one non-hairline border.
const RADIO_RING_BORDER: f64 = 1.5;
/// `.radio-dot.sel::after{inset:3px}` — the selected dot's inset inside the
/// ring, which makes the dot itself `RADIO_RING - 2 * RADIO_DOT_INSET` wide.
const RADIO_DOT_INSET: f64 = 3.0;

/// The remember row's copy — also the toggle's accessibility label, so the
/// switch announces the thing the row is about rather than "switch".
const REMEMBER_LABEL: &str = "Remember for 4 weeks";

/// The manually selectable [`TrustMode`]s, safest first. See [`trust_selector`]
/// for why `Pinned` is absent.
const TRUST_OPTIONS: [TrustMode; 2] = [TrustMode::System, TrustMode::NoVerification];

/// What the selector says instead of a selection while the form carries
/// [`TrustMode::Pinned`] from a saved server. Non-interactive. `pub` because
/// `tests/connect.rs` asserts on this exact run.
pub const PINNED_CAPTION: &str = "pinned certificate — from the saved server";

/// [`PINNED_CAPTION`]'s sibling for the *other* source of a pin: a scanned
/// pairing code.
pub const PAIRED_CAPTION: &str = "pinned certificate — from the scanned QR code";

/// Piece 5 — the remember toggle and the trust-mode selector. They share one piece
/// because the splash's stagger table has exactly eight slots and the mock renders
/// them as adjacent siblings with no entrance of their own.
pub fn remember_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
) -> AnyView<S> {
    let remembered = live.is_none_or(|live| live.controller.remember.get());
    let controller = live.map(|live| Arc::clone(&live.controller));
    // A controlled component: the toggle reports the *requested* value it never
    // self-applies, and the callback drives `toggle_remember`, so the controller's
    // flip (and the persistence it triggers) stays the single source of truth.
    // `.label(REMEMBER_LABEL)` is the toggle's own accessibility label.
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
                // The flexible label takes every pixel the toggle doesn't, which
                // is what pins the toggle to the trailing edge (`Flex`'s v1 has no
                // space-between to reach for).
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

/// The `.radio-opt` block: **two** rows over a three-way enum, the riskier one
/// carrying its `.warn-chip`, plus the caption that stands in for a selection the
/// user cannot make by hand.
///
/// Only `System` and `NoVerification` (the posture that genuinely skips
/// certificate validation, hence the chip) are a *manual* choice.
/// [`TrustMode::Pinned`] arrives with the thing that pinned it — a QR pairing or a
/// saved server — never from a user picking it against a certificate they don't
/// have yet, so offering it as a row would be an affordance with nothing behind
/// it. Tapping a pinned saved row *does* set the posture, so when neither row owns
/// the current mode both read unselected and a caption names where the pin came
/// from.
///
/// Hand-composed rather than `frust::radio`: `RadioWidget` shapes its label at
/// `TextStyle::default()` and fixes its ring/dot metrics, neither of which the
/// design's row can be reached from; `chrome::radio_semantics` contributes the
/// `Role::RadioButton` node the composition would otherwise lose.
fn trust_selector<S: 'static>(theme: &Theme, colors: &Palette, live: Option<&Live>) -> AnyView<S> {
    let selected = live.map_or(TrustMode::System, |live| live.controller.trust_mode.get());
    let mut rows: Vec<AnyView<S>> = TRUST_OPTIONS
        .iter()
        .map(|&mode| trust_row(theme, colors, mode, selected == mode, live))
        .collect();
    if !TRUST_OPTIONS.contains(&selected) {
        // Two sources put a pin on this form; the caption names the live one.
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

/// This mode's `.radio-opt` label. [`TrustMode::Pinned`] is never a row; its arm
/// is kept total so a fourth posture cannot be added without naming it.
fn trust_option_label(mode: TrustMode) -> &'static str {
    match mode {
        TrustMode::System => "Public domain (CA)",
        TrustMode::NoVerification => "Trust anyway",
        TrustMode::Pinned => "Pinned certificate",
    }
}

/// One `.radio-opt` row: the ring, the label, and — on the risky posture — the
/// trailing `.warn-chip`, pressable across its whole width.
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
    // The `Role::RadioButton` node covers the ring and label only, so the chip
    // keeps its own `Role::Status` node — a leaf radio around the whole row would
    // swallow it.
    let option: AnyView<S> = match live {
        None => any(option),
        Some(_) => any(radio_semantics(option, label, selected)),
    };
    // The flexible option is what pushes the chip to the trailing edge
    // (`margin-left:auto` with no `space-between` to reach for).
    let mut items = vec![flexible(1, option)];
    // Only on the posture that actually skips certificate validation. The chip is
    // `glyph::badge` — knowingly a shade larger than the mock's one-off
    // `.warn-chip`, and the identical chip the Sessions strip renders for the
    // identical posture. One gap: `BadgeVariant::Warning` paints no border where
    // `.warn-chip` asks for one, and the app cannot supply it.
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
            // Pressable across the whole row, chip included — the recognizer
            // sits outside the flex, not on the option alone.
            any(GestureDetector(body)
                .on_tap(move |_state: &mut S| controller.select_trust_mode(mode)))
        }
    }
}

/// `.radio-dot`: a 16px ring whose selected state re-inks it amber and drops an
/// inset-3px amber dot into it.
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

    /// Every posture, for the label-uniqueness sweep below — the *selector*
    /// offers only [`TRUST_OPTIONS`].
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
        // The product rule (see `trust_selector`): a pin arrives with the thing
        // that pinned it and is never hand-picked, so it must not be a row.
        assert_eq!(TRUST_OPTIONS.len(), 2, "the mock draws exactly two rows");
        assert!(
            !TRUST_OPTIONS.contains(&TrustMode::Pinned),
            "pinned is not a manual choice"
        );
        assert_eq!(trust_option_label(TRUST_OPTIONS[0]), "Public domain (CA)");
        assert_eq!(trust_option_label(TRUST_OPTIONS[1]), "Trust anyway");
        // …and the state it *can* still arrive in has copy of its own, or the
        // selector would render two unselected rows and no explanation.
        assert!(!PINNED_CAPTION.is_empty());
    }

    #[test]
    fn the_selected_radio_dot_stays_inside_its_ring() {
        // `inset:3px` on a 16px ring leaves a 10px dot; a wider inset than the
        // ring's own radius would be a negative-size layout panic in waiting.
        const { assert!(2.0 * RADIO_DOT_INSET < RADIO_RING) };
        const { assert!(RADIO_RING_BORDER < RADIO_RING / 2.0) };
    }
}
