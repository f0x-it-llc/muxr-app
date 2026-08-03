//! `terminal` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`pages`] | `terminal_page` (the route's `Component`), its `Actions` bundle, the overflow menu and the "panes & tabs" sheet |
//! | [`controller`] | `TerminalController` — the screen's view model, hosted by `use_controller` |
//! | [`pieces`] | one fn per visual element of `docs/terminal.html` v2, plus this screen's token palette |
//! | [`chrome`] | the hand-rolled `View`/`Widget` pairs those pieces are built from, and the three vector marks `frust::icons` lacks |
//! | [`term`] | the **real** terminal grid — VT emulator, run batching, cell geometry, the grid widget — not yet mounted by any screen |
//!
//! [`pages::open_terminal_menu`] is the one item outside this feature's own
//! call graph: `crate::shell` forwards the app bar's `⋮` press to it (the bar
//! is app chrome, the menu on it is this feature's).
//!
//! [`pieces`] and [`term`] are the two halves of a migration in progress: the
//! screen paints the mock transcript today, and cutting it over to the live
//! grid is its own task (see [`term`]'s module docs).

pub mod chrome;
pub mod controller;
pub mod pages;
pub mod pieces;
pub mod term;
