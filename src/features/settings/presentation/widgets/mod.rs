//! The `/settings` screen's widget clusters, transcribed from
//! `docs/redesign.html`'s **settings** block (screen 5) and its **picker
//! sheet** (screen 6) — arranged, not resolved, by
//! [`pages`](super::pages)'s `Component::build`. Page-coupled by design:
//! these modules read the page's state type and call its `pages::open_*`
//! shims directly (the screen-state form of the sanctioned pages↔widgets
//! back-edge — see `docs/FEATURES_ARCHITECTURE.md`'s widgets/ paragraph).
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
//! | [`pro`] | the `muxr pro` group: the plan row, which opens the paywall short of Pro |
//! | [`cards`] | the Tablet card grid: group cards, the Disconnect card, the gap helper |
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
pub(super) use misc::disconnect_button;
pub(super) use pickers::{Picker, PickerKind, picker_sheet};
pub(super) use pro::pro_rows;
// Test-visible: `tests/settings.rs` imports these directly rather than
// hard-coding the row's own literals (the same `pub` re-export shape
// `features::pro::presentation::widgets` already uses for its own
// test-facing constants).
pub use pro::{PLAN_LABEL, PRO_GROUP_LABEL};
pub(super) use rows::{RowAction, SCROLL_PAD_BOTTOM, SCROLL_PAD_TOP, group_label, page_tone};
pub(super) use saved_servers::{no_saved_servers_row, saved_server_item};
pub(super) use tokens::{create_token_row, open_create_token_dialog, secret_reveal_sheet};
