//! `.term-statusrow`: the pane title and chrome-band buttons, plus the Tablet kebab's menu.

use std::rc::Rc;

use frust::authoring::text::{LineHeight, TextOverflow};
use frust::{
    Align, Alignment, EdgeInsets, Either, GestureDetector, Padding, SizedBox, View, container,
    flexible, icons, inflexible, stack, text,
};
use frust_glyph::{MenuEntry, breadcrumb, crumb, menu_item, menu_item_danger, menu_separator};

use crate::ui::paint::RADIUS_FULL;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::spacing;

use super::super::chrome::{EXPAND_SIZE, expand_button};
use super::card::CardInk;
use super::util::{centered_row, h_gap, read_only_badge, sized};

/// `.term-statusrow{padding:14px 14px 14px 18px}` with the vertical pair tightened to 6px.
const STATUS_PAD_TOP: f64 = 6.0;
const STATUS_PAD_RIGHT: f64 = 14.0;
const STATUS_PAD_LEFT: f64 = 18.0;
/// The expand button's whole press target: the 28px button plus the padding above and below.
const STATUS_ROW_H: f64 = EXPAND_SIZE + 2.0 * STATUS_PAD_TOP;
/// `.term-pane-title{font-size:13.5px}`, overriding only the display role's size.
const TITLE_SIZE: f32 = 13.5;
/// The display role's 36px line box would be taller than [`STATUS_ROW_H`] itself.
const TITLE_LINE_HEIGHT: f32 = 18.0;

/// A word per state, never the `⛶`/`⤡` glyph it draws.
const EXPAND_LABEL: &str = "Fullscreen";
const COLLAPSE_LABEL: &str = "Exit fullscreen";
const PANES_LABEL: &str = "Panes & tabs";
const ATTENTION_DOT: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalMenuChoice {
    Rename,
    Immersive,
    KeyBar,
    Detach,
}

/// `None` marks the separator, which still occupies an index. Glyph's menu has no checked-item
/// variant, so "Show key bar" carries a leading check only while it is on.
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

pub(crate) fn terminal_menu(fullscreen: bool, key_bar_visible: bool) -> Vec<MenuEntry> {
    terminal_menu_table(fullscreen, key_bar_visible)
        .into_iter()
        .map(|(_, entry)| entry)
        .collect()
}

/// Decodes through the builder's own table; its flags change only icons, so any pair decodes alike.
pub(crate) fn terminal_menu_choice(index: usize) -> Option<TerminalMenuChoice> {
    terminal_menu_table(false, false)
        .get(index)
        .and_then(|(choice, _)| *choice)
}

/// `attention` draws the warning dot at the top-right; the page sets it while an agent is blocked.
pub(crate) struct PanesControl<State> {
    on_tap: Rc<dyn Fn(&mut State)>,
    attention: bool,
}

impl<State> PanesControl<State> {
    pub(crate) fn new(on_tap: impl Fn(&mut State) + 'static) -> Self {
        Self {
            on_tap: Rc::new(on_tap),
            attention: false,
        }
    }

    pub(crate) fn with_attention(mut self, attention: bool) -> Self {
        self.attention = attention;
        self
    }
}

/// One struct to stay under clippy's `too_many_arguments` threshold.
pub(crate) struct StatusOptions<State> {
    pub(crate) fullscreen: bool,
    pub(crate) read_only: bool,
    /// `None` everywhere but Compact fullscreen.
    pub(crate) panes: Option<PanesControl<State>>,
}

/// The padding sits on the title so the button column runs the full [`STATUS_ROW_H`] as one target.
/// The column is a [`Stack`] with the button over a full-height [`GestureDetector`]: hit-testing is
/// topmost-first and stops at the first handler, so each press fires the callback once.
pub(crate) fn status_row<State: 'static, F: Fn(&mut State) + 'static>(
    title: &str,
    crumbs: &[String],
    options: StatusOptions<State>,
    colors: &Palette,
    ink: &CardInk,
    on_toggle: F,
) -> impl View<State> {
    let StatusOptions {
        fullscreen,
        read_only,
        panes,
    } = options;
    let theme = active_theme();
    let mut title_style = sized(&theme.type_scale.display_small, TITLE_SIZE, ink.prompt);
    title_style.line_height = LineHeight::Absolute(TITLE_LINE_HEIGHT);

    let (mark, label) = if fullscreen {
        (icons::FULLSCREEN_EXIT, COLLAPSE_LABEL)
    } else {
        (icons::FULLSCREEN_EXPAND, EXPAND_LABEL)
    };
    // `.expand-btn.on{...}`, the same trio as `:hover`, which a touch screen has no notion of.
    let (fill, tint, border) = if fullscreen {
        (colors.amber_faint, colors.amber, colors.amber_border)
    } else {
        (ink.expand_fill, colors.fg_muted, ink.card_border)
    };

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
    if let Some(panes) = panes {
        children.push(h_gap(spacing::SM));
        children.push(inflexible(panes_column(panes, colors, ink)));
    }
    children.push(h_gap(spacing::SM));
    children.push(inflexible(stack().child(press_band).child(button)));
    // The button column owns the vertical padding.
    children.push(h_gap(STATUS_PAD_RIGHT));
    container(centered_row(children)).fill(ink.head)
}

/// Neutral tint, since it toggles nothing. The attention dot is stacked under the press band so it
/// never takes a press.
fn panes_column<State: 'static>(
    panes: PanesControl<State>,
    colors: &Palette,
    ink: &CardInk,
) -> impl View<State> {
    let PanesControl { on_tap, attention } = panes;
    let on_button = Rc::clone(&on_tap);
    let button = SizedBox::<State>(Some(EXPAND_SIZE), Some(STATUS_ROW_H)).child(Align(
        Alignment::CENTER,
        expand_button(icons::MORE_VERT, PANES_LABEL, move |state: &mut State| {
            on_button(state)
        })
        .tones(ink.expand_fill, colors.fg_muted, ink.card_border),
    ));
    let press_band = GestureDetector(SizedBox::<State>(Some(EXPAND_SIZE), Some(STATUS_ROW_H)))
        .on_tap(move |state: &mut State| on_tap(state));
    let dot = if attention {
        Either::Left(
            SizedBox::<State>(Some(EXPAND_SIZE), Some(STATUS_ROW_H)).child(Align(
                Alignment::TOP_RIGHT,
                container(SizedBox::<State>(Some(ATTENTION_DOT), Some(ATTENTION_DOT)))
                    .fill(colors.warning)
                    .radius(RADIUS_FULL),
            )),
        )
    } else {
        Either::Right(SizedBox::<State>(Some(EXPAND_SIZE), Some(STATUS_ROW_H)))
    };
    stack().child(dot).child(press_band).child(button)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_expand_affordance_announces_a_word_and_not_a_mark() {
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
        assert_eq!(STATUS_ROW_H, 40.0);
        assert!(
            f64::from(TITLE_LINE_HEIGHT) + 2.0 * STATUS_PAD_TOP < STATUS_ROW_H,
            "the title's line box never decides the row's height"
        );
    }

    #[test]
    fn the_panes_control_names_what_it_opens() {
        assert_eq!(PANES_LABEL, "Panes & tabs");
        assert!(
            PANES_LABEL
                .chars()
                .all(|c| c.is_ascii_alphabetic() || c == ' ' || c == '&'),
            "{PANES_LABEL:?} is a name, not a mark"
        );
    }

    #[test]
    fn the_panes_control_starts_without_an_attention_dot() {
        let control = PanesControl::<()>::new(|_| {});
        assert!(!control.attention);
        assert!(control.with_attention(true).attention);
    }

    fn attention_dots(attention: bool) -> Vec<(f64, f64)> {
        use frust::authoring::{Color, Point, Size};
        use frust_core::{FrameTime, PaintScene, RenderRoot};
        use frust_text::TextContext;

        #[derive(Default)]
        struct Rec {
            rounded: Vec<(Size, f64, Color)>,
        }
        impl PaintScene for Rec {
            fn fill_rect(&mut self, _origin: Point, _size: Size, _color: Color) {}
            fn draw_text(&mut self, _origin: Point, _text: &str) {}
            fn fill_rounded_rect(&mut self, _origin: Point, size: Size, radius: f64, color: Color) {
                self.rounded.push((size, radius, color));
            }
        }

        let theme = frust_glyph::baseline();
        let colors = Palette::from_theme(&theme);
        let ink = CardInk::from_theme(&theme);
        let mut logic = |_state: &mut ()| {
            status_row(
                "zsh",
                &[],
                StatusOptions {
                    fullscreen: true,
                    read_only: false,
                    panes: Some(PanesControl::new(|_: &mut ()| {}).with_attention(attention)),
                },
                &colors,
                &ink,
                |_: &mut ()| {},
            )
        };
        let mut root = RenderRoot::new();
        let mut tcx = TextContext::new();
        root.rebuild(&mut logic, &mut ());
        let tcx_any: &mut dyn std::any::Any = &mut tcx;
        root.layout_with_text(Size::new(360.0, 80.0), tcx_any);
        let mut rec = Rec::default();
        root.paint(&mut rec, FrameTime::from_nanos(0));
        rec.rounded
            .into_iter()
            .filter(|(size, _, color)| *color == colors.warning && size.width == ATTENTION_DOT)
            .map(|(size, radius, _)| (size.width, radius))
            .collect()
    }

    #[test]
    fn the_attention_dot_is_painted_round_only_while_attention_is_on() {
        let on = attention_dots(true);
        assert_eq!(on.len(), 1, "one dot: {on:?}");
        assert!(
            on[0].1 >= ATTENTION_DOT / 2.0,
            "a circle, not a square: {on:?}"
        );
        assert!(attention_dots(false).is_empty());
    }
}
