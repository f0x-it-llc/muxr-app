//! The `/terminal` screen's independently-composable widgets, transcribed from
//! `docs/terminal.html` (**v2**, "One less row" — it supersedes
//! `docs/redesign.html`'s terminal blocks wholesale: there is no tabs strip,
//! no path bar, no cmd hint, no FAB and no keyboard bar).
//!
//! One module per cluster, one fn per visual element, so [`pages`](super::pages)'
//! `Component::build` and the sheet builder read as arrangements rather than
//! walls of nesting (the `features::sessions`/`features::connect` precedent):
//!
//! | Module | Mock element / what it owns |
//! |---|---|
//! | [`card`] | the card's own `GlyphInk` ink ([`card::CardInk`]) and `.term-body` — the scrollable, staggered, caret-terminated transcript |
//! | [`status`] | `.term-statusrow` — the pane title plus the one expand button |
//! | [`link_strip`] | the reconnecting/disconnected/ended band between the two (no mock counterpart — a live `muxrd` session's own link state) |
//! | [`pills`] | `.pill`, `.pill.sel`, `.pill.add`, and the trailing edit affordance every renamable chip carries |
//! | [`panes`] | `.pane-item` and `.new-pane-btn` — the sheet's pane section's two rows |
//! | [`sheet_bits`] | `.sheet-toprow`, `.sheet-section-lbl`, and the sheet's own scrollable region |
//! | [`panes_sheet`] | `#pickerSheet` itself — the arrangement of everything above, over the screen's `Actions` bundle |
//! | [`new_tab_dialog`] | the add-tab row's hand-rolled name dialog, the one prompt here whose blank field is a valid submit |
//! | [`util`] | the row/column/gap/`TextStyle` helpers every module above builds from |
//!
//! # Two colour vocabularies, and the line between them
//!
//! The **card** is inked from the framework's own brightness-invariant
//! terminal palette ([`card::CardInk`], over `GlyphInk`), plus the two
//! hairline tones no token carries — see that module's own docs.
//!
//! Everything *outside* the card — the sheet, its pills, the link strip's own
//! run — resolves [`crate::ui::palette::Palette`], the app's one shared
//! scheme-derived vocabulary, because it is ordinary app chrome that happens
//! to be about a terminal. The live grid's third vocabulary, the ANSI
//! `TerminalPalette`, stays where the emulator that needs it does
//! (`super::term::style`).

pub(crate) mod card;
pub(crate) mod link_strip;
pub(crate) mod new_tab_dialog;
pub(crate) mod panes;
pub(crate) mod panes_sheet;
pub(crate) mod pills;
pub(crate) mod sheet_bits;
pub(crate) mod status;
pub(crate) mod util;
