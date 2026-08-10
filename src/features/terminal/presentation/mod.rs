//! `terminal` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`pages`] | `terminal_page` (the route's `Component`), its `Actions` bundle, the overflow menu and the "panes & tabs" sheet |
//! | [`controller`] | `TerminalController` — the screen's view model, hosted by `use_controller` |
//! | [`pieces`] | one fn per visual element of `docs/terminal.html` v2, plus this screen's token palette |
//! | [`chrome`] | the hand-rolled `View`/`Widget` pairs those pieces are built from, and the three vector marks `frust::icons` lacks |
//! | [`term`] | the **real** terminal grid — VT emulator, run batching, cell geometry, the grid widget, and the keystroke source — what an attached session mounts (`pages`'s pane-content switch) |
//! | [`keybar`] | the IME-active-only key bar (`Esc`/`Tab`/arrows/`PgUp`/`PgDn`/`Del` + the one-shot `Ctrl`/`Alt` latch) — mounted the same way, live-mode only |
//!
//! [`pages::open_terminal_menu`] is the one item outside this feature's own
//! call graph: `crate::shell` forwards the app bar's `⋮` press to it (the bar
//! is app chrome, the menu on it is this feature's).
//!
//! [`pieces`] and [`term`] are the two halves of the screen's card body: the
//! mock transcript renders by default (every existing route/navigation), and
//! [`pages::TerminalScreen::build`] switches to [`term::terminal_surface`]
//! once the live pane has been entered for that mount — see `pages`'s own
//! module docs ("Pane-content switch") for the full wiring and why the mock
//! stays the untouched default.

pub mod chrome;
pub mod controller;
pub mod keybar;
pub mod pages;
pub mod pieces;
pub mod term;
