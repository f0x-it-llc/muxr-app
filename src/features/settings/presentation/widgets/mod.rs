//! The `/settings` screen's individually-composable pieces, transcribed from
//! `docs/redesign.html`'s **settings** block (screen 5) and its **picker
//! sheet** (screen 6) — arranged, not resolved, by
//! [`pages`](super::pages)'s `Component::build`.
//!
//! One file per cluster, re-exported flat here so `pages` reads
//! `widgets::foo(..)` exactly as it used to read the single-file `pieces`
//! module:
//!
//! | Module | Owns |
//! |---|---|
//! | [`rows`] | `.set-row`/`.set-group-lbl` and the shared layout helpers every other module draws with |
//! | [`tokens`] | `.token-item`, the create dialog, the one-time secret reveal |
//! | [`pickers`] | screen 6's picker sheet (`layout refresh`/`font size`/`cursor style`) |
//! | [`font_picker`] | the two-section font-family sheet, reusing `pickers`' row idiom |
//! | [`saved_servers`] | the `saved servers` group, over `connect`'s own list |
//! | [`groups`] | the four groups' row lists, pure arrangement over the above |
//! | [`misc`] | the bell row and Disconnect — the two rows outside every other cluster |
//!
//! # Colour and marks
//!
//! Amber appears only as brand/CTA/selection state — the create row's `+`
//! and a picker's selected row — never as a health signal: the endpoint
//! reads `success`, `insecure (plaintext)` reads `warning` (the redesign's
//! own correction — it used to read as a success state), revoke/Disconnect
//! read `error`. Every mark is a vendored `frust::icons` vector or a
//! single-font-safe text mark — **never an emoji** (`widgets::rows`' own
//! doc has the tofu-on-iPhone story).

mod font_picker;
mod groups;
mod misc;
mod pickers;
mod rows;
mod saved_servers;
mod tokens;

pub(super) use font_picker::{
    FONT_META_BUILT_IN, FONT_META_DOWNLOADED, FontPicker, FontRow, font_picker_sheet,
};
pub(super) use groups::{appearance_rows, connection_rows, poll_rows, token_rows};
pub(super) use misc::disconnect_button;
pub(super) use pickers::{Picker, PickerKind, picker_sheet};
pub(super) use rows::{RowAction, SCROLL_PAD_BOTTOM, SCROLL_PAD_TOP, group_label, page_tone};
pub(super) use saved_servers::{no_saved_servers_row, saved_server_item};
pub(super) use tokens::{create_token_row, open_create_token_dialog, secret_reveal_sheet};
