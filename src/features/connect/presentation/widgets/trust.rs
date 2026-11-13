//! Piece 5: `.remember-row` plus the `.radio-opt` trust selector under it.

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
const RADIO_GAP: f64 = 9.0;
/// `frust_glyph::radio`'s own `LABEL_SIZE`, not exported, transcribed so the splash's static
/// row is exactly as tall as the live one.
const RADIO_LABEL_SIZE: f32 = 13.0;
const RADIO_RING: f64 = 16.0;
const RADIO_RING_BORDER: f64 = 1.5;
const RADIO_DOT_INSET: f64 = 3.0;

/// A conservative floor, cosmetic only. The mock pins this row at ~34px, below the 44px
/// platform floor the app's other full-bleed targets clear.
const ROW_PRESS_FLOOR: f64 = 2.0 * RADIO_PAD_Y + RADIO_RING;

/// Also the toggle's accessibility label, so the switch announces what the row is about.
const REMEMBER_LABEL: &str = "Remember for 4 weeks";

const TRUST_OPTIONS: [TrustMode; 2] = [TrustMode::System, TrustMode::NoVerification];

/// `pub`: `tests/connect.rs` asserts on this exact run.
pub const PINNED_CAPTION: &str = "pinned certificate — from the saved server";

pub const PAIRED_CAPTION: &str = "pinned certificate — from the scanned QR code";

/// One piece because the stagger table has exactly eight slots.
pub fn remember_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    live: Option<&Live>,
) -> impl View<S> + use<S> {
    let remembered = live.is_none_or(|live| live.controller.remember.get());
    let controller = live.map(|live| Arc::clone(&live.controller));
    // Controlled: the toggle never self-applies; the controller's flip is the source of truth.
    let toggle_view = toggle::<S, _>(remembered, move |_state: &mut S, _requested: bool| {
        if let Some(controller) = &controller {
            controller.toggle_remember();
        }
    })
    .label(REMEMBER_LABEL);
    backed(
        block_column::<S, _>((
            centered_row::<S>(vec![
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

/// Only `System` and `NoVerification` are a manual choice: `Pinned` arrives with what pinned
/// it. The splash hand-composes its ring and dot because `frust_glyph::radio` always wires a
/// press and a `Role::RadioButton` node.
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

/// Kept total so a fourth posture cannot be added without naming it.
fn trust_option_label(mode: TrustMode) -> &'static str {
    match mode {
        TrustMode::System => "Public domain (CA)",
        TrustMode::NoVerification => "Trust anyway",
        TrustMode::Pinned => "Pinned certificate",
    }
}

fn trust_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    mode: TrustMode,
    selected: bool,
    live: Option<&Live>,
) -> impl View<S> + use<S> {
    let label = trust_option_label(mode);
    let option = match live {
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
    let mut items = vec![flexible(1, option)];
    // `BadgeVariant::Warning` paints no border where the mock's chip asks for one.
    if mode == TrustMode::NoVerification {
        items.push(inflexible(h_gap::<S>(spacing::MD)));
        items.push(inflexible(badge("insecure", BadgeVariant::Warning)));
    }
    let content = Padding(
        EdgeInsets::symmetric(0.0, RADIO_PAD_Y),
        centered_row::<S>(items),
    );
    match live {
        None => Either::Left(content),
        Some(live) => {
            let controller = Arc::clone(&live.controller);
            Either::Right(full_row_press(content, move |_state: &mut S| {
                controller.select_trust_mode(mode)
            }))
        }
    }
}

/// Not a `GestureDetector` around `content`: it fires its own `on_tap` whatever the child
/// returns, double-firing on `radio`'s bounds. `content` and an invisible background are
/// [`Stack`] siblings instead, so exactly one fires.
fn full_row_press<S: 'static>(
    content: impl View<S>,
    on_tap: impl Fn(&mut S) + 'static,
) -> impl View<S> {
    let background =
        GestureDetector(colored_box::<S>().size(f64::INFINITY, ROW_PRESS_FLOOR)).on_tap(on_tap);
    stack().child(background).child(content)
}

fn radio_label_style(theme: &Theme, ink: Color) -> TextStyle {
    TextStyle::new(RADIO_LABEL_SIZE, ink)
        .with_family(theme.type_scale.body_large.family.clone())
        .with_weight(FontWeight::REGULAR)
}

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

    mod full_row_press_mechanism {
        use super::*;
        use frust_core::{InputEvent, PointerButton, PointerEvent, PointerPhase, RenderRoot};
        use kurbo::{Point, Size};

        #[derive(Default)]
        struct Counts {
            radio: u32,
            background: u32,
        }

        // A labelless `radio` wrapped by `full_row_press`: a naive nested detector double-fires.
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
