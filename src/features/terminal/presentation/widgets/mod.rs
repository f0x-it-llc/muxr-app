//! The `/terminal` screen's widgets, transcribed from `docs/terminal.html` v2.
//! One module per cluster, one fn per visual element: [`card`] the card's
//! `GlyphInk` ink and `.term-body`, [`status`] `.term-statusrow`,
//! [`link_strip`] the live link band, [`pills`] `.pill` plus its edit
//! affordance, [`panes`] `.pane-item`/`.new-pane-btn`, [`sheet_bits`] the
//! sheet's top row/labels/scroll region, [`panes_sheet`] `#pickerSheet`
//! itself, [`new_tab_dialog`] the add-tab prompt, [`util`] the
//! row/column/gap/`TextStyle` helpers.
//!
//! **Page-coupled exceptions** (see `docs/FEATURES_ARCHITECTURE.md`'s
//! widgets/ paragraph): [`panes_sheet`] imports `pages::Actions` (an
//! overlay-host page, callbacks run over `&mut AppState`); [`link_strip`]
//! imports `pages::TerminalState` directly (its Retry callback needs screen
//! state). Everything else takes only resolved colours/callbacks.
//!
//! **Two colour vocabularies**: the card inks from the brightness-invariant
//! [`card::CardInk`] (over `GlyphInk`); everything else resolves
//! [`crate::ui::palette::Palette`]. The grid's ANSI `TerminalPalette` stays
//! with the emulator (`super::term::style`).

pub(crate) mod card;
pub(crate) mod link_strip;
pub(crate) mod new_tab_dialog;
pub(crate) mod panes;
pub(crate) mod panes_sheet;
pub(crate) mod pills;
pub(crate) mod sheet_bits;
pub(crate) mod status;
pub(crate) mod util;
