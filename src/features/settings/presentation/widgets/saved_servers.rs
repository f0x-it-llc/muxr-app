//! `delete credential` drops the stored secret and revokes its token, leaving the row dialable;
//! `forget` removes the profile and its credential, filed under the endpoint. The actions sit on
//! their own row because `Flex` neither wraps nor shrinks an inflexible tail on a narrow phone.

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

const SAVED_SERVER_LINE_GAP: f64 = spacing::SM;
/// `.token-scope{padding:2px 7px}` — this card's chips share it.
const CHIP_PAD_X: f64 = 7.0;
const CHIP_PAD_Y: f64 = spacing::MICRO;
const ADD_MARGIN_TOP: f64 = spacing::BASE;
const ADD_MARGIN_BOTTOM: f64 = spacing::TIGHT;
const ADD_PAD: f64 = spacing::MD;
const ADD_GAP: f64 = spacing::XS;

pub(super) const DELETE_CREDENTIAL_LABEL: &str = "delete credential";
pub(super) const FORGET_SERVER_LABEL: &str = "forget";
const CREDENTIAL_CHIP: &str = "credential saved";
const CONNECTED_CHIP: &str = "connected";
const ADD_SERVER_PLUS: &str = "+";
const ADD_SERVER_LABEL: &str = "add server";
const NO_SAVED_SERVERS: &str = "no saved servers";

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

pub(in super::super) fn no_saved_servers_row(colors: &Palette) -> impl View<SettingsState> {
    let theme = active_theme();
    Padding(
        EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
        text(NO_SAVED_SERVERS).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
    )
}

fn chip(label: &'static str, ink: Color, fill: Color) -> impl View<SettingsState> {
    let theme = active_theme();
    container(Padding(
        EdgeInsets::symmetric(CHIP_PAD_X, CHIP_PAD_Y),
        text(label).style(inked(&theme.type_scale.label_small, ink)),
    ))
    .fill(fill)
    .radius(RADIUS_FULL)
}
