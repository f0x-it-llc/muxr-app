//! The `/terminal` screen's widgets, transcribed from `docs/terminal.html` v2.
//! One module per cluster, one fn per visual element, so
//! [`pages`](super::pages)' `Component::build` and the sheet builder read as
//! arrangements rather than walls of nesting: [`card`] owns the card's own
//! `GlyphInk` ink and `.term-body`, [`status`] the `.term-statusrow`,
//! [`link_strip`] the live link band between them, [`pills`] the `.pill` family and
//! its trailing edit affordance, [`panes`] `.pane-item`/`.new-pane-btn`,
//! [`sheet_bits`] the sheet's top row, section labels and scroll region,
//! [`panes_sheet`] `#pickerSheet` itself, [`new_tab_dialog`] the add-tab row's
//! hand-rolled prompt, and [`util`] the row/column/gap/`TextStyle` helpers.
//!
//! **Not every module here is independently composable.** Most take only
//! already-resolved colours/callbacks and know nothing of `pages`, but
//! [`panes_sheet`] and [`link_strip`] are the sheet/status-strip's own
//! **page-coupled** exceptions: both import `super::super::pages::Actions`/
//! `TerminalState` directly (a sanctioned pages↔widgets back-edge, the
//! `features::sessions` `sheet_action` precedent — see
//! `docs/FEATURES_ARCHITECTURE.md`'s widgets/ paragraph) rather than taking
//! that state through a plain fn argument.
//!
//! **Two colour vocabularies**: the **card** inks from the framework's
//! brightness-invariant terminal palette ([`card::CardInk`], over `GlyphInk`) plus
//! the two hairline tones no token carries, while everything *outside* it resolves
//! [`crate::ui::palette::Palette`], since that is ordinary app chrome that happens
//! to be about a terminal. The grid's third vocabulary, the ANSI `TerminalPalette`,
//! stays with the emulator (`super::term::style`).

pub(crate) mod card;
pub(crate) mod link_strip;
pub(crate) mod new_tab_dialog;
pub(crate) mod panes;
pub(crate) mod panes_sheet;
pub(crate) mod pills;
pub(crate) mod sheet_bits;
pub(crate) mod status;
pub(crate) mod util;
