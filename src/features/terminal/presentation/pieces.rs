//! The `/terminal` screen's independently-composable pieces, transcribed from
//! `docs/terminal.html` (**v2**, "One less row" — it supersedes
//! `docs/redesign.html`'s terminal blocks wholesale: there is no tabs strip,
//! no path bar, no cmd hint, no FAB and no keyboard bar).
//!
//! One fn per visual element, so [`pages`](super::pages)'s `Component::build`
//! and the sheet builder read as arrangements rather than walls of nesting
//! (the `features::sessions`/`features::connect` precedent):
//!
//! | Piece | Mock element |
//! |---|---|
//! | [`status_row`] | `.term-statusrow` — the pane title plus the one expand button |
//! | [`card_body`] | `.term-body` — the scrollable, staggered, caret-terminated transcript |
//! | [`sheet_top_row`] | `.sheet-toprow` — `panes & tabs` + the round close button |
//! | [`section_label`] | `.sheet-section-lbl` — the amber `//` prefix and its dim label |
//! | [`pill`] | `.pill`, `.pill.sel`, `.pill.add` |
//! | [`pane_item`] | `.pane-item` — the mark box, the name/path pair, the `focused` chip |
//! | [`new_pane_row`] | `.new-pane-btn` — the dashed `+ new pane · long-press for floating` |
//!
//! # Colours: the canonical token wins over the mock's hex
//!
//! The card's own ink is **`GlyphInk`** (`theme.extension::<GlyphInk>()`), not
//! `terminal.html`'s `--term-*` variables: `terminal_bg` for the body,
//! `terminal_head` for the status band, `terminal_prompt` for the prompt runs,
//! the caret and the pane title, `terminal_out`/`terminal_comment` for the two
//! dimmer tiers. That is the framework's own brightness-invariant terminal
//! palette, and using it means this screen matches `frust::glyph::term_block`
//! wherever the two ever sit on the same page. (`GlyphInk::terminal_fg`, the
//! *untyped* body tone, is the one field [`Palette`] does not resolve: v2
//! types every line it prints `p`/`out`/`cm`, so nothing would ink from it.)
//!
//! Two card tones have **no** token and so are local constants:
//! [`TERMINAL_BORDER_RGB`] (the mock's `--term-border`) and the expand
//! button's resting wash. `GlyphInk` carries no border/hairline field at all —
//! filed as a FINDINGS candidate rather than approximated with an unrelated
//! role, since a `ColorScheme` outline is brightness-swapped and this card is
//! not.
//!
//! Everything *outside* the card — the sheet, its pills, the menu — uses the
//! ordinary Glyph scheme roles the rest of the app uses, because it is
//! ordinary app chrome that happens to be about a terminal.

use std::rc::Rc;

use frust::authoring::text::{FontStyle, LineHeight, TextStyle};
use frust::authoring::{Color, Size};
use frust::{
    Align, Alignment, AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexChild, FlexView,
    GestureDetector, GlyphInk, Padding, SizedBox, Stack, Theme, any, flexible, icon, icons,
    inflexible, scroll_view, text,
};

use crate::features::terminal::domain::entities::{TabEntry, TermLine};
use crate::ui::topbar::icon_button;

use super::chrome::{
    EXPAND_SIZE, FULLSCREEN_EXIT, FULLSCREEN_EXPAND, LineStyles, PANE_MARK, RADIUS_FULL,
    clipped_label, expand_button, term_body, term_box,
};

// ---------------------------------------------------------------------------
// Transcribed constants
// ---------------------------------------------------------------------------

/// `.term-statusrow{padding:14px 14px 14px 18px}` — with the **vertical** pair
/// tightened to 6px on the device gate: the mock's 14px made a 56px band of
/// chrome above the terminal, and the title bar is meant to be as small as it
/// can be while still carrying the one expand button. The two horizontal
/// values are the mock's, untouched.
const STATUS_PAD_TOP: f64 = 6.0;
const STATUS_PAD_RIGHT: f64 = 14.0;
const STATUS_PAD_LEFT: f64 = 18.0;
/// The status row's whole height, and so the expand button's press target (see
/// [`status_row`]): the 28px button — the floor the row cannot shrink past —
/// plus [`STATUS_PAD_TOP`] above and below it. The title's own line box (18px)
/// is well under it and never decides this number.
pub(super) const STATUS_ROW_H: f64 = EXPAND_SIZE + 2.0 * STATUS_PAD_TOP;
/// `.term-pane-title{font-size:13.5px}` — a fixed-size logotype-ish run, so it
/// overrides the display role's size (and only that; family/weight stay Space
/// Mono 700, which *is* what the role carries) — the `crate::ui::topbar`
/// brand-wordmark precedent.
const TITLE_SIZE: f32 = 13.5;
/// …and its line box, tightened to match (the display role's 36px box would
/// make "centred in the status row" mean something else entirely — and would
/// now be taller than [`STATUS_ROW_H`] itself).
const TITLE_LINE_HEIGHT: f32 = 18.0;
/// `.term-body{padding:18px 20px}`.
const BODY_PAD_X: f64 = 20.0;
const BODY_PAD_Y: f64 = 18.0;
/// `.term-body{font-size:12.5px; line-height:2}`.
const BODY_SIZE: f32 = 12.5;
const BODY_LINE_HEIGHT: f32 = 2.0;

/// `.sheet-toprow .lbl{font-size:12.5px}` and the sheet's own label text.
const SHEET_LABEL_SIZE: f32 = 12.5;
pub(super) const SHEET_LABEL: &str = "panes & tabs";
/// `.sheet-close{width:26px;height:26px}` and the vector inside it.
const CLOSE_SIZE: f64 = 26.0;
const CLOSE_ICON: f64 = 13.0;
/// The close button's accessible name — a *word*, never the `×` it draws.
pub(super) const CLOSE_LABEL: &str = "Close";
/// `.sheet-section-lbl{font-size:10px; margin:16px 0 10px; gap:6px}`.
const SECTION_SIZE: f32 = 10.0;
pub(super) const SECTION_TOP_GAP: f64 = 16.0;
pub(super) const SECTION_BOTTOM_GAP: f64 = 10.0;
const SECTION_GAP: f64 = 6.0;
/// `.sheet-section-lbl::before{content:'//'}`.
const SECTION_PREFIX: &str = "//";
/// `.pill{font-size:11px; padding:8px 13px}` and `.pill-row{gap:8px}`.
const PILL_SIZE: f32 = 11.0;
const PILL_PAD_X: f64 = 13.0;
const PILL_PAD_Y: f64 = 8.0;
pub(super) const PILL_GAP: f64 = 8.0;
/// `.pane-item{padding:11px 12px; gap:10px; margin-bottom:8px}`.
const PANE_PAD_X: f64 = 12.0;
const PANE_PAD_Y: f64 = 11.0;
const PANE_GAP: f64 = 10.0;
pub(super) const PANE_ROW_GAP: f64 = 8.0;
/// `.pane-item .ic{width:26px;height:26px}` and the mark drawn inside it.
const PANE_ICON_BOX: f64 = 26.0;
const PANE_ICON: f64 = 13.0;
/// `.pane-item .name{font-size:11.5px}` / `.path{font-size:9.5px}`.
const PANE_NAME_SIZE: f32 = 11.5;
const PANE_PATH_SIZE: f32 = 9.5;
/// `.focused-chip{font-size:9px; padding:3px 8px}`.
const CHIP_SIZE: f32 = 9.0;
const CHIP_PAD_X: f64 = 8.0;
const CHIP_PAD_Y: f64 = 3.0;
/// `.focused-chip` text, and `.new-pane-btn`'s two runs.
pub(super) const FOCUSED_CHIP: &str = "focused";
const NEW_PANE_PLUS: &str = "+";
const NEW_PANE_LABEL: &str = "new pane \u{b7} long-press for floating";
/// `.new-pane-btn{padding:11px; font-size:11px}`.
const NEW_PANE_PAD: f64 = 11.0;
/// Gap between the amber `+` and the label beside it — the source's
/// `<b>+</b> new pane` is one text node whose space is the gap; composed as
/// two runs, it needs a real one.
const NEW_PANE_GAP: f64 = 6.0;

/// The expand button's accessible names — a *word* per state, never the
/// `⛶`/`⤡` it draws (`crate::ui::topbar`'s `BACK_LABEL` rule).
pub(super) const EXPAND_LABEL: &str = "Fullscreen";
pub(super) const COLLAPSE_LABEL: &str = "Exit fullscreen";

/// `--term-border: rgba(255,238,210,0.07)` — the card's hairline.
///
/// A **local** constant: `GlyphInk` carries no border/hairline field, and the
/// nearest `ColorScheme` outline is brightness-swapped while this card is not
/// (see the [module docs](self)). Filed as a FINDINGS candidate.
pub(super) const TERMINAL_BORDER_RGB: (u8, u8, u8) = (0xff, 0xee, 0xd2);
pub(super) const TERMINAL_BORDER_ALPHA: f32 = 0.07;
/// `.expand-btn{background:rgba(255,238,210,0.06)}` — same family, one step
/// fainter, and the same gap.
const EXPAND_FILL_ALPHA: f32 = 0.06;
/// `--amber-faint: rgba(255,182,39,0.12)` / `--amber-border: rgba(...,0.35)`,
/// derived from the theme's own `primary` rather than re-spelling the hex.
const AMBER_FAINT_ALPHA: f32 = 0.12;
const AMBER_BORDER_ALPHA: f32 = 0.35;
/// `--fg-dim` has no `ColorScheme` role of its own (frust-theme records it as
/// a documented leftover), so the dimmest ink derives from
/// `on_surface_variant` by alpha — the same derivation, with the same
/// constant, both other screens document.
const FG_DIM_ALPHA: f32 = 0.62;

// ---------------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------------

/// The active theme, or the Glyph baseline when none is threaded (headless
/// tests, a shell with no design system installed). **Never**
/// `Theme::neutral`/`m3_baseline` — see `lib.rs`'s design-system note.
///
/// Deliberately a local copy of the other two screens' helper of the same name
/// rather than an import: a feature never reaches into a sibling's
/// `presentation` (`docs/ARCHITECTURE.md`'s cross-feature rule).
pub(super) fn active_theme() -> Theme {
    frust::use_context::<Theme>().unwrap_or_else(Theme::glyph_baseline)
}

/// Every colour this screen paints, resolved once per build from the active
/// theme (the `palette` shape both other screens use).
#[derive(Clone, Copy)]
pub(super) struct Palette {
    /// `GlyphInk::terminal_bg` — the card body.
    pub(super) card: Color,
    /// `GlyphInk::terminal_head` — the status row's band.
    pub(super) head: Color,
    /// `GlyphInk::terminal_prompt` — the pane title, the prompt runs, the
    /// caret, and the expand button's on-state ink.
    pub(super) prompt: Color,
    /// `GlyphInk::terminal_out` / `terminal_comment` — the two dimmer tiers.
    pub(super) out: Color,
    pub(super) comment: Color,
    /// `--term-border`, local (see [`TERMINAL_BORDER_RGB`]).
    pub(super) card_border: Color,
    /// `.expand-btn`'s resting wash.
    pub(super) expand_fill: Color,
    /// `--bg-void`: the page tone *behind* the card. The shell clears to
    /// `--bg-surface` (the card tone of the rest of the app), so a screen that
    /// painted no background would show the card's 14px gutter in the wrong
    /// tone entirely.
    pub(super) void: Color,
    /// `--bg-base`: the sheet pane item's fill.
    pub(super) base: Color,
    /// `--bg-raised`: a pill's fill, the sheet close button's disc, the pane
    /// item's mark box.
    pub(super) raised: Color,
    /// `--border-bright`: every sheet outline.
    pub(super) outline: Color,
    /// `--amber`: selection ink inside the sheet, and the `+` marks.
    pub(super) amber: Color,
    /// `--amber-dim`: the section labels' `//` prefix.
    pub(super) amber_dim: Color,
    /// `--amber-faint` / `--amber-border`: a selected pill's wash and edge.
    pub(super) amber_faint: Color,
    pub(super) amber_border: Color,
    /// `--fg` / `--fg-muted` / `--fg-dim`.
    pub(super) fg: Color,
    pub(super) fg_muted: Color,
    pub(super) fg_dim: Color,
}

/// Resolve [`Palette`] from `theme`.
pub(super) fn palette(theme: &Theme) -> Palette {
    let scheme = theme.scheme();
    // The terminal's own ink is brightness-invariant and lives in a theme
    // *extension*, exactly as `frust::glyph::term_block` resolves it.
    let ink = theme
        .extension::<GlyphInk>()
        .copied()
        .unwrap_or_else(GlyphInk::default_ink);
    let (r, g, b) = TERMINAL_BORDER_RGB;
    let hairline = Color::from_rgb8(r, g, b);
    Palette {
        card: ink.terminal_bg,
        head: ink.terminal_head,
        prompt: ink.terminal_prompt,
        out: ink.terminal_out,
        comment: ink.terminal_comment,
        card_border: hairline.with_alpha(TERMINAL_BORDER_ALPHA),
        expand_fill: hairline.with_alpha(EXPAND_FILL_ALPHA),
        void: scheme.surface_container_lowest,
        base: scheme.surface_container_low,
        raised: scheme.surface_container_high,
        outline: scheme.outline,
        amber: scheme.primary,
        // `--amber-dim` is M3's brightness-invariant `primary_fixed_dim`,
        // which Glyph authors from the very same `#c98a12` swatch.
        amber_dim: scheme.primary_fixed_dim,
        amber_faint: scheme.primary.with_alpha(AMBER_FAINT_ALPHA),
        amber_border: scheme.primary.with_alpha(AMBER_BORDER_ALPHA),
        fg: scheme.on_surface,
        fg_muted: scheme.on_surface_variant,
        fg_dim: scheme.on_surface_variant.with_alpha(FG_DIM_ALPHA),
    }
}

/// `style` re-inked with `color` and resized — every run picks a Glyph type
/// role for family/weight and overrides only what the mock's own CSS does, so
/// nothing falls back to `TextStyle::default()`'s 16px black.
fn sized(style: &TextStyle, size: f32, color: Color) -> TextStyle {
    let mut style = style.clone();
    style.size = size;
    style.color = color;
    style
}

/// The three inks the card's transcript prints in — `.term-body`'s 12.5px/2.0
/// monospace, tinted per line kind, with `.cm` italic.
pub(super) fn line_styles(theme: &Theme, colors: &Palette) -> LineStyles {
    let mut base = sized(&theme.type_scale.body_medium, BODY_SIZE, colors.prompt);
    base.line_height = LineHeight::FontSizeRelative(BODY_LINE_HEIGHT);
    let mut comment = base.clone();
    comment.color = colors.comment;
    comment.style = FontStyle::Italic;
    LineStyles {
        prompt: base.clone(),
        output: TextStyle {
            color: colors.out,
            ..base
        },
        comment,
    }
}

// ---------------------------------------------------------------------------
// Small layout helpers
// ---------------------------------------------------------------------------

/// A `gap`-wide horizontal spacer.
fn h_gap<State: 'static>(gap: f64) -> FlexChild<State> {
    inflexible(SizedBox::<State>(Some(gap), None))
}

/// A `gap`-tall vertical spacer, as a view.
pub(super) fn v_gap<State: 'static>(gap: f64) -> AnyView<State> {
    any(SizedBox::<State>(None, Some(gap)))
}

/// A vertically-centred row.
fn row<State: 'static>(children: Vec<FlexChild<State>>) -> FlexView<State> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

/// A column whose children each fill the available width.
pub(super) fn column<State: 'static>(children: Vec<AnyView<State>>) -> FlexView<State> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Stretch)
}

// ---------------------------------------------------------------------------
// The card
// ---------------------------------------------------------------------------

/// `.term-statusrow` — the pane title (ellipsized, single line) and, at the
/// trailing edge, the **one** fullscreen affordance.
///
/// The row this replaced in v1 was three cosmetic dots; here the trailing
/// circle is an actual button, and it is the only place fullscreen is reachable
/// from — no menu row, no gesture (the mock: "one button, always in the same
/// place").
///
/// # The row is [`STATUS_ROW_H`] tall, and so is the button's press target
///
/// The padding is applied to the **title** rather than to the whole row, which
/// is what lets the button's column run the row's full height: a 28px circle
/// centred in a 40px column, with the padding band above and below it part of
/// the same target. Padding the row instead would put those two 6px bands
/// outside every child and leave a 28px target in a 40px row — a thumb-sized
/// regression against the 56px row this replaced, where the same circle at
/// least had 14px of visual slack around it.
///
/// Two press paths, one callback, never both: the column is a [`Stack`] whose
/// **top** child is the button itself and whose bottom child is a
/// [`GestureDetector`] over the full-height box. A hit-test runs topmost-first
/// and stops at the first child that handles, so a press on the circle is the
/// button's (press-scale visual included) and a press on the band above or
/// below it is the detector's — the same toggle either way.
pub(super) fn status_row<State: 'static, F: Fn(&mut State) + 'static>(
    title: &str,
    fullscreen: bool,
    colors: &Palette,
    on_toggle: F,
) -> AnyView<State> {
    let theme = active_theme();
    let mut title_style = sized(&theme.type_scale.display_small, TITLE_SIZE, colors.prompt);
    title_style.line_height = LineHeight::Absolute(TITLE_LINE_HEIGHT);

    let (mark, label) = if fullscreen {
        (FULLSCREEN_EXIT, COLLAPSE_LABEL)
    } else {
        (FULLSCREEN_EXPAND, EXPAND_LABEL)
    };
    // `.expand-btn.on{background:var(--amber-faint); color:var(--amber);
    // border-color:var(--amber-border)}` — the same trio the `:hover` state
    // uses, which a touch screen has no notion of.
    let (fill, ink, border) = if fullscreen {
        (colors.amber_faint, colors.amber, colors.amber_border)
    } else {
        (colors.expand_fill, colors.fg_muted, colors.card_border)
    };

    // Shared so both halves of the press column below can fire it; the
    // callback itself is `Fn`, so a clone of the handle is all either needs.
    let on_toggle = Rc::new(on_toggle);
    let on_button = Rc::clone(&on_toggle);
    let button = SizedBox::<State>(Some(EXPAND_SIZE), Some(STATUS_ROW_H)).child(Align(
        Alignment::CENTER,
        expand_button(mark, label, move |state: &mut State| on_button(state))
            .tones(fill, ink, border)
            .on(fullscreen),
    ));
    let press_band = GestureDetector(SizedBox::<State>(Some(EXPAND_SIZE), Some(STATUS_ROW_H)))
        .on_tap(move |state: &mut State| on_toggle(state));

    any(term_box(
        row(vec![
            flexible(
                1,
                Padding(
                    EdgeInsets {
                        left: STATUS_PAD_LEFT,
                        top: STATUS_PAD_TOP,
                        right: 0.0,
                        bottom: STATUS_PAD_TOP,
                    },
                    clipped_label(title, title_style),
                ),
            ),
            h_gap(PILL_GAP),
            inflexible(Stack(vec![any(press_band), any(button)])),
            // The row's own trailing padding, as a spacer: the button column
            // owns the vertical padding, so this axis is all that is left of
            // the `Padding` that used to wrap everything.
            h_gap(STATUS_PAD_RIGHT),
        ]),
        colors.head,
        0.0,
    ))
}

/// `.term-body` — the transcript, scrollable, staggered and caret-terminated.
///
/// `epoch` is the controller's replay token: a change to it (a tab switch)
/// replays the reveal from the top, which is the mock's `renderTab`→
/// `playReveal` behaviour.
pub(super) fn card_body<State: 'static>(
    lines: Vec<TermLine>,
    epoch: u64,
    colors: &Palette,
) -> AnyView<State> {
    let theme = active_theme();
    any(scroll_view(Padding(
        EdgeInsets::symmetric(BODY_PAD_X, BODY_PAD_Y),
        term_body(lines, epoch, line_styles(&theme, colors), colors.prompt),
    )))
}

// ---------------------------------------------------------------------------
// The "panes & tabs" sheet
// ---------------------------------------------------------------------------

/// `.sheet-toprow` — the sheet's own label and its round close button.
///
/// The handle above it is the scaffold's (`crate::ui::sheet` draws one at the
/// panel's top), so this deliberately draws none: two would read as a mistake.
pub(super) fn sheet_top_row<State: 'static, F: Fn(&mut State) + 'static>(
    colors: &Palette,
    on_close: F,
) -> AnyView<State> {
    let theme = active_theme();
    any(row(vec![
        flexible(
            1,
            text(SHEET_LABEL).style(sized(
                &theme.type_scale.label_large,
                SHEET_LABEL_SIZE,
                colors.fg,
            )),
        ),
        // `crate::ui::topbar::icon_button`'s third caller (after the bar's own
        // slots and the Connect FAB): a *labelled* `Role::Button` over one
        // vector, which a bare `GestureDetector` — contributing no node of its
        // own — could not give a screen reader.
        inflexible(
            icon_button(icons::CLOSE, CLOSE_LABEL, on_close)
                .ink(colors.fg_dim)
                .fill(colors.raised)
                .icon_size(CLOSE_ICON)
                .box_size(Size::new(CLOSE_SIZE, CLOSE_SIZE))
                .radius(CLOSE_SIZE / 2.0),
        ),
    ]))
}

/// `.sheet-section-lbl` — a dim 10px label behind an amber `//` prefix.
pub(super) fn section_label<State: 'static>(
    label: impl Into<String>,
    colors: &Palette,
) -> AnyView<State> {
    let theme = active_theme();
    let prefix = sized(
        &theme.type_scale.label_small,
        SECTION_SIZE,
        colors.amber_dim,
    );
    let body = sized(&theme.type_scale.label_small, SECTION_SIZE, colors.fg_dim);
    any(row(vec![
        inflexible(text(SECTION_PREFIX).style(prefix)),
        h_gap(SECTION_GAP),
        inflexible(text(label.into()).style(body)),
    ]))
}

/// Which of `.pill`'s three states a pill is in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PillState {
    /// `.pill` — raised fill, bright hairline, muted ink.
    Idle,
    /// `.pill.sel` — the amber trio.
    Selected,
    /// `.pill.add` — a dashed hairline and the dimmest ink.
    Add,
}

/// One `.pill`. The label is content text, which is why the add pills' `+` is
/// a **run** rather than a vector: it is inside the sentence, not chrome
/// (`crate::ui::topbar`'s "font glyphs are for content text only").
pub(super) fn pill<State: 'static, F: Fn(&mut State) + 'static>(
    label: impl Into<String>,
    state: PillState,
    colors: &Palette,
    on_tap: F,
) -> AnyView<State> {
    let theme = active_theme();
    let (fill, border, ink) = match state {
        PillState::Idle => (colors.raised, colors.outline, colors.fg_muted),
        PillState::Selected => (colors.amber_faint, colors.amber_border, colors.amber),
        PillState::Add => (colors.raised, colors.outline, colors.fg_dim),
    };
    any(GestureDetector(
        term_box(
            Padding(
                EdgeInsets::symmetric(PILL_PAD_X, PILL_PAD_Y),
                text(label.into()).style(sized(&theme.type_scale.body_small, PILL_SIZE, ink)),
            ),
            fill,
            RADIUS_FULL,
        )
        .border(border)
        .dashed(state == PillState::Add),
    )
    .on_tap(on_tap))
}

/// `.pane-item` — the mark box, the name/path pair, and the `focused` chip.
///
/// The mark is [`PANE_MARK`], an app-supplied vector: the mock sets `▤` as
/// text, which is not in the bundled Glyph faces (see
/// `super::chrome::PANE_MARK`).
pub(super) fn pane_item<State: 'static>(tab: &TabEntry, colors: &Palette) -> AnyView<State> {
    let theme = active_theme();
    // The mock draws exactly one pane and marks it focused; when a real pane
    // tree lands this becomes per-pane.
    let focused = true;
    let (mark_fill, mark_ink) = if focused {
        (colors.amber_faint, colors.amber)
    } else {
        (colors.raised, colors.fg_muted)
    };
    let border = if focused {
        colors.amber_border
    } else {
        colors.outline
    };

    let info = column::<State>(vec![
        any(text(tab.pane_name.clone()).style(sized(
            &theme.type_scale.body_small,
            PANE_NAME_SIZE,
            colors.fg,
        ))),
        any(text(tab.pane_path.clone()).style(sized(
            &theme.type_scale.label_small,
            PANE_PATH_SIZE,
            colors.fg_dim,
        ))),
    ]);

    let chip = term_box(
        Padding(
            EdgeInsets::symmetric(CHIP_PAD_X, CHIP_PAD_Y),
            text(FOCUSED_CHIP).style(sized(
                &theme.type_scale.label_small,
                CHIP_SIZE,
                colors.amber,
            )),
        ),
        colors.amber_faint,
        RADIUS_FULL,
    )
    .border(colors.amber_border);

    any(term_box(
        Padding(
            EdgeInsets::symmetric(PANE_PAD_X, PANE_PAD_Y),
            row(vec![
                inflexible(term_box(
                    SizedBox::<State>(Some(PANE_ICON_BOX), Some(PANE_ICON_BOX)).child(Align(
                        Alignment::CENTER,
                        icon(PANE_MARK).size(PANE_ICON).color(mark_ink),
                    )),
                    mark_fill,
                    theme.shape.extra_small,
                )),
                h_gap(PANE_GAP),
                flexible(1, info),
                h_gap(PANE_GAP),
                inflexible(chip),
            ]),
        ),
        colors.base,
        theme.shape.small,
    )
    .border(border))
}

/// `.new-pane-btn` — the dashed, centred row that closes the sheet's pane
/// section. The `+` is amber because it is the call to action.
pub(super) fn new_pane_row<State: 'static, F: Fn(&mut State) + 'static>(
    colors: &Palette,
    on_tap: F,
) -> AnyView<State> {
    let theme = active_theme();
    let label = row(vec![
        inflexible(text(NEW_PANE_PLUS).style(sized(
            &theme.type_scale.body_small,
            PILL_SIZE,
            colors.amber,
        ))),
        h_gap(NEW_PANE_GAP),
        inflexible(text(NEW_PANE_LABEL).style(sized(
            &theme.type_scale.body_small,
            PILL_SIZE,
            colors.fg_dim,
        ))),
    ]);
    any(GestureDetector(
        term_box(
            Padding(
                EdgeInsets::all(NEW_PANE_PAD),
                Align(Alignment::CENTER, label),
            ),
            Color::TRANSPARENT,
            theme.shape.small,
        )
        .border(colors.outline)
        .dashed(true),
    )
    .on_tap(on_tap))
}

#[cfg(test)]
mod tests {
    use super::super::chrome::CARD_RADIUS;
    use super::*;

    #[test]
    fn the_card_takes_its_ink_from_glyph_and_not_from_the_mocks_hexes() {
        // The canonical-token rule, as a tripwire: every card tone is the
        // theme extension's, never `terminal.html`'s `--term-body`/`--term-out`
        // literals (which differ from Glyph's by a shade).
        let theme = Theme::glyph_baseline();
        let ink = theme
            .extension::<GlyphInk>()
            .copied()
            .expect("the Glyph baseline carries GlyphInk");
        let colors = palette(&theme);

        assert_eq!(colors.card, ink.terminal_bg);
        assert_eq!(colors.head, ink.terminal_head);
        assert_eq!(colors.prompt, ink.terminal_prompt);
        assert_eq!(colors.out, ink.terminal_out);
        assert_eq!(colors.comment, ink.terminal_comment);
        // …and specifically NOT the mock's own body/out hexes.
        assert_ne!(colors.card, Color::from_rgb8(0x1b, 0x18, 0x11));
        assert_ne!(colors.out, Color::from_rgb8(0x9b, 0x91, 0x7a));
    }

    #[test]
    fn the_cards_hairline_is_the_mocks_own_since_no_token_carries_it() {
        // The documented gap: `GlyphInk` has no border field, so this one tone
        // stays a local constant — and stays *exactly* the mock's rgba.
        let colors = palette(&Theme::glyph_baseline());
        let (r, g, b) = TERMINAL_BORDER_RGB;
        assert_eq!((r, g, b), (0xff, 0xee, 0xd2));
        assert_eq!(colors.card_border.components[3], TERMINAL_BORDER_ALPHA);
        assert_eq!(colors.expand_fill.components[3], EXPAND_FILL_ALPHA);
    }

    #[test]
    fn the_sheets_amber_trio_derives_from_the_themes_own_accent() {
        let theme = Theme::glyph_baseline();
        let colors = palette(&theme);
        let primary = theme.scheme().primary;
        assert_eq!(colors.amber, primary);
        assert_eq!(colors.amber_faint, primary.with_alpha(AMBER_FAINT_ALPHA));
        assert_eq!(colors.amber_border, primary.with_alpha(AMBER_BORDER_ALPHA));
        assert_eq!(colors.amber_dim, theme.scheme().primary_fixed_dim);
    }

    #[test]
    fn a_comment_line_is_the_only_italic_one() {
        let theme = Theme::glyph_baseline();
        let styles = line_styles(&theme, &palette(&theme));
        assert_eq!(styles.comment.style, FontStyle::Italic);
        assert_eq!(styles.prompt.style, FontStyle::Normal);
        assert_eq!(styles.output.style, FontStyle::Normal);
        // …and all three carry the mock's own size and doubled line box.
        for style in [&styles.prompt, &styles.output, &styles.comment] {
            assert_eq!(style.size, BODY_SIZE);
            assert_eq!(
                style.line_height,
                LineHeight::FontSizeRelative(BODY_LINE_HEIGHT)
            );
        }
    }

    #[test]
    fn every_affordance_announces_a_word_and_not_a_mark() {
        // The a11y half of the vector rule: a label is a *name*, never the
        // shape of the glyph the mock happened to draw.
        for label in [EXPAND_LABEL, COLLAPSE_LABEL, CLOSE_LABEL] {
            assert!(
                label.chars().all(|c| c.is_ascii_alphabetic() || c == ' '),
                "{label:?} is a name, not a mark"
            );
        }
    }

    #[test]
    fn the_transcribed_geometry_is_the_mocks() {
        // A 14px padding and an 18px one live a line apart in the source; a
        // swap is invisible in review and obvious on screen. The vertical pair
        // is the one deliberate departure from the mock (see
        // `STATUS_PAD_TOP`).
        assert_eq!(
            (STATUS_PAD_TOP, STATUS_PAD_RIGHT, STATUS_PAD_LEFT),
            (6.0, 14.0, 18.0)
        );
        assert_eq!((BODY_PAD_X, BODY_PAD_Y), (20.0, 18.0));
        assert_eq!(CARD_RADIUS, 22.0);
        assert_eq!(EXPAND_SIZE, 28.0);
        assert_eq!(CLOSE_SIZE, 26.0);
        assert_eq!(PANE_ICON_BOX, 26.0);
    }

    #[test]
    fn the_status_row_is_as_short_as_its_button_allows() {
        // 40px: the expand button plus one tightened padding either side —
        // and the *whole* of that height is its press target (`status_row`'s
        // own doc), so the shorter row costs the thumb nothing.
        assert_eq!(STATUS_ROW_H, 40.0);
        assert!(
            f64::from(TITLE_LINE_HEIGHT) + 2.0 * STATUS_PAD_TOP < STATUS_ROW_H,
            "the title's line box never decides the row's height"
        );
    }
}
