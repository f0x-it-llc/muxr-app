//! `terminal` presentation — the only layer in this feature that touches
//! `frust`'s reactive/widget vocabulary.
//!
//! | Module | What it owns |
//! |---|---|
//! | [`pages`] | `terminal_page` (the route's `Component`), its `Actions` bundle, and the overflow menu |
//! | [`controller`] | `TerminalController` — the screen's view model, hosted by `use_controller` |
//! | [`widgets`] | one module per visual cluster of `docs/terminal.html` v2 — the card, the status row, the pills/panes, the "panes & tabs" sheet and its dialog |
//! | [`chrome`] | the hand-rolled `View`/`Widget` pairs those widgets are built from, and the three vector marks `frust::icons` lacks |
//! | [`term`] | the **real** terminal grid — VT emulator, run batching, cell geometry, the grid widget, and the keystroke source — what an attached session mounts (`pages`'s pane-content switch) |
//! | [`keybar`] | the IME-active-only key bar (`Esc`/`Tab`/arrows/`PgUp`/`PgDn`/`Del` + the one-shot `Ctrl`/`Alt`/`Shift` latch) — mounted the same way, live-mode only |
//!
//! [`pages::open_terminal_menu`] is the one item outside this feature's own
//! call graph: `crate::shell` forwards the app bar's `⋮` press to it.
//!
//! [`widgets`] and [`term`] are the two halves of the card body — the mock
//! transcript by default, [`term::terminal_surface`] once the live pane has
//! been entered (`pages`' pane-content switch).

pub mod chrome;
pub mod controller;
pub mod keybar;
pub mod pages;
pub mod term;
pub mod widgets;
