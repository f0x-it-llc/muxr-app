//! The `/terminal` screen's widgets, one module per visual cluster. Only [`link_strip`] imports
//! `pages::TerminalState` (Retry needs screen state); the rest take resolved colours and callbacks.
//! The card inks from [`card::CardInk`]; everything else resolves [`crate::ui::palette::Palette`].

pub(crate) mod agents_section;
pub(crate) mod card;
pub(crate) mod link_strip;
pub(crate) mod new_tab_dialog;
pub(crate) mod rail;
pub(crate) mod side_sheet_tree;
pub(crate) mod status;
pub(crate) mod util;
