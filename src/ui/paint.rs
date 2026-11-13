//! Paint-primitive tokens shared by every feature's hand-rolled chrome.

/// Hairline border width in logical px.
pub const HAIRLINE: f64 = 1.0;

/// `--radius-full: 999px`; an oversized radius clamps to half the shorter side: a pill.
pub const RADIUS_FULL: f64 = 999.0;

/// `box-shadow:0 0 5px` on a status dot, as a gaussian std-dev.
pub const GLOW_STD_DEV: f64 = 2.5;
/// How far the glow bleeds past the shape's edge on each side, in logical px.
pub const GLOW_SPREAD: f64 = 2.0;

/// The on/off step of a dashed border in logical px; CSS leaves `dashed`'s period to the UA.
pub const DASH_ON: f64 = 4.0;
pub const DASH_OFF: f64 = 4.0;
