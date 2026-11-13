//! The `/settings` screen's widget clusters, re-exported flat for `pages`. Amber marks
//! brand, CTA and selection state only, never health; every mark is a `frust::icons` vector
//! or a single-font-safe text mark, never an emoji.

mod cards;
mod font_picker;
mod groups;
mod misc;
mod pickers;
mod pro;
mod rows;
mod saved_servers;
mod tokens;

pub(super) use cards::tablet_scroller;
pub(super) use font_picker::{
    FONT_META_BUILT_IN, FONT_META_DOWNLOADED, FontPicker, FontRow, font_picker_sheet,
};
pub(super) use groups::{appearance_rows, connection_rows, poll_rows, token_rows};
pub(super) use misc::{DISCONNECT_COPY, DISCONNECT_LABEL, disconnect_button};
pub(super) use pickers::{Picker, PickerKind, picker_sheet};
pub(super) use pro::pro_rows;
pub use pro::{PLAN_LABEL, PRO_GROUP_LABEL};
pub(super) use rows::{RowAction, SCROLL_PAD_BOTTOM, SCROLL_PAD_TOP, group_label, page_tone};
pub(super) use saved_servers::{no_saved_servers_row, saved_server_item};
pub(super) use tokens::{create_token_row, open_create_token_dialog, secret_reveal_sheet};
