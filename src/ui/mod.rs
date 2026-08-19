//! Shared, cross-feature widget helpers — the shape `examples/huddle`'s
//! `ui/` (toast, sheet, scaffold, fill_box, …) suggests this grows into as
//! muxr needs more app-wide chrome shared by more than one feature.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`backdrop`] | Full-bleed page background the Connect screen and splash overlay paint |
//! | [`dialogs`] | Confirm / name-input modal helpers over `frust_glyph`'s dialog |
//! | [`palette`] | The app's one theme-derived colour vocabulary, union of the four per-feature `Palette`s — not yet adopted |
//! | [`sheet`] | The modal bottom-sheet adapter over `frust_glyph`'s own sheet (`show_sheet` + its dismissal gate) |
//! | [`spacing`] | Named spacing tokens on the measured de-facto scale — not yet adopted |
//! | [`splash`] | The cold-start splash sequence (design reference: `docs/splash.html`/`docs/muxr-app-icons`) |
//! | [`toasts`] | The failure→toast queue handle and the toast layer the shell mounts |
//! | [`topbar`] | The app's bar furniture: the brand row and icon slot Glyph's own app bar wears, plus `icon_button` and the page clearance |

pub mod backdrop;
pub mod dialogs;
pub mod palette;
pub mod sheet;
pub mod spacing;
pub mod splash;
pub mod toasts;
pub mod topbar;
