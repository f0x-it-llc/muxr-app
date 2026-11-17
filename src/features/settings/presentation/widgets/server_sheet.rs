//! One saved server's actions, as a bottom sheet over app settings: switch, delete its
//! credential, forget it. Page-agnostic: a row borrow plus the three callbacks.

use std::rc::Rc;

use frust::authoring::text::TextOverflow;
use frust::icons::{CHEVRON_RIGHT, CLOSE, LOCK};
use frust::{
    AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, Either, FlexView, GestureDetector,
    IconSource, Padding, SizedBox, View, any, button, divider, flexible, icon, inflexible, text,
};

use crate::AppState;
use crate::features::connect::domain::entities::{SavedServer, epoch_millis};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::groups::tls_posture;
use super::rows::{ROW_GAP, ROW_LINE_GAP, ROW_PAD_Y, centered_row, stretch_column};
use super::saved_servers::status_dot;

const HEADER_GAP: f64 = spacing::SM;
const SECTION_GAP: f64 = spacing::XL;
const MARK_SLOT: f64 = 28.0;
const MARK_ICON: f64 = spacing::LG;
const CHEVRON: f64 = 13.0;

pub const SWITCH_LABEL: &str = "Switch to this server";
pub const SAVED_CREDENTIAL_LABEL: &str = "saved credential";
const SAVED_CREDENTIAL_VALUE: &str = "on this device";
pub const DELETE_LABEL: &str = "delete";
pub const FORGET_SERVER_LABEL: &str = "forget server";
const FORGET_SERVER_VALUE: &str = "remove the profile and its credential";
pub const SHEET_FOOTNOTE: &str =
    "Tokens and server details open from server settings once connected.";

pub(in super::super) type SheetAction = Rc<dyn Fn(&mut AppState)>;

pub(in super::super) struct ServerSheetActions {
    pub on_switch: SheetAction,
    pub on_delete_credential: SheetAction,
    pub on_forget: SheetAction,
}

/// `endpoint · trust posture · last used`.
fn sheet_meta(server: &SavedServer) -> String {
    let (posture, _) = tls_posture(server.trust_mode);
    format!(
        "{} \u{b7} {posture} \u{b7} {}",
        server.endpoint(),
        server.last_used_label(epoch_millis())
    )
}

/// `server` is `None` once the row left the store, so the closing sheet has nothing to act on.
pub(in super::super) fn server_sheet(
    server: Option<&SavedServer>,
    colors: &Palette,
    actions: &ServerSheetActions,
) -> impl View<AppState> + use<> {
    let theme = active_theme();
    let Some(server) = server else {
        return Either::Left(
            text(SHEET_FOOTNOTE).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
        );
    };
    let header = centered_row(vec![
        inflexible(status_dot::<AppState>(false, colors)),
        flexible(
            1,
            text(server.label.clone())
                .style(inked(&theme.type_scale.title_small_emphasized, colors.fg))
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ),
    ])
    .gap(ROW_GAP);

    let on_switch = Rc::clone(&actions.on_switch);
    let switch = button(SWITCH_LABEL, move |state: &mut AppState| on_switch(state))
        .style(ButtonStyle::Primary);

    let credential = if server.has_credential {
        let on_delete = Rc::clone(&actions.on_delete_credential);
        let delete = button(DELETE_LABEL, move |state: &mut AppState| on_delete(state))
            .style(ButtonStyle::Danger)
            .small();
        Some(action_row(
            LOCK,
            SAVED_CREDENTIAL_LABEL,
            SAVED_CREDENTIAL_VALUE,
            false,
            any(delete),
            colors,
        ))
    } else {
        None
    };

    let chevron = icon(CHEVRON_RIGHT).size(CHEVRON).color(colors.fg_faintest);
    let on_forget = Rc::clone(&actions.on_forget);
    let forget = GestureDetector(action_row(
        CLOSE,
        FORGET_SERVER_LABEL,
        FORGET_SERVER_VALUE,
        true,
        any(chevron),
        colors,
    ))
    .on_tap(move |state: &mut AppState| on_forget(state));

    let mut body: Vec<AnyView<AppState>> = vec![
        any(header),
        any(text(sheet_meta(server))
            .style(inked(&theme.type_scale.label_small, colors.fg_dim))
            .max_lines(1)
            .overflow(TextOverflow::Ellipsis)),
        any(SizedBox::<AppState>(None, Some(SECTION_GAP - HEADER_GAP))),
        any(switch),
    ];
    body.extend(credential.map(any));
    body.push(any(forget));
    body.push(any(SizedBox::<AppState>(
        None,
        Some(SECTION_GAP - HEADER_GAP),
    )));
    body.push(any(
        text(SHEET_FOOTNOTE).style(inked(&theme.type_scale.label_small, colors.fg_dim))
    ));
    Either::Right(
        FlexView::new(Axis::Vertical, body.into_iter().map(inflexible).collect())
            .cross_axis(CrossAxisAlignment::Stretch)
            .gap(HEADER_GAP),
    )
}

fn action_row(
    mark: IconSource,
    label: &'static str,
    value: &'static str,
    danger: bool,
    trailing: AnyView<AppState>,
    colors: &Palette,
) -> impl View<AppState> + use<> {
    let theme = active_theme();
    let tone = if danger {
        colors.error
    } else {
        colors.fg_muted
    };
    let label_ink = if danger { colors.error } else { colors.fg };
    let body = stretch_column((
        text(label).style(inked(&theme.type_scale.label_large, label_ink)),
        text(value).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
    ))
    .gap(ROW_LINE_GAP);
    stretch_column((
        Padding(
            EdgeInsets::symmetric(0.0, ROW_PAD_Y),
            centered_row(vec![
                inflexible(
                    SizedBox::<AppState>(Some(MARK_SLOT), None)
                        .child(icon(mark).size(MARK_ICON).color(tone)),
                ),
                flexible(1, body),
                inflexible(trailing),
            ])
            .gap(ROW_GAP),
        ),
        divider(colors.rule),
    ))
}
