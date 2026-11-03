//! The `saved servers` group: `connect`'s own remembered-server list, three
//! rows per card (identity + chips, endpoint + recency, then the row's own
//! delete actions on a line of their own), closed by a dashed `+ add server`
//! row that pushes the add-server page.
//!
//! The connected server's card wears a `connected` chip; every other card's
//! identity and meta lines are one tap target that switches to it (the
//! actions line keeps its own buttons, outside that target).
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

use frust::authoring::{Color, DashPattern};
use frust::{
    Align, Alignment, BorderStyle, ButtonStyle, EdgeInsets, Either, GestureDetector, Padding,
    SizedBox, View, button, container, divider, flexible, inflexible, text,
};

use crate::features::connect::domain::entities::{SavedServer, epoch_millis};
use crate::features::settings::presentation::controller::SavedServerRow;
use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::paint::{DASH_OFF, DASH_ON, HAIRLINE};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::rows::{
    RADIUS_FULL, ROW_GAP, ROW_PAD_X, ROW_PAD_Y, centered_row, h_gap, spacer, stretch_column,
};

/// The vertical rhythm inside this card's three rows — modeled on the
/// sessions screen's own taller-card precedent (`CARD_LINE_GAP`, 8px) rather
/// than the tighter two-line token row, since the actions row needs room of
/// its own to read as separate from the meta line above it.
const SAVED_SERVER_LINE_GAP: f64 = spacing::SM;
/// `.token-scope{padding:2px 7px}` — this card's chips share it.
const CHIP_PAD_X: f64 = 7.0;
const CHIP_PAD_Y: f64 = spacing::MICRO;
/// The `+ add server` row's box: the `create new token` row's own margins,
/// padding and `+`-to-label gap, so the two dashed rows read as one idiom.
const ADD_MARGIN_TOP: f64 = spacing::BASE;
const ADD_MARGIN_BOTTOM: f64 = spacing::TIGHT;
const ADD_PAD: f64 = spacing::MD;
const ADD_GAP: f64 = spacing::XS;

/// The saved-server row's two actions — also how a test finds each button.
pub(super) const DELETE_CREDENTIAL_LABEL: &str = "delete credential";
pub(super) const FORGET_SERVER_LABEL: &str = "forget";
/// The chip a saved row carries while a biometric-gated credential is stored
/// for it (the Flutter settings row's own words).
const CREDENTIAL_CHIP: &str = "credential saved";
/// The chip on the connected server's card.
const CONNECTED_CHIP: &str = "connected";
/// The trailing row's mark and label.
const ADD_SERVER_PLUS: &str = "+";
const ADD_SERVER_LABEL: &str = "add server";
/// What the `saved servers` group renders with nothing in it.
const NO_SAVED_SERVERS: &str = "no saved servers";

/// One row of the group: a server card, or the closing `+ add server` row.
pub(in super::super) fn saved_server_item(
    row: &SavedServerRow,
    colors: &Palette,
) -> impl View<SettingsState> + use<> {
    match row {
        SavedServerRow::Server { server, active } => {
            Either::Left(server_card(server, *active, colors))
        }
        SavedServerRow::AddServer => Either::Right(add_server_row(colors)),
    }
}

/// A remembered server's card — see the [module docs](self).
fn server_card(
    server: &SavedServer,
    active: bool,
    colors: &Palette,
) -> impl View<SettingsState> + use<> {
    let theme = active_theme();
    let endpoint = server.endpoint();

    let mut identity = vec![flexible(
        1,
        text(server.label.clone()).style(inked(&theme.type_scale.body_small, colors.fg)),
    )];
    if active {
        identity.push(h_gap(ROW_GAP));
        identity.push(inflexible(chip(
            CONNECTED_CHIP,
            colors.success,
            colors.success_faint,
        )));
    }
    if server.has_credential {
        identity.push(h_gap(ROW_GAP));
        identity.push(inflexible(chip(
            CREDENTIAL_CHIP,
            colors.cyan,
            colors.cyan_faint,
        )));
    }

    let meta_text = format!("{} · {}", endpoint, server.last_used_label(epoch_millis()));
    let meta = centered_row(vec![flexible(
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

    let summary = stretch_column((
        centered_row(identity),
        SizedBox::<SettingsState>(None, Some(SAVED_SERVER_LINE_GAP)),
        meta,
    ));
    // The connected card switches nowhere; every other one switches to itself.
    let summary = if active {
        Either::Left(summary)
    } else {
        Either::Right(
            GestureDetector(summary)
                .on_tap(move |state: &mut SettingsState| pages::switch_server(state, &endpoint)),
        )
    };

    stretch_column((
        Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            stretch_column((
                summary,
                SizedBox::<SettingsState>(None, Some(SAVED_SERVER_LINE_GAP)),
                centered_row(actions),
            )),
        ),
        divider(colors.rule),
    ))
}

/// The dashed `+ add server` row, in the `create new token` row's idiom.
fn add_server_row(colors: &Palette) -> impl View<SettingsState> + use<> {
    let theme = active_theme();
    let label = centered_row(vec![
        inflexible(text(ADD_SERVER_PLUS).style(inked(&theme.type_scale.body_small, colors.amber))),
        h_gap(ADD_GAP),
        inflexible(
            text(ADD_SERVER_LABEL).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        ),
    ]);
    Padding(
        EdgeInsets {
            left: ROW_PAD_X,
            top: ADD_MARGIN_TOP,
            right: ROW_PAD_X,
            bottom: ADD_MARGIN_BOTTOM,
        },
        GestureDetector(
            container(Padding(
                EdgeInsets::all(ADD_PAD),
                Align(Alignment::CENTER, label),
            ))
            .radius(theme.shape.small)
            .border(colors.outline, HAIRLINE)
            .border_style(BorderStyle::Dashed(DashPattern::new(DASH_ON, DASH_OFF))),
        )
        .on_tap(|state: &mut SettingsState| pages::open_add_server(state)),
    )
}

/// The `saved servers` group with nothing in it — a plain muted line, in the
/// same box a row would occupy.
pub(in super::super) fn no_saved_servers_row(colors: &Palette) -> impl View<SettingsState> {
    let theme = active_theme();
    Padding(
        EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
        text(NO_SAVED_SERVERS).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
    )
}

/// A card chip — hand-composed like a token's scope chip: `credential saved`
/// in the cyan/info hue that chip reserves for a fact about a credential,
/// `connected` in the success hue.
fn chip(label: &'static str, ink: Color, fill: Color) -> impl View<SettingsState> {
    let theme = active_theme();
    container(Padding(
        EdgeInsets::symmetric(CHIP_PAD_X, CHIP_PAD_Y),
        text(label).style(inked(&theme.type_scale.label_small, ink)),
    ))
    .fill(fill)
    .radius(RADIUS_FULL)
}
