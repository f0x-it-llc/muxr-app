//! `.term-statusrow` — the pane title plus the one expand button, the whole of
//! the card's chrome band.

use std::rc::Rc;

use frust::authoring::text::{LineHeight, TextOverflow};
use frust::{
    Align, Alignment, AnyView, EdgeInsets, GestureDetector, Padding, SizedBox, Stack, any,
    container, flexible, icons, inflexible, text,
};
use frust_glyph::{MenuEntry, breadcrumb, crumb, menu_item, menu_item_danger, menu_separator};

use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;

use super::super::chrome::{EXPAND_SIZE, expand_button};
use super::card::CardInk;
use super::util::{h_gap, read_only_badge, row, sized};

/// `.term-statusrow{padding:14px 14px 14px 18px}` with the **vertical** pair
/// tightened to 6px — the mock's 14px made a 56px chrome band. Horizontal
/// values are the mock's, untouched.
const STATUS_PAD_TOP: f64 = 6.0;
const STATUS_PAD_RIGHT: f64 = 14.0;
const STATUS_PAD_LEFT: f64 = 18.0;
/// The row's whole height, and so the expand button's press target: the
/// 28px button (the row's shrink floor) plus [`STATUS_PAD_TOP`] above and
/// below. The title's own line box never decides this number.
const STATUS_ROW_H: f64 = EXPAND_SIZE + 2.0 * STATUS_PAD_TOP;
/// `.term-pane-title{font-size:13.5px}` — a fixed-size logotype-ish run,
/// overriding only the display role's size (the `crate::ui::topbar`
/// brand-wordmark precedent).
const TITLE_SIZE: f32 = 13.5;
/// Its line box, tightened to match — the display role's 36px box would be
/// taller than [`STATUS_ROW_H`] itself.
const TITLE_LINE_HEIGHT: f32 = 18.0;

/// The expand button's accessible names — a *word* per state, never the
/// `⛶`/`⤡` it draws (`crate::ui::topbar`'s `BACK_LABEL` rule).
const EXPAND_LABEL: &str = "Fullscreen";
const COLLAPSE_LABEL: &str = "Exit fullscreen";

/// What the Tablet's terminal menu asks for — [`terminal_menu_choice`] maps a
/// picked entry index here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalMenuChoice {
    /// The existing rename dialog (or its read-only toast).
    Rename,
    /// The existing fullscreen toggle.
    Immersive,
    /// The key-bar visibility signal.
    KeyBar,
    /// The existing detach action.
    Detach,
}

/// The kebab's one table: each entry beside the choice it answers (`None` for
/// the separator, which still occupies an index). Glyph's menu has no
/// checked-item variant, so "Show key bar" carries a leading check icon
/// **only while it is on** (the menu is rebuilt on every open).
fn terminal_menu_table(
    fullscreen: bool,
    key_bar_visible: bool,
) -> Vec<(Option<TerminalMenuChoice>, MenuEntry)> {
    let immersive = if fullscreen {
        icons::FULLSCREEN_EXIT
    } else {
        icons::FULLSCREEN_EXPAND
    };
    let mut key_bar = menu_item("Show key bar");
    if key_bar_visible {
        key_bar = key_bar.icon(icons::CHECK);
    }
    vec![
        (
            Some(TerminalMenuChoice::Rename),
            menu_item("Rename session").icon(icons::EDIT),
        ),
        (
            Some(TerminalMenuChoice::Immersive),
            menu_item("Immersive mode").icon(immersive),
        ),
        (Some(TerminalMenuChoice::KeyBar), key_bar),
        (None, menu_separator()),
        (
            Some(TerminalMenuChoice::Detach),
            menu_item_danger("Detach session").icon(icons::LOGOUT),
        ),
    ]
}

/// The Tablet kebab's menu entries, straight from [`terminal_menu_table`].
pub(crate) fn terminal_menu(fullscreen: bool, key_bar_visible: bool) -> Vec<MenuEntry> {
    terminal_menu_table(fullscreen, key_bar_visible)
        .into_iter()
        .map(|(_, entry)| entry)
        .collect()
}

/// The [`TerminalMenuChoice`] behind entry `index` of [`terminal_menu`], read
/// back through the same table (the separator answers `None`).
pub(crate) fn terminal_menu_choice(index: usize) -> Option<TerminalMenuChoice> {
    terminal_menu_table(false, false)
        .get(index)
        .and_then(|(choice, _)| *choice)
}

/// `.term-statusrow` — the pane title (ellipsized, single line) and, at the
/// trailing edge, the **one** fullscreen affordance: no menu row, no
/// gesture ("one button, always in the same place").
///
/// The padding is applied to the **title**, not the whole row — that lets
/// the button's column run the row's full [`STATUS_ROW_H`]: a 28px circle
/// centred in a 40px column, the padding band above/below part of the same
/// target (padding the row instead would leave a 28px target in a 40px row).
///
/// Two press paths, one callback, never both: the column is a [`Stack`]
/// whose **top** child is the button and whose bottom is a
/// [`GestureDetector`] over the full-height box. Hit-testing runs
/// topmost-first and stops at the first handler, so a press on the circle
/// is the button's and a press above/below it is the detector's.
///
/// `crumbs` (empty hides it) is Glyph's breadcrumb after the title, and
/// `read_only` adds the cyan chip before the button.
pub(crate) fn status_row<State: 'static, F: Fn(&mut State) + 'static>(
    title: &str,
    crumbs: &[String],
    fullscreen: bool,
    read_only: bool,
    colors: &Palette,
    ink: &CardInk,
    on_toggle: F,
) -> AnyView<State> {
    let theme = active_theme();
    let mut title_style = sized(&theme.type_scale.display_small, TITLE_SIZE, ink.prompt);
    title_style.line_height = LineHeight::Absolute(TITLE_LINE_HEIGHT);

    let (mark, label) = if fullscreen {
        (icons::FULLSCREEN_EXIT, COLLAPSE_LABEL)
    } else {
        (icons::FULLSCREEN_EXPAND, EXPAND_LABEL)
    };
    // `.expand-btn.on{...}` — the same trio the `:hover` state uses, which a
    // touch screen has no notion of.
    let (fill, tint, border) = if fullscreen {
        (colors.amber_faint, colors.amber, colors.amber_border)
    } else {
        (ink.expand_fill, colors.fg_muted, ink.card_border)
    };

    // Shared so both halves of the press column can fire it (each just clones the handle).
    let on_toggle = Rc::new(on_toggle);
    let on_button = Rc::clone(&on_toggle);
    let button = SizedBox::<State>(Some(EXPAND_SIZE), Some(STATUS_ROW_H)).child(Align(
        Alignment::CENTER,
        expand_button(mark, label, move |state: &mut State| on_button(state))
            .tones(fill, tint, border)
            .on(fullscreen),
    ));
    let press_band = GestureDetector(SizedBox::<State>(Some(EXPAND_SIZE), Some(STATUS_ROW_H)))
        .on_tap(move |state: &mut State| on_toggle(state));

    let mut children = vec![flexible(
        1,
        Padding(
            EdgeInsets {
                left: STATUS_PAD_LEFT,
                top: STATUS_PAD_TOP,
                right: 0.0,
                bottom: STATUS_PAD_TOP,
            },
            text(title)
                .style(title_style)
                .max_lines(1)
                .overflow(TextOverflow::Ellipsis),
        ),
    )];
    if !crumbs.is_empty() {
        children.push(h_gap(spacing::SM));
        let trail = crumbs
            .iter()
            .map(|name| crumb::<State>(name.clone()))
            .collect();
        children.push(inflexible(breadcrumb(trail)));
    }
    if read_only {
        children.push(h_gap(spacing::SM));
        children.push(inflexible(read_only_badge()));
    }
    children.push(h_gap(spacing::SM));
    children.push(inflexible(Stack(vec![any(press_band), any(button)])));
    // Trailing padding as a spacer — the button column owns the vertical padding.
    children.push(h_gap(STATUS_PAD_RIGHT));
    any(container(row(children)).fill(ink.head))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_expand_affordance_announces_a_word_and_not_a_mark() {
        // The a11y half of the vector rule.
        for label in [EXPAND_LABEL, COLLAPSE_LABEL] {
            assert!(
                label.chars().all(|c| c.is_ascii_alphabetic() || c == ' '),
                "{label:?} is a name, not a mark"
            );
        }
    }

    #[test]
    fn the_terminal_menu_composes_the_checked_item_from_a_leading_icon() {
        let labels: Vec<_> = terminal_menu(false, true)
            .iter()
            .map(|entry| match entry {
                MenuEntry::Item(item) => Some((item.label.clone(), item.icon.is_some())),
                MenuEntry::Separator => None,
            })
            .collect();
        assert_eq!(
            labels[0].as_ref().map(|l| l.0.as_str()),
            Some("Rename session")
        );
        assert_eq!(
            labels[1].as_ref().map(|l| l.0.as_str()),
            Some("Immersive mode")
        );
        assert_eq!(labels[2], Some(("Show key bar".to_string(), true)));
        assert_eq!(labels[3], None);
        assert_eq!(
            labels[4].as_ref().map(|l| l.0.as_str()),
            Some("Detach session")
        );
        let off = terminal_menu(false, false);
        assert!(matches!(&off[2], MenuEntry::Item(item) if item.icon.is_none()));
        assert_eq!(terminal_menu_choice(3), None);
        assert_eq!(terminal_menu_choice(4), Some(TerminalMenuChoice::Detach));
    }

    #[test]
    fn the_transcribed_geometry_is_the_mocks() {
        assert_eq!(
            (STATUS_PAD_TOP, STATUS_PAD_RIGHT, STATUS_PAD_LEFT),
            (6.0, 14.0, 18.0)
        );
        assert_eq!(EXPAND_SIZE, 28.0);
    }

    #[test]
    fn the_status_row_is_as_short_as_its_button_allows() {
        // The *whole* height is the press target (`status_row`'s own doc).
        assert_eq!(STATUS_ROW_H, 40.0);
        assert!(
            f64::from(TITLE_LINE_HEIGHT) + 2.0 * STATUS_PAD_TOP < STATUS_ROW_H,
            "the title's line box never decides the row's height"
        );
    }
}
