//! The `/sessions` screen's independently-composable pieces, transcribed from
//! `docs/redesign.html`'s **sessions** and **picker sheet** blocks.
//!
//! One `pub(super)` fn per visual element, so
//! [`pages`](super::pages)'s `Component::build` reads as an arrangement rather
//! than a wall of nesting (the [`super::super::super::connect`]-screen
//! precedent, which splits its own eight pieces out the same way):
//!
//! | Piece | Redesign element |
//! |---|---|
//! | [`server_strip`] | `.conn-row` — endpoint, TLS posture badge, latency badge |
//! | [`credential_banner`] | the save-credential offer (no HTML block; see below) |
//! | [`session_card`] | `.sess-card` — avatar, name, chips, counts, pane pills, actions |
//! | [`new_session_row`] | `.new-sess-btn` — the dashed "+ new session" row |
//! | [`empty_view`] | the emptied-list state (no HTML block; Glyph's own `empty_state`) |
//! | [`picker_rows`] | the picker sheet's `.pick-row` list |
//!
//! # Real components over hand-composed chrome
//!
//! Every element that has a Glyph catalog counterpart uses it —
//! [`glyph_card`] for the card, [`avatar`] for `.sess-avatar`, [`badge`] for
//! the two `.chip`s and the latency pill, [`tag`] for the `.pane-pill`s,
//! [`alert`] for the offer banner, [`empty_state`] for the empty list, and
//! [`glyph_list`] for the picker rows. Nothing here paints its own rectangle;
//! the two places the redesign's CSS has no catalog counterpart (the card's
//! left status accent bar, the dashed new-session border) degrade to a
//! catalog affordance rather than growing a bespoke widget: the status is
//! already carried by the running/resurrectible chip, and the dashed row is a
//! `Secondary` button.
//!
//! # Tokens
//!
//! `Theme::glyph_baseline()` is canonical where it and the HTML disagree.
//! `--fg-dim` has no `ColorScheme` role of its own (frust-theme records it as a
//! documented leftover), so the dimmest ink derives from `on_surface_variant`
//! by alpha — the same derivation, with the same constant, the Connect
//! screen's own pieces document.

use frust::authoring::Color;
use frust::authoring::text::TextStyle;
use frust::glyph::{
    AlertVariant, BadgeVariant, GlyphListItem, alert, avatar, badge, empty_state, glyph_card,
    glyph_list, glyph_list_item, tag,
};
use frust::{
    AnyView, Axis, ButtonStyle, CrossAxisAlignment, FlexChild, FlexView, SizedBox, Theme, any,
    button, flexible, inflexible, text,
};

use crate::connection::{ServerInfo, TrustMode};
use crate::features::sessions::domain::entities::{Session, SessionStatus};

use super::pages::SessionsState;

/// `.sess-avatar{width:26px;height:26px}`.
const AVATAR_SIZE: f64 = 26.0;
/// `.sess-top{gap:10px}` / `.sess-panes{gap:6px}` — the two intra-card gaps.
const ROW_GAP: f64 = 10.0;
const PILL_GAP: f64 = 6.0;
/// `.sess-card` inner vertical rhythm (`margin-bottom:10px` on `.sess-top`
/// and `.sess-meta`). The card widget owns its own slot gap, so this is only
/// the gap *within* a slot.
const LINE_GAP: f64 = 6.0;
/// `--fg-dim` derived from `on_surface_variant` by alpha — see the
/// [module docs](self)'s Tokens note (the constant is the Connect screen's).
const FG_DIM_ALPHA: f32 = 0.62;
/// How many `.pane-pill` previews a card shows (the redesign draws two).
const MAX_PREVIEW_PILLS: usize = 2;

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

/// `style` re-inked with `color` — every run picks a Glyph type role for
/// size/family/weight and an explicit role-resolved ink, so nothing falls back
/// to `TextStyle::default()`'s 16px black.
fn inked(style: &TextStyle, color: Color) -> TextStyle {
    let mut style = style.clone();
    style.color = color;
    style
}

/// The dimmest label ink — see [`FG_DIM_ALPHA`].
fn fg_dim(theme: &Theme) -> Color {
    theme.scheme().on_surface_variant.with_alpha(FG_DIM_ALPHA)
}

/// A `gap`-wide horizontal spacer.
fn h_gap(gap: f64) -> FlexChild<SessionsState> {
    inflexible(SizedBox::<SessionsState>(Some(gap), None))
}

/// A `gap`-tall vertical spacer.
fn v_gap(gap: f64) -> FlexChild<SessionsState> {
    inflexible(SizedBox::<SessionsState>(None, Some(gap)))
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

// ---------------------------------------------------------------------------
// `.conn-row` — the server-info strip
// ---------------------------------------------------------------------------

/// The connected server's facts: endpoint + a live dot, the TLS posture badge,
/// the daemon version, and the round-trip latency badge — read from
/// [`crate::connection::ConnectionController::server_info`], which is
/// app-scoped precisely because this screen renders it (see that module's
/// docs).
///
/// Latency lives **here only**: Settings renders the endpoint/TLS/version
/// again, but a live round-trip time belongs next to the session list it
/// gates.
///
/// `None` renders a muted "not connected" line rather than nothing: the
/// session guard means it is unreachable in practice, and a silent gap would
/// be the worse failure if it ever were.
pub(super) fn server_strip(info: Option<&ServerInfo>) -> AnyView<SessionsState> {
    let theme = active_theme();
    let scheme = theme.scheme();
    let Some(info) = info else {
        return any(
            text("not connected").style(inked(&theme.type_scale.body_small, fg_dim(&theme)))
        );
    };
    let top = row(vec![
        inflexible(badge("connected", BadgeVariant::Success).dot(true)),
        h_gap(ROW_GAP),
        flexible(
            1,
            text(info.endpoint.clone())
                .style(inked(&theme.type_scale.body_medium, scheme.on_surface)),
        ),
        h_gap(ROW_GAP),
        inflexible(badge(
            format!("{} ms", info.latency_ms),
            BadgeVariant::Accent,
        )),
    ]);
    let bottom = row(vec![
        inflexible(badge(
            info.tls_label.clone(),
            trust_variant(info.trust_mode),
        )),
        spacer(),
        inflexible(
            text(info.version.clone()).style(inked(&theme.type_scale.label_small, fg_dim(&theme))),
        ),
    ]);
    any(column(vec![any(top), any(bottom)]).cross_axis(CrossAxisAlignment::Stretch))
}

/// The badge hue the server strip prints a TLS posture in — `.warn-badge` (the
/// redesign's amber "insecure" pill) for the one posture that actually skips
/// certificate validation, a quiet neutral wash for the two that don't.
///
/// Branches on the [`TrustMode`] **variant**, never on
/// [`ServerInfo::tls_label`]: the label is display text (and the Connect
/// screen's own trust selector marks the same posture with the same
/// `BadgeVariant::Warning` chip, from the same enum).
fn trust_variant(mode: TrustMode) -> BadgeVariant {
    match mode {
        TrustMode::TrustOnFirstUse => BadgeVariant::Warning,
        TrustMode::System | TrustMode::Pinned => BadgeVariant::Neutral,
    }
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
/// [`crate::connection::ConnectionController`]; accepting also raises the
/// "saved (mock)" toast, because nothing is actually persisted yet — no
/// secure-storage plugin is wired.
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
                crate::ui::toasts::app_toasts().show(format!("Saved {saved_label} (mock)"));
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

/// One session card: `.sess-top` (avatar + name + status/backend chips) as the
/// title slot, `.sess-meta` + `.sess-panes` as the description slot, and the
/// three per-session actions as the footer slot.
///
/// The actions are what the redesign's static mock leaves implicit and the
/// Flutter app makes explicit: **Attach** (the whole point of the screen —
/// pushes `/terminal`), **Rename** (the name-input dialog) and **Kill** (the
/// confirm dialog). Each closure captures only the session id, and re-reads
/// the live session from the controller when it needs more (a card built three
/// frames ago must not rename by a stale name).
pub(super) fn session_card(session: &Session) -> AnyView<SessionsState> {
    let theme = active_theme();
    let scheme = theme.scheme();
    let id = session.id.clone();

    let status_variant = if session.status.is_live() {
        BadgeVariant::Success
    } else {
        BadgeVariant::Neutral
    };
    let title = row(vec![
        inflexible(avatar(session.initial()).size(AVATAR_SIZE)),
        h_gap(ROW_GAP),
        flexible(
            1,
            text(session.name.clone())
                .style(inked(&theme.type_scale.body_medium, scheme.on_surface)),
        ),
        h_gap(PILL_GAP),
        inflexible(badge(session.status.label(), status_variant).dot(session.status.is_live())),
        h_gap(PILL_GAP),
        inflexible(badge(session.backend.label(), BadgeVariant::Neutral)),
    ]);

    let mut desc_children = vec![
        any(text(session.layout.counts_line())
            .style(inked(&theme.type_scale.label_small, fg_dim(&theme)))),
        any(text(format!("created {}", session.created))
            .style(inked(&theme.type_scale.label_small, fg_dim(&theme)))),
    ];
    if !session.layout.preview.is_empty() {
        desc_children.push(any(pane_pills(&session.layout.preview)));
    }
    let desc = FlexView::new(Axis::Vertical, interleave(desc_children, LINE_GAP))
        .cross_axis(CrossAxisAlignment::Stretch);

    let attach_id = id.clone();
    let rename_id = id.clone();
    let footer = row(vec![
        inflexible(
            button("Attach", move |state: &mut SessionsState| {
                super::pages::attach(state, &attach_id);
            })
            .style(ButtonStyle::Primary)
            .small(),
        ),
        spacer(),
        inflexible(
            button("Rename", move |state: &mut SessionsState| {
                super::pages::open_rename_dialog(state, &rename_id);
            })
            .style(ButtonStyle::Ghost)
            .small(),
        ),
        h_gap(PILL_GAP),
        inflexible(
            button("Kill", move |state: &mut SessionsState| {
                super::pages::open_kill_dialog(state, &id);
            })
            .style(ButtonStyle::Danger)
            .small(),
        ),
    ]);

    any(glyph_card().title(title).desc(desc).footer(footer))
}

/// `.sess-panes` — up to [`MAX_PREVIEW_PILLS`] pane-title pills.
fn pane_pills(preview: &[String]) -> FlexView<SessionsState> {
    let mut children = Vec::new();
    for title in preview.iter().take(MAX_PREVIEW_PILLS) {
        if !children.is_empty() {
            children.push(h_gap(PILL_GAP));
        }
        children.push(inflexible(tag::<SessionsState>(title.clone())));
    }
    children.push(spacer());
    row(children)
}

/// `[a, b, c]` with a `gap`-tall spacer between consecutive items, as flex
/// children.
fn interleave(views: Vec<AnyView<SessionsState>>, gap: f64) -> Vec<FlexChild<SessionsState>> {
    let mut children = Vec::with_capacity(views.len() * 2);
    for (index, view) in views.into_iter().enumerate() {
        if index > 0 {
            children.push(v_gap(gap));
        }
        children.push(inflexible(view));
    }
    children
}

// ---------------------------------------------------------------------------
// `.new-sess-btn` and the empty state
// ---------------------------------------------------------------------------

/// `.new-sess-btn` — the create-a-session row that closes the list. The app
/// bar carries the same action; this row is the one the redesign draws, and
/// the one a thumb reaches after scrolling.
pub(super) fn new_session_row() -> AnyView<SessionsState> {
    any(button(NEW_SESSION_LABEL, |state: &mut SessionsState| {
        super::pages::open_create_dialog(state);
    })
    .style(ButtonStyle::Secondary))
}

/// The "+ new session" label — one spelling, shared by the list row and this
/// screen's empty state.
pub(super) const NEW_SESSION_LABEL: &str = "+ new session";

/// What the list shows once every session has been killed. The redesign has no
/// block for it (its mock is never empty), so this is Glyph's own
/// [`empty_state`] panel with the same create action the list row carries.
pub(super) fn empty_view() -> AnyView<SessionsState> {
    any(
        empty_state("No sessions", "Create one to start working on this server.")
            .glyph("\u{25a3}")
            .action(
                button(NEW_SESSION_LABEL, |state: &mut SessionsState| {
                    super::pages::open_create_dialog(state);
                })
                .style(ButtonStyle::Primary),
            ),
    )
}

// ---------------------------------------------------------------------------
// The picker sheet's rows
// ---------------------------------------------------------------------------

/// The picker sheet's `.pick-row` list: one row per session, showing the same
/// facts the card does in one line. Selecting a row is the *same* attach path
/// the card's Attach button takes — the sheet is a shortcut, never a second
/// way to do the thing.
///
/// Built over `AppState` (not [`SessionsState`]): a sheet is a pushed
/// navigator page, and this navigator's pages are `AnyView<AppState>` — so
/// the row callbacks reach the controller through the captured handle rather
/// than through component state, exactly as the dialogs do.
pub(super) fn picker_rows(
    sessions: &[Session],
    on_select: impl Fn(&str) + 'static,
) -> AnyView<crate::AppState> {
    let items: Vec<GlyphListItem> = sessions
        .iter()
        .map(|session| {
            glyph_list_item(session.initial(), session.name.clone())
                .sub(session.layout.counts_line())
                .meta(status_meta(session.status))
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

/// The trailing meta a picker row prints for `status`.
fn status_meta(status: SessionStatus) -> &'static str {
    status.label()
}

/// The picker sheet's heading, over `AppState` like the rows.
pub(super) fn picker_heading(count: usize) -> AnyView<crate::AppState> {
    let theme = active_theme();
    any(text(format!("Attach to a session ({count})")).style(inked(
        &theme.type_scale.title_small,
        theme.scheme().on_surface,
    )))
}
