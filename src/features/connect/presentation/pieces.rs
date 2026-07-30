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
//! | 5 | [`remember_row`] | 580ms |
//! | 6 | [`connect_button`] | 720ms |
//! | 7 | [`saved_address`] | 860ms |
//!
//! - [`super::pages::connect_page`] lays them out plainly for the `/` route.
//! - [`crate::ui::splash::sequence`] lays out the *same* list through the
//!   *same* function with each piece wrapped in a spring-drop entrance, so the
//!   splash's final frame and the route's first frame are the same picture.
//!
//! # Static today
//!
//! Every piece is presentation only: the two fields are real
//! [`text_input`]s bound to an empty value and a no-op `on_change` (so they
//! show their placeholder and can never accumulate text), the button's
//! `on_press` is a no-op, and the toggle is drawn in its on-state rather than
//! being a control. Phase 2 upgrades each in place — that is why they are
//! separate functions rather than one inlined page.
//!
//! # Real components vs. hand-composed
//!
//! Facade/Glyph components carry the pieces wherever one fits — [`text`] for
//! every run, [`text_input`] for both fields, [`button`] for the CTA. The
//! rest of `docs/splash.html`'s Connect mock is borders and washes with no
//! catalog counterpart (`glyph::badge` is a lowercase status pill, not a
//! two-line bordered card; there is no Glyph toggle, and the `material`
//! catalog's `Switch` is not compiled into this app — see `Cargo.toml`'s
//! `default-features = false, features = ["glyph", …]`), so those compose
//! from [`super::chrome`]'s panel/swatch primitives.
//!
//! # Tokens
//!
//! `Theme::glyph_baseline()`'s tokens are canonical where they and the
//! source's CSS disagree (radii, type sizes, ink). Two of the source's colors
//! have no `ColorScheme` role and are derived from one that does, by alpha,
//! with the derivation noted at the constant.

use frust::authoring::text::TextStyle;
use frust::authoring::{Color, Size};
use frust::{
    Align, Alignment, AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexView, Padding, SizedBox,
    StatusPalette, Theme, any, button, flexible, inflexible, text, text_input,
};

use super::chrome::{panel, swatch};
use crate::AppState;

// ---------------------------------------------------------------------------
// Geometry (transcribed from `docs/splash.html`'s CSS, once)
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
/// `.toggle::after{width:15px;height:15px;top:2px;left:17px}` — the knob in
/// its **on** position (the mock renders the toggle already switched on).
const KNOB: f64 = 15.0;
const KNOB_LEFT: f64 = 17.0;
const KNOB_TOP: f64 = 2.0;
/// `.dot-ok{width:7px;height:7px}`.
const DOT: f64 = 7.0;
/// `.saved{gap:10px}`.
const SAVED_GAP: f64 = 10.0;

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
    /// `--bg-void`: the page tone, backing each piece so the splash's rain
    /// can't bleed through its text mid-assembly.
    void: Color,
    /// `--border-bright`: every hairline border in the mock.
    outline: Color,
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
}

/// Resolve [`Palette`] from `theme`.
fn palette(theme: &Theme) -> Palette {
    let scheme = theme.scheme();
    Palette {
        void: scheme.surface_container_lowest,
        outline: scheme.outline,
        amber: scheme.primary_container,
        amber_faint: scheme.primary.with_alpha(AMBER_FAINT_ALPHA),
        amber_border: scheme.primary.with_alpha(AMBER_BORDER_ALPHA),
        fg: scheme.on_surface,
        fg_muted: scheme.on_surface_variant,
        fg_dim: scheme.on_surface_variant.with_alpha(FG_DIM_ALPHA),
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
fn v_gap(gap: f64) -> AnyView<AppState> {
    any(SizedBox::<AppState>(None, Some(gap)))
}

/// A `gap`-wide spacer between row items.
fn h_gap(gap: f64) -> AnyView<AppState> {
    any(SizedBox::<AppState>(Some(gap), None))
}

/// A vertical stack whose children each fill the available width (the CSS
/// block-flow the mock's `.ui-layer`/`.field`/`.heading` all assume).
fn column(children: Vec<AnyView<AppState>>) -> FlexView<AppState> {
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
fn hugging_column(children: Vec<AnyView<AppState>>) -> FlexView<AppState> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Center)
}

/// A horizontal row with its items vertically centered.
fn row(children: Vec<frust::FlexChild<AppState>>) -> FlexView<AppState> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

/// A full-width piece backing: the `background:var(--bg-void)` every
/// text-bearing `.piece` carries, square-cornered like the source's.
fn backed(child: impl frust::View<AppState>, colors: &Palette) -> AnyView<AppState> {
    any(panel(child, colors.void, 0.0))
}

// ---------------------------------------------------------------------------
// The eight pieces
// ---------------------------------------------------------------------------

/// Piece 0 — `.connect-mark`: a bordered 60×60 tile holding a 2×2 grid whose
/// diagonal is solid amber and whose off-diagonal is the faint amber wash,
/// centered in the layer (`margin:0 auto`).
pub fn connect_mark(theme: &Theme) -> AnyView<AppState> {
    let colors = palette(theme);
    let cell = Size::new(MARK_CELL, MARK_CELL);
    let cell_radius = theme.shape.extra_small;
    // `.connect-mark span:nth-child(1),(4)` — faint wash + hairline accent
    // border; `(2),(3)` — the solid accent fill.
    let faint = || any(swatch(cell, colors.amber_faint, cell_radius).border(colors.amber_border));
    let solid = || any(swatch(cell, colors.amber, cell_radius));
    let grid = hugging_column(vec![
        any(row(vec![
            inflexible(faint()),
            inflexible(h_gap(MARK_GAP)),
            inflexible(solid()),
        ])),
        v_gap(MARK_GAP),
        any(row(vec![
            inflexible(solid()),
            inflexible(h_gap(MARK_GAP)),
            inflexible(faint()),
        ])),
    ]);
    let tile = panel(
        Padding(EdgeInsets::all(MARK_PAD), grid),
        colors.void,
        theme.shape.medium,
    )
    .border(colors.outline);
    any(Align(Alignment::CENTER, tile))
}

/// Piece 1 — `.connect-badge`: a centered, bordered two-line box naming the
/// daemon and the multiplexers it fronts.
pub fn server_badge(theme: &Theme) -> AnyView<AppState> {
    let colors = palette(theme);
    // Hugging, not stretched: the box is `margin:0 auto`-width, so the lines
    // center themselves against each other rather than against the layer.
    let lines = hugging_column(vec![
        any(text("muxrd server").style(inked(&theme.type_scale.body_small, colors.fg_muted))),
        v_gap(BADGE_LINE_GAP),
        any(text("zellij · herdr").style(inked(&theme.type_scale.label_small, colors.fg_dim))),
    ]);
    let box_ = panel(
        Padding(EdgeInsets::symmetric(BADGE_PAD_X, BADGE_PAD_Y), lines),
        colors.void,
        theme.shape.small,
    )
    .border(colors.outline);
    any(Align(Alignment::CENTER, box_))
}

/// Piece 2 — `.heading`: the screen's title and its one-line explanation,
/// both centered.
pub fn heading(theme: &Theme) -> AnyView<AppState> {
    let colors = palette(theme);
    backed(
        column(vec![
            any(Align(
                Alignment::CENTER,
                text("Connect to server").style(inked(&theme.type_scale.title_medium, colors.fg)),
            )),
            v_gap(HEADING_GAP),
            any(Align(
                Alignment::CENTER,
                text("Enter credentials to attach to a remote zellij or herdr session.")
                    .style(inked(&theme.type_scale.body_small, colors.fg_muted)),
            )),
        ]),
        &colors,
    )
}

/// Piece 3 — the first `.field`: the server URL.
pub fn server_url_field(theme: &Theme) -> AnyView<AppState> {
    field(theme, "SERVER URL", "zelli.example.com:50051")
}

/// Piece 4 — the second `.field`: the login token.
pub fn token_field(theme: &Theme) -> AnyView<AppState> {
    field(theme, "LOGIN TOKEN", "paste or scan a token")
}

/// A `.field`: an uppercase micro label over a placeholder-bearing input.
///
/// The label is uppercased in the source (`text-transform:uppercase`) at the
/// call site instead, since no `TextStyle` field expresses a transform.
fn field(theme: &Theme, label: &str, placeholder: &str) -> AnyView<AppState> {
    let colors = palette(theme);
    backed(
        column(vec![
            any(text(label).style(inked(&theme.type_scale.label_small, colors.fg_dim))),
            v_gap(FIELD_GAP),
            // Controlled, and permanently empty: `on_change` drops the
            // requested text, so the field always re-renders its placeholder
            // (`readonly` in the source). Phase 2 swaps in the real signal.
            any(
                text_input::<AppState, _>("", |_state: &mut AppState, _text: String| {})
                    .placeholder(placeholder)
                    .text_style(inked(&theme.type_scale.body_medium, colors.fg)),
            ),
        ]),
        &colors,
    )
}

/// Piece 5 — `.remember-row`: a label pushed left of an on-state pill toggle.
pub fn remember_row(theme: &Theme) -> AnyView<AppState> {
    let colors = palette(theme);
    backed(
        row(vec![
            // The flexible label takes every pixel the toggle doesn't, which
            // is what pins the toggle to the trailing edge (`Flex`'s v1
            // `MainAxisAlignment` has only `Start`, so there is no
            // space-between to reach for).
            flexible(
                1,
                any(text("Remember for 4 weeks")
                    .style(inked(&theme.type_scale.body_small, colors.fg_muted))),
            ),
            inflexible(toggle_on(&colors)),
        ]),
        &colors,
    )
}

/// The `.toggle` in its on-state: a faint amber pill, hairline accent border,
/// knob parked at the trailing end. Static art, not a control — Phase 2
/// replaces it with a real one.
fn toggle_on(colors: &Palette) -> AnyView<AppState> {
    let track = swatch(
        Size::new(TOGGLE_W, TOGGLE_H),
        colors.amber_faint,
        // `border-radius:999px` — a pill. The Glyph shape scale spells `full`
        // as `f64::INFINITY`, so the drawable radius is half the track.
        TOGGLE_H / 2.0,
    )
    .border(colors.amber_border);
    let knob = Padding(
        EdgeInsets {
            left: KNOB_LEFT,
            top: KNOB_TOP,
            right: TOGGLE_W - KNOB_LEFT - KNOB,
            bottom: TOGGLE_H - KNOB_TOP - KNOB,
        },
        swatch(Size::new(KNOB, KNOB), colors.amber, KNOB / 2.0),
    );
    any(frust::Stack(vec![any(track), any(knob)]))
}

/// Piece 6 — `.btn-primary`: the amber CTA.
///
/// A real [`button`] (Glyph-themed through `ButtonStyle::Primary`'s
/// `primary`/`on_primary` roles — the source's `#ffb627` on `#241a04`), so
/// the press feedback, disabled/loading states and accessibility role all
/// come from the framework rather than being repainted here. **Centered at
/// its natural width** rather than stretched full-bleed like the source's
/// block-level button: `ButtonWidget` pins its label at the leading padding,
/// so a stretched button would render a left-hugged label in a full-width
/// bar. Phase 2's real CTA can revisit this once the widget grows a label
/// alignment.
pub fn connect_button(_theme: &Theme) -> AnyView<AppState> {
    any(Align(
        Alignment::CENTER,
        button("→ connect", |_state: &mut AppState| {}),
    ))
}

/// Piece 7 — `.saved`: the last-used address behind a green status dot.
pub fn saved_address(theme: &Theme) -> AnyView<AppState> {
    let colors = palette(theme);
    backed(
        row(vec![
            inflexible(any(swatch(Size::new(DOT, DOT), colors.success, DOT / 2.0))),
            inflexible(h_gap(SAVED_GAP)),
            inflexible(any(
                text("100.71.31.57:50051").style(inked(&theme.type_scale.body_small, colors.fg))
            )),
        ]),
        &colors,
    )
}

/// The eight pieces, in `docs/splash.html`'s source order — which is also the
/// order their `data-delay` staggers are indexed by
/// (`splash::spec::PIECE_DELAYS_MS`).
pub fn connect_pieces(theme: &Theme) -> Vec<AnyView<AppState>> {
    vec![
        connect_mark(theme),
        server_badge(theme),
        heading(theme),
        server_url_field(theme),
        token_field(theme),
        remember_row(theme),
        connect_button(theme),
        saved_address(theme),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::splash::spec;

    #[test]
    fn there_is_one_piece_per_source_stagger() {
        let theme = Theme::glyph_baseline();
        assert_eq!(
            connect_pieces(&theme).len(),
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
    fn the_toggle_knob_sits_inside_its_track() {
        // `toggle_on`'s knob padding is `track - inset - knob` on each
        // trailing edge; a negative one would be a layout panic in waiting.
        const { assert!(KNOB_LEFT + KNOB <= TOGGLE_W) };
        const { assert!(KNOB_TOP + KNOB <= TOGGLE_H) };
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
        // `--bg-void` #0a0c11 and `--success` #5fd88f, reached through roles
        // rather than hexes.
        assert_eq!(colors.void, Color::from_rgb8(0x0a, 0x0c, 0x11));
        assert_eq!(colors.success, Color::from_rgb8(0x5f, 0xd8, 0x8f));
        assert_eq!(colors.amber, Color::from_rgb8(0xff, 0xb6, 0x27));
        assert!((colors.amber_faint.components[3] - AMBER_FAINT_ALPHA).abs() < 1e-6);
    }
}
