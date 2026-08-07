//! The `/sessions` screen's independently-composable pieces, transcribed from
//! `docs/redesign.html`'s **sessions** block (CSS lines 92–113, markup lines
//! 227–257).
//!
//! One `pub(super)` fn per visual element, so [`pages`](super::pages)'s
//! `Component::build` reads as an arrangement rather than a wall of nesting
//! (the `features::connect`-screen precedent, which splits its own eight pieces
//! out the same way):
//!
//! | Piece | Redesign element |
//! |---|---|
//! | [`server_strip`] | `.conn-row` — success dot, `host:port`, `zellij 0.44.3 · 36ms`, the `insecure` chip |
//! | [`credential_banner`] | the save-credential offer (no HTML block; see below) |
//! | [`session_card`] | `.sess-card` — accent, avatar, name, chips, the three meta spans, pane pills |
//! | [`new_session_row`] | `.new-sess-btn` — the dashed, centered "+ new session" row |
//! | [`empty_view`] | the emptied-list state (no HTML block; Glyph's own `empty_state`) |
//! | [`sheet_heading`]/[`sheet_row`] | the long-press management sheet's header and its rows |
//! | [`picker_rows`] | the picker sheet's rows — **unlinked from this screen** (see below) |
//!
//! # Colour roles: amber is the brand, green is health
//!
//! The one rule this screen's round-0 review turned on: **amber appears only as
//! brand or call-to-action** (the top bar's mark and wordmark, the `+` on the
//! new-session row) and **never** as a health or selection signal. Liveness is
//! `success` — the card's 2px left accent, its avatar wash, its `running` chip,
//! and the strip's dot — and everything inert is a neutral wash. A running
//! session must never read as "selected", and an amber avatar (which is what
//! `glyph::avatar` paints by default) is exactly that mistake.
//!
//! # Real components over hand-composed chrome
//!
//! Every element with a Glyph catalog counterpart uses it — [`badge`] for the
//! three chips, [`avatar`] for `.sess-avatar` (recoloured per the rule above),
//! [`alert`] for the offer banner, [`empty_state`] for the empty list,
//! [`glyph_list`] for the picker rows, and [`button`] for the sheet's rows. The
//! boxes the catalog has no counterpart for are [`super::chrome`]'s: the card
//! (`glyph_card`'s title/desc/footer slots and its own inset can express
//! neither the 2px left accent nor `.sess-card`'s 14px padding), the
//! `.pane-pill`s (`glyph::tag` is a raised pill, not a `bg-base` tile), the
//! `.conn-row`'s bottom rule, and the dashed new-session outline.
//!
//! # What is *not* wired here any more
//!
//! [`picker_rows`] and [`picker_heading`] survive the redesign as `pub` items
//! with no caller on this screen: the Sessions app-bar "Switch" action is gone
//! (create now lives on the dashed row and the empty state; attach is a tap on
//! a card), and the picker sheet belongs to the terminal screen's Phase-3 FAB.
//! They are kept rather than deleted so that screen inherits the composition
//! rather than re-deriving it.
//!
//! # Tokens
//!
//! `Theme::glyph_baseline()` is canonical where it and the HTML disagree.
//! `--fg-dim` has no `ColorScheme` role of its own (frust-theme records it as a
//! documented leftover), so the dimmest ink derives from `on_surface_variant`
//! by alpha — the same derivation, with the same constant, the Connect screen's
//! own pieces document.

use frust::authoring::text::TextStyle;
use frust::authoring::{Color, Size};
use frust::glyph::{
    AlertVariant, BadgeVariant, GlyphListItem, alert, avatar, badge, empty_state, glyph_list,
    glyph_list_item,
};
use frust::motion::AnimatedOpacity;
use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, FlexChild,
    FlexView, GestureDetector, Padding, SizedBox, StatusPalette, Theme, any, button, flexible,
    inflexible, text,
};

use crate::AppState;
use crate::connection::{ServerInfo, TrustMode};
use crate::features::sessions::domain::entities::{Session, SessionStatus};

use super::chrome::{RADIUS_FULL, dashed_box, panel, swatch};
use super::pages::SessionsState;

/// `.sess-avatar{width:26px;height:26px}`.
const AVATAR_SIZE: f64 = 26.0;
/// `.sess-card{padding:14px}`.
const CARD_PAD: f64 = 14.0;
/// `.sess-top{gap:10px}` — avatar to name.
const ROW_GAP: f64 = 10.0;
/// `.sess-badges{gap:6px}` / `.sess-panes{gap:6px}`.
const PILL_GAP: f64 = 6.0;
/// `.sess-meta{gap:12px}` — between the three meta spans.
const META_GAP: f64 = 12.0;
/// `.sess-top{margin-bottom:10px}` / `.sess-meta{margin-bottom:10px}` — the
/// card's inner vertical rhythm.
const CARD_LINE_GAP: f64 = 10.0;
/// `.pane-pill{padding:8px 10px}`.
const PILL_PAD_X: f64 = 10.0;
const PILL_PAD_Y: f64 = 8.0;
/// `.conn-row{padding:13px 16px}`.
const STRIP_PAD_X: f64 = 16.0;
const STRIP_PAD_Y: f64 = 13.0;
/// `.conn-row{gap:10px}`.
const STRIP_GAP: f64 = 10.0;
/// `.dot-ok{width:7px;height:7px}`.
const DOT: f64 = 7.0;
/// `.new-sess-btn{padding:13px}`.
const NEW_ROW_PAD: f64 = 13.0;
/// Gap between the amber `+` and the "new session" label — the source's
/// `<b>+</b> new session` is one text node whose space is the gap; composed as
/// two runs, it needs a real one.
const NEW_ROW_GAP: f64 = 6.0;
/// `.sess-card.idle{opacity:0.55}` — the whole-card dim for a session that
/// isn't running.
const IDLE_OPACITY: f64 = 0.55;
/// `--fg-dim` derived from `on_surface_variant` by alpha — see the
/// [module docs](self)'s Tokens note (the constant is the Connect screen's).
const FG_DIM_ALPHA: f32 = 0.62;
/// How many `.pane-pill` previews a card shows (the redesign draws two).
const MAX_PREVIEW_PILLS: usize = 2;
/// The label the `.conn-row` prints for the one TLS posture that skips
/// certificate validation — the source's `.warn-badge`.
const INSECURE_CHIP: &str = "insecure";
/// The separator between the strip's version and its latency
/// (`zellij 0.44.3 · 36ms`).
const META_SEPARATOR: &str = " \u{b7} ";

/// The active theme, or the Glyph baseline when none is threaded (headless
/// tests, a shell with no design system installed). **Never**
/// `Theme::neutral`/`m3_baseline` — see `lib.rs`'s design-system note.
///
/// Deliberately a local copy of the Connect screen's helper of the same name
/// rather than an import: a feature never reaches into a sibling's
/// `presentation` (`docs/ARCHITECTURE.md`'s cross-feature rule).
pub(super) fn active_theme() -> Theme {
    frust::use_context::<Theme>().unwrap_or_else(Theme::glyph_baseline)
}

/// Every colour this screen paints, resolved once per build from the active
/// theme (the Connect screen's `palette` shape).
#[derive(Clone, Copy)]
pub(super) struct Palette {
    /// `--bg-base`: the screen's page tone, and the `.pane-pill` fill. The
    /// shell clears to `--bg-surface` (the *card* tone), so a screen that
    /// paints no page background would show cards with no edge at all.
    pub(super) base: Color,
    /// `--bg-surface`: the card fill.
    pub(super) surface: Color,
    /// `--bg-raised`: an idle card's avatar wash.
    pub(super) raised: Color,
    /// `--border-bright`: the card and dashed-row outlines.
    pub(super) outline: Color,
    /// `--border`: the `.conn-row` rule and the `.pane-pill` hairline.
    pub(super) rule: Color,
    /// `--amber`: brand/CTA ink only (see the [module docs](self)).
    pub(super) amber: Color,
    /// `--fg`: primary text.
    pub(super) fg: Color,
    /// `--fg-muted`: secondary text.
    pub(super) fg_muted: Color,
    /// `--fg-dim`, derived (see [`FG_DIM_ALPHA`]).
    pub(super) fg_dim: Color,
    /// `--success`: liveness — the card accent, the avatar ink, the strip dot.
    pub(super) success: Color,
    /// `--success-faint`: the running avatar's wash.
    pub(super) success_faint: Color,
}

/// Resolve [`Palette`] from `theme`.
pub(super) fn palette(theme: &Theme) -> Palette {
    let scheme = theme.scheme();
    // Per `docs/CODE_STANDARDS.md`'s Glyph precedent: a per-status colour with
    // no `ColorScheme` field resolves `StatusPalette` first. A Glyph theme
    // always carries it; `primary` is the last-resort ink if some other theme
    // is ever installed.
    let status = theme.extension::<StatusPalette>();
    Palette {
        base: scheme.surface_container_low,
        surface: scheme.surface,
        raised: scheme.surface_container_high,
        outline: scheme.outline,
        rule: scheme.outline_variant,
        amber: scheme.primary,
        fg: scheme.on_surface,
        fg_muted: scheme.on_surface_variant,
        fg_dim: scheme.on_surface_variant.with_alpha(FG_DIM_ALPHA),
        success: status.map_or(scheme.primary, |s| s.colors(theme.brightness).success),
        success_faint: status.map_or(scheme.surface_container_high, |s| {
            s.colors(theme.brightness).success_container
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

/// A `gap`-wide horizontal spacer.
fn h_gap(gap: f64) -> FlexChild<SessionsState> {
    inflexible(SizedBox::<SessionsState>(Some(gap), None))
}

/// A flexible spacer that pushes what follows it to the trailing edge — Flex
/// v1 has no `space-between` (`MainAxisAlignment::Start` only), the same trick
/// `ui::dialogs` and the Connect screen's `remember_row` use.
fn spacer() -> FlexChild<SessionsState> {
    flexible(1, SizedBox::<SessionsState>(None, None))
}

/// A vertically-centered row.
fn row(children: Vec<FlexChild<SessionsState>>) -> FlexView<SessionsState> {
    FlexView::new(Axis::Horizontal, children).cross_axis(CrossAxisAlignment::Center)
}

/// A column whose children each fill the available width.
pub(super) fn column(children: Vec<AnyView<SessionsState>>) -> FlexView<SessionsState> {
    FlexView::new(
        Axis::Vertical,
        children.into_iter().map(inflexible).collect(),
    )
    .cross_axis(CrossAxisAlignment::Stretch)
}

/// `[a, b, c]` with a `gap`-tall spacer between consecutive items, as flex
/// children.
fn interleave(views: Vec<AnyView<SessionsState>>, gap: f64) -> Vec<FlexChild<SessionsState>> {
    let mut children = Vec::with_capacity(views.len() * 2);
    for (index, view) in views.into_iter().enumerate() {
        if index > 0 {
            children.push(inflexible(SizedBox::<SessionsState>(None, Some(gap))));
        }
        children.push(inflexible(view));
    }
    children
}

/// The screen's page tone behind `body` — `--bg-base`, which the shell's own
/// clear colour (`--bg-surface`) is emphatically not. See [`Palette::base`].
pub(super) fn page_tone(
    body: impl frust::View<SessionsState>,
    colors: &Palette,
) -> AnyView<SessionsState> {
    any(panel(body, colors.base, 0.0))
}

// ---------------------------------------------------------------------------
// `.conn-row` — the server-info strip
// ---------------------------------------------------------------------------

/// The connected server's facts as one fixed strip under the top bar: a
/// glowing success dot, `host:port`, the dim `zellij 0.44.3 · 36ms` meta, and —
/// only for the posture that actually skips certificate validation — the
/// `insecure` warning chip. Closed by a hairline rule.
///
/// Read from [`crate::connection::ConnectionController::server_info`], which is
/// app-scoped precisely because this screen renders it (see that module's
/// docs). Latency lives **here only**: Settings renders the endpoint and TLS
/// posture again, but a live round-trip time belongs next to the session list
/// it gates.
///
/// `None` renders a muted "not connected" line rather than nothing: the session
/// guard means it is unreachable in practice, and a silent gap would be the
/// worse failure if it ever were.
pub(super) fn server_strip(info: Option<&ServerInfo>, colors: &Palette) -> AnyView<SessionsState> {
    let theme = active_theme();
    let content = match info {
        None => row(vec![inflexible(
            text("not connected").style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        )]),
        Some(info) => {
            let mut children = vec![
                inflexible(
                    swatch(Size::new(DOT, DOT), colors.success, RADIUS_FULL).glow(colors.success),
                ),
                h_gap(STRIP_GAP),
                flexible(
                    1,
                    text(info.endpoint.clone())
                        .style(inked(&theme.type_scale.body_medium, colors.fg)),
                ),
                h_gap(STRIP_GAP),
                inflexible(
                    text(meta_line(info)).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
                ),
            ];
            if let Some(chip) = insecure_chip(info.trust_mode) {
                children.push(h_gap(STRIP_GAP));
                children.push(inflexible(badge(chip, BadgeVariant::Warning)));
            }
            row(children)
        }
    };
    any(panel(
        Padding(
            EdgeInsets::symmetric(STRIP_PAD_X, STRIP_PAD_Y),
            content.cross_axis(CrossAxisAlignment::Center),
        ),
        Color::TRANSPARENT,
        0.0,
    )
    .bottom_rule(colors.rule))
}

/// The strip's dim meta text: `"zellij 0.44.3 · 36ms"`.
fn meta_line(info: &ServerInfo) -> String {
    format!("{}{}{}ms", info.version, META_SEPARATOR, info.latency_ms)
}

/// The `.warn-badge` the strip prints for a TLS posture — `Some` only for the
/// one posture that actually skips certificate validation, since the redesign
/// flags a risk rather than narrating every posture (Settings prints the full
/// [`ServerInfo::tls_label`] instead).
///
/// Branches on the [`TrustMode`] **variant**, never on
/// [`ServerInfo::tls_label`]: the label is display text, and the Connect
/// screen's own trust selector marks the same posture with the same
/// `BadgeVariant::Warning` chip, from the same enum.
fn insecure_chip(mode: TrustMode) -> Option<&'static str> {
    match mode {
        TrustMode::TrustOnFirstUse => Some(INSECURE_CHIP),
        TrustMode::System | TrustMode::Pinned => None,
    }
}

/// The host half of a `host:port` endpoint — what the top bar's centered title
/// shows (`100.71.31.57`, while the strip under it prints the whole
/// `100.71.31.57:50051`).
///
/// `pub(crate)` rather than `pub(super)` because the bar moved out of this
/// screen: `crate::routes`' `BarTitle::ConnectedHost` resolves `/sessions`'
/// title through this one function rather than minting a second copy of the
/// split beside the shell.
pub(crate) fn host_of(endpoint: &str) -> &str {
    endpoint.rsplit_once(':').map_or(endpoint, |(host, _)| host)
}

// ---------------------------------------------------------------------------
// The save-credential offer banner
// ---------------------------------------------------------------------------

/// The offer to save the just-connected server's credential, hosted on this
/// screen rather than on Connect because Connect unmounts on the redirect that
/// lands here (see [`crate::connection`]'s "Credential offer placement").
///
/// `label` is the server's own label, carried through from the connect form.
/// Both buttons clear the offer through
/// [`crate::connection::ConnectionController`]; accepting hands off to the
/// REAL gated save (mint → biometric gate → store, via the connection's
/// `CredentialSaver` seam), which reports its own outcome on every
/// non-success branch — so the toast here only announces the hand-off, never
/// a result the gate hasn't produced yet.
pub(super) fn credential_banner(label: &str) -> AnyView<SessionsState> {
    let banner = alert(
        AlertVariant::Info,
        "Save this server?",
        format!("Keep {label}'s credential so the next connect is one tap."),
    );
    let saved_label = label.to_string();
    let actions = row(vec![
        spacer(),
        inflexible(
            button("Not now", |state: &mut SessionsState| {
                state.connection.dismiss_credential_offer();
            })
            .style(ButtonStyle::Ghost)
            .small(),
        ),
        h_gap(PILL_GAP),
        inflexible(
            button("Save", move |state: &mut SessionsState| {
                state.connection.accept_credential_offer();
                crate::ui::toasts::app_toasts().show(format!("Saving {saved_label}…"));
            })
            .style(ButtonStyle::Primary)
            .small(),
        ),
    ]);
    any(column(vec![any(banner), any(actions)]))
}

// ---------------------------------------------------------------------------
// `.sess-card`
// ---------------------------------------------------------------------------

/// One session card: the whole box is the control.
///
/// **A tap attaches; a long-press opens the management sheet** — the redesign
/// draws no per-card buttons, and the Attach/Rename/Kill row this replaced was
/// the round-0 review's loudest miss. The gesture pair is
/// [`GestureDetector`]'s: its long-press fires on the first pointer event after
/// the 500ms threshold and *never* also fires the tap, which is what makes
/// "long-press must not attach" a property of the framework rather than of a
/// guard written here.
///
/// Running and idle differ in four places, all of them role-correct: the 2px
/// `success` left accent (absent when idle, though its width is still
/// reserved), the avatar's wash and ink, the status chip, and — for an idle
/// card — a whole-card dim to [`IDLE_OPACITY`] plus no `.sess-panes` row at all.
pub(super) fn session_card(session: &Session, colors: &Palette) -> AnyView<SessionsState> {
    let theme = active_theme();
    let live = session.status.is_live();
    let (avatar_bg, avatar_ink) = if live {
        (colors.success_faint, colors.success)
    } else {
        (colors.raised, colors.fg_muted)
    };

    let top = row(vec![
        inflexible(
            avatar(session.initial())
                .size(AVATAR_SIZE)
                .background(avatar_bg)
                .foreground(avatar_ink)
                // `.sess-avatar` has no border of its own; `avatar` always
                // strokes one, so it is stroked in nothing.
                .border_color(Color::TRANSPARENT),
        ),
        h_gap(ROW_GAP),
        flexible(
            1,
            text(session.name.clone()).style(inked(&theme.type_scale.label_large, colors.fg)),
        ),
        h_gap(PILL_GAP),
        inflexible(badge(
            session.status.label(),
            status_variant(session.status),
        )),
        h_gap(PILL_GAP),
        inflexible(badge(session.backend.label(), BadgeVariant::Neutral)),
    ]);

    let meta_style = inked(&theme.type_scale.label_small, colors.fg_dim);
    let meta = row(vec![
        inflexible(text(session.layout.panes_label()).style(meta_style.clone())),
        h_gap(META_GAP),
        inflexible(text(session.layout.tabs_label()).style(meta_style.clone())),
        h_gap(META_GAP),
        inflexible(text(session.layout.third.label()).style(meta_style)),
        spacer(),
    ]);

    let mut lines = vec![any(top), any(meta)];
    // An idle card draws no pills at all (the redesign's `jumping-peach`), so
    // liveness gates the row as well as the preview being non-empty.
    if live && !session.layout.preview.is_empty() {
        lines.push(any(pane_pills(&session.layout.preview, colors)));
    }
    let body = FlexView::new(Axis::Vertical, interleave(lines, CARD_LINE_GAP))
        .cross_axis(CrossAxisAlignment::Stretch);

    let card = panel(
        Padding(EdgeInsets::all(CARD_PAD), body),
        colors.surface,
        theme.shape.medium,
    )
    .border(colors.outline)
    .left_accent(live.then_some(colors.success));

    let attach_id = session.id.clone();
    let manage_id = session.id.clone();
    let pressable = GestureDetector(card)
        .on_tap(move |state: &mut SessionsState| super::pages::attach(state, &attach_id))
        .on_long_press(move |state: &mut SessionsState| {
            super::pages::open_manage_sheet(state, &manage_id)
        });

    if live {
        any(pressable)
    } else {
        // `.sess-card.idle{opacity:0.55}`. Frust has no *static* opacity
        // wrapper; `AnimatedOpacity` settles at its initial target with no
        // animate-in (Flutter's implicit-animation convention — only a later
        // change animates), so a pinned constant costs one layer push and no
        // extra frames. A card that later stops running fades, which is the
        // right behaviour anyway.
        any(AnimatedOpacity(IDLE_OPACITY, pressable))
    }
}

/// The hue a status chip prints in — `success` for the live one (a genuine
/// health state), a neutral wash for everything else. Never amber: a chip is
/// not a selection.
fn status_variant(status: SessionStatus) -> BadgeVariant {
    if status.is_live() {
        BadgeVariant::Success
    } else {
        BadgeVariant::Neutral
    }
}

/// `.sess-panes` — up to [`MAX_PREVIEW_PILLS`] equal-width (`flex:1`) pane
/// tiles.
fn pane_pills(preview: &[String], colors: &Palette) -> FlexView<SessionsState> {
    let theme = active_theme();
    let mut children = Vec::new();
    for title in preview.iter().take(MAX_PREVIEW_PILLS) {
        if !children.is_empty() {
            children.push(h_gap(PILL_GAP));
        }
        children.push(flexible(
            1,
            panel(
                Padding(
                    EdgeInsets::symmetric(PILL_PAD_X, PILL_PAD_Y),
                    text(title.clone()).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
                ),
                colors.base,
                theme.shape.extra_small,
            )
            .border(colors.rule),
        ));
    }
    row(children)
}

// ---------------------------------------------------------------------------
// `.new-sess-btn` and the empty state
// ---------------------------------------------------------------------------

/// `.new-sess-btn` — the dashed, centre-aligned row that closes the list, and
/// (with the empty state) one of the screen's only two create entry points now
/// that the stock app bar and its `+` are gone.
///
/// The `+` is amber because it is the call to action; the label beside it is
/// muted. See [`super::chrome::dashed_box`] for why the outline is hand-drawn.
///
/// `read_only` "disables" it the same way `ui::dialogs`' submit button does
/// (that module's framework-gap note: frust's `Button` has no disabled
/// variant) — a no-op `on_tap` is the behavioral half; the same whole-element
/// dim [`session_card`] uses for an idle card ([`IDLE_OPACITY`]) is the
/// visual half, one "this is inert right now" language for the screen rather
/// than a second one invented just for read-only affordances.
pub(super) fn new_session_row(colors: &Palette, read_only: bool) -> AnyView<SessionsState> {
    let theme = active_theme();
    let label = row(vec![
        inflexible(text(NEW_SESSION_PLUS).style(inked(&theme.type_scale.body_small, colors.amber))),
        h_gap(NEW_ROW_GAP),
        inflexible(
            text(NEW_SESSION_LABEL).style(inked(&theme.type_scale.body_small, colors.fg_muted)),
        ),
    ]);
    let dashed = GestureDetector(dashed_box(
        Padding(
            EdgeInsets::all(NEW_ROW_PAD),
            Align(Alignment::CENTER, label),
        ),
        colors.outline,
        theme.shape.medium,
    ))
    .on_tap(move |state: &mut SessionsState| {
        if !read_only {
            super::pages::open_create_dialog(state);
        }
    });
    if read_only {
        any(AnimatedOpacity(IDLE_OPACITY, dashed))
    } else {
        any(dashed)
    }
}

/// The amber call-to-action mark on the new-session row (`.new-sess-btn b`).
pub(super) const NEW_SESSION_PLUS: &str = "+";
/// The new-session row's label — one spelling, shared by the row and the
/// empty state's own action.
pub(super) const NEW_SESSION_LABEL: &str = "new session";

/// What the list shows once every session has been killed. The redesign has no
/// block for it (its mock is never empty), so this is Glyph's own
/// [`empty_state`] panel with the same create action the dashed row carries.
///
/// `read_only` "disables" the action the same way [`new_session_row`] does —
/// see that fn's doc.
pub(super) fn empty_view(read_only: bool) -> AnyView<SessionsState> {
    let style = if read_only {
        ButtonStyle::Ghost
    } else {
        ButtonStyle::Primary
    };
    any(
        empty_state("No sessions", "Create one to start working on this server.")
            .glyph("\u{25a3}")
            .action(
                button(
                    format!("{NEW_SESSION_PLUS} {NEW_SESSION_LABEL}"),
                    move |state: &mut SessionsState| {
                        if !read_only {
                            super::pages::open_create_dialog(state);
                        }
                    },
                )
                .style(style),
            ),
    )
}

// ---------------------------------------------------------------------------
// The management sheet's own pieces (over `AppState`, like the dialogs)
// ---------------------------------------------------------------------------

/// The management sheet's header: the session's own name, so a sheet opened
/// from a long-press says *which* session it is about.
///
/// Built over `AppState` (not [`SessionsState`]): a sheet is a pushed navigator
/// page, and this navigator's pages are `AnyView<AppState>` — the same
/// component boundary the dialogs cross.
pub(super) fn sheet_heading(title: impl Into<String>) -> AnyView<AppState> {
    let theme = active_theme();
    let colors = palette(&theme);
    any(text(title.into()).style(inked(&theme.type_scale.label_large, colors.fg)))
}

/// One management-sheet row: a full-width, 12.5px labelled action.
///
/// The ordinary rows are `Secondary` — a raised fill with a hairline, the
/// treatment the sheet CSS gives its own `.pill`/`.pane-item` rows — and
/// `destructive` swaps in the danger one (error ink + error hairline), which is
/// the sheet's only colour cue that Kill is not like its two neighbours.
/// `disabled` overrides either with `Ghost` (the same "disabled" language
/// [`new_session_row`] and [`empty_view`] use) — the row stays on screen, a
/// tap on it just does nothing; the caller's `on_press` is never invoked while
/// `disabled`.
pub(super) fn sheet_row(
    label: impl Into<String>,
    destructive: bool,
    disabled: bool,
    on_press: impl Fn(&mut AppState) + 'static,
) -> AnyView<AppState> {
    let style = if disabled {
        ButtonStyle::Ghost
    } else if destructive {
        ButtonStyle::Danger
    } else {
        ButtonStyle::Secondary
    };
    any(button(label.into(), move |state: &mut AppState| {
        if !disabled {
            on_press(state);
        }
    })
    .style(style))
}

// ---------------------------------------------------------------------------
// The picker sheet's rows — retained for the terminal screen (see module docs)
// ---------------------------------------------------------------------------

/// The picker sheet's rows: one row per session, showing the same facts a card
/// does in one line.
///
/// **No caller on this screen** — see the [module docs](self)'s "What is not
/// wired here any more". The terminal screen's Phase-3 FAB is what mounts this
/// next; it is `pub` (rather than dead) for exactly that reason.
pub fn picker_rows(
    sessions: &[Session],
    on_select: impl Fn(&str) + 'static,
) -> AnyView<crate::AppState> {
    let items: Vec<GlyphListItem> = sessions
        .iter()
        .map(|session| {
            glyph_list_item(session.initial(), session.name.clone())
                .sub(session.layout.counts_line())
                .meta(session.status.label())
                .chevron(true)
        })
        .collect();
    let ids: Vec<String> = sessions.iter().map(|s| s.id.clone()).collect();
    any(glyph_list::<crate::AppState>(items).on_press(
        move |_state: &mut crate::AppState, index| {
            if let Some(id) = ids.get(index) {
                on_select(id);
            }
        },
    ))
}

/// The picker sheet's heading, over `AppState` like its rows. Unwired here for
/// the same reason [`picker_rows`] is.
pub fn picker_heading(count: usize) -> AnyView<crate::AppState> {
    let theme = active_theme();
    any(text(format!("Attach to a session ({count})")).style(inked(
        &theme.type_scale.title_small,
        theme.scheme().on_surface,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strip renders five of [`ServerInfo`]'s facts; the rest belong to
    /// screens that are not this one, so this fixture leaves them empty.
    fn info(mode: TrustMode) -> ServerInfo {
        ServerInfo {
            endpoint: "100.71.31.57:50051".to_string(),
            trust_mode: mode,
            tls_label: "Trust on first use".to_string(),
            version: "zellij 0.44.3".to_string(),
            latency_ms: 36,
            backend_versions: Vec::new(),
            available_backends: Vec::new(),
            capabilities: std::collections::BTreeSet::new(),
            is_read_only: false,
        }
    }

    #[test]
    fn the_strips_meta_line_is_the_redesigns() {
        assert_eq!(
            meta_line(&info(TrustMode::TrustOnFirstUse)),
            "zellij 0.44.3 \u{b7} 36ms"
        );
    }

    #[test]
    fn only_the_validation_skipping_posture_earns_the_insecure_chip() {
        assert_eq!(insecure_chip(TrustMode::TrustOnFirstUse), Some("insecure"));
        assert_eq!(insecure_chip(TrustMode::System), None);
        assert_eq!(insecure_chip(TrustMode::Pinned), None);
    }

    #[test]
    fn the_top_bars_title_is_the_host_without_its_port() {
        assert_eq!(host_of("100.71.31.57:50051"), "100.71.31.57");
        assert_eq!(host_of("mock.example:7777"), "mock.example");
        // No port at all: the whole string is the host.
        assert_eq!(host_of("mock.example"), "mock.example");
    }

    #[test]
    fn liveness_is_success_and_never_the_brand_hue() {
        // The round-0 correction, as a tripwire: a running chip is the success
        // variant, an idle one is neutral, and neither is `Accent` (amber).
        assert_eq!(
            status_variant(SessionStatus::Running),
            BadgeVariant::Success
        );
        assert_eq!(
            status_variant(SessionStatus::Resurrectible),
            BadgeVariant::Neutral
        );
    }

    #[test]
    fn the_idle_dim_matches_the_source() {
        // `.sess-card.idle{opacity:0.55}` — a dim, not a disappearance.
        assert!((IDLE_OPACITY - 0.55).abs() < 1e-9, "{IDLE_OPACITY}");
    }

    #[test]
    fn the_cards_meta_row_gap_is_the_sources_and_not_the_chip_gap() {
        // Two different 6/12px gaps live a few lines apart in the source; a
        // swap is invisible in code review and obvious on screen.
        assert_eq!(META_GAP, 12.0);
        assert_eq!(PILL_GAP, 6.0);
    }
}
