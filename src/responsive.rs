//! `WIDE_BREAKPOINT_PX` is the one width breakpoint every screen's responsive
//! width-clamp pass reads — see `docs/ARCHITECTURE.md`'s Responsive section
//! for the read/branch contract and the `None`-takes-narrow-branch rule.
//! Conservative only: width-clamp and padding scaling, never a *structural*
//! layout switch.
//!
//! Connect reads it one level down, inside `connect_layout` rather than the
//! screen's own `build`, since its cold-start splash renders the same layout
//! through no `Component` of its own.

/// The narrow/wide switch point, in logical px (density-independent).
///
/// Sourced from Material 3's compact/medium window-size-class boundary
/// (<https://m3.material.io/foundations/layout/applying-layout/window-size-classes>)
/// rather than a number invented for this app.
pub const WIDE_BREAKPOINT_PX: f64 = 600.0;
