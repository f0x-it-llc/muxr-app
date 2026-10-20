//! Piece 5 — `.remember-row` plus the `.radio-opt` trust selector under it.

use std::sync::Arc;

use frust::authoring::Color;
use frust::authoring::text::{FontWeight, TextStyle};
use frust::{
    AnyView, EdgeInsets, Either, GestureDetector, Get, Padding, Theme, View, any, colored_box,
    flexible, inflexible, stack, text,
};
use frust_glyph::{BadgeVariant, badge, radio, toggle};

use super::pairing::pairing_chrome;
use super::{Live, backed, block_column, centered_row, h_gap, v_gap};
use crate::connection::TrustMode;
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;

const RADIO_PAD_Y: f64 = 9.0;
/// Ring to its label.
const RADIO_GAP: f64 = 9.0;
/// The live row's label size — `frust_glyph::radio`'s own `LABEL_SIZE`, not
/// exported, transcribed here so the splash's static row is exactly as tall
/// as the live one and every piece below it lands on the same pixels at the
/// handoff.
const RADIO_LABEL_SIZE: f32 = 13.0;
/// The ring's outer box.
const RADIO_RING: f64 = 16.0;
const RADIO_RING_BORDER: f64 = 1.5;
/// The selected dot's inset inside the ring; the dot itself is
/// `RADIO_RING - 2 * RADIO_DOT_INSET` wide.
const RADIO_DOT_INSET: f64 = 3.0;

/// A conservative floor for [`full_row_press`]'s background:
/// [`RADIO_RING`] plus [`RADIO_PAD_Y`] on each edge — may fall short of an
/// unusually tall label's own (unknown-here) shaped height, cosmetic only.
///
/// This app's other full-bleed targets clear the 44px platform floor
/// (`qr_affordance::MIN_TAP_TARGET`/`saved::DELETE_TARGET`/
/// `panes::PANE_EDIT_TAP_BOX`); this row's mock pins a slimmer ~34px
/// instead, preserved here — growing to 44px is Ed's call, not this fix's.
const ROW_PRESS_FLOOR: f64 = 2.0 * RADIO_PAD_Y + RADIO_RING;

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
) -> impl View<S> + use<S> {
    let remembered = live.is_none_or(|live| live.controller.remember.get());
    let controller = live.map(|live| Arc::clone(&live.controller));
    // A controlled component: the toggle reports the requested value it never
    // self-applies; the controller's flip stays the single source of truth.
    let toggle_view = toggle::<S, _>(remembered, move |_state: &mut S, _requested: bool| {
        if let Some(controller) = &controller {
            controller.toggle_remember();
        }
    })
    .label(REMEMBER_LABEL);
    backed(
        block_column::<S, _>((
            centered_row::<S>(vec![
                // The flexible label takes every pixel the toggle doesn't,
                // pinning the toggle to the trailing edge.
                flexible(
                    1,
                    text(REMEMBER_LABEL)
                        .style(inked(&theme.type_scale.body_small, colors.fg_muted)),
                ),
                inflexible(toggle_view),
            ]),
            v_gap::<S>(spacing::SHEET),
            trust_selector::<S>(theme, colors, live),
        )),
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
/// The live row renders through [`frust_glyph::radio`], whose ring/dot
/// geometry ([`RADIO_RING`]/[`RADIO_RING_BORDER`]/[`RADIO_DOT_INSET`]/
/// [`RADIO_GAP`]) was transcribed *from* this row (see that widget's own
/// module docs), so the swap-in matches pixel-for-pixel. The splash's inert
/// copy still hand-composes its ring+dot from [`frust::colored_box`] (see
/// [`radio_dot`]) and shapes its label at the radio's own metrics
/// ([`radio_label_style`]): `frust_glyph::radio` always wires a working
/// press and a `Role::RadioButton` node, which the splash's static art must
/// never carry.
fn trust_selector<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
) -> impl View<S> + use<S> {
    let selected = live.map_or(TrustMode::System, |live| live.controller.trust_mode.get());
    let mut rows: Vec<AnyView<S>> = TRUST_OPTIONS // erasure: keep accumulator into Vec<AnyView>
        .iter()
        .map(|&mode| any(trust_row(theme, colors, mode, selected == mode, live)))
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
    block_column::<S, _>(rows)
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
/// chip. Live, the ring+label pair is one [`frust_glyph::radio`] (its own
/// node and press untouched), but the row's *press* target is the whole
/// padded row via [`full_row_press`] — see that function's docs.
fn trust_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    mode: TrustMode,
    selected: bool,
    live: Option<&Live>,
) -> impl View<S> + use<S> {
    let label = trust_option_label(mode);
    let option = match live {
        // Splash: static art with no recognizer and no node — nothing to operate.
        None => Either::Left(centered_row::<S>(vec![
            inflexible(radio_dot::<S>(colors, selected)),
            inflexible(h_gap::<S>(RADIO_GAP)),
            inflexible(text(label).style(radio_label_style(theme, colors.fg))),
        ])),
        Some(live) => {
            let controller = Arc::clone(&live.controller);
            Either::Right(
                radio::<S, _>(selected, move |_state: &mut S| {
                    controller.select_trust_mode(mode)
                })
                .label(label),
            )
        }
    };
    // The flexible option pushes the chip to the trailing edge.
    let mut items = vec![flexible(1, option)];
    // Only on the posture that actually skips certificate validation.
    // `BadgeVariant::Warning` paints no border where the mock's chip asks
    // for one, and the app cannot supply it.
    if mode == TrustMode::NoVerification {
        items.push(inflexible(h_gap::<S>(spacing::MD)));
        items.push(inflexible(badge("insecure", BadgeVariant::Warning)));
    }
    let content = Padding(
        EdgeInsets::symmetric(0.0, RADIO_PAD_Y),
        centered_row::<S>(items),
    );
    match live {
        // Splash: inert art — no press layer to add.
        None => Either::Left(content),
        Some(live) => {
            let controller = Arc::clone(&live.controller);
            Either::Right(full_row_press(content, move |_state: &mut S| {
                controller.select_trust_mode(mode)
            }))
        }
    }
}

/// Wraps `content` (a padded [`trust_row`]) in a full-row press target
/// covering what `content`'s own live [`frust_glyph::radio`] does **not**
/// — chip, gap, [`RADIO_PAD_Y`] gutter — without `radio` firing twice.
///
/// Not a `GestureDetector` around `content`: it always forwards to its
/// child *and* fires its own `on_tap` regardless of the child's own
/// result, so it would double-fire on `radio`'s own bounds (the
/// "siblings, not nested" hazard `pills::pill` also guards against).
/// Instead `content` and an invisible background are [`Stack`] siblings,
/// topmost-first hit-tested: a tap inside `radio`'s own bounds is
/// `Handled` there and never reaches the background; every other tap
/// falls through as `Ignored` and reaches it instead. Exactly one fires,
/// by construction — see `full_row_press_mechanism` below.
/// [`ROW_PRESS_FLOOR`] sizes the background's own height.
fn full_row_press<S: 'static>(
    content: impl View<S>,
    on_tap: impl Fn(&mut S) + 'static,
) -> impl View<S> {
    let background =
        GestureDetector(colored_box::<S>().size(f64::INFINITY, ROW_PRESS_FLOOR)).on_tap(on_tap);
    stack().child(background).child(content)
}

/// The static row's label at [`frust_glyph::radio`]'s own style — the
/// theme's `bodyLarge` family at [`RADIO_LABEL_SIZE`], regular weight —
/// inked `ink`; never used for the live row, which shapes its own.
fn radio_label_style(theme: &Theme, ink: Color) -> TextStyle {
    TextStyle {
        family: theme.type_scale.body_large.family.clone(),
        weight: FontWeight::REGULAR,
        ..TextStyle::new(RADIO_LABEL_SIZE, ink)
    }
}

/// A 16px ring whose selected state re-inks it amber and drops an inset amber
/// dot into it — the splash's inert copy of [`frust_glyph::radio`]'s own
/// art (see [`trust_selector`]'s docs); never used for the live row.
fn radio_dot<S: 'static>(colors: &Palette, selected: bool) -> impl View<S> + use<S> {
    let ring_ink = if selected {
        colors.amber
    } else {
        colors.outline
    };
    let ring = colored_box::<S>()
        .size(RADIO_RING, RADIO_RING)
        .radius(RADIO_RING / 2.0)
        .border(ring_ink, RADIO_RING_BORDER);
    if !selected {
        return Either::Left(ring);
    }
    let dot = RADIO_RING - 2.0 * RADIO_DOT_INSET;
    Either::Right(
        stack().child(ring).child(Padding(
            EdgeInsets::all(RADIO_DOT_INSET),
            colored_box::<S>()
                .size(dot, dot)
                .radius(dot / 2.0)
                .fill(colors.amber),
        )),
    )
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

    // Colocated widget-mechanism test for `full_row_press`'s press-routing
    // (screen behavior tests live in `tests/connect.rs`).
    mod full_row_press_mechanism {
        use super::*;
        use frust_core::{InputEvent, PointerButton, PointerEvent, PointerPhase, RenderRoot};
        use kurbo::{Point, Size};

        #[derive(Default)]
        struct Counts {
            radio: u32,
            background: u32,
        }

        // A labelless `radio` (hit box = [`RADIO_RING`]) wrapped by
        // `full_row_press` — reproduces a naive nested detector's double-fire.
        fn logic(_state: &mut Counts) -> AnyView<Counts> // erasure: keep stored view table/registry
        {
            let radio_view = radio::<Counts, _>(false, |state: &mut Counts| {
                state.radio += 1;
            });
            any(full_row_press(radio_view, |state: &mut Counts| {
                state.background += 1;
            }))
        }

        fn dispatch(
            root: &mut RenderRoot<Counts, AnyView<Counts>>,
            state: &mut Counts,
            phase: PointerPhase,
            x: f64,
            y: f64,
        ) {
            root.event(
                state,
                &InputEvent::Pointer(PointerEvent {
                    phase,
                    position: Point::new(x, y),
                    button: PointerButton::Primary,
                }),
            );
        }

        fn tap(root: &mut RenderRoot<Counts, AnyView<Counts>>, state: &mut Counts, x: f64, y: f64) {
            dispatch(root, state, PointerPhase::Down, x, y);
            dispatch(root, state, PointerPhase::Up, x, y);
        }

        #[test]
        fn radio_and_background_never_both_fire_for_the_same_tap() {
            let mut root: RenderRoot<Counts, AnyView<Counts>> = RenderRoot::new();
            let mut state = Counts::default();
            root.rebuild(&mut logic, &mut state);
            root.layout(Size::new(400.0, 200.0));

            tap(&mut root, &mut state, 4.0, 4.0);
            assert_eq!(state.radio, 1, "a tap on the ring fires radio exactly once");
            assert_eq!(state.background, 0, "…never also the background");

            tap(&mut root, &mut state, 300.0, 4.0);
            assert_eq!(state.radio, 1, "the off-radio tap must not reach radio");
            assert_eq!(state.background, 1, "…and fires the background once");
        }
    }
}
