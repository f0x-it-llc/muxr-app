//! Named spacing tokens in logical px, transcribed from the design mocks' recurring values; a
//! one-off value stays feature-local.

pub const MICRO: f64 = 2.0;
pub const TIGHT: f64 = 4.0;
pub const XS: f64 = 6.0;
pub const SM: f64 = 8.0;
pub const MD: f64 = 10.0;
pub const BASE: f64 = 12.0;
pub const LG: f64 = 14.0;
pub const XL: f64 = 16.0;
pub const XXL: f64 = 20.0;
pub const SHEET: f64 = 22.0;
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
