//! The one width breakpoint every screen's responsive width-clamp pass reads
//! (FINDINGS #21) — a small, `presentation`-adjacent module beside
//! `connection.rs`/`failure.rs` rather than a new `ui/` file, since this
//! constant is app-scoped vocabulary more than one feature's `presentation`
//! layer imports (`docs/ARCHITECTURE.md`'s cross-feature rule: app-scoped
//! state/constants live in an app-level module reached via `crate::`, never a
//! sibling feature import).
//!
//! `examples/glyph-catalog/src/pages/responsive.rs` is the reference idiom
//! every one of this app's four screens copies: read
//! `use_context::<frust::WindowMetrics>()` once at the top of
//! `Component::build`, branch on this one constant, and never touch layout
//! again. `None` (no shell has published metrics yet — including every
//! headless test in `tests/` that does not explicitly provide one) takes the
//! same branch as a genuinely narrow window.
//!
//! Connect reads it one level down, inside
//! `features::connect::presentation::pages::connect_layout` rather than the
//! screen's own `build` — same build pass, same single read, but in the one
//! function *both* renderings of that screen assemble through. Its cold-start
//! splash lays the same pieces out through that layout with no `Component` of
//! its own, so a clamp applied in the screen's `build` was a width the splash
//! did not have and jumped to at the handoff. A screen with only one
//! rendering keeps the plain idiom above.
//!
//! **Conservative only.** This pass is width-clamps and padding scaling —
//! never a *structural* layout switch (no side-by-side panes, no two-column
//! settings). Those need frust's still-WIP responsive primitive and a design
//! mock neither exists yet; see `docs/ARCHITECTURE.md`'s Rebuild model note.

/// The narrow/wide switch point, in logical px (== dp on Android, pt on iOS —
/// `WindowMetrics::size` is already density-independent).
///
/// Sourced from Material 3's compact/medium window-size-class boundary
/// (<https://m3.material.io/foundations/layout/applying-layout/window-size-classes>),
/// the same well-known, already-vetted cutoff `examples/glyph-catalog`'s
/// `responsive.rs` uses, rather than a number invented for this app. One
/// constant, shared by all four screens' `presentation` layers — a future
/// screen imports this rather than re-declaring its own.
pub const WIDE_BREAKPOINT_PX: f64 = 600.0;
