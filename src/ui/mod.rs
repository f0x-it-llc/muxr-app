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
//!
//! [`splash::sequence`] is the orchestrator `crate::shell` mounts over the
//! navigator on a cold start; everything else under [`splash`] is one of the
//! sequence's independently mountable beats. [`toasts::toast_layer`] is one of
//! the two things `crate::shell` mounts permanently — between the bar and the
//! splash — and [`topbar`] is the other, *floating over* the navigator between
//! it and the toasts (see [`crate::shell::root_view`] for the layer order and
//! why the bar no longer shares a band with the navigator).
//!
//! [`dialogs`] and [`sheet`] are the **presentation substrate** every screen
//! shares: a feature's `presentation` layer calls into them rather than
//! composing its own modal chrome, so "what a muxr dialog looks like" has one
//! answer. [`topbar`] went one step further in the same direction: **no screen
//! mounts it at all** any more. It is app chrome `crate::shell` owns, titled
//! per route from `crate::routes::route_meta`, so its title crossfades across a
//! navigation instead of being torn down and rebuilt with each screen (the
//! terminal screen's `⋮` joins that one bar's trailing slot as
//! [`crate::routes::BarAction::TerminalMenu`]).

pub mod backdrop;
pub mod dialogs;
pub mod palette;
pub mod sheet;
pub mod spacing;
pub mod splash;
pub mod toasts;
pub mod topbar;
