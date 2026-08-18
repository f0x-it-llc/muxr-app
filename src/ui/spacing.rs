//! Named spacing tokens on the app's measured de-facto scale — the values
//! `docs/redesign.html`'s screens already repeat, transcribed exactly so no
//! rendered value moved when call sites switched over from local constants.
//!
//! Eleven tiers cover the values that recur across more than one screen; an
//! odd one-off stays feature-local — a token here is a claim that the value
//! recurs, not that it's merely convenient.

/// Hairline-scale gap — a badge's line gap, an accent bar's width, a keybar
/// cap's inter-glyph gap.
pub const MICRO: f64 = 2.0;
/// Smallest real gap — a dashed border's on/off dash length, a sheet row's
/// gap.
pub const TIGHT: f64 = 4.0;
/// A run or heading gap — a section header's gap above its body.
pub const XS: f64 = 6.0;
/// A pill/chip's own padding, a status strip's inter-item gap.
pub const SM: f64 = 8.0;
/// A list row's gap, a card's line gap, a strip's vertical padding.
pub const MD: f64 = 10.0;
/// A settings row's gap, a mark tile's padding.
pub const BASE: f64 = 12.0;
/// A card's own padding, a mark icon's size.
pub const LG: f64 = 14.0;
/// A row's horizontal inner padding — the single most common edge pad.
pub const XL: f64 = 16.0;
/// A panel or sheet's own padding, an icon slot's size.
pub const XXL: f64 = 20.0;
/// A card/layer's horizontal padding, a wide-viewport wrap pad.
pub const SHEET: f64 = 22.0;
/// Hero-scale spacing — a layer's vertical padding, a scroll view's bottom
/// clearance.
pub const HERO: f64 = 30.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scale_matches_the_measured_de_facto_values_in_order() {
        assert_eq!(
            [MICRO, TIGHT, XS, SM, MD, BASE, LG, XL, XXL, SHEET, HERO],
            [2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 20.0, 22.0, 30.0]
        );
    }
}
