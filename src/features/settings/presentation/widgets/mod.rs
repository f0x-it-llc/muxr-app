//! The settings screens' widget clusters, re-exported flat for `pages` and `app_pages`. Amber
//! marks brand, CTA and selection state only, never health; every mark is a `frust::icons`
//! vector or a single-font-safe text mark, never an emoji.

mod cards;
mod font_picker;
mod groups;
mod identity;
mod misc;
mod pickers;
mod pro;
mod rows;
mod saved_servers;
mod server_sheet;
mod tokens;

pub(super) use cards::{CardColumn, ServerCards, card_grid, server_tablet_scroller};
pub(super) use font_picker::{
    FONT_META_BUILT_IN, FONT_META_DOWNLOADED, FontPicker, FontRow, font_picker_sheet,
};
pub(super) use groups::{
    about_rows, app_settings_rows, appearance_rows, forget_server_row, poll_rows,
    saved_credential_row, token_rows, tokens_row,
};
pub(super) use identity::identity_card;
pub(super) use misc::{DISCONNECT_COPY, DISCONNECT_LABEL, disconnect_button};
pub(super) use pickers::{Picker, PickerKind, picker_sheet};
pub(super) use pro::pro_rows;
pub use pro::{PLAN_LABEL, PRO_GROUP_LABEL};
pub(super) use rows::{RowAction, RowHost, group_label, page_tone, phone_scroller};
pub use saved_servers::{CONNECTED_META, CREDENTIAL_CHIP, SERVERS_GROUP_LABEL, THIS_SERVER_CHIP};
pub(super) use saved_servers::{no_saved_servers_row, saved_server_item};
pub use server_sheet::{
    DELETE_LABEL, FORGET_SERVER_LABEL, SAVED_CREDENTIAL_LABEL, SHEET_FOOTNOTE, SWITCH_LABEL,
};
pub(super) use server_sheet::{ServerSheetActions, SheetAction, server_sheet};
pub(super) use tokens::{create_token_row, open_create_token_dialog, secret_reveal_sheet};
