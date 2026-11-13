//! A minted token's secret is shown once, through [`secret_reveal_sheet`], and never persisted.
//! On a read-only session the mint and revoke controls stay reachable but styled down (frust's
//! `Button` has no disabled state); the caller's own guard blocks the action and toasts.

use frust::authoring::DashPattern;
use frust::{
    Align, Alignment, AnyView, BorderStyle, ButtonStyle, CrossAxisAlignment, EdgeInsets, Either,
    GestureDetector, Get, GetUntracked, NavigatorController, Padding, PopResult, RwSignal, Set,
    SizedBox, View, any, button, column, container, divider, flexible, inflexible, text,
    text_input,
};
use frust_glyph::{glyph_dialog, show_glyph_dialog, toggle};

use crate::AppState;
use crate::features::settings::domain::entities::{AuthToken, MintedToken, TokenScope};
use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::features::settings::presentation::scramble::{self, ScramblePose};
use crate::ui::paint::{DASH_OFF, DASH_ON};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;
use crate::ui::toasts::app_toasts;

use super::pickers::PICK_TITLE_GAP;
use super::rows::{
    HAIRLINE, RADIUS_FULL, ROW_GAP, ROW_LINE_GAP, ROW_PAD_X, ROW_PAD_Y, centered_row, h_gap,
    spacer, stretch_column,
};

/// `.token-top{margin-bottom:4px}`.
const TOKEN_LINE_GAP: f64 = spacing::TIGHT;
/// `.token-scope{padding:2px 7px}`.
const CHIP_PAD_X: f64 = 7.0;
const CHIP_PAD_Y: f64 = spacing::MICRO;
/// `.add-token-btn{margin:12px 16px 4px; padding:10px}`.
const ADD_MARGIN_TOP: f64 = spacing::BASE;
const ADD_MARGIN_BOTTOM: f64 = spacing::TIGHT;
const ADD_PAD: f64 = spacing::MD;
const ADD_GAP: f64 = spacing::XS;
const DIALOG_FIELD_GAP: f64 = spacing::XL;

pub(super) const CREATE_TOKEN_PLUS: &str = "+";
pub(super) const CREATE_TOKEN_LABEL: &str = "create new token";
pub(super) const REVOKE_LABEL: &str = "revoke";
const CREATE_TOKEN_TITLE: &str = "New token";
const DIALOG_CANCEL_LABEL: &str = "Cancel";
const DIALOG_SUBMIT_LABEL: &str = "Save";
pub(super) const READ_ONLY_TOGGLE_LABEL: &str = "read-only";
pub(super) const SECRET_REVEAL_TITLE: &str = "token secret";
const COPY_SECRET_LABEL: &str = "Copy secret";
const SECRET_DONE_LABEL: &str = "Done";
const SECRET_CAPTION: &str =
    "This is the only time this secret is shown. Copy it now — it can't be viewed again.";
const SECRET_ALREADY_DISMISSED: &str = "This secret has already been dismissed.";
/// Duplicated, since a feature never imports a sibling's `presentation`.
const COPIED_SECRET_TOAST: &str = "Copied to clipboard";
const COPY_SECRET_FAILED_TOAST: &str = "Couldn't copy to clipboard";

pub(super) fn token_item(
    token: &AuthToken,
    colors: &Palette,
    revoke_enabled: bool,
) -> AnyView<SettingsState> // erasure: keep accumulator into Vec<AnyView>
{
    let theme = active_theme();
    let name = token.name.clone();
    let name_line = text(token.name.clone()).style(inked(&theme.type_scale.body_small, colors.fg));
    let revoke = button(REVOKE_LABEL, move |state: &mut SettingsState| {
        pages::open_revoke_dialog(state, &name)
    })
    .style(if revoke_enabled {
        ButtonStyle::Danger
    } else {
        ButtonStyle::Ghost
    })
    .small();
    any(token_row(name_line, token, revoke, None, colors))
}

/// `token` is the scramble's own snapshot. The revoke button stays as a handlerless stand-in,
/// so the row neither jumps when the animation starts nor offers a second confirmation.
pub(super) fn scrambling_token_item(
    token: &AuthToken,
    pose: &ScramblePose,
    colors: &Palette,
) -> AnyView<SettingsState> // erasure: keep accumulator into Vec<AnyView>
{
    let theme = active_theme();
    let ink = if pose.glitching {
        colors.error
    } else {
        colors.fg
    };
    let name_line = text(pose.display.clone()).style(inked(&theme.type_scale.body_small, ink));
    let revoke = button(REVOKE_LABEL, |_state: &mut SettingsState| {})
        .style(ButtonStyle::Danger)
        .small();
    // The ticker rides inside the row it drives, so the scroller's paint
    // culling can never leave one running without the other.
    let ticker = pose.running.then(scramble::frame_ticker);
    let row = token_row(name_line, token, revoke, ticker, colors);
    any(scramble::collapsing(pose.ratio, row))
}

fn token_row(
    name_line: impl View<SettingsState>,
    token: &AuthToken,
    revoke: impl View<SettingsState>,
    tail: Option<AnyView<SettingsState>>,
    colors: &Palette,
) -> impl View<SettingsState> {
    let theme = active_theme();
    let top = centered_row(vec![
        flexible(1, name_line),
        h_gap(ROW_GAP),
        inflexible(scope_chip(token.scope, colors)),
    ]);
    let meta = centered_row(vec![
        inflexible(
            text(token.created_label()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
        ),
        spacer(),
        inflexible(revoke),
    ]);
    // erasure: keep accumulator into Vec<AnyView>
    let mut lines = vec![
        any(top),
        any(SizedBox::<SettingsState>(None, Some(TOKEN_LINE_GAP))),
        any(meta),
    ];
    lines.extend(tail);
    stretch_column((
        Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            stretch_column(lines),
        ),
        divider(colors.rule),
    ))
}

/// Hand-composed: `glyph::badge` has no cyan/info variant for the read-only scope.
fn scope_chip(scope: TokenScope, colors: &Palette) -> impl View<SettingsState> {
    let theme = active_theme();
    let (fill, ink, border) = match scope {
        TokenScope::ReadWrite => (colors.raised, colors.fg_muted, Some(colors.outline)),
        TokenScope::ReadOnly => (colors.cyan_faint, colors.cyan, None),
    };
    let mut chip = container(Padding(
        EdgeInsets::symmetric(CHIP_PAD_X, CHIP_PAD_Y),
        text(scope.label()).style(inked(&theme.type_scale.label_small, ink)),
    ))
    .fill(fill)
    .radius(RADIUS_FULL);
    if let Some(border) = border {
        chip = chip.border(border, HAIRLINE);
    }
    chip
}

pub(in super::super) fn create_token_row(
    colors: &Palette,
    enabled: bool,
) -> impl View<SettingsState> {
    let theme = active_theme();
    let (plus_ink, label_ink) = if enabled {
        (colors.amber, colors.fg_dim)
    } else {
        (colors.fg_faintest, colors.fg_faintest)
    };
    let label = centered_row(vec![
        inflexible(text(CREATE_TOKEN_PLUS).style(inked(&theme.type_scale.body_small, plus_ink))),
        h_gap(ADD_GAP),
        inflexible(text(CREATE_TOKEN_LABEL).style(inked(&theme.type_scale.body_small, label_ink))),
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
        .on_tap(|state: &mut SettingsState| pages::open_create_token_dialog(state)),
    )
}

/// `on_submit` gets the trimmed, non-empty name; every other dismissal closes without a callback.
pub(in super::super) fn open_create_token_dialog(
    overlays: &NavigatorController<AppState>,
    on_submit: impl Fn(&mut AppState, String, bool) + 'static,
) {
    let name = RwSignal::new(String::new());
    let read_only = RwSignal::new(false);
    let page_nav = overlays.clone();
    show_glyph_dialog(
        overlays,
        move || {
            let theme = active_theme();
            let colors = Palette::from_theme(&theme);
            let current = name.get();
            let ro = read_only.get();
            let can_submit = create_token_submittable(&current);
            let enter_nav = page_nav.clone();
            let submit_nav = page_nav.clone();
            let cancel_nav = page_nav.clone();
            let submit_current = current.clone();
            let field = text_input(current, move |_state: &mut AppState, next: String| {
                name.set(next)
            })
            .on_submit(move |_state: &mut AppState, typed: String| {
                submit_create_token(&enter_nav, &typed, read_only.get_untracked())
            });
            let toggle = dialog_read_only_row(ro, &colors, move |_state: &mut AppState| {
                read_only.set(!read_only.get_untracked())
            });
            glyph_dialog()
                .title(CREATE_TOKEN_TITLE)
                .content(
                    column()
                        .child(field)
                        .child(SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP)))
                        .child(toggle)
                        .cross_axis(CrossAxisAlignment::Stretch),
                )
                .action(
                    button(DIALOG_CANCEL_LABEL, move |_state: &mut AppState| {
                        cancel_nav.pop()
                    })
                    .style(ButtonStyle::Ghost)
                    .small(),
                )
                .action(
                    button(DIALOG_SUBMIT_LABEL, move |_state: &mut AppState| {
                        if can_submit {
                            submit_create_token(
                                &submit_nav,
                                &submit_current,
                                read_only.get_untracked(),
                            );
                        }
                    })
                    .style(if can_submit {
                        ButtonStyle::Primary
                    } else {
                        ButtonStyle::Ghost
                    })
                    .small(),
                )
        },
        move |state: &mut AppState, result: PopResult| {
            if let Some((typed, read_only)) = result.take::<(String, bool)>() {
                on_submit(state, typed, read_only);
            }
        },
    );
}

fn create_token_submittable(text: &str) -> bool {
    !text.trim().is_empty()
}

fn submit_create_token(nav: &NavigatorController<AppState>, typed: &str, read_only: bool) {
    let name = typed.trim();
    if name.is_empty() {
        return;
    }
    nav.pop_with_result(PopResult::of((name.to_string(), read_only)));
}

/// No outer `GestureDetector`: it and the toggle both capture the pointer on `Down`.
fn dialog_read_only_row(
    on: bool,
    colors: &Palette,
    on_toggle: impl Fn(&mut AppState) + 'static,
) -> impl View<AppState> {
    let theme = active_theme();
    let toggle_view = toggle::<AppState, _>(on, move |state: &mut AppState, _requested: bool| {
        on_toggle(state)
    })
    .label(READ_ONLY_TOGGLE_LABEL);
    centered_row(vec![
        flexible(
            1,
            text(READ_ONLY_TOGGLE_LABEL).style(inked(&theme.type_scale.label_large, colors.fg)),
        ),
        inflexible(toggle_view),
    ])
}

/// `minted` is `None` only when the pending secret was cleared before this body re-ran.
pub(in super::super) fn secret_reveal_sheet<D>(
    minted: Option<&MintedToken>,
    colors: &Palette,
    on_done: D,
) -> impl View<AppState> + use<D>
where
    D: Fn(&mut AppState) + 'static,
{
    let theme = active_theme();
    let Some(minted) = minted else {
        return Either::Left(
            text(SECRET_ALREADY_DISMISSED)
                .style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        );
    };
    // The rendered secret is a plain `String` outside `Zeroizing`'s scrub-on-drop for the rest
    // of this build call: the shaped glyph run and the clipboard write both need one.
    let secret = minted.token.to_string();
    let copy_secret = secret.clone();
    Either::Right(stretch_column((
        text(SECRET_REVEAL_TITLE).style(inked(&theme.type_scale.title_small_emphasized, colors.fg)),
        SizedBox::<AppState>(None, Some(PICK_TITLE_GAP)),
        text(minted.name.clone()).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        SizedBox::<AppState>(None, Some(ROW_LINE_GAP)),
        container(Padding(
            EdgeInsets::all(ROW_PAD_X),
            text(secret).style(inked(&theme.type_scale.title_small, colors.fg)),
        ))
        .fill(colors.raised)
        .radius(theme.shape.small),
        SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP)),
        button(COPY_SECRET_LABEL, move |_state: &mut AppState| {
            let copied = frust_clipboard::Clipboard::set_text_sensitive(&copy_secret).is_ok();
            app_toasts().show(if copied {
                COPIED_SECRET_TOAST
            } else {
                COPY_SECRET_FAILED_TOAST
            });
        })
        .style(ButtonStyle::Primary)
        .small(),
        SizedBox::<AppState>(None, Some(TOKEN_LINE_GAP)),
        text(SECRET_CAPTION).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
        SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP)),
        button(SECRET_DONE_LABEL, move |state: &mut AppState| {
            on_done(state)
        })
        .style(ButtonStyle::Ghost)
        .small(),
    )))
}
