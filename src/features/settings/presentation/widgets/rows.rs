//! `.set-row`/`.set-group-lbl` — the row primitive every group is built from,
//! plus the shared layout helpers ([`h_gap`]/[`spacer`]/[`row`]/[`column`])
//! and the screen's page tone ([`page_tone`]) every other `widgets` module
//! draws with.
//!
//! Geometry transcribed from `docs/redesign.html`'s settings block (CSS lines
//! 153–179). [`RowMark`]/[`MARK_*`] cover every group's tile: a vendored
//! `frust::icons` vector, or a single-font-safe text mark (ASCII or Geometric
//! Shapes only) for the two backend initials and the mock's literal glyphs —
//! **never an emoji**, which resolves through platform font fallback (the
//! tofu-on-iPhone defect this app has hit before).

use frust::authoring::{Color, Size};
use frust::icons::{CHEVRON_RIGHT, FORMAT_SIZE, LINK, LOCK, NOTIFICATIONS, REFRESH};
use frust::{
    AnyView, Axis, CrossAxisAlignment, EdgeInsets, FlexChild, FlexView, GestureDetector,
    IconSource, Padding, SizedBox, any, flexible, icon, inflexible, text,
};

use crate::features::settings::presentation::chrome::panel;
use crate::features::settings::presentation::pages::{self, SettingsState};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::pickers::PickerKind;

/// `.settings-scroll{padding:6px 0 30px}`.
pub(in super::super) const SCROLL_PAD_TOP: f64 = 6.0;
pub(in super::super) const SCROLL_PAD_BOTTOM: f64 = spacing::HERO;
/// `.set-group-lbl{padding:16px 16px 8px}`.
pub(super) const GROUP_PAD_TOP: f64 = spacing::XL;
pub(super) const GROUP_PAD_BOTTOM: f64 = spacing::SM;
/// `.set-row{padding:11px 16px; gap:12px}` — also `.token-item`'s padding.
pub(super) const ROW_PAD_X: f64 = spacing::XL;
pub(super) const ROW_PAD_Y: f64 = 11.0;
pub(super) const ROW_GAP: f64 = spacing::BASE;
/// `.set-ic{width:28px;height:28px}`; the vector inside it (`font-size:12px`
/// in the mock, sized up to match a Material Symbols vector's drawn weight).
const MARK_BOX: f64 = 28.0;
const MARK_ICON: f64 = spacing::LG;
/// `.set-value{margin-top:2px}`.
pub(super) const ROW_LINE_GAP: f64 = spacing::MICRO;
/// `.set-chev{font-size:13px}`.
const CHEVRON: f64 = 13.0;

/// A `gap`-wide horizontal spacer.
pub(super) fn h_gap<S: 'static>(gap: f64) -> FlexChild<S> {
    inflexible(SizedBox::<S>(Some(gap), None))
}

/// A flexible spacer pushing what follows it to the trailing edge — Flex v1
/// has no `space-between`.
pub(super) fn spacer<S: 'static>() -> FlexChild<S> {
    flexible(1, SizedBox::<S>(None, None))
}

/// A vertically-centered row.
pub(super) fn row<S: 'static>(children: Vec<FlexChild<S>>) -> FlexView<S> {
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
/// clear colour (`--bg-surface`) is not.
pub(in super::super) fn page_tone(
    body: impl frust::View<SettingsState>,
    colors: &Palette,
) -> AnyView<SettingsState> {
    any(panel(body, colors.base, 0.0))
}

/// The band above each group — 10px dim, letter-spaced, uppercased here since
/// `text` carries no CSS `text-transform`.
pub(in super::super) fn group_label(label: &str, colors: &Palette) -> AnyView<SettingsState> {
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

/// What sits in a row's 28px `.set-ic` tile — a vendored vector, or a
/// single-font-safe text mark (`abc`, `▮`, a backend initial).
#[derive(Clone, Copy)]
pub(super) enum RowMark {
    Vector(IconSource),
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

/// What a row *does*, and so what sits in its trailing slot. The bell row is
/// not a `RowAction` — the real switch owns its own tap target (see
/// `widgets::misc::bell_notifications_row`).
#[derive(Clone, Copy)]
pub(in super::super) enum RowAction {
    /// Inert: no chevron, no gesture (the version rows and connection info).
    Static,
    /// A chevron row that opens a picker sheet.
    Picker(PickerKind),
}

/// One `.set-row`: the mark tile, the label over its value, and a trailing
/// chevron for anything that leads somewhere. For [`RowAction::Picker`] the
/// **whole row** is the tap target — the mock draws no separate hit area, and
/// a 28px chevron alone would be unreachable on a phone.
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
    if let Some(trailing) = trailing_slot(action, colors) {
        children.push(h_gap(ROW_GAP));
        children.push(inflexible(trailing));
    }

    let box_ = panel(
        Padding(EdgeInsets::symmetric(ROW_PAD_X, ROW_PAD_Y), row(children)),
        Color::TRANSPARENT,
        0.0,
    )
    .bottom_rule(colors.rule);

    match action {
        RowAction::Static => any(box_),
        RowAction::Picker(_) => any(GestureDetector(box_)
            .on_tap(move |state: &mut SettingsState| pages::row_tapped(state, action))),
    }
}

/// `.set-ic` — the 28px raised tile holding a row's mark. Shared with
/// `widgets::misc::bell_notifications_row`, the one row outside this file
/// that also uses it.
pub(super) fn mark_tile(mark: RowMark, colors: &Palette) -> AnyView<SettingsState> {
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
/// somewhere, nothing for a static fact.
fn trailing_slot(action: RowAction, colors: &Palette) -> Option<AnyView<SettingsState>> {
    match action {
        RowAction::Static => None,
        RowAction::Picker(_) => Some(any(icon(CHEVRON_RIGHT)
            .size(CHEVRON)
            .color(colors.fg_faintest))),
    }
}

// ---------------------------------------------------------------------------
// The mark set, named once
// ---------------------------------------------------------------------------

/// `.set-ic` for `server url` — a link, standing in for the mock's 🌐.
pub(super) const MARK_SERVER_URL: RowMark = RowMark::Vector(LINK);
/// …for `tls certificate` — the mock's 🔓 as a vector. Not state-dependent:
/// the row's *value* carries the posture, so a second signal in the tile
/// would say the same thing twice.
pub(super) const MARK_TLS: RowMark = RowMark::Vector(LOCK);
/// …for `zellij version`/`herdr version` — the backend's initial, the same
/// "initial in a tile" device `.sess-avatar` uses.
pub(super) const MARK_ZELLIJ: RowMark = RowMark::Text("z");
pub(super) const MARK_HERDR: RowMark = RowMark::Text("h");
/// …for `font family` — the mock's literal `abc`, kept as text.
pub(super) const MARK_FONT_FAMILY: RowMark = RowMark::Text("abc");
/// …for `font size` — the vendored `format_size` vector rather than the
/// mock's literal `12`, which would go stale the moment the picker changed
/// it.
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
    fn no_mark_is_an_emoji() {
        // A text mark must stay inside the BMP ranges the app's one bundled
        // family actually covers — ASCII or Geometric Shapes — because
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
}
