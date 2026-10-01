//! The `/terminal` screen's widgets, transcribed from `docs/terminal.html` v2
//! and its own side-sheet design mock. One module per cluster, one fn per
//! visual element: [`card`] the card's `GlyphInk` ink and `.term-body`,
//! [`status`] `.term-statusrow` (title, breadcrumb, read-only chip) and the Tablet kebab's menu, [`link_strip`] the live link band,
//! [`side_sheet_tree`] the "panes & tabs" spaces → tabs → panes containment
//! tree body (the phone's sheet and the Tablet's rail alike), [`rail`] the
//! Tablet's docked rail and mini rail around it, [`new_tab_dialog`] the add-tab prompt, [`util`] the
//! row/column/gap/`TextStyle` helpers.
//!
//! **Page-coupled exception** (see `docs/FEATURES_ARCHITECTURE.md`'s
//! widgets/ paragraph): [`link_strip`] imports `pages::TerminalState` directly
//! (its Retry callback needs screen state). Everything else, [`side_sheet_tree`]
//! included, takes only resolved colours/callbacks — the side sheet's own
//! `Actions`-coupled bridge (building its `TreeData`/`TreeActions` off the
//! controller, and its sticky footer) is `pages.rs`'s own action wiring, and the
//! rail's (`rail_view`, the row-menu dispatch) is `actions.rs`' — not a widgets/ module.
//!
//! **Two colour vocabularies**: the card inks from the brightness-invariant
//! [`card::CardInk`] (over `GlyphInk`); everything else resolves
//! [`crate::ui::palette::Palette`]. The grid's ANSI `TerminalPalette` stays
//! with the emulator (`super::term::style`).

pub(crate) mod card;
pub(crate) mod link_strip;
pub(crate) mod new_tab_dialog;
pub(crate) mod rail;
pub(crate) mod side_sheet_tree;
pub(crate) mod status;
pub(crate) mod util;
