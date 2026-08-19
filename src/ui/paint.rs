//! Paint-primitive tokens repeated, byte-for-byte, across every feature's
//! hand-rolled chrome — a hairline border, a full-pill radius, a status
//! dot's glow, a dashed border's on/off step. One declaration each, so no
//! future edit can drift one feature's chrome away from the rest.

/// Hairline border width in logical px (`1px solid` on every bordered rule
/// across the four features' design references). Matches `frust-widgets`'
/// own `BORDER_WIDTH`.
pub const HAIRLINE: f64 = 1.0;

/// `--radius-full: 999px` — kurbo's own `RoundedRect` clamps an oversized
/// radius to half the shorter side, so a caller can spell "pill" without
/// knowing the box's measured height.
pub const RADIUS_FULL: f64 = 999.0;

/// `box-shadow:0 0 5px var(--success)` on a status dot — the blur radius as
/// a gaussian std-dev for `container(..).glow(..)`.
pub const GLOW_STD_DEV: f64 = 2.5;
/// How far [`GLOW_STD_DEV`]'s glow bleeds past the glowing shape's own edge,
/// on each side.
pub const GLOW_SPREAD: f64 = 2.0;

/// `border:1px dashed` — the on/off step of a
/// [`frust::authoring::DashPattern`] border, in logical px. CSS leaves
/// `dashed`'s period to the UA; 4/4 is the rendering every dashed border in
/// the app already converged on independently.
pub const DASH_ON: f64 = 4.0;
pub const DASH_OFF: f64 = 4.0;
