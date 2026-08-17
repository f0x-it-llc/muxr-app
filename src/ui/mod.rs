//! Shared, cross-feature widget helpers. `examples/huddle`'s `ui/` (toast,
//! sheet, scaffold, fill_box, …) is the shape this module grows into as muxr
//! needs its own app-wide chrome shared by more than one feature.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`backdrop`] | The full-bleed page background both the Connect screen and the splash overlay paint |
//! | [`dialogs`] | Confirm / name-input modal helpers over `frust_glyph`'s dialog |
//! | [`palette`] | The app's one theme-derived colour vocabulary, the union of the four per-feature `Palette`s — not yet adopted by any screen |
//! | [`sheet`] | The modal bottom-sheet adapter over `frust_glyph`'s own sheet (`show_sheet` + its dismissal gate) |
//! | [`spacing`] | Named spacing tokens on the app's measured de-facto scale — not yet adopted by any screen |
//! | [`splash`] | The cold-start splash sequence (see `docs/splash.html`/`docs/muxr-app-icons` for the design reference) |
//! | [`toasts`] | The failure→toast queue handle and the toast layer the shell mounts |
//! | [`topbar`] | The branded `muxr` top bar (brand-or-back leading slot + crossfading centered title + one icon slot) |

pub mod backdrop;
pub mod dialogs;
pub mod palette;
pub mod sheet;
pub mod spacing;
pub mod splash;
pub mod toasts;
pub mod topbar;
