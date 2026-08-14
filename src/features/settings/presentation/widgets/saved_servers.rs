//! The `saved servers` group: `connect`'s own remembered-server list, three
//! rows per card (identity + credential chip, endpoint + recency, then the
//! row's own delete actions on a line of their own).
//!
//! The label/meta rows are `flexible` rather than truncated with an ellipsis
//! (frust's `text` carries no overflow vocabulary at all —
//! `features::terminal::presentation::chrome`'s own module docs record the
//! same gap), and the two delete actions sit on their own row rather than the
//! meta row's trailing edge: an inflexible tail there clipped off a narrow
//! phone's right edge, since `Flex` neither wraps nor shrinks an inflexible
//! child.
//!
//! Two actions, because they are two different deletions a user asking for
//! one rarely means the other: `delete credential` (only where one is
//! stored) drops the stored secret and revokes the token behind it, leaving
//! the row dialable by hand; `forget` removes the whole profile — and its
//! credential with it, since the endpoint is the key that secret is filed
//! under.

use frust::authoring::Color;
use frust::{
    AnyView, ButtonStyle, EdgeInsets, Padding, SizedBox, any, button, flexible, inflexible, text,
};

use crate::features::connect::domain::entities::{SavedServer, epoch_millis};
use crate::features::settings::presentation::chrome::{RADIUS_FULL, panel};
use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::rows::{ROW_GAP, ROW_PAD_X, ROW_PAD_Y, column, h_gap, row, spacer};

/// The vertical rhythm inside this card's three rows — modeled on the
/// sessions screen's own taller-card precedent (`CARD_LINE_GAP`, 8px) rather
/// than the tighter two-line token row, since the actions row needs room of
/// its own to read as separate from the meta line above it.
const SAVED_SERVER_LINE_GAP: f64 = spacing::SM;
/// `.token-scope{padding:2px 7px}` — this card's credential chip shares it.
const CHIP_PAD_X: f64 = 7.0;
const CHIP_PAD_Y: f64 = spacing::MICRO;

/// The saved-server row's two actions — also how a test finds each button.
pub(super) const DELETE_CREDENTIAL_LABEL: &str = "delete credential";
pub(super) const FORGET_SERVER_LABEL: &str = "forget";
/// The chip a saved row carries while a biometric-gated credential is stored
/// for it (the Flutter settings row's own words).
const CREDENTIAL_CHIP: &str = "credential saved";
/// What the `saved servers` group renders with nothing in it.
const NO_SAVED_SERVERS: &str = "no saved servers";

pub(in super::super) fn saved_server_item(
    server: &SavedServer,
    colors: &Palette,
) -> AnyView<SettingsState> {
    let theme = active_theme();
    let endpoint = server.endpoint();

    let mut identity = vec![flexible(
        1,
        text(server.label.clone()).style(inked(&theme.type_scale.body_small, colors.fg)),
    )];
    if server.has_credential {
        identity.push(h_gap(ROW_GAP));
        identity.push(inflexible(credential_chip(colors)));
    }

    let meta_text = format!("{} · {}", endpoint, server.last_used_label(epoch_millis()));
    let meta = row(vec![flexible(
        1,
        text(meta_text).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
    )]);

    let mut actions = vec![spacer()];
    if server.has_credential {
        let key = endpoint.clone();
        actions.push(inflexible(
            button(DELETE_CREDENTIAL_LABEL, move |state: &mut SettingsState| {
                pages::open_delete_credential_dialog(state, &key);
            })
            .style(ButtonStyle::Danger)
            .small(),
        ));
        actions.push(h_gap(ROW_GAP));
    }
    let key = endpoint.clone();
    actions.push(inflexible(
        button(FORGET_SERVER_LABEL, move |state: &mut SettingsState| {
            pages::open_forget_server_dialog(state, &key);
        })
        .style(ButtonStyle::Ghost)
        .small(),
    ));

    any(panel(
        Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            column(vec![
                any(row(identity)),
                any(SizedBox::<SettingsState>(None, Some(SAVED_SERVER_LINE_GAP))),
                any(meta),
                any(SizedBox::<SettingsState>(None, Some(SAVED_SERVER_LINE_GAP))),
                any(row(actions)),
            ]),
        ),
        Color::TRANSPARENT,
        0.0,
    )
    .bottom_rule(colors.rule))
}

/// The `saved servers` group with nothing in it — a plain muted line, in the
/// same box a row would occupy.
pub(in super::super) fn no_saved_servers_row(colors: &Palette) -> AnyView<SettingsState> {
    let theme = active_theme();
    any(Padding(
        EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
        text(NO_SAVED_SERVERS).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
    ))
}

/// The `credential saved` pill — hand-composed like a token's scope chip, in
/// the cyan/info hue that chip reserves for a fact about a credential rather
/// than about a token's power.
fn credential_chip(colors: &Palette) -> AnyView<SettingsState> {
    let theme = active_theme();
    any(panel(
        Padding(
            EdgeInsets::symmetric(CHIP_PAD_X, CHIP_PAD_Y),
            text(CREDENTIAL_CHIP).style(inked(&theme.type_scale.label_small, colors.cyan)),
        ),
        colors.cyan_faint,
        RADIUS_FULL,
    ))
}
