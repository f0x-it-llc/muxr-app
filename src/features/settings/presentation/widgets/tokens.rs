//! The `auth tokens` group's rows ([`token_item`]/[`scrambling_token_item`]
//! share one [`token_row`] box, so a resting row and a revoking one cannot
//! drift a pixel apart), the dashed `+ create new token` row and its create
//! dialog, and the one-time secret reveal sheet.
//!
//! **Contract:** a freshly minted token's secret is shown exactly once,
//! through [`secret_reveal_sheet`], then gone for good — nothing here (or in
//! the controller) persists it past that sheet's own dismissal.
//! **Contract:** [`token_item`]'s `revoke_enabled` and [`create_token_row`]'s
//! `enabled` both mirror `ConnectionController::is_read_only` — a read-only
//! connected session cannot mint or revoke, though both controls stay
//! reachable (styled down, not disabled) since frust's `Button` has no
//! disabled state; the real gate is the caller's own guard, which raises the
//! honest toast.

use frust::authoring::Color;
use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, FlexView,
    GestureDetector, Get, GetUntracked, NavigatorController, Padding, PopResult, RwSignal, Set,
    SizedBox, any, button, flexible, inflexible, text, text_input,
};
use frust_glyph::{glyph_dialog, show_glyph_dialog};
use frust_material::switch;

use crate::AppState;
use crate::features::settings::domain::entities::{AuthToken, MintedToken, TokenScope};
use crate::features::settings::presentation::chrome::{RADIUS_FULL, panel, switch_semantics};
use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::features::settings::presentation::scramble::{self, ScramblePose};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;
use crate::ui::toasts::app_toasts;

use super::pickers::PICK_TITLE_GAP;
use super::rows::{ROW_GAP, ROW_LINE_GAP, ROW_PAD_X, ROW_PAD_Y, column, h_gap, row, spacer};

/// `.token-top{margin-bottom:4px}`.
const TOKEN_LINE_GAP: f64 = spacing::TIGHT;
/// `.token-scope{padding:2px 7px}`.
const CHIP_PAD_X: f64 = 7.0;
const CHIP_PAD_Y: f64 = spacing::MICRO;
/// `.add-token-btn{margin:12px 16px 4px; padding:10px}`.
const ADD_MARGIN_TOP: f64 = spacing::BASE;
const ADD_MARGIN_BOTTOM: f64 = spacing::TIGHT;
const ADD_PAD: f64 = spacing::MD;
/// The gap between the amber `+` and its label — the source's `<b>+</b>
/// create` is one text node whose space is the gap; composed as two runs it
/// needs a real one.
const ADD_GAP: f64 = spacing::XS;
/// Vertical gap inside the create dialog's `.content()` column, and between
/// the secret-reveal sheet's own stacked rows.
const DIALOG_FIELD_GAP: f64 = spacing::XL;

pub(super) const CREATE_TOKEN_PLUS: &str = "+";
pub(super) const CREATE_TOKEN_LABEL: &str = "create new token";
/// Also how a test finds the button.
pub(super) const REVOKE_LABEL: &str = "revoke";
const CREATE_TOKEN_TITLE: &str = "New token";
/// Mirrors `ui::dialogs::name_input_dialog`'s own spelling.
const DIALOG_CANCEL_LABEL: &str = "Cancel";
const DIALOG_SUBMIT_LABEL: &str = "Save";
pub(super) const READ_ONLY_TOGGLE_LABEL: &str = "read-only";
pub(super) const SECRET_REVEAL_TITLE: &str = "token secret";
const COPY_SECRET_LABEL: &str = "Copy secret";
const SECRET_DONE_LABEL: &str = "Done";
/// Flutter parity's "you won't see this again" warning, spelled out.
const SECRET_CAPTION: &str =
    "This is the only time this secret is shown. Copy it now — it can't be viewed again.";
/// The vanishingly unlikely race where the pending secret was already
/// cleared by the time this body re-ran (the sheet is on its way down
/// anyway — see [`secret_reveal_sheet`]).
const SECRET_ALREADY_DISMISSED: &str = "This secret has already been dismissed.";
/// The terminal Copy chip's own wording, duplicated since a feature never
/// imports a sibling's `presentation`.
const COPIED_SECRET_TOAST: &str = "Copied to clipboard";
const COPY_SECRET_FAILED_TOAST: &str = "Couldn't copy to clipboard";

// ---------------------------------------------------------------------------
// `.token-item`
// ---------------------------------------------------------------------------

pub(super) fn token_item(
    token: &AuthToken,
    colors: &Palette,
    revoke_enabled: bool,
) -> AnyView<SettingsState> {
    let theme = active_theme();
    let name = token.name.clone();
    let name_line =
        any(text(token.name.clone()).style(inked(&theme.type_scale.body_small, colors.fg)));
    let revoke = any(button(REVOKE_LABEL, move |state: &mut SettingsState| {
        pages::open_revoke_dialog(state, &name)
    })
    .style(if revoke_enabled {
        ButtonStyle::Danger
    } else {
        ButtonStyle::Ghost
    })
    .small());
    token_row(name_line, token, revoke, None, colors)
}

/// A `.token-item` mid-revoke: the same row, with the name line replaced by
/// this frame's scramble and the whole thing collapsing. `token` is the
/// scramble's own snapshot, not a row out of the live list — the RPC's
/// refresh routinely removes the token while this is still on screen (see
/// `scramble`'s own module docs). The trailing button is a **geometric
/// stand-in**: same widget, same style, same measured height, no handler —
/// dropping it would jump the row the instant the animation started, and
/// leaving it live would offer a second confirmation for a token already on
/// its way out.
pub(super) fn scrambling_token_item(
    token: &AuthToken,
    pose: &ScramblePose,
    colors: &Palette,
) -> AnyView<SettingsState> {
    let theme = active_theme();
    let ink = if pose.glitching {
        colors.error
    } else {
        colors.fg
    };
    let name_line = any(text(pose.display.clone()).style(inked(&theme.type_scale.body_small, ink)));
    let revoke = any(button(REVOKE_LABEL, |_state: &mut SettingsState| {})
        .style(ButtonStyle::Danger)
        .small());
    // The ticker rides inside the row it drives, so the scroller's paint
    // culling can never leave one running without the other.
    let ticker = pose.running.then(scramble::frame_ticker);
    let row = token_row(name_line, token, revoke, ticker, colors);
    scramble::collapsing(pose.ratio, row)
}

/// The `.token-item` box both callers above share: `name_line` and `revoke`
/// over the snapshot's scope chip and created line, plus an optional
/// zero-height `tail`.
fn token_row(
    name_line: AnyView<SettingsState>,
    token: &AuthToken,
    revoke: AnyView<SettingsState>,
    tail: Option<AnyView<SettingsState>>,
    colors: &Palette,
) -> AnyView<SettingsState> {
    let theme = active_theme();
    let top = row(vec![
        flexible(1, name_line),
        h_gap(ROW_GAP),
        inflexible(scope_chip(token.scope, colors)),
    ]);
    let meta = row(vec![
        inflexible(
            text(token.created_label()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
        ),
        spacer(),
        inflexible(revoke),
    ]);
    let mut lines = vec![
        any(top),
        any(SizedBox::<SettingsState>(None, Some(TOKEN_LINE_GAP))),
        any(meta),
    ];
    lines.extend(tail);
    any(panel(
        Padding(EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y), column(lines)),
        Color::TRANSPARENT,
        0.0,
    )
    .bottom_rule(colors.rule))
}

/// `.token-scope` — the 9px pill naming what a token may do. Hand-composed
/// for both scopes rather than half-reached through `glyph::badge`, which
/// has no cyan/info variant for the read-only one (recorded as a framework
/// gap, not a preference).
fn scope_chip(scope: TokenScope, colors: &Palette) -> AnyView<SettingsState> {
    let theme = active_theme();
    let (fill, ink, border) = match scope {
        TokenScope::ReadWrite => (colors.raised, colors.fg_muted, Some(colors.outline)),
        TokenScope::ReadOnly => (colors.cyan_faint, colors.cyan, None),
    };
    let mut chip = panel(
        Padding(
            EdgeInsets::symmetric(CHIP_PAD_X, CHIP_PAD_Y),
            text(scope.label()).style(inked(&theme.type_scale.label_small, ink)),
        ),
        fill,
        RADIUS_FULL,
    );
    if let Some(border) = border {
        chip = chip.border(border);
    }
    any(chip)
}

// ---------------------------------------------------------------------------
// `.add-token-btn` and the create dialog
// ---------------------------------------------------------------------------

/// The dashed, centre-aligned row that closes the token group — the screen's
/// only way to mint one. While `enabled` is `false` the `+`/label both dim to
/// [`Palette::fg_faintest`]; the row still opens the dialog on tap, whose own
/// guard is what actually blocks the mint.
pub(in super::super) fn create_token_row(
    colors: &Palette,
    enabled: bool,
) -> AnyView<SettingsState> {
    let theme = active_theme();
    let (plus_ink, label_ink) = if enabled {
        (colors.amber, colors.fg_dim)
    } else {
        (colors.fg_faintest, colors.fg_faintest)
    };
    let label = row(vec![
        inflexible(text(CREATE_TOKEN_PLUS).style(inked(&theme.type_scale.body_small, plus_ink))),
        h_gap(ADD_GAP),
        inflexible(text(CREATE_TOKEN_LABEL).style(inked(&theme.type_scale.body_small, label_ink))),
    ]);
    any(Padding(
        EdgeInsets {
            left: ROW_PAD_X,
            top: ADD_MARGIN_TOP,
            right: ROW_PAD_X,
            bottom: ADD_MARGIN_BOTTOM,
        },
        GestureDetector(
            panel(
                Padding(EdgeInsets::all(ADD_PAD), Align(Alignment::CENTER, label)),
                Color::TRANSPARENT,
                theme.shape.small,
            )
            .dashed_border(colors.outline),
        )
        .on_tap(|state: &mut SettingsState| pages::open_create_token_dialog(state)),
    ))
}

/// Open the mint-a-token dialog: a name field and the read-only toggle,
/// composed directly over `glyph_dialog`/`show_glyph_dialog` (rather than
/// `ui::dialogs::name_input_dialog`, which has no toggle slot and cannot be
/// widened from outside its own tree). The field and toggle are the dialog's
/// real `.content()`; Cancel/Save are two real `.action(...)` buttons.
///
/// `on_submit` receives the **trimmed**, non-empty name and the toggle's
/// value; every dismissal route (cancel, scrim, `Escape`, back) closes the
/// dialog without calling back.
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
                .content(any(FlexView::new(
                    Axis::Vertical,
                    vec![
                        inflexible(field),
                        inflexible(SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP))),
                        inflexible(toggle),
                    ],
                )
                .cross_axis(CrossAxisAlignment::Stretch)))
                .action(any(button(
                    DIALOG_CANCEL_LABEL,
                    move |_state: &mut AppState| cancel_nav.pop(),
                )
                .style(ButtonStyle::Ghost)
                .small()))
                .action(any(button(
                    DIALOG_SUBMIT_LABEL,
                    move |_state: &mut AppState| {
                        // "Disabled": the style below is the visual half,
                        // this guard the behavioral one.
                        if can_submit {
                            submit_create_token(
                                &submit_nav,
                                &submit_current,
                                read_only.get_untracked(),
                            );
                        }
                    },
                )
                .style(if can_submit {
                    ButtonStyle::Primary
                } else {
                    ButtonStyle::Ghost
                })
                .small()))
        },
        move |state: &mut AppState, result: PopResult| {
            if let Some((typed, read_only)) = result.take::<(String, bool)>() {
                on_submit(state, typed, read_only);
            }
        },
    );
}

/// Whether `text` is a usable name (non-blank once trimmed) — mirrors
/// `ui::dialogs::name_input_dialog`'s own `submittable`.
fn create_token_submittable(text: &str) -> bool {
    !text.trim().is_empty()
}

/// Pop the create dialog carrying the trimmed name and the toggle's value, or
/// do nothing with no usable name — the one gate both the Enter key and the
/// Save button route through.
fn submit_create_token(nav: &NavigatorController<AppState>, typed: &str, read_only: bool) {
    let name = typed.trim();
    if name.is_empty() {
        return;
    }
    nav.pop_with_result(PopResult::of((name.to_string(), read_only)));
}

/// The create dialog's read-only toggle: a flexible label plus the real,
/// themed `frust_material::switch` (the same shape `features::connect`'s own
/// `remember_row` draws) — **no outer `GestureDetector`**, since both
/// `SwitchWidget` and `GestureDetectorWidget` capture the pointer on `Down`,
/// and nesting the real switch inside a row-wide detector would race the same
/// gesture twice.
///
/// Built over `AppState` — a dialog is a page on the *app's* navigator — with
/// a controlled-component callback: the switch reports the *requested* value
/// and the caller flips its own `read_only` signal, whose re-run reflects
/// back into `on`.
fn dialog_read_only_row(
    on: bool,
    colors: &Palette,
    on_toggle: impl Fn(&mut AppState) + 'static,
) -> AnyView<AppState> {
    let theme = active_theme();
    let toggle = any(switch_semantics(
        switch::<AppState, _>(on, move |state: &mut AppState, _requested: bool| {
            on_toggle(state)
        }),
        READ_ONLY_TOGGLE_LABEL,
        on,
    ));
    any(row(vec![
        flexible(
            1,
            any(text(READ_ONLY_TOGGLE_LABEL).style(inked(&theme.type_scale.label_large, colors.fg))),
        ),
        inflexible(toggle),
    ]))
}

// ---------------------------------------------------------------------------
// The one-time secret reveal sheet — over `AppState`, like the picker sheet
// ---------------------------------------------------------------------------

/// The reveal sheet's own content: title, the minted token's name, its
/// secret, a Copy action and the "gone forever" caption. `minted` is `None`
/// only in the vanishingly unlikely race where the pending secret was
/// already cleared by the time this body re-ran; it renders a bare caption
/// rather than panicking.
pub(in super::super) fn secret_reveal_sheet(
    minted: Option<&MintedToken>,
    colors: &Palette,
    on_done: impl Fn(&mut AppState) + 'static,
) -> AnyView<AppState> {
    let theme = active_theme();
    let Some(minted) = minted else {
        return any(text(SECRET_ALREADY_DISMISSED)
            .style(inked(&theme.type_scale.body_small, colors.fg_dim)));
    };
    // One residue `Zeroizing` cannot cover: rendering the secret needs a
    // plain `String` this render pass owns (the shaped glyph run and the
    // clipboard write both need one), so it lives outside the wrapper's
    // scrub-on-drop for the rest of this build call.
    let secret = minted.token.to_string();
    let copy_secret = secret.clone();
    let children: Vec<AnyView<AppState>> = vec![
        any(text(SECRET_REVEAL_TITLE)
            .style(inked(&theme.type_scale.title_small_emphasized, colors.fg))),
        any(SizedBox::<AppState>(None, Some(PICK_TITLE_GAP))),
        any(text(minted.name.clone()).style(inked(&theme.type_scale.body_small, colors.fg_dim))),
        any(SizedBox::<AppState>(None, Some(ROW_LINE_GAP))),
        any(panel(
            Padding(
                EdgeInsets::all(ROW_PAD_X),
                text(secret).style(inked(&theme.type_scale.title_small, colors.fg)),
            ),
            colors.raised,
            theme.shape.small,
        )),
        any(SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP))),
        any(button(COPY_SECRET_LABEL, move |_state: &mut AppState| {
            let copied = frust_clipboard::Clipboard::set_text_sensitive(&copy_secret).is_ok();
            app_toasts().show(if copied {
                COPIED_SECRET_TOAST
            } else {
                COPY_SECRET_FAILED_TOAST
            });
        })
        .style(ButtonStyle::Primary)
        .small()),
        any(SizedBox::<AppState>(None, Some(TOKEN_LINE_GAP))),
        any(text(SECRET_CAPTION).style(inked(&theme.type_scale.label_small, colors.fg_dim))),
        any(SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP))),
        any(button(SECRET_DONE_LABEL, move |state: &mut AppState| {
            on_done(state)
        })
        .style(ButtonStyle::Ghost)
        .small()),
    ];
    any(column(children))
}
