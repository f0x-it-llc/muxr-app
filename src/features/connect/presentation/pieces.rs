//! The Connect screen's eight pieces, each independently mountable.
//!
//! One `pub fn` per `.piece` element in `docs/splash.html`'s `.ui-layer`, in
//! source order — [`connect_pieces`] returns exactly that list, and both
//! consumers walk it:
//!
//! | # | Piece | `data-delay` |
//! |---|---|---|
//! | 0 | [`connect_mark`] — the 2×2 amber grid tile | 0ms |
//! | 1 | [`server_badge`] — "muxrd server" / "zellij · herdr" | 130ms |
//! | 2 | [`heading`] — "Connect to server" + subtitle | 260ms |
//! | 3 | [`server_url_field`] | 390ms |
//! | 4 | [`token_field`] | 480ms |
//! | 5 | [`remember_row`] — the remember toggle **and** the trust-mode selector | 580ms |
//! | 6 | [`connect_button`] | 720ms |
//! | 7 | [`saved_address`] — the "saved servers" header + its rows | 860ms |
//!
//! - [`super::pages::connect_page`] mounts them live, inside the screen
//!   `Component` that hosts their controller.
//! - [`crate::ui::splash::sequence`] lays out the *same* list through the
//!   *same* function with each piece wrapped in a spring-drop entrance, so the
//!   splash's final frame and the route's first frame are the same picture.
//!
//! # Live vs. splash: the `Option<&Live>` parameter
//!
//! Five of the eight take an `Option<&`[`Live`]`>`. [`Live`] is the screen's own
//! wiring — its hosted [`ConnectController`] plus the "connect now" action — and
//! `Some(..)` is what turns a piece into a control: bound `text_input`s, a
//! pressable toggle, selectable trust rows, a firing CTA, tappable saved rows.
//! `None` renders the *same picture* with no callbacks and no repository data:
//! that is what the splash composes, since it assembles the screen before any
//! controller exists (`crate::AppState::splash_overlay` builds these outside any
//! screen). Every default is chosen so the two coincide — the toggle starts on,
//! the trust selector starts on "Public domain (CA)", the fields start empty —
//! so the overlay still unmounts onto the picture underneath it.
//!
//! The one deliberate divergence is piece 7: the splash's copy renders the
//! mock's single stand-in row, the live screen renders the repository's list.
//! It is the last piece in a top-anchored column, so every piece above it is
//! positionally identical across the handoff.
//!
//! # Generic over the state type
//!
//! Every piece is `<State>`-generic rather than pinned to
//! [`crate::AppState`]: the splash composes them under the app state, while the
//! live screen composes them under its own `Component::State` (the charter's
//! per-screen state boundary). No callback here reads the state it is handed —
//! they all drive the controller's signals — so one body serves both.
//!
//! # Real components vs. hand-composed
//!
//! Facade/Glyph components carry the pieces wherever one fits — [`text`] for
//! every run, [`text_input`] for both fields, [`button`] for the CTA,
//! [`radio`] for the trust rows (the mock's `.radio-opt`/`.radio-dot` shape
//! exactly), [`glyph::badge`] for the "insecure"/"read-only" chips, and
//! [`GestureDetector`] for the tappable saved rows. The rest of the mock is
//! borders and washes with no catalog counterpart (there is no Glyph toggle, and
//! the `material` catalog's `Switch` is not compiled into this app — see
//! `Cargo.toml`'s `default-features = false, features = ["glyph", …]`), so those
//! compose from [`super::chrome`]'s panel/swatch primitives, with
//! [`frust::motion::AnimatedOpacity`] carrying the toggle's on/off crossfade.
//!
//! # Tokens
//!
//! `Theme::glyph_baseline()`'s tokens are canonical where they and the
//! source's CSS disagree (radii, type sizes, ink). Two of the source's colors
//! have no `ColorScheme` role and are derived from one that does, by alpha,
//! with the derivation noted at the constant. Where the two *mocks* disagree
//! (`docs/splash.html` is older than `docs/redesign.html`), the redesign wins
//! — see [`Palette::page`] and `super::pages`' `LAYER_GAP`.
//!
//! Two type sizes are deliberately the Glyph role rather than the mock's
//! number: `.field input{font-size:12px}` and `.connect .sub{11.5px}` become
//! `body_medium` (13) and `body_small` (11), because the Glyph scale's
//! mobile-legibility floor is a framework policy this app does not relitigate
//! per screen.

use std::rc::Rc;
use std::sync::Arc;

use frust::authoring::text::{FamilyName, FontFamily, GenericSlot, TextStyle};
use frust::authoring::{Color, Size};
use frust::glyph::{BadgeVariant, badge};
use frust::motion::AnimatedOpacity;
use frust::{
    Align, Alignment, AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexView, GestureDetector,
    Get, Padding, SizedBox, StatusPalette, Theme, any, button, flexible, inflexible, radio, text,
    text_input,
};

use super::chrome::{panel, swatch, switch_semantics};
use super::controller::ConnectController;
use crate::connection::TrustMode;
use crate::features::connect::domain::entities::SavedServer;

// ---------------------------------------------------------------------------
// The screen's live wiring
// ---------------------------------------------------------------------------

/// What a piece needs to be a control rather than a picture: the screen's
/// hosted controller, plus the one action it cannot express itself.
///
/// Built fresh on every `ConnectScreen::build` (its two handles are cheap
/// clones) and passed down by reference; a piece clones out of it into each
/// callback it wires.
///
/// [`Self::on_connect`] is a state-free `Rc<dyn Fn()>` rather than a controller
/// method because connecting also *navigates*, and the navigator handle it needs
/// (`frust::Router`, `Rc`-backed) cannot live on a `Send + Sync` controller —
/// the screen owns it and closes over it here. Same shape, and for the same
/// reason, as `crate::ui::sheet`'s `on_dismiss`.
pub struct Live {
    /// The screen's form/view model.
    pub controller: Arc<ConnectController>,
    /// Validate, connect, and navigate — see the type docs.
    pub on_connect: Rc<dyn Fn()>,
}

// ---------------------------------------------------------------------------
// Geometry (transcribed from `docs/splash.html` and `docs/redesign.html`'s CSS,
// once)
// ---------------------------------------------------------------------------

/// `.connect-mark{width:60px;height:60px}`.
const MARK_SIZE: f64 = 60.0;
/// `.connect-mark{padding:12px}`.
const MARK_PAD: f64 = 12.0;
/// `.connect-mark{gap:5px}`.
const MARK_GAP: f64 = 5.0;
/// One grid cell: the tile minus its padding and the single gap, halved
/// (`grid-template-columns:1fr 1fr`).
const MARK_CELL: f64 = (MARK_SIZE - 2.0 * MARK_PAD - MARK_GAP) / 2.0;
/// `.connect-badge{padding:8px 12px}`.
const BADGE_PAD_X: f64 = 12.0;
const BADGE_PAD_Y: f64 = 8.0;
/// `.connect-badge b{margin-bottom:2px}`.
const BADGE_LINE_GAP: f64 = 2.0;
/// `.heading h2{margin-bottom:6px}`.
const HEADING_GAP: f64 = 6.0;
/// `.field{gap:6px}` — between a field's label and its input.
const FIELD_GAP: f64 = 6.0;
/// `.toggle{width:36px;height:21px}`.
const TOGGLE_W: f64 = 36.0;
const TOGGLE_H: f64 = 21.0;
/// `.toggle::after{width:15px;height:15px;top:2px;left:2px}` — the knob, and
/// its **off** inset; `.toggle.on::after{left:17px}` is the on one.
const KNOB: f64 = 15.0;
const KNOB_LEFT_OFF: f64 = 2.0;
const KNOB_LEFT_ON: f64 = 17.0;
const KNOB_TOP: f64 = 2.0;
/// `.dot-ok{width:7px;height:7px}`.
const DOT: f64 = 7.0;
/// `.saved-row{gap:10px}` — dot to text, and text to badge.
const SAVED_GAP: f64 = 10.0;
/// `.saved-row{padding:11px 0}`.
const SAVED_ROW_PAD_Y: f64 = 11.0;
/// `.saved-hdr{gap:8px}` — label to the hairline rule that fills the rest.
const SAVED_HDR_GAP: f64 = 8.0;
/// `.saved-hdr::after{height:1px}` — the rule's weight.
const RULE_HEIGHT: f64 = 1.0;
/// Gap under the "saved servers" header, before the first row. The source has
/// none of its own (`.saved-row`'s `padding:11px 0` supplies it); named here
/// because this column stacks the header as a sibling rather than as a `::after`.
const SAVED_HDR_BOTTOM_GAP: f64 = 2.0;
/// `.saved-row .addr` over `.saved-row .meta`, in logical px — an original
/// micro-gap (the source stacks two `div`s with no explicit spacing).
const SAVED_LINE_GAP: f64 = 2.0;
/// Gap between the remember row and the trust-mode selector under it — the
/// `.connect{gap:22px}` step, since the mock renders them as two siblings of
/// that flex column.
const OPTIONS_GAP: f64 = 22.0;
/// `.radio-opt{padding:9px 0}`.
const RADIO_PAD_Y: f64 = 9.0;

/// The remember row's copy — also the toggle's accessibility label, so the
/// switch announces the thing the row is about rather than "switch".
const REMEMBER_LABEL: &str = "Remember for 4 weeks";

/// `.field input[placeholder]` — the server-URL field's ghost text. `pub`
/// because it is the exact string `tests/connect.rs`'s shaping suite measures
/// (see [`field_font_family`]); a copy there could drift.
pub const SERVER_URL_PLACEHOLDER: &str = "zelli.example.com:50051";
/// `.field input[placeholder]` — the token field's. See
/// [`SERVER_URL_PLACEHOLDER`]; this one is the all-letter control the device
/// rendered correctly.
pub const TOKEN_PLACEHOLDER: &str = "paste or scan a token";

// ---------------------------------------------------------------------------
// Token derivations
// ---------------------------------------------------------------------------

/// `--amber-faint: rgba(255,182,39,0.12)` — the accent at 12%, over whatever
/// is behind it. No `ColorScheme` role carries a translucent accent wash
/// (`primary_container` is the *opaque bright fill*), so it derives from
/// `primary` by alpha rather than becoming a second amber constant.
const AMBER_FAINT_ALPHA: f32 = 0.12;
/// `--amber-border: rgba(255,182,39,0.35)` — same derivation, hairline weight.
const AMBER_BORDER_ALPHA: f32 = 0.35;
/// `--fg-dim` (`#6b6556` dark) against `--fg-muted` (`#a39c88`).
/// `frust_theme`'s Glyph mapping records `fg-dim` as a **documented leftover**
/// with no `ColorScheme` role of its own (`on_surface_variant` already
/// carries `fg-muted`), so the dimmest label ink derives from that role by
/// alpha: 0.62 over the page tone reproduces the source pair within a few
/// units per channel, and tracks any re-themed `on_surface_variant` instead
/// of freezing one dark-mode hex.
const FG_DIM_ALPHA: f32 = 0.62;

/// Every color the Connect pieces paint, resolved once per build from the
/// active theme (see [`palette`]).
#[derive(Clone, Copy)]
struct Palette {
    /// `--bg-base`: the page tone, backing each piece so the splash's rain
    /// can't bleed through its text mid-assembly.
    ///
    /// The *same* role [`crate::ui::backdrop::page_backdrop`] paints, on
    /// purpose: over the route's own backdrop these backings are invisible
    /// (`docs/redesign.html` gives `.connect` no background of its own —
    /// `.screen{background:var(--bg-base)}` is the whole page), while the
    /// splash still gets something opaque between its rain and the assembling
    /// text. `docs/splash.html`'s older `--bg-void` for both is superseded —
    /// see that backdrop's module docs.
    page: Color,
    /// `--border-bright`: every hairline border in the mock.
    outline: Color,
    /// `--border`: the fainter rule under the "saved servers" header
    /// (`ColorScheme::outline_variant` is Glyph's own `border` token — the
    /// 0.09-alpha sibling of `outline`'s 0.18, already flattened).
    rule: Color,
    /// `--bg-overlay`: the toggle track in its **off** state.
    overlay: Color,
    /// The bright accent fill (`primary_container`, Glyph's accent-role split
    /// — the *fill* half, never `primary`, which is accent ink).
    amber: Color,
    /// `--amber-faint`, derived (see [`AMBER_FAINT_ALPHA`]).
    amber_faint: Color,
    /// `--amber-border`, derived (see [`AMBER_BORDER_ALPHA`]).
    amber_border: Color,
    /// `--fg`: primary text.
    fg: Color,
    /// `--fg-muted`: secondary text.
    fg_muted: Color,
    /// `--fg-dim`, derived (see [`FG_DIM_ALPHA`]).
    fg_dim: Color,
    /// `--success`: the saved-address status dot.
    success: Color,
    /// `--error`: the server-URL label's ink while the form is invalid.
    error: Color,
}

/// Resolve [`Palette`] from `theme`.
fn palette(theme: &Theme) -> Palette {
    let scheme = theme.scheme();
    Palette {
        page: scheme.surface_container_low,
        outline: scheme.outline,
        rule: scheme.outline_variant,
        overlay: scheme.surface_container_highest,
        amber: scheme.primary_container,
        amber_faint: scheme.primary.with_alpha(AMBER_FAINT_ALPHA),
        amber_border: scheme.primary.with_alpha(AMBER_BORDER_ALPHA),
        fg: scheme.on_surface,
        fg_muted: scheme.on_surface_variant,
        fg_dim: scheme.on_surface_variant.with_alpha(FG_DIM_ALPHA),
        error: scheme.error,
        // Per `docs/CODE_STANDARDS.md`'s Glyph precedent: a per-status color
        // with no `ColorScheme` field resolves `StatusPalette` first. A Glyph
        // theme always carries it; `primary` is the last-resort ink if some
        // other theme is ever installed.
        success: theme
            .extension::<StatusPalette>()
            .map_or(scheme.primary, |status| {
                status.colors(theme.brightness).success
            }),
    }
}

/// The active theme, or the Glyph baseline when none is threaded (headless
/// tests, a shell with no design system installed). **Never**
/// `Theme::neutral`/`m3_baseline` — see `lib.rs`'s design-system note.
pub fn active_theme() -> Theme {
    frust::use_context::<Theme>().unwrap_or_else(Theme::glyph_baseline)
}

/// `style` re-inked with `color` — the pieces pick a Glyph type role for
/// size/family/weight/tracking and an explicit role-resolved ink, so nothing
/// falls back to `TextStyle::default()`'s 16px black.
fn inked(style: &TextStyle, color: Color) -> TextStyle {
    let mut style = style.clone();
    style.color = color;
    style
}

/// A `gap`-tall spacer between stacked pieces/rows.
fn v_gap<S: 'static>(gap: f64) -> AnyView<S> {
    any(SizedBox::<S>(None, Some(gap)))
}

/// A `gap`-wide spacer between row items.
fn h_gap<S: 'static>(gap: f64) -> AnyView<S> {
    any(SizedBox::<S>(Some(gap), None))
}

/// A vertical stack whose children each fill the available width (the CSS
/// block-flow the mock's `.ui-layer`/`.field`/`.heading` all assume).
fn column<S: 'static>(children: Vec<AnyView<S>>) -> FlexView<S> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Stretch)
}

/// A vertical stack that **shrink-wraps** to its widest child and centers each
/// one — the `margin:0 auto` blocks (`.connect-mark`'s grid,
/// `.connect-badge`'s two lines), whose boxes must hug their content rather
/// than span the layer. Stretching one of these is the exact defect that
/// turned the mark's 60px tile into a full-width bar.
fn hugging_column<S: 'static>(children: Vec<AnyView<S>>) -> FlexView<S> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Center)
}

/// A horizontal row with its items vertically centered.
fn row<S: 'static>(children: Vec<frust::FlexChild<S>>) -> FlexView<S> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

/// A full-width piece backing in the page tone, square-cornered like the
/// source's. See [`Palette::page`] for why this is the page's own colour
/// rather than a second one.
fn backed<S: 'static>(child: impl frust::View<S>, colors: &Palette) -> AnyView<S> {
    any(panel(child, colors.page, 0.0))
}

// ---------------------------------------------------------------------------
// The eight pieces
// ---------------------------------------------------------------------------

/// Piece 0 — `.connect-mark`: a bordered 60×60 tile holding a 2×2 grid whose
/// diagonal is solid amber and whose off-diagonal is the faint amber wash,
/// centered in the layer (`margin:0 auto`). Pure decoration — no [`Live`].
pub fn connect_mark<S: 'static>(theme: &Theme) -> AnyView<S> {
    let colors = palette(theme);
    let cell = Size::new(MARK_CELL, MARK_CELL);
    let cell_radius = theme.shape.extra_small;
    // `.connect-mark span:nth-child(1),(4)` — faint wash + hairline accent
    // border; `(2),(3)` — the solid accent fill.
    let faint = || any(swatch(cell, colors.amber_faint, cell_radius).border(colors.amber_border));
    let solid = || any(swatch(cell, colors.amber, cell_radius));
    let grid = hugging_column::<S>(vec![
        any(row::<S>(vec![
            inflexible(faint()),
            inflexible(h_gap::<S>(MARK_GAP)),
            inflexible(solid()),
        ])),
        v_gap::<S>(MARK_GAP),
        any(row::<S>(vec![
            inflexible(solid()),
            inflexible(h_gap::<S>(MARK_GAP)),
            inflexible(faint()),
        ])),
    ]);
    let tile = panel(
        Padding(EdgeInsets::all(MARK_PAD), grid),
        colors.page,
        theme.shape.medium,
    )
    .border(colors.outline);
    any(Align(Alignment::CENTER, tile))
}

/// Piece 1 — `.connect-badge`: a centered, bordered two-line box naming the
/// daemon and the multiplexers it fronts. Pure decoration — no [`Live`].
pub fn server_badge<S: 'static>(theme: &Theme) -> AnyView<S> {
    let colors = palette(theme);
    // Hugging, not stretched: the box is `margin:0 auto`-width, so the lines
    // center themselves against each other rather than against the layer.
    let lines = hugging_column::<S>(vec![
        any(text("muxrd server").style(inked(&theme.type_scale.body_small, colors.fg_muted))),
        v_gap::<S>(BADGE_LINE_GAP),
        any(text("zellij · herdr").style(inked(&theme.type_scale.label_small, colors.fg_dim))),
    ]);
    let box_ = panel(
        Padding(EdgeInsets::symmetric(BADGE_PAD_X, BADGE_PAD_Y), lines),
        colors.page,
        theme.shape.small,
    )
    .border(colors.outline);
    any(Align(Alignment::CENTER, box_))
}

/// Piece 2 — `.heading`: the screen's title and its one-line explanation,
/// both centered. Pure copy — no [`Live`].
pub fn heading<S: 'static>(theme: &Theme) -> AnyView<S> {
    let colors = palette(theme);
    backed(
        column::<S>(vec![
            any(Align(
                Alignment::CENTER,
                text("Connect to server").style(inked(&theme.type_scale.title_medium, colors.fg)),
            )),
            v_gap::<S>(HEADING_GAP),
            any(Align(
                Alignment::CENTER,
                text("Enter credentials to attach to a remote zellij or herdr session.")
                    .style(inked(&theme.type_scale.body_small, colors.fg_muted)),
            )),
        ]),
        &colors,
    )
}

/// Piece 3 — the first `.field`: the server URL, bound to
/// [`ConnectController::url`].
///
/// Its label inks `error` while [`ConnectController::invalid`] is set — the
/// screen's whole validation surface, since `frust`'s `TextInput` exposes no
/// error state of its own (FINDINGS #16 covers the neighbouring disabled gap).
pub fn server_url_field<S: 'static>(theme: &Theme, live: Option<&Live>) -> AnyView<S> {
    let invalid = live.is_some_and(|live| live.controller.invalid.get());
    let value = live
        .map(|live| live.controller.url.get())
        .unwrap_or_default();
    let controller = live.map(|live| Arc::clone(&live.controller));
    field(
        theme,
        "SERVER URL",
        SERVER_URL_PLACEHOLDER,
        invalid,
        value,
        move |next| {
            if let Some(controller) = &controller {
                controller.set_url(next);
            }
        },
    )
}

/// Piece 4 — the second `.field`: the login token, bound to
/// [`ConnectController::token`].
///
/// The source marks this input `type="password"`; `frust`'s `TextInput` has no
/// obscured mode (FINDINGS #20), so the token renders in the clear — a known,
/// filed gap rather than a decision.
///
// TODO(#20 adoption): swap the plain input for the obscured mode the moment
// this machine's `frust` dev carries it. Deliberately *not* faked here — a
// hand-masked value would have to shadow the controller's real one and would
// still leak through the IME surface `TextInputWidget` publishes.
pub fn token_field<S: 'static>(theme: &Theme, live: Option<&Live>) -> AnyView<S> {
    let value = live
        .map(|live| live.controller.token.get())
        .unwrap_or_default();
    let controller = live.map(|live| Arc::clone(&live.controller));
    field(
        theme,
        "LOGIN TOKEN",
        TOKEN_PLACEHOLDER,
        false,
        value,
        move |next| {
            if let Some(controller) = &controller {
                controller.set_token(next);
            }
        },
    )
}

/// A `.field`: an uppercase micro label over a placeholder-bearing input.
///
/// The label is uppercased in the source (`text-transform:uppercase`) at the
/// call site instead, since no `TextStyle` field expresses a transform.
///
/// `on_change` is invoked with the requested text; a splash-composed field
/// passes one that drops it, so the field always re-renders its placeholder
/// (`readonly` in the source).
///
/// # Chrome the app cannot reach (FINDINGS #16 family)
///
/// `.field input` asks for `padding:11px 12px`, a 1px `border-bright` hairline
/// and, on focus, `border-color:var(--amber-border)` plus a
/// `box-shadow:0 0 0 3px var(--amber-faint)` halo. `TextInputWidget` bakes its
/// own `PAD_X`/`PAD_Y` (8/6), `BORDER_W` (1.5) and focus treatment (an accent
/// border, no halo) into private constants, and exposes only
/// [`text_input`]'s `text_style`. What *does* land: the radius is already
/// `--radius-sm` (6px), the fill is themed from `surface` (`--bg-surface`) and
/// the idle border from `outline` (`--border-bright`) — so the colours are
/// right and only the metrics are off. Nothing here can close the rest
/// app-side; it is a framework gap, not a decision.
fn field<S: 'static>(
    theme: &Theme,
    label: &str,
    placeholder: &str,
    invalid: bool,
    value: String,
    on_change: impl Fn(String) + 'static,
) -> AnyView<S> {
    let colors = palette(theme);
    let label_ink = if invalid { colors.error } else { colors.fg_dim };
    backed(
        column::<S>(vec![
            any(text(label).style(inked(&theme.type_scale.label_small, label_ink))),
            v_gap::<S>(FIELD_GAP),
            any(
                text_input::<S, _>(value, move |_state: &mut S, next: String| on_change(next))
                    .placeholder(placeholder)
                    .text_style(field_text_style(theme, colors.fg)),
            ),
        ]),
        &colors,
    )
}

/// The body style both fields shape their content **and** their placeholder
/// with — `body_medium` re-inked, with one deliberate change to its family.
/// See [`field_font_family`].
///
/// `pub` so `tests/connect.rs`'s placeholder-shaping suite can shape the real
/// strings with the real style, rather than a copy that could drift out from
/// under the very defect it guards.
pub fn field_text_style(theme: &Theme, ink: Color) -> TextStyle {
    let mut style = inked(&theme.type_scale.body_medium, ink);
    style.family = field_font_family(&style.family);
    style
}

/// `family` with a **resolvable terminal fallback** appended
/// ([`GenericSlot::SansSerif`]) — the app-side half of the "the placeholder's
/// digits are individually wide-spaced" defect.
///
/// # The defect, and its two causes
///
/// On device the server-URL placeholder rendered `zelli.example.com: 5 0 0 5 1`
/// while the token field's all-letter placeholder was fine. Two framework facts
/// compose into it:
///
/// 1. **`TextInputWidget` owns a private `TextContext::new()`** (its module docs
///    explain why: edits must apply during the clockless event pass, where no
///    shared context is threaded). `frust::register_app_fonts` — which
///    `glyph_theme::install()` uses to publish the bundled IBM Plex Mono /
///    Space Mono faces — is drained only into the *shell's* shared context. So
///    the Glyph faces never reach any text field: every `text` run on this
///    screen shapes as IBM Plex Mono, and the two fields alone do not.
/// 2. **fontique 0.11 leaves `GenericFamily::Monospace` empty on Android**: its
///    `DEFAULT_GENERIC_FAMILIES` maps the slot to the literal family name
///    `"monospace"`, which the `/system/fonts` scan never produces, and the
///    `fonts.xml` alias pass that could supply it runs *after* the generic map
///    is built.
///
/// Together, the Glyph type scale's `["IBM Plex Mono"] + monospace` stack
/// resolves to **nothing** inside a field on Android. parley then falls back
/// per cluster, splitting the string into two runs — the letters onto the Latin
/// fallback, the digits onto one that advances them a full em. Measured
/// headlessly at 13px (`tests/connect.rs`'s placeholder suite): letters
/// ~2.9–10.8px, digits exactly **13.0** each.
///
/// Appending a generic that *does* resolve everywhere collapses it back to one
/// run at the letters' own advances (measured: digits 7.23px, identical to the
/// letters' proportional metrics). Mono is still preferred and still wins
/// wherever it resolves — desktop and iOS reach a real monospace through the
/// generic slot — so this costs the fields nothing where nothing was broken.
///
/// **Scoped to the fields on purpose**: every other run on this screen goes
/// through `text`, which shapes against the shell's font-registered context and
/// resolves IBM Plex Mono correctly. Widening this would swap a correct family
/// for a fallback stack in the places that don't need it.
fn field_font_family(family: &FontFamily) -> FontFamily {
    let mut names: Vec<FamilyName> = match family {
        FontFamily::SystemUi => vec![FamilyName::Generic(GenericSlot::SystemUi)],
        FontFamily::Named(names) => names.iter().cloned().map(FamilyName::Named).collect(),
        FontFamily::NamedWithGeneric(names) => names.clone(),
    };
    names.push(FamilyName::Generic(GenericSlot::SansSerif));
    FontFamily::NamedWithGeneric(names)
}

/// Piece 5 — `.remember-row` plus the `.radio-opt` block under it: the
/// remember toggle and the trust-mode selector.
///
/// The two share one piece because the splash's stagger table has exactly eight
/// slots (`spec::PIECE_DELAYS_MS`) and the mock renders these as adjacent
/// siblings with no separate entrance of their own.
pub fn remember_row<S: 'static>(theme: &Theme, live: Option<&Live>) -> AnyView<S> {
    let colors = palette(theme);
    let remembered = live.is_none_or(|live| live.controller.remember.get());
    let toggle: AnyView<S> = match live {
        // Splash: the static on-state art, exactly as the mock draws it — no
        // recognizer, and no `Role::Switch` node either, since there is nothing
        // there to operate.
        None => toggle_art(&colors, true),
        Some(live) => {
            let controller = Arc::clone(&live.controller);
            any(switch_semantics(
                GestureDetector(toggle_art::<S>(&colors, remembered))
                    .on_tap(move |_state: &mut S| controller.toggle_remember()),
                REMEMBER_LABEL,
                remembered,
            ))
        }
    };
    backed(
        column::<S>(vec![
            any(row::<S>(vec![
                // The flexible label takes every pixel the toggle doesn't, which
                // is what pins the toggle to the trailing edge (`Flex`'s v1
                // `MainAxisAlignment` has only `Start`, so there is no
                // space-between to reach for).
                flexible(
                    1,
                    any(text(REMEMBER_LABEL)
                        .style(inked(&theme.type_scale.body_small, colors.fg_muted))),
                ),
                inflexible(toggle),
            ])),
            v_gap::<S>(OPTIONS_GAP),
            trust_selector(live),
        ]),
        &colors,
    )
}

/// The `.toggle` pill at `on`: a 36×21 track that crossfades from
/// `--bg-overlay` to the faint amber wash, and a 15px knob that crossfades
/// between its two parked positions.
///
/// Position is animated by **opacity**, not by offset: `frust::motion` ships
/// `AnimatedOpacity`/`AnimatedScale` and no animated-offset wrapper, so two
/// knobs are drawn (one per end) and faded against each other. Both timings
/// default from `Theme.motion`, which is what makes the whole control collapse
/// under `reduce_motion` for free (`docs/CODE_STANDARDS.md`'s transition rule).
///
/// # Both tracks fade; neither is a floor
///
/// `.toggle.on{background:var(--amber-faint)}` *replaces* the off track's
/// `--bg-overlay`, and `--amber-faint` is `rgba(255,182,39,0.12)` — 12% alpha.
/// Painting it over an opaque `--bg-overlay` composites 88% of the dark track
/// straight back through, which is exactly the "on toggle with a dark track"
/// the device showed; the same goes for the amber border over the neutral
/// `outline` one. So the **off** track is faded out by its own
/// `AnimatedOpacity` rather than left underneath as an opaque base, and at rest
/// the on state is the amber wash over the page tone, as authored.
fn toggle_art<S: 'static>(colors: &Palette, on: bool) -> AnyView<S> {
    // `border-radius:999px` — a pill. The Glyph shape scale spells `full` as
    // `f64::INFINITY`, so the drawable radius is half the track.
    let radius = TOGGLE_H / 2.0;
    let size = Size::new(TOGGLE_W, TOGGLE_H);
    let off_track = swatch(size, colors.overlay, radius).border(colors.outline);
    let on_track = swatch(size, colors.amber_faint, radius).border(colors.amber_border);
    let knob = |left: f64, fill: Color| {
        Padding(
            EdgeInsets {
                left,
                top: KNOB_TOP,
                right: TOGGLE_W - left - KNOB,
                bottom: TOGGLE_H - KNOB_TOP - KNOB,
            },
            swatch(Size::new(KNOB, KNOB), fill, KNOB / 2.0),
        )
    };
    let (on_opacity, off_opacity) = if on { (1.0, 0.0) } else { (0.0, 1.0) };
    any(frust::Stack(vec![
        any(AnimatedOpacity::<S, _>(off_opacity, off_track)),
        any(AnimatedOpacity::<S, _>(on_opacity, on_track)),
        any(AnimatedOpacity::<S, _>(
            off_opacity,
            knob(KNOB_LEFT_OFF, colors.fg_dim),
        )),
        any(AnimatedOpacity::<S, _>(
            on_opacity,
            knob(KNOB_LEFT_ON, colors.amber),
        )),
    ]))
}

/// The `.radio-opt` block: one row per [`TrustMode`], the riskiest one carrying
/// the mock's `.warn-chip`.
///
/// Three rows where the mock draws two: `TrustMode` is a three-way enum
/// (`System`/`TrustOnFirstUse`/`Pinned`) and the mock predates the pinned-cert
/// pairing flow, so "Trust anyway" is split into the two postures it actually
/// covers. The chip stays on the one that genuinely skips validation.
///
/// Built from [`radio`] rather than [`frust::glyph::segmented_control`]: the
/// mock's own shape is a stacked radio list with a per-row trailing chip, which
/// a segmented control (one pill, equal-width labels, no per-segment slot)
/// cannot render.
fn trust_selector<S: 'static>(live: Option<&Live>) -> AnyView<S> {
    let selected = live.map_or(TrustMode::System, |live| live.controller.trust_mode.get());
    let rows = TRUST_MODES
        .iter()
        .map(|&mode| trust_row(mode, selected == mode, live))
        .collect();
    any(column::<S>(rows))
}

/// Every [`TrustMode`], in the order the selector stacks them: safest first.
const TRUST_MODES: [TrustMode; 3] = [
    TrustMode::System,
    TrustMode::TrustOnFirstUse,
    TrustMode::Pinned,
];

/// This mode's `.radio-opt` label.
fn trust_option_label(mode: TrustMode) -> &'static str {
    match mode {
        // The mock's own string for the CA-verified row.
        TrustMode::System => "Public domain (CA)",
        TrustMode::TrustOnFirstUse => "Trust on first use",
        TrustMode::Pinned => "Pinned certificate",
    }
}

/// This mode's short form, for a saved row's meta line (the mock's
/// "insecure-dev TLS · port 50051").
fn trust_meta_label(mode: TrustMode) -> &'static str {
    match mode {
        TrustMode::System => "system TLS",
        TrustMode::TrustOnFirstUse => "first-use TLS",
        TrustMode::Pinned => "pinned TLS",
    }
}

/// One `.radio-opt` row.
///
/// Takes no [`Palette`]: both halves theme themselves — [`radio`] resolves its
/// ring/dot from `outline`/`primary`, and [`badge`] resolves the warning wash
/// from `StatusPalette` — so there is nothing here for the piece's own resolved
/// inks to do.
fn trust_row<S: 'static>(mode: TrustMode, selected: bool, live: Option<&Live>) -> AnyView<S> {
    let mut option = radio::<S>(selected, trust_option_label(mode));
    if let Some(live) = live {
        let controller = Arc::clone(&live.controller);
        option = option.on_select(move |_state: &mut S| controller.select_trust_mode(mode));
    }
    let mut items = vec![flexible(1, any(option))];
    // `.radio-opt .warn-chip` — only on the posture that actually skips
    // certificate validation.
    //
    // The chip is `glyph::badge`, not a hand-rolled pill, and it is knowingly
    // a shade larger than the mock's one-off `.warn-chip` (10.5px/9×4 medium
    // against 9px/6×2): those are the Glyph catalog's *own* authored `.badge`
    // metrics, and this module's Tokens rule makes the design system canonical
    // where it and a mock's ad-hoc rule disagree. It is also the identical
    // chip the Sessions strip renders for the identical posture, so forking one
    // of them to chase 1.5px would be the worse outcome. The one genuine gap:
    // `BadgeVariant::Warning` paints no border, where `.warn-chip` asks for a
    // 1px `--warning-border` — `resolve_badge_colors` returns `None` for every
    // variant but `Neutral`, and the app cannot supply one.
    if mode == TrustMode::TrustOnFirstUse {
        items.push(inflexible(h_gap::<S>(SAVED_GAP)));
        items.push(inflexible(any(badge("insecure", BadgeVariant::Warning))));
    }
    any(Padding(
        EdgeInsets::symmetric(0.0, RADIO_PAD_Y),
        row::<S>(items),
    ))
}

/// Piece 6 — `.btn-primary`: the amber CTA, firing [`Live::on_connect`].
///
/// A real [`button`] (Glyph-themed through `ButtonStyle::Primary`'s
/// `primary`/`on_primary` roles — the source's `#ffb627` on `#241a04`), so
/// the press feedback, disabled/loading states and accessibility role all
/// come from the framework rather than being repainted here. **Centered at
/// its natural width** rather than stretched full-bleed like the source's
/// block-level button: `ButtonWidget` pins its label at the leading padding
/// (FINDINGS #15), so a stretched button would render a left-hugged label in a
/// full-width bar.
///
// TODO(#15 adoption): the label-centering fix has landed frust-side but is not
// in this machine's `frust` dev yet. When it is, drop the `Align` and let the
// button stretch to the layer width (`.btn-primary` is block-level with
// `text-align:center`). Deliberately *not* faked with a hand-painted
// full-width bar in the meantime: that would throw away the press feedback,
// the disabled/loading states and the `Role::Button` node.
pub fn connect_button<S: 'static>(_theme: &Theme, live: Option<&Live>) -> AnyView<S> {
    let on_connect = live.map(|live| Rc::clone(&live.on_connect));
    any(Align(
        Alignment::CENTER,
        button("→ connect", move |_state: &mut S| {
            if let Some(on_connect) = &on_connect {
                on_connect();
            }
        }),
    ))
}

/// Piece 7 — `.saved-hdr` + `.saved-row`: the saved-server list, each row
/// tappable to refill the form.
///
/// With no [`Live`] (the splash) this renders the mock's single stand-in row —
/// see the [module docs](self)'s note on the one deliberate splash/route
/// divergence.
pub fn saved_address<S: 'static>(theme: &Theme, live: Option<&Live>) -> AnyView<S> {
    let colors = palette(theme);
    let servers = match live {
        None => vec![splash_placeholder_row()],
        Some(live) => live.controller.saved.get(),
    };
    let mut children = vec![saved_header(theme, &colors), v_gap(SAVED_HDR_BOTTOM_GAP)];
    for (index, server) in servers.iter().enumerate() {
        children.push(saved_row(theme, &colors, server, index, live));
    }
    backed(column::<S>(children), &colors)
}

/// The row the splash paints in place of the repository's list: the mock's own
/// `.saved-row`, which is also where the seeded store's first entry came from
/// (`data::repositories::mock_saved_servers`).
fn splash_placeholder_row() -> SavedServer {
    SavedServer {
        host: "100.71.31.57".to_string(),
        port: 50051,
        trust_mode: TrustMode::TrustOnFirstUse,
        label: "Home rig".to_string(),
        last_used: "2 hours ago".to_string(),
        read_only: false,
    }
}

/// `.saved-hdr`: an uppercase micro label followed by a hairline rule filling
/// the rest of the row (`::after{flex:1;height:1px}`).
fn saved_header<S: 'static>(theme: &Theme, colors: &Palette) -> AnyView<S> {
    any(row::<S>(vec![
        inflexible(any(
            text("SAVED SERVERS").style(inked(&theme.type_scale.label_small, colors.fg_dim))
        )),
        inflexible(h_gap::<S>(SAVED_HDR_GAP)),
        // A zero-width swatch under a flexible slot: the flex's tight
        // cross-constraint is what gives it the remaining width.
        flexible(
            1,
            any(swatch(Size::new(0.0, RULE_HEIGHT), colors.rule, 0.0)),
        ),
    ]))
}

/// One `.saved-row`: status dot, `host:port` over its meta line, and a
/// read-only badge where the stored credential is limited.
fn saved_row<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    server: &SavedServer,
    index: usize,
    live: Option<&Live>,
) -> AnyView<S> {
    let meta = format!(
        "{} · {} · port {}",
        trust_meta_label(server.trust_mode),
        server.last_used,
        server.port
    );
    let mut items = vec![
        // `.dot-ok{box-shadow:0 0 5px var(--success)}` — the glow is part of
        // the dot, not decoration: it is what reads as "live" at 7px. Same
        // treatment, same constants, as the Sessions strip's own dot.
        inflexible(any(
            swatch(Size::new(DOT, DOT), colors.success, DOT / 2.0).glow(colors.success)
        )),
        inflexible(h_gap::<S>(SAVED_GAP)),
        flexible(
            1,
            any(column::<S>(vec![
                any(text(server.endpoint()).style(inked(&theme.type_scale.body_small, colors.fg))),
                v_gap::<S>(SAVED_LINE_GAP),
                any(text(meta).style(inked(&theme.type_scale.label_small, colors.fg_dim))),
            ])),
        ),
    ];
    if server.read_only {
        items.push(inflexible(h_gap::<S>(SAVED_GAP)));
        items.push(inflexible(any(badge("read-only", BadgeVariant::Neutral))));
    }
    let body = Padding(EdgeInsets::symmetric(0.0, SAVED_ROW_PAD_Y), row::<S>(items));
    match live {
        None => any(body),
        Some(live) => {
            let controller = Arc::clone(&live.controller);
            any(GestureDetector(body).on_tap(move |_state: &mut S| controller.select_saved(index)))
        }
    }
}

// ---------------------------------------------------------------------------
// The list
// ---------------------------------------------------------------------------

/// The eight pieces, in `docs/splash.html`'s source order — which is also the
/// order their `data-delay` staggers are indexed by
/// (`splash::spec::PIECE_DELAYS_MS`).
///
/// `live` is `Some(..)` for the screen and `None` for the splash — see the
/// [module docs](self).
pub fn connect_pieces_with<S: 'static>(theme: &Theme, live: Option<&Live>) -> Vec<AnyView<S>> {
    vec![
        connect_mark(theme),
        server_badge(theme),
        heading(theme),
        server_url_field(theme, live),
        token_field(theme, live),
        remember_row(theme, live),
        connect_button(theme, live),
        saved_address(theme, live),
    ]
}

/// The eight pieces with **no** live wiring — the shared splash/route seam
/// (`crate::AppState::splash_overlay` and `tests/sequence.rs` both call exactly
/// this). See [`connect_pieces_with`].
pub fn connect_pieces<S: 'static>(theme: &Theme) -> Vec<AnyView<S>> {
    connect_pieces_with(theme, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppState;
    use crate::ui::splash::spec;

    #[test]
    fn there_is_one_piece_per_source_stagger() {
        let theme = Theme::glyph_baseline();
        assert_eq!(
            connect_pieces::<AppState>(&theme).len(),
            spec::PIECE_DELAYS_MS.len(),
            "the piece list and the `data-delay` table are the same eight \
             elements, in the same order"
        );
    }

    #[test]
    fn the_grid_cells_tile_the_marks_content_box() {
        // 2 cells + 1 gap + 2 paddings == the tile edge (the CSS grid's own
        // arithmetic, asserted so a later tweak to one constant can't
        // silently unbalance the tile).
        assert!(
            (2.0 * MARK_CELL + MARK_GAP + 2.0 * MARK_PAD - MARK_SIZE).abs() < 1e-9,
            "cells must exactly fill the mark's content box"
        );
    }

    #[test]
    fn the_toggle_knob_sits_inside_its_track_at_both_ends() {
        // Each parked knob's padding is `track - inset - knob` on the trailing
        // edges; a negative one would be a layout panic in waiting.
        const { assert!(KNOB_LEFT_OFF + KNOB <= TOGGLE_W) };
        const { assert!(KNOB_LEFT_ON + KNOB <= TOGGLE_W) };
        const { assert!(KNOB_TOP + KNOB <= TOGGLE_H) };
        // …and the two positions are genuinely distinct, or the toggle would
        // have no on/off tell at all.
        const { assert!(KNOB_LEFT_OFF < KNOB_LEFT_ON) };
    }

    #[test]
    fn every_derived_ink_stays_a_legal_alpha() {
        for alpha in [AMBER_FAINT_ALPHA, AMBER_BORDER_ALPHA, FG_DIM_ALPHA] {
            assert!((0.0..=1.0).contains(&alpha));
        }
    }

    #[test]
    fn the_palette_resolves_glyphs_page_tone_and_status_hue() {
        let theme = Theme::glyph_baseline();
        let colors = palette(&theme);
        // `--bg-base` #10131a and `--success` #5fd88f, reached through roles
        // rather than hexes.
        assert_eq!(colors.page, Color::from_rgb8(0x10, 0x13, 0x1a));
        assert_eq!(colors.success, Color::from_rgb8(0x5f, 0xd8, 0x8f));
        assert_eq!(colors.amber, Color::from_rgb8(0xff, 0xb6, 0x27));
        assert!((colors.amber_faint.components[3] - AMBER_FAINT_ALPHA).abs() < 1e-6);
    }

    #[test]
    fn the_pieces_back_themselves_in_the_very_tone_the_backdrop_paints() {
        // The route mounts `ui::backdrop::page_backdrop` under these pieces;
        // if the two tones ever diverge the screen bands into eight visible
        // blocks, and the splash/route handoff steps colour mid-frame.
        let theme = Theme::glyph_baseline();
        assert_eq!(
            palette(&theme).page,
            theme.scheme().surface_container_low,
            "the piece backing and `page_backdrop` resolve the same role"
        );
    }

    #[test]
    fn a_fields_font_stack_always_ends_in_a_resolvable_generic() {
        // The whole of `field_font_family`'s defect: a stack that resolves to
        // nothing sends parley per-cluster, and the digits come back a full em
        // wide. A generic that resolves on every target platform is the floor
        // that cannot happen under.
        let theme = Theme::glyph_baseline();
        let style = field_text_style(&theme, Color::WHITE);
        let FontFamily::NamedWithGeneric(names) = &style.family else {
            panic!("a field's family must be a stack ending in a generic slot");
        };
        assert_eq!(
            names.last(),
            Some(&FamilyName::Generic(GenericSlot::SansSerif)),
            "got {names:?}"
        );
        // …without giving up the mono the Glyph body role asks for first.
        assert!(
            names.contains(&FamilyName::Named("IBM Plex Mono".to_string())),
            "the bundled Glyph face is still preferred: {names:?}"
        );
        assert!(
            names.contains(&FamilyName::Generic(GenericSlot::Monospace)),
            "…and so is a platform monospace, before the rescue: {names:?}"
        );
    }

    #[test]
    fn the_toggle_track_is_the_sources_own_pill() {
        // `.toggle{width:36px;height:21px}` with a 15px knob — the numbers the
        // device gate reads as "visibly larger" if one of them drifts.
        const { assert!(TOGGLE_W == 36.0) };
        const { assert!(TOGGLE_H == 21.0) };
        const { assert!(KNOB == 15.0) };
    }

    #[test]
    fn every_trust_mode_has_its_own_pair_of_labels() {
        let mut options: Vec<&str> = TRUST_MODES
            .iter()
            .copied()
            .map(trust_option_label)
            .collect();
        let mut metas: Vec<&str> = TRUST_MODES.iter().copied().map(trust_meta_label).collect();
        options.sort_unstable();
        options.dedup();
        metas.sort_unstable();
        metas.dedup();
        assert_eq!(options.len(), TRUST_MODES.len());
        assert_eq!(metas.len(), TRUST_MODES.len());
    }

    #[test]
    fn the_splash_placeholder_is_the_mocks_own_saved_row() {
        assert_eq!(splash_placeholder_row().endpoint(), "100.71.31.57:50051");
    }
}
