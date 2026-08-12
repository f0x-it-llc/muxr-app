//! The `/settings` screen's independently-composable pieces, transcribed from
//! `docs/redesign.html`'s **settings** block (CSS lines 153–179, screen-5
//! markup lines 311–347) and its **picker sheet** (screen 6, lines 349–370).
//!
//! One `pub(super)` fn per visual element, so [`pages`](super::pages)'s
//! `Component::build` reads as an arrangement rather than a wall of nesting
//! (the shape both other real screens in this app use):
//!
//! | Piece | Redesign element |
//! |---|---|
//! | [`group_label`] | `.set-group-lbl` — the 10px dim uppercase band above each group |
//! | [`settings_row`] | `.set-row` — mark tile, label over value, trailing chevron/toggle |
//! | [`token_item`] | `.token-item` — name + scope chip, `created <date>` + `revoke` |
//! | [`saved_server_item`] | the same shape for a remembered server — label + credential chip, endpoint + its two delete actions |
//! | [`create_token_row`] | `.add-token-btn` — the dashed `+ create new token` row |
//! | [`disconnect_button`] | `.disconnect-btn` — the outlined error-hued full-width action |
//! | [`picker_sheet`] | screen 6 — the title row and its `.pick-row`s, one of them amber-filled |
//!
//! # Colour roles
//!
//! The same rule the sessions screen's round-0 review settled: **amber appears
//! only as brand, call-to-action or selection state** — here that is the `+` on
//! the dashed create row and the picker sheet's selected row — and never as a
//! health signal. Health is semantic: the endpoint reads `success` because a
//! live connection is a good state, `insecure (plaintext)` reads `warning`
//! because skipping certificate validation is a real risk (the redesign's own
//! note: it "moved from green to warning yellow — it was previously colored
//! like a success state"), and revoke/Disconnect read `error`.
//!
//! # Marks: vectors where one fits, single-font text where the mock says text
//!
//! `.set-ic` carries an emoji in the mock (🌐/🔓/📡/🔌/🔔) plus three literal
//! text marks (`abc`, `12`, `▮`). **No emoji ships**: a colour-emoji glyph
//! resolves through platform font fallback, which is exactly the failure that
//! put a tofu ⚙ on an iPhone earlier in this app's life. So each mark is either
//! a vendored `frust::icons` vector (`LINK`, `LOCK`, `FORMAT_SIZE`, `REFRESH`,
//! `NOTIFICATIONS`) or a text mark drawn from the same single-font set the
//! sessions screen's meta glyphs already ship on device — ASCII, or Geometric
//! Shapes (`▮`, U+25AE, the block beside `▣`/`◧`/`◎`/`◆`). The two version rows
//! take the backend's initial (`z`/`h`), the same "initial in a tile" device
//! `.sess-avatar` uses, since neither `📡` nor `🔌` has a vendored counterpart
//! and inventing one for a static row is not worth a bespoke `IconData`.
//!
//! # Real components over hand-composed chrome
//!
//! Every element with a facade/Glyph counterpart uses it — [`icon`] for every
//! vector, [`text`] for every run, and [`button`] for `revoke`/`Disconnect`
//! (`ButtonStyle::Danger` is exactly the mock's transparent fill + error
//! hairline + error ink). The boxes with no counterpart are
//! [`super::chrome`]'s: the row's bottom hairline, the 28px mark tile, the two
//! scope chips, the dashed create row, and the picker's full-bleed selected
//! row.
//!
//! The **scope chips** are the one place a shipping widget is declined
//! deliberately: `glyph::badge` covers the read-write chip exactly
//! (`BadgeVariant::Neutral` *is* raised-fill + muted ink + bright hairline) but
//! has no cyan/info variant for the read-only one, and building the pair two
//! different ways would give two different pill geometries three pixels apart.
//! Both are therefore hand-composed through one [`scope_chip`]. Recorded as a
//! framework gap (a `BadgeVariant::Info` over the theme's existing
//! `StatusColors::info`), not a preference.
//!
//! # Tokens
//!
//! `Theme::glyph_baseline()` is canonical where it and the HTML disagree (the
//! mock's 10/10.5/11.5/12/12.5/14px ladder collapses onto Glyph's
//! micro/caption/label/title roles). `--fg-dim` and `--fg-faintest` have no
//! `ColorScheme` role of their own (frust-theme records `fg-dim` as a
//! documented leftover), so both derive from `on_surface_variant` by alpha —
//! the dim one with the same constant the Connect and Sessions screens already
//! document.

use std::rc::Rc;

use frust::authoring::text::TextStyle;
use frust::authoring::{Color, Size};
use frust::glyph::{glyph_dialog, show_glyph_dialog};
use frust::icons::{CHECK, CHEVRON_RIGHT, FORMAT_SIZE, LINK, LOCK, NOTIFICATIONS, REFRESH};
use frust::motion::AnimatedOpacity;
use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, FlexChild,
    FlexView, GestureDetector, Get, GetUntracked, IconSource, NavigatorController, Padding,
    PopResult, RwSignal, Set, SizedBox, Stack, StatusPalette, Theme, any, button, flexible, icon,
    inflexible, switch, text, text_input,
};

use crate::AppState;
use crate::features::connect::domain::entities::{SavedServer, epoch_millis};
use crate::features::settings::domain::entities::{AuthToken, MintedToken, TokenScope};
use crate::ui::toasts::app_toasts;

use super::chrome::{RADIUS_FULL, panel, switch_semantics};
use super::pages::SettingsState;

// ---------------------------------------------------------------------------
// Geometry (transcribed from `docs/redesign.html`'s CSS, once)
// ---------------------------------------------------------------------------

/// `.settings-scroll{padding:6px 0 30px}`.
pub(super) const SCROLL_PAD_TOP: f64 = 6.0;
pub(super) const SCROLL_PAD_BOTTOM: f64 = 30.0;
/// `.set-group-lbl{padding:16px 16px 8px}`.
const GROUP_PAD_TOP: f64 = 16.0;
const GROUP_PAD_BOTTOM: f64 = 8.0;
/// `.set-row{padding:11px 16px}` — also `.token-item`'s.
const ROW_PAD_X: f64 = 16.0;
const ROW_PAD_Y: f64 = 11.0;
/// `.set-row{gap:12px}` — the mark tile to the body, and the body to the
/// trailing slot.
const ROW_GAP: f64 = 12.0;
/// `.set-ic{width:28px;height:28px}`.
const MARK_BOX: f64 = 28.0;
/// The vector inside that tile, in logical px. The mock sets `font-size:12px`
/// on an emoji glyph, whose ink fills rather less of its em box than a Material
/// Symbols vector does of its 24px design box — 14 matches the drawn weight.
const MARK_ICON: f64 = 14.0;
/// `.set-value{margin-top:2px}`.
const ROW_LINE_GAP: f64 = 2.0;
/// `.set-chev{font-size:13px}` — the chevron vector's side length.
const CHEVRON: f64 = 13.0;
/// `.token-top{margin-bottom:4px}`.
const TOKEN_LINE_GAP: f64 = 4.0;
/// `.token-scope{padding:2px 7px}`.
const CHIP_PAD_X: f64 = 7.0;
const CHIP_PAD_Y: f64 = 2.0;
/// `.add-token-btn{margin:12px 16px 4px; padding:10px}`.
const ADD_MARGIN_TOP: f64 = 12.0;
const ADD_MARGIN_BOTTOM: f64 = 4.0;
const ADD_PAD: f64 = 10.0;
/// The gap between the amber `+` and its label — the source's `<b>+</b> create`
/// is one text node whose space is the gap; composed as two runs it needs a
/// real one (the sessions screen's dashed row makes the same note).
const ADD_GAP: f64 = 6.0;
/// `.disconnect-btn{margin:20px 16px 4px}`.
const DISCONNECT_MARGIN_TOP: f64 = 20.0;
const DISCONNECT_MARGIN_BOTTOM: f64 = 4.0;
/// `.toggle{width:36px;height:21px}` and `.toggle::after` — the knob, its
/// **off** inset (`left:2px;top:2px`) and its **on** one (`left:17px`).
const TOGGLE_W: f64 = 36.0;
const TOGGLE_H: f64 = 21.0;
const KNOB: f64 = 15.0;
const KNOB_LEFT_OFF: f64 = 2.0;
const KNOB_LEFT_ON: f64 = 17.0;
const KNOB_TOP: f64 = 2.0;
/// `.pick-row{padding:13px 4px}`.
const PICK_PAD_X: f64 = 4.0;
const PICK_PAD_Y: f64 = 13.0;
/// `.pick-row.sel{margin:0 -20px}` — how far the selected row's fill escapes
/// the sheet's own content gutter (`crate::ui::sheet`'s `PANEL_PAD`, the value
/// this must equal for the fill to reach the panel's edges).
const PICK_BLEED: f64 = 20.0;
/// `.sheet-toprow{margin-bottom:18px}` — the picker title to its first row.
const PICK_TITLE_GAP: f64 = 18.0;
/// The `✓` on the selected picker row.
const CHECK_SIZE: f64 = 13.0;
/// Vertical gap between a dialog's field/toggle/action rows — the same value
/// `ui::dialogs::name_input_dialog`'s own `FIELD_ACTIONS_GAP` uses, duplicated
/// here since this feature composes its own create-token dialog directly (see
/// [`open_create_token_dialog`]'s own doc for why).
const DIALOG_FIELD_GAP: f64 = 16.0;
/// Horizontal gap between the create dialog's Cancel/Save buttons — mirrors
/// `ui::dialogs`'s own `ACTION_GAP`.
const DIALOG_ACTION_GAP: f64 = 10.0;

// ---------------------------------------------------------------------------
// Copy
// ---------------------------------------------------------------------------

/// The amber call-to-action mark on the dashed create row (`.add-token-btn b`).
pub(super) const CREATE_TOKEN_PLUS: &str = "+";
/// …and its label.
pub(super) const CREATE_TOKEN_LABEL: &str = "create new token";
/// `.revoke-btn`'s label — also how a test finds the button.
pub(super) const REVOKE_LABEL: &str = "revoke";
/// The saved-server row's two actions — also how a test finds each button.
pub(super) const DELETE_CREDENTIAL_LABEL: &str = "delete credential";
pub(super) const FORGET_SERVER_LABEL: &str = "forget";
/// The chip a saved row carries while a biometric-gated credential is stored
/// for it. The Flutter settings row's own words.
pub(super) const CREDENTIAL_CHIP: &str = "credential saved";
/// What the `saved servers` group renders with nothing in it.
pub(super) const NO_SAVED_SERVERS: &str = "no saved servers";
/// `.disconnect-btn`'s label.
pub(super) const DISCONNECT_LABEL: &str = "Disconnect";
/// The bell row's `.set-value` — copy, not a preference: the switch beside it
/// is the state, and this says what being on *means*.
pub(super) const BELL_VALUE: &str = "haptic + badge";
/// The create dialog's own title.
const CREATE_TOKEN_TITLE: &str = "New token";
/// …its Cancel/Save labels and the read-only toggle's row label — mirrors
/// `ui::dialogs::name_input_dialog`'s own spelling.
const DIALOG_CANCEL_LABEL: &str = "Cancel";
const DIALOG_SUBMIT_LABEL: &str = "Save";
pub(super) const READ_ONLY_TOGGLE_LABEL: &str = "read-only";
/// The one-time secret reveal sheet's own title.
pub(super) const SECRET_REVEAL_TITLE: &str = "token secret";
/// …its Copy action.
const COPY_SECRET_LABEL: &str = "Copy secret";
/// …its dismiss action.
const SECRET_DONE_LABEL: &str = "Done";
/// The caption under the secret — Flutter parity's "you won't see this again"
/// warning, spelled out rather than assumed.
const SECRET_CAPTION: &str =
    "This is the only time this secret is shown. Copy it now — it can't be viewed again.";
/// What the reveal sheet renders in the vanishingly unlikely race where its
/// pending secret was already cleared by the time this body re-ran (the sheet
/// is on its way down anyway — see [`secret_reveal_sheet`]'s own doc).
const SECRET_ALREADY_DISMISSED: &str = "This secret has already been dismissed.";
/// What a successful/failed secret copy says — the terminal Copy chip's own
/// wording (`features::terminal::presentation::term::chip`), duplicated here
/// since a feature never imports a sibling's `presentation`.
const COPIED_SECRET_TOAST: &str = "Copied to clipboard";
const COPY_SECRET_FAILED_TOAST: &str = "Couldn't copy to clipboard";

// ---------------------------------------------------------------------------
// Token derivations
// ---------------------------------------------------------------------------

/// `--amber-faint: rgba(255,182,39,0.12)` — the accent at 12% over whatever is
/// behind it (the picker's selected row, the toggle's on-track). No
/// `ColorScheme` role carries a translucent accent wash, so it derives from
/// `primary` by alpha — the Connect screen's own constant.
const AMBER_FAINT_ALPHA: f32 = 0.12;
/// `--amber-border: rgba(255,182,39,0.35)` — same derivation, hairline weight.
const AMBER_BORDER_ALPHA: f32 = 0.35;
/// `--fg-dim` derived from `on_surface_variant` by alpha — the constant both
/// other screens document.
const FG_DIM_ALPHA: f32 = 0.62;
/// `--fg-faintest` (`#332f26` dark) — the chevron's ink, and the faintest tone
/// in the whole system. Derived the same way as [`FG_DIM_ALPHA`] and for the
/// same reason (no `ColorScheme` role carries it): ~0.22 of `--fg-muted` over
/// the page tone reproduces the source within a few units per channel, and
/// tracks a re-themed `on_surface_variant` instead of freezing one dark hex.
const FG_FAINTEST_ALPHA: f32 = 0.22;

/// The active theme, or the Glyph baseline when none is threaded (headless
/// tests, a shell with no design system installed). **Never**
/// `Theme::neutral`/`m3_baseline` — see `lib.rs`'s design-system note.
///
/// Deliberately a local copy of the other screens' helper of the same name
/// rather than an import: a feature never reaches into a sibling's
/// `presentation` (`docs/ARCHITECTURE.md`'s cross-feature rule).
pub(super) fn active_theme() -> Theme {
    frust::use_context::<Theme>().unwrap_or_else(Theme::glyph_baseline)
}

/// Every colour this screen paints, resolved once per build from the active
/// theme.
#[derive(Clone, Copy)]
pub(super) struct Palette {
    /// `--bg-base`: the screen's page tone. The shell clears to `--bg-surface`,
    /// so a screen that painted no page background would sit a shade too light.
    pub(super) base: Color,
    /// `--bg-raised`: the mark tile and the read-write chip's fill.
    pub(super) raised: Color,
    /// `--bg-overlay`: the toggle's **off** track.
    pub(super) overlay: Color,
    /// `--border-bright`: the read-write chip's hairline and the dashed row's.
    pub(super) outline: Color,
    /// `--border`: every row's bottom rule, and the picker's separators.
    pub(super) rule: Color,
    /// `--amber`: brand/CTA/selection ink only (see the [module docs](self)).
    pub(super) amber: Color,
    /// `--amber-faint`, derived (see [`AMBER_FAINT_ALPHA`]).
    pub(super) amber_faint: Color,
    /// `--amber-border`, derived (see [`AMBER_BORDER_ALPHA`]).
    pub(super) amber_border: Color,
    /// The bright accent *fill* — the toggle's on-knob (never `primary`, which
    /// is accent ink; under Glyph light the two differ).
    pub(super) amber_fill: Color,
    /// `--fg`: primary text.
    pub(super) fg: Color,
    /// `--fg-muted`: the mark tiles' ink and the read-write chip's.
    pub(super) fg_muted: Color,
    /// `--fg-dim`, derived (see [`FG_DIM_ALPHA`]) — every `.set-value`.
    pub(super) fg_dim: Color,
    /// `--fg-faintest`, derived (see [`FG_FAINTEST_ALPHA`]) — the chevrons.
    pub(super) fg_faintest: Color,
    /// `--success`: the endpoint's value ink.
    pub(super) success: Color,
    /// `--warning`: `insecure (plaintext)`'s.
    pub(super) warning: Color,
    /// `--cyan`: the read-only chip's ink.
    pub(super) cyan: Color,
    /// `--cyan-faint`: its fill.
    pub(super) cyan_faint: Color,
}

/// Resolve [`Palette`] from `theme`.
pub(super) fn palette(theme: &Theme) -> Palette {
    let scheme = theme.scheme();
    // Per `docs/CODE_STANDARDS.md`'s Glyph precedent: a per-status colour with
    // no `ColorScheme` field resolves `StatusPalette` first. A Glyph theme
    // always carries it; `primary`/`surface_container_high` are the last-resort
    // inks if some other theme is ever installed.
    let status = theme.extension::<StatusPalette>();
    Palette {
        base: scheme.surface_container_low,
        raised: scheme.surface_container_high,
        overlay: scheme.surface_container_highest,
        outline: scheme.outline,
        rule: scheme.outline_variant,
        amber: scheme.primary,
        amber_faint: scheme.primary.with_alpha(AMBER_FAINT_ALPHA),
        amber_border: scheme.primary.with_alpha(AMBER_BORDER_ALPHA),
        amber_fill: scheme.primary_container,
        fg: scheme.on_surface,
        fg_muted: scheme.on_surface_variant,
        fg_dim: scheme.on_surface_variant.with_alpha(FG_DIM_ALPHA),
        fg_faintest: scheme.on_surface_variant.with_alpha(FG_FAINTEST_ALPHA),
        success: status.map_or(scheme.primary, |s| s.colors(theme.brightness).success),
        warning: status.map_or(scheme.primary, |s| s.colors(theme.brightness).warning),
        // The Glyph scheme's `tertiary` family *is* the cyan info hue (its only
        // spare), so it is the honest fallback for a theme with no status
        // palette rather than a second amber.
        cyan: status.map_or(scheme.tertiary, |s| s.colors(theme.brightness).info),
        cyan_faint: status.map_or(scheme.tertiary_container, |s| {
            s.colors(theme.brightness).info_container
        }),
    }
}

/// `style` re-inked with `color` — every run picks a Glyph type role for
/// size/family/weight and an explicit role-resolved ink, so nothing falls back
/// to `TextStyle::default()`'s 16px black.
fn inked(style: &TextStyle, color: Color) -> TextStyle {
    let mut style = style.clone();
    style.color = color;
    style
}

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

/// A `gap`-wide horizontal spacer.
fn h_gap<S: 'static>(gap: f64) -> FlexChild<S> {
    inflexible(SizedBox::<S>(Some(gap), None))
}

/// A flexible spacer that pushes what follows it to the trailing edge — Flex
/// v1 has no `space-between` (`MainAxisAlignment::Start` only), the same trick
/// `ui::dialogs` and both other screens use.
fn spacer<S: 'static>() -> FlexChild<S> {
    flexible(1, SizedBox::<S>(None, None))
}

/// A vertically-centered row.
fn row<S: 'static>(children: Vec<FlexChild<S>>) -> FlexView<S> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

/// A column whose children each fill the available width.
pub(super) fn column<S: 'static>(children: Vec<AnyView<S>>) -> FlexView<S> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Stretch)
}

/// The screen's page tone behind `body` — `--bg-base`, which the shell's own
/// clear colour (`--bg-surface`) is emphatically not. See [`Palette::base`].
pub(super) fn page_tone(
    body: impl frust::View<SettingsState>,
    colors: &Palette,
) -> AnyView<SettingsState> {
    any(panel(body, colors.base, 0.0))
}

// ---------------------------------------------------------------------------
// `.set-group-lbl`
// ---------------------------------------------------------------------------

/// The band above each group — 10px dim, letter-spaced, and **uppercased here**
/// because the source does it in CSS (`text-transform:uppercase`) and `text`
/// carries no transform of its own.
pub(super) fn group_label(label: &str, colors: &Palette) -> AnyView<SettingsState> {
    let theme = active_theme();
    any(Padding(
        EdgeInsets {
            left: ROW_PAD_X,
            top: GROUP_PAD_TOP,
            right: ROW_PAD_X,
            bottom: GROUP_PAD_BOTTOM,
        },
        text(label.to_uppercase()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
    ))
}

// ---------------------------------------------------------------------------
// `.set-row`
// ---------------------------------------------------------------------------

/// What sits in a row's 28px `.set-ic` tile. See the [module docs](self) for
/// why there is a text arm at all — and why there is no emoji one.
#[derive(Clone, Copy)]
pub(super) enum RowMark {
    /// A vendored `frust::icons` vector.
    Vector(IconSource),
    /// A single-font-safe text mark (`abc`, `▮`, a backend initial).
    Text(&'static str),
}

/// The semantic hue a `.set-value` prints in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ValueTone {
    /// `--fg-dim` — every value that is merely a fact.
    Plain,
    /// `.set-value.ok` — the live endpoint, and a validated TLS posture.
    Ok,
    /// `.set-value.warn` — the posture that skips certificate validation.
    Warn,
}

/// Which picker sheet a row opens. One enum, four sheets, **one** builder —
/// see [`picker_sheet`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum PickerKind {
    /// `terminal appearance` → font family.
    FontFamily,
    /// `terminal appearance` → font size.
    FontSize,
    /// `terminal appearance` → cursor style.
    CursorStyle,
    /// `session poll` → layout refresh.
    LayoutRefresh,
}

impl PickerKind {
    /// The sheet's own title (screen 6's `.sheet-toprow .lbl`) — the row's
    /// label, verbatim, so the sheet says which row raised it.
    pub(super) fn title(self) -> &'static str {
        match self {
            PickerKind::FontFamily => "font family",
            PickerKind::FontSize => "font size",
            PickerKind::CursorStyle => "cursor style",
            PickerKind::LayoutRefresh => "layout refresh",
        }
    }
}

/// What a row *does*, which also decides what sits in its trailing slot.
#[derive(Clone, Copy)]
pub(super) enum RowAction {
    /// Inert: no chevron, no gesture (the version rows and connection info rows).
    Static,
    /// A chevron row that opens a picker sheet.
    Picker(PickerKind),
    /// The bell row: a trailing switch at this state, flipped by a tap
    /// anywhere on the row.
    Bell(bool),
}

impl RowAction {
    /// Whether a tap on this row does anything at all.
    fn interactive(self) -> bool {
        !matches!(self, RowAction::Static)
    }
}

/// One `.set-row`: the mark tile, the label over its value, and a trailing
/// chevron or switch.
///
/// The **whole row** is the control (the mock draws no separate hit target, and
/// a 28px chevron would be an unreachable one on a phone), so the bell row's
/// switch flips on a tap anywhere along it — the switch is the state, not the
/// button.
pub(super) fn settings_row(
    mark: RowMark,
    label: &'static str,
    value: impl Into<String>,
    tone: ValueTone,
    action: RowAction,
    colors: &Palette,
) -> AnyView<SettingsState> {
    let theme = active_theme();
    let value_ink = match tone {
        ValueTone::Plain => colors.fg_dim,
        ValueTone::Ok => colors.success,
        ValueTone::Warn => colors.warning,
    };
    let body = column(vec![
        any(text(label).style(inked(&theme.type_scale.label_large, colors.fg))),
        any(SizedBox::<SettingsState>(None, Some(ROW_LINE_GAP))),
        any(text(value.into()).style(inked(&theme.type_scale.body_small, value_ink))),
    ]);

    let mut children = vec![
        inflexible(mark_tile(mark, colors)),
        h_gap(ROW_GAP),
        flexible(1, any(body)),
    ];
    if let Some(trailing) = trailing_slot(action, label, colors) {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(trailing));
    }

    let box_ = panel(
        Padding(EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y), row(children)),
        Color::TRANSPARENT,
        0.0,
    )
    .bottom_rule(colors.rule);

    if action.interactive() {
        any(GestureDetector(box_)
            .on_tap(move |state: &mut SettingsState| super::pages::row_tapped(state, action)))
    } else {
        any(box_)
    }
}

/// `.set-ic` — the 28px raised tile holding the row's mark.
fn mark_tile(mark: RowMark, colors: &Palette) -> AnyView<SettingsState> {
    let theme = active_theme();
    let content: AnyView<SettingsState> = match mark {
        RowMark::Vector(source) => any(icon(source).size(MARK_ICON).color(colors.fg_muted)),
        RowMark::Text(glyph) => {
            any(text(glyph).style(inked(&theme.type_scale.label_small, colors.fg_muted)))
        }
    };
    any(panel(content, colors.raised, theme.shape.extra_small).fixed(Size::new(MARK_BOX, MARK_BOX)))
}

/// A row's trailing slot: the faintest chevron for anything that leads
/// somewhere, the switch for the bell row, nothing for a static fact.
fn trailing_slot(
    action: RowAction,
    label: &'static str,
    colors: &Palette,
) -> Option<AnyView<SettingsState>> {
    match action {
        RowAction::Static => None,
        RowAction::Picker(_) => Some(any(icon(CHEVRON_RIGHT)
            .size(CHEVRON)
            .color(colors.fg_faintest))),
        // The switch announces the *row's* subject rather than "switch" — the
        // same choice the Connect screen's remember toggle makes.
        RowAction::Bell(on) => Some(any(switch_semantics(toggle_art(on, colors), label, on))),
    }
}

/// `.toggle` — the 36×21 pill and its 15px knob, in the requested state.
///
/// Both states are composed and cross-faded rather than one being recoloured,
/// which is what makes the on-track the amber wash **over the page tone** the
/// design authored: painting a 12%-alpha wash over an opaque `--bg-overlay`
/// track would composite 88% of that dark track straight back through (the
/// exact defect the Connect screen's own toggle documents, transcribed here
/// rather than re-discovered).
fn toggle_art(on: bool, colors: &Palette) -> AnyView<SettingsState> {
    let radius = TOGGLE_H / 2.0;
    let track = Size::new(TOGGLE_W, TOGGLE_H);
    let off_track = panel(
        SizedBox::<SettingsState>(None, None),
        colors.overlay,
        radius,
    )
    .border(colors.outline)
    .fixed(track);
    let on_track = panel(
        SizedBox::<SettingsState>(None, None),
        colors.amber_faint,
        radius,
    )
    .border(colors.amber_border)
    .fixed(track);
    let knob = |left: f64, fill: Color| {
        Padding(
            EdgeInsets {
                left,
                top: KNOB_TOP,
                right: TOGGLE_W - left - KNOB,
                bottom: TOGGLE_H - KNOB_TOP - KNOB,
            },
            panel(SizedBox::<SettingsState>(None, None), fill, KNOB / 2.0)
                .fixed(Size::new(KNOB, KNOB)),
        )
    };
    let (on_opacity, off_opacity) = if on { (1.0, 0.0) } else { (0.0, 1.0) };
    any(Stack(vec![
        any(AnimatedOpacity::<SettingsState, _>(off_opacity, off_track)),
        any(AnimatedOpacity::<SettingsState, _>(on_opacity, on_track)),
        any(AnimatedOpacity::<SettingsState, _>(
            off_opacity,
            knob(KNOB_LEFT_OFF, colors.fg_dim),
        )),
        any(AnimatedOpacity::<SettingsState, _>(
            on_opacity,
            knob(KNOB_LEFT_ON, colors.amber_fill),
        )),
    ]))
}

// ---------------------------------------------------------------------------
// `.token-item` and `.add-token-btn`
// ---------------------------------------------------------------------------

/// One `.token-item`: the token's name and scope chip on the top line, its
/// creation date and the `revoke` action on the meta line.
///
/// `revoke_enabled` is `false` while the connected session is itself
/// read-only (`ConnectionController::is_read_only`'s "primary defence" note —
/// see `crate::connection`'s module docs): the button still fires
/// `open_revoke_dialog` either way (that call's own guard is what actually
/// blocks it, and raises the honest toast), but its style drops to
/// [`ButtonStyle::Ghost`] — the same "no disabled `Button` state" workaround
/// `ui::dialogs`'s own submit button uses.
pub(super) fn token_item(
    token: &AuthToken,
    colors: &Palette,
    revoke_enabled: bool,
) -> AnyView<SettingsState> {
    let theme = active_theme();
    let name = token.name.clone();
    let top = row(vec![
        flexible(
            1,
            text(token.name.clone()).style(inked(&theme.type_scale.body_small, colors.fg)),
        ),
        h_gap(ROW_GAP),
        inflexible(scope_chip(token.scope, colors)),
    ]);
    let meta = row(vec![
        inflexible(
            text(token.created_label()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
        ),
        spacer(),
        inflexible(
            button(REVOKE_LABEL, move |state: &mut SettingsState| {
                super::pages::open_revoke_dialog(state, &name)
            })
            .style(if revoke_enabled {
                ButtonStyle::Danger
            } else {
                ButtonStyle::Ghost
            })
            .small(),
        ),
    ]);
    any(panel(
        Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            column(vec![
                any(top),
                any(SizedBox::<SettingsState>(None, Some(TOKEN_LINE_GAP))),
                any(meta),
            ]),
        ),
        Color::TRANSPARENT,
        0.0,
    )
    .bottom_rule(colors.rule))
}

/// One saved server, in the same two-line shape as a [`token_item`]: the rig's
/// label and its credential chip on the top line, its endpoint and the row's
/// delete actions on the meta line.
///
/// Two actions, because they are two different deletions and a user asking for
/// one rarely means the other: `delete credential` (only where one is stored)
/// drops the stored secret and revokes the token behind it, leaving the row
/// dialable by hand; `forget` removes the whole profile — and its credential
/// with it, since the endpoint is the key that secret is filed under.
pub(super) fn saved_server_item(server: &SavedServer, colors: &Palette) -> AnyView<SettingsState> {
    let theme = active_theme();
    let endpoint = server.endpoint();
    let mut top = vec![flexible(
        1,
        text(server.label.clone()).style(inked(&theme.type_scale.body_small, colors.fg)),
    )];
    if server.has_credential {
        top.push(h_gap(ROW_GAP));
        top.push(inflexible(credential_chip(colors)));
    }

    let meta_text = format!("{} · {}", endpoint, server.last_used_label(epoch_millis()));
    let mut meta = vec![
        inflexible(text(meta_text).style(inked(&theme.type_scale.label_small, colors.fg_dim))),
        spacer(),
    ];
    if server.has_credential {
        let key = endpoint.clone();
        meta.push(inflexible(
            button(DELETE_CREDENTIAL_LABEL, move |state: &mut SettingsState| {
                super::pages::open_delete_credential_dialog(state, &key);
            })
            .style(ButtonStyle::Danger)
            .small(),
        ));
        meta.push(h_gap(ROW_GAP));
    }
    let key = endpoint.clone();
    meta.push(inflexible(
        button(FORGET_SERVER_LABEL, move |state: &mut SettingsState| {
            super::pages::open_forget_server_dialog(state, &key);
        })
        .style(ButtonStyle::Ghost)
        .small(),
    ));

    any(panel(
        Padding(
            EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
            column(vec![
                any(row(top)),
                any(SizedBox::<SettingsState>(None, Some(TOKEN_LINE_GAP))),
                any(row(meta)),
            ]),
        ),
        Color::TRANSPARENT,
        0.0,
    )
    .bottom_rule(colors.rule))
}

/// The `saved servers` group with nothing in it — a plain muted line, in the
/// same box a row would occupy, rather than an empty group label with nothing
/// under it.
pub(super) fn no_saved_servers_row(colors: &Palette) -> AnyView<SettingsState> {
    let theme = active_theme();
    any(Padding(
        EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y),
        text(NO_SAVED_SERVERS).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
    ))
}

/// The `credential saved` pill — the same hand-composed shape as a
/// [`scope_chip`], in the cyan/info hue that chip already reserves for a fact
/// about a credential rather than about a token's power.
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

/// `.token-scope` — the 9px pill naming what a token may do.
///
/// Hand-composed for both scopes rather than half-reached through
/// `glyph::badge`; see the [module docs](self)'s note on that gap.
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

/// `.add-token-btn` — the dashed, centre-aligned row that closes the token
/// group, and the screen's only way to mint one.
///
/// The `+` is amber because it is the call to action; the label beside it is
/// dim (`.add-token-btn{color:var(--fg-dim)}` with an amber `<b>`). While
/// `enabled` is `false` (a read-only connected session — see
/// [`token_item`]'s own doc) both dim to [`Palette::fg_faintest`] instead: the
/// row still opens `open_create_token_dialog` on tap, whose own guard is what
/// actually blocks the mint and raises the honest toast.
pub(super) fn create_token_row(colors: &Palette, enabled: bool) -> AnyView<SettingsState> {
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
        .on_tap(|state: &mut SettingsState| super::pages::open_create_token_dialog(state)),
    ))
}

// ---------------------------------------------------------------------------
// The create-token dialog — over `AppState`, like the picker sheet
// ---------------------------------------------------------------------------

/// Open the mint-a-token dialog: a name field and the read-only toggle
/// Flutter's own create flow offers. `ui::dialogs::name_input_dialog` has no
/// toggle slot, and this feature cannot widen a file outside its own tree
/// (`docs/ARCHITECTURE.md`'s layer table), so this composes the same
/// `glyph_dialog`/`show_glyph_dialog` primitives that helper wraps, directly
/// — the same shape (a controlled [`RwSignal`] field, a "disabled" submit per
/// the framework's own no-disabled-`Button` gap `ui::dialogs`'s own doc
/// names) with one more control.
///
/// `on_submit` receives the **trimmed**, non-empty name and the toggle's
/// value; every dismissal route (cancel, scrim, `Escape`, back) closes the
/// dialog without calling back, exactly like `name_input_dialog`.
pub(super) fn open_create_token_dialog(
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
            let colors = palette(&theme);
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
            let buttons = FlexView::new(
                Axis::Horizontal,
                vec![
                    flexible(1, SizedBox::<AppState>(None, None)),
                    inflexible(
                        button(DIALOG_CANCEL_LABEL, move |_state: &mut AppState| {
                            cancel_nav.pop()
                        })
                        .style(ButtonStyle::Ghost)
                        .small(),
                    ),
                    inflexible(SizedBox::<AppState>(Some(DIALOG_ACTION_GAP), None)),
                    inflexible(
                        button(DIALOG_SUBMIT_LABEL, move |_state: &mut AppState| {
                            // "Disabled": the style below is the visual half,
                            // this guard the behavioral one (see the module
                            // docs' framework-gap note).
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
                    ),
                ],
            );
            glyph_dialog()
                .title(CREATE_TOKEN_TITLE)
                .action(any(FlexView::new(
                    Axis::Vertical,
                    vec![
                        inflexible(field),
                        inflexible(SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP))),
                        inflexible(toggle),
                        inflexible(SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP))),
                        inflexible(buttons),
                    ],
                )
                .cross_axis(CrossAxisAlignment::Stretch)))
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
/// do nothing when there is no usable name — the one gate both the Enter key
/// and the Save button route through (mirrors
/// `ui::dialogs::name_input_dialog`'s own `submit` helper).
fn submit_create_token(nav: &NavigatorController<AppState>, typed: &str, read_only: bool) {
    let name = typed.trim();
    if name.is_empty() {
        return;
    }
    nav.pop_with_result(PopResult::of((name.to_string(), read_only)));
}

/// The create dialog's read-only toggle: a flexible label plus the real,
/// themed `frust::switch` — the same shape `features::connect`'s own
/// `remember_row` draws for Connect's remember toggle, down to the "no outer
/// `GestureDetector`" part.
///
/// **Why no outer `GestureDetector` wraps the row (unlike this dialog's old
/// hand-drawn pill, and unlike the bell row's [`toggle_art`], which still has
/// one):** both `SwitchWidget` and `GestureDetectorWidget` call
/// `ctx.capture_pointer()` on `Down`, so nesting the real switch inside a
/// row-wide detector would be two captures racing the same gesture. The
/// switch owns its own tap target instead, and the label beside it stops
/// being tappable as a consequence — Connect's own remember row makes the
/// identical trade for the identical reason.
///
/// Built over `AppState` — a dialog is a page on the *app's* navigator, not
/// [`SettingsState`] — with a callback shaped like Connect's: the switch
/// reports the *requested* value it never self-applies (a controlled
/// component), and the callback ignores it, flipping the caller's own
/// `read_only` signal instead — the single source of truth the dialog body's
/// re-run then reflects back into `on`.
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

/// The reveal sheet's own content: title, the minted token's name, its secret
/// (in the type scale's own monospace face — every Glyph role is, so there is
/// no separate "mono" token to reach for), a Copy action and the "gone
/// forever" caption. `minted` is `None` only in the vanishingly unlikely race
/// where the pending secret was already cleared by the time this body re-ran
/// (the sheet is on its way down anyway); it renders a bare caption rather
/// than panicking.
///
/// Built by [`super::pages`] from
/// [`SettingsController::pending_token_secret`](super::controller::SettingsController::pending_token_secret),
/// exactly the split [`picker_sheet`] uses for the same reason: this file
/// never reaches into the controller itself.
pub(super) fn secret_reveal_sheet(
    minted: Option<&MintedToken>,
    colors: &Palette,
    on_done: impl Fn(&mut AppState) + 'static,
) -> AnyView<AppState> {
    let theme = active_theme();
    let Some(minted) = minted else {
        return any(text(SECRET_ALREADY_DISMISSED)
            .style(inked(&theme.type_scale.body_small, colors.fg_dim)));
    };
    // One residue `Zeroizing` cannot cover, same as `crate::connection`'s own
    // documented note on its encoded request frame: rendering the secret at
    // all needs a plain `String` this render pass owns (the shaped glyph run
    // and the clipboard write both need one), so it lives outside the
    // wrapper's scrub-on-drop for the rest of this build call.
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

// ---------------------------------------------------------------------------
// `.disconnect-btn`
// ---------------------------------------------------------------------------

/// The screen's last element: a full-width outlined error-hued action.
///
/// `ButtonStyle::Danger` *is* the mock's `.disconnect-btn` (transparent fill,
/// error hairline, error ink), so this reaches for the shipping widget rather
/// than composing a fourth bordered box.
pub(super) fn disconnect_button() -> AnyView<SettingsState> {
    any(Padding(
        EdgeInsets {
            left: ROW_PAD_X,
            top: DISCONNECT_MARGIN_TOP,
            right: ROW_PAD_X,
            bottom: DISCONNECT_MARGIN_BOTTOM,
        },
        button(DISCONNECT_LABEL, |state: &mut SettingsState| {
            super::pages::open_disconnect_dialog(state)
        })
        .style(ButtonStyle::Danger),
    ))
}

// ---------------------------------------------------------------------------
// The picker sheet (screen 6) — over `AppState`, like the dialogs
// ---------------------------------------------------------------------------

/// One picker sheet's content: a title and the options it offers, with at most
/// one of them already chosen.
///
/// Built by [`pages`](super::pages) from the controller's signals and rendered
/// by [`picker_sheet`] — **four rows of the screen share this one pair**, which
/// is the whole point of screen 6's note ("same picker pattern reused for
/// layout refresh, font size, and font family").
pub(super) struct Picker {
    /// The sheet's heading.
    pub(super) title: &'static str,
    /// Every option, in the order the domain declares them.
    pub(super) options: Vec<String>,
    /// Which one is current, if the stored value is still an offered choice.
    pub(super) selected: Option<usize>,
}

/// The pick handler, shared by every row of one sheet: which option index was
/// chosen, plus the `&mut AppState` a navigator page's callback is handed.
type OnPick = Rc<dyn Fn(&mut AppState, usize)>;

/// Screen 6, rendered: a 14px semibold title over one `.pick-row` per option,
/// the selected one filled amber full-bleed and carrying a `✓`.
///
/// Built over `AppState` (not [`SettingsState`]): a sheet is a pushed navigator
/// page, and this navigator's pages are `AnyView<AppState>` — the same
/// component boundary the dialogs cross. `on_pick` therefore takes
/// `&mut AppState` and carries its result back through a captured controller
/// handle.
pub(super) fn picker_sheet(
    picker: &Picker,
    colors: &Palette,
    on_pick: impl Fn(&mut AppState, usize) + 'static,
) -> AnyView<AppState> {
    let theme = active_theme();
    let on_pick: OnPick = Rc::new(on_pick);
    let last = picker.options.len().saturating_sub(1);
    let mut children = vec![
        any(text(picker.title).style(inked(&theme.type_scale.title_small_emphasized, colors.fg))),
        any(SizedBox::<AppState>(None, Some(PICK_TITLE_GAP))),
    ];
    for (index, option) in picker.options.iter().enumerate() {
        children.push(pick_row(
            option,
            picker.selected == Some(index),
            index < last,
            colors,
            {
                let on_pick = Rc::clone(&on_pick);
                move |state: &mut AppState| on_pick(state, index)
            },
        ));
    }
    any(column(children))
}

/// One `.pick-row`. The selected one fills `--amber-faint` **past the sheet's
/// own gutter** (`.pick-row.sel{margin:0 -20px}`, reproduced by
/// [`super::chrome::PanelView::bleed`] since frust has no negative padding),
/// inks its label amber, and carries the vendored `CHECK` vector; every row but
/// the last closes with a hairline.
fn pick_row(
    option: &str,
    selected: bool,
    ruled: bool,
    colors: &Palette,
    on_tap: impl Fn(&mut AppState) + 'static,
) -> AnyView<AppState> {
    let theme = active_theme();
    let ink = if selected { colors.amber } else { colors.fg };
    let mut children = vec![flexible(
        1,
        text(option.to_string()).style(inked(&theme.type_scale.label_large, ink)),
    )];
    if selected {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(icon(CHECK).size(CHECK_SIZE).color(colors.amber)));
    }
    let fill = if selected {
        colors.amber_faint
    } else {
        Color::TRANSPARENT
    };
    let mut box_ = panel(
        Padding(EdgeInsets::symmetric(PICK_PAD_X, PICK_PAD_Y), row(children)),
        fill,
        theme.shape.small,
    );
    if selected {
        box_ = box_.bleed(PICK_BLEED);
    }
    if ruled {
        box_ = box_.bottom_rule(colors.rule);
    }
    any(GestureDetector(box_).on_tap(on_tap))
}

// ---------------------------------------------------------------------------
// The mark set, named once
// ---------------------------------------------------------------------------

/// `.set-ic` for `server url` — a link, standing in for the mock's 🌐.
pub(super) const MARK_SERVER_URL: RowMark = RowMark::Vector(LINK);
/// …for `tls certificate` — the mock's 🔓 as a vector. Deliberately **not**
/// state-dependent: the row's *value* carries the posture (green `public CA`
/// vs amber `insecure (plaintext)`), and a second signal in the tile would say
/// the same thing twice.
pub(super) const MARK_TLS: RowMark = RowMark::Vector(LOCK);
/// …for `zellij version`/`herdr version` — the backend's initial, the same
/// device `.sess-avatar` uses (see the [module docs](self)).
pub(super) const MARK_ZELLIJ: RowMark = RowMark::Text("z");
pub(super) const MARK_HERDR: RowMark = RowMark::Text("h");
/// …for `font family` — the mock's literal `abc`, kept as text.
pub(super) const MARK_FONT_FAMILY: RowMark = RowMark::Text("abc");
/// …for `font size`. The mock's mark is the literal `12`, which would go stale
/// the moment the picker changed it; the vendored `format_size` vector says the
/// same thing and cannot drift.
pub(super) const MARK_FONT_SIZE: RowMark = RowMark::Vector(FORMAT_SIZE);
/// …for `cursor style` — `▮` (U+25AE), the mock's own mark, from the same
/// Geometric Shapes block the sessions screen already ships on device.
pub(super) const MARK_CURSOR: RowMark = RowMark::Text("\u{25ae}");
/// …for `layout refresh` — the mock's `↻` as a vector.
pub(super) const MARK_REFRESH: RowMark = RowMark::Vector(REFRESH);
/// …for `bell notifications` — the mock's 🔔 as a vector.
pub(super) const MARK_BELL: RowMark = RowMark::Vector(NOTIFICATIONS);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_picker_titles_itself_with_the_row_that_raised_it() {
        assert_eq!(PickerKind::FontFamily.title(), "font family");
        assert_eq!(PickerKind::FontSize.title(), "font size");
        assert_eq!(PickerKind::CursorStyle.title(), "cursor style");
        assert_eq!(PickerKind::LayoutRefresh.title(), "layout refresh");
    }

    #[test]
    fn only_a_static_row_is_inert() {
        assert!(!RowAction::Static.interactive());
        assert!(RowAction::Picker(PickerKind::FontSize).interactive());
        assert!(RowAction::Bell(true).interactive());
    }

    #[test]
    fn no_mark_is_an_emoji() {
        // The tripwire for this screen's hardest-won rule (see the module
        // docs): a text mark must stay inside the BMP ranges the app's one
        // bundled family actually covers — ASCII or Geometric Shapes — because
        // anything else resolves through platform font fallback.
        for mark in [
            MARK_SERVER_URL,
            MARK_TLS,
            MARK_ZELLIJ,
            MARK_HERDR,
            MARK_FONT_FAMILY,
            MARK_FONT_SIZE,
            MARK_CURSOR,
            MARK_REFRESH,
            MARK_BELL,
        ] {
            let RowMark::Text(glyph) = mark else {
                continue;
            };
            for c in glyph.chars() {
                let cp = c as u32;
                assert!(
                    c.is_ascii() || (0x25A0..=0x25FF).contains(&cp),
                    "{glyph:?} carries U+{cp:04X}, outside ASCII/Geometric Shapes"
                );
            }
        }
    }

    #[test]
    fn the_toggles_knob_never_escapes_its_track() {
        const { assert!(KNOB_LEFT_OFF + KNOB <= TOGGLE_W) };
        const { assert!(KNOB_LEFT_ON + KNOB <= TOGGLE_W) };
        const { assert!(KNOB_TOP + KNOB <= TOGGLE_H) };
        // …and the on-state knob really is the right-hand one.
        const { assert!(KNOB_LEFT_OFF < KNOB_LEFT_ON) };
    }

    #[test]
    fn the_selected_rows_bleed_is_the_sheets_own_gutter() {
        // `crate::ui::sheet`'s `PANEL_PAD` is private, so this is a transcribed
        // 20 rather than an equality against it. If the two ever diverge the
        // amber fill stops short of (or runs past) the sheet's edges, which is
        // the one thing screen 6's selected row is supposed to do — and
        // `tests/settings.rs` asserts the measured geometry end to end, which
        // is what actually catches a drift.
        assert_eq!(PICK_BLEED, 20.0);
    }

    #[test]
    fn the_derived_alphas_stay_legal_and_ordered() {
        for alpha in [
            AMBER_FAINT_ALPHA,
            AMBER_BORDER_ALPHA,
            FG_DIM_ALPHA,
            FG_FAINTEST_ALPHA,
        ] {
            assert!((0.0..=1.0).contains(&alpha), "{alpha}");
        }
        // "Faintest" has to actually be fainter than "dim", or the chevrons
        // would compete with the values beside them.
        const { assert!(FG_FAINTEST_ALPHA < FG_DIM_ALPHA) };
    }
}
