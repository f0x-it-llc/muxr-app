//! App settings' `servers` group: one compact row per saved server, the whole row its press
//! target. The connected row opens server settings; any other opens its sheet.

use frust::authoring::DashPattern;
use frust::authoring::text::TextOverflow;
use frust::icons::CHEVRON_RIGHT;
use frust::{
    Align, Alignment, AnyView, BorderStyle, EdgeInsets, GestureDetector, Padding, View, any,
    colored_box, container, divider, flexible, icon, inflexible, text,
};
use frust_glyph::{BadgeVariant, badge};

use crate::features::connect::domain::entities::{SavedServer, epoch_millis};
use crate::features::settings::presentation::app_pages::{self, AppSettingsState};
use crate::features::settings::presentation::controller::SavedServerRow;
use crate::ui::paint::{DASH_OFF, DASH_ON, GLOW_SPREAD, GLOW_STD_DEV, HAIRLINE, RADIUS_FULL};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::rows::{ROW_GAP, ROW_LINE_GAP, ROW_PAD_X, ROW_PAD_Y, centered_row, stretch_column};

const STATUS_DOT: f64 = 7.0;
/// `.set-chev{font-size:13px}`, as on every other settings row.
const CHEVRON: f64 = 13.0;
const ADD_MARGIN_TOP: f64 = spacing::BASE;
const ADD_MARGIN_BOTTOM: f64 = spacing::TIGHT;
const ADD_PAD: f64 = spacing::MD;
const ADD_GAP: f64 = spacing::XS;

pub const SERVERS_GROUP_LABEL: &str = "servers";
pub const THIS_SERVER_CHIP: &str = "this server";
pub const CREDENTIAL_CHIP: &str = "credential";
pub const CONNECTED_META: &str = "connected";
const ADD_SERVER_PLUS: &str = "+";
const ADD_SERVER_LABEL: &str = "add server";
const NO_SAVED_SERVERS: &str = "no saved servers";

pub(in super::super) fn saved_server_item(
    row: &SavedServerRow,
    colors: &Palette,
) -> AnyView<AppSettingsState> // erasure: keep accumulator into Vec<AnyView>
{
    match row {
        SavedServerRow::Server { server, active } => any(server_row(server, *active, colors)),
        SavedServerRow::AddServer => any(add_server_row(colors)),
    }
}

/// Green with a glow for the connected server, a dim dot otherwise.
pub(super) fn status_dot<S: 'static>(live: bool, colors: &Palette) -> AnyView<S> {
    let dot = colored_box::<S>()
        .size(STATUS_DOT, STATUS_DOT)
        .radius(RADIUS_FULL);
    if live {
        any(dot
            .fill(colors.success)
            .glow(colors.success, GLOW_STD_DEV, GLOW_SPREAD))
    } else {
        any(dot.fill(colors.outline))
    }
}

fn server_row(
    server: &SavedServer,
    connected: bool,
    colors: &Palette,
) -> impl View<AppSettingsState> + use<> {
    let theme = active_theme();
    let endpoint = server.endpoint();
    let state = if connected {
        CONNECTED_META.to_string()
    } else {
        server.last_used_label(epoch_millis())
    };
    let body = stretch_column((
        text(server.label.clone())
            .style(inked(&theme.type_scale.label_large, colors.fg))
            .max_lines(1)
            .overflow(TextOverflow::Ellipsis),
        text(format!("{endpoint} \u{b7} {state}"))
            .style(inked(&theme.type_scale.label_small, colors.fg_dim))
            .max_lines(1)
            .overflow(TextOverflow::Ellipsis),
    ))
    .gap(ROW_LINE_GAP);

    let mut children = vec![inflexible(status_dot(connected, colors)), flexible(1, body)];
    if connected {
        children.push(inflexible(badge(THIS_SERVER_CHIP, BadgeVariant::Success)));
    } else if server.has_credential {
        children.push(inflexible(badge(CREDENTIAL_CHIP, BadgeVariant::Info)));
    }
    children.push(inflexible(
        icon(CHEVRON_RIGHT).size(CHEVRON).color(colors.fg_faintest),
    ));

    GestureDetector(stretch_column((
        Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            centered_row(children).gap(ROW_GAP),
        ),
        divider(colors.rule),
    )))
    .on_tap(move |state: &mut AppSettingsState| app_pages::open_saved_server(state, &endpoint))
}

fn add_server_row(colors: &Palette) -> impl View<AppSettingsState> + use<> {
    let theme = active_theme();
    let label = centered_row(vec![
        inflexible(text(ADD_SERVER_PLUS).style(inked(&theme.type_scale.body_small, colors.amber))),
        inflexible(
            text(ADD_SERVER_LABEL).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        ),
    ])
    .gap(ADD_GAP);
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
        .on_tap(|state: &mut AppSettingsState| app_pages::open_add_server(state)),
    )
}

pub(in super::super) fn no_saved_servers_row(colors: &Palette) -> AnyView<AppSettingsState> {
    let theme = active_theme();
    any(Padding(
        EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
        text(NO_SAVED_SERVERS).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
    ))
}
