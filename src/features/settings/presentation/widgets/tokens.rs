//! The token list — filter, scope segments, 52px rows with a ⋯ menu — shared by the tokens page
//! and the Tablet card. A minted token's secret is shown once, through [`secret_reveal_sheet`],
//! and never persisted. A read-only session's guard toasts rather than mints or revokes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, EventCtx, EventResult, InputEvent, LayoutCtx,
    PaintCtx, PaintScene, Point, Rect, SemanticsCtx, Size, Widget, build_child, rebuild_child,
    route_event_single, teardown_child,
};
use frust::{
    Align, Alignment, AnyView, Axis, ButtonStyle, CrossAxisAlignment, EdgeInsets, Either, FlexView,
    GestureDetector, Get, GetUntracked, NavigatorController, Padding, PopResult, RwSignal, Set,
    SizedBox, View, any, button, column, container, divider, flexible, icons, inflexible,
    safe_area, scroll_view, text, text_input,
};
use frust_glyph::{
    MenuEntry, glyph_card, glyph_dialog, menu_item, menu_item_danger, menu_separator,
    segmented_control, show_glyph_dialog, toggle,
};

use crate::AppState;
use crate::features::settings::domain::entities::{AuthToken, MintedToken, TokenScope};
use crate::features::settings::presentation::controller::{ScopeFilter, TokenRow};
use crate::features::settings::presentation::pages::{self, SettingsState, TokenFrame};
use crate::features::settings::presentation::scramble::{self, ScramblePose};
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;
use crate::ui::toasts::app_toasts;
use crate::ui::topbar::icon_button;

use super::pickers::PICK_TITLE_GAP;
use super::rows::{
    HAIRLINE, RADIUS_FULL, ROW_LINE_GAP, ROW_PAD_X, SCROLL_PAD_BOTTOM, SETTINGS_CLAMP_WIDTH,
    centered_row, group_label, stretch_column,
};

/// A row is 52px, its hairline included.
const ROW_HEIGHT: f64 = 52.0;
const TOKEN_LINE_GAP: f64 = spacing::MICRO;
/// `.token-scope{padding:2px 7px}`.
const CHIP_PAD_X: f64 = 7.0;
const CHIP_PAD_Y: f64 = spacing::MICRO;
/// The ⋯ press target, and the mark inside it.
const MORE_BOX: f64 = 44.0;
const MORE_MARK: f64 = 18.0;
const FILTER_TOP: f64 = spacing::LG;
const FILTER_GAP: f64 = spacing::BASE;
const STATUS_PAD: f64 = spacing::HERO;
const DIALOG_FIELD_GAP: f64 = spacing::XL;

pub const FILTER_PLACEHOLDER: &str = "filter tokens";
pub const NO_MATCH_LABEL: &str = "no tokens match";
pub const THIS_DEVICE_PREFIX: &str = "this device \u{b7} ";
pub const NEW_TOKEN_ACTION: &str = "+ new token";
const TOKENS_LOADING: &str = "loading\u{2026}";
const TOKENS_FAILED: &str = "couldn't load tokens";
const MENU_COPY_NAME: &str = "Copy name";
const MENU_REVOKE: &str = "Revoke\u{2026}";
const CREATE_TOKEN_TITLE: &str = "New token";
const DIALOG_CANCEL_LABEL: &str = "Cancel";
const DIALOG_SUBMIT_LABEL: &str = "Save";
pub(super) const READ_ONLY_TOGGLE_LABEL: &str = "read-only";
pub(super) const SECRET_REVEAL_TITLE: &str = "token secret";
const COPY_SECRET_LABEL: &str = "Copy secret";
const SECRET_DONE_LABEL: &str = "Done";
const SECRET_CAPTION: &str =
    "This is the only time this secret is shown. Copy it now — it can't be viewed again.";
const SECRET_ALREADY_DISMISSED: &str = "This secret has already been dismissed.";
/// Duplicated, since a feature never imports a sibling's `presentation`.
const COPIED_TOAST: &str = "Copied to clipboard";
const COPY_FAILED_TOAST: &str = "Couldn't copy to clipboard";

/// A row's ⋯ menu, in its on-screen order (a separator sits between the two).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenMenu {
    CopyName,
    Revoke,
}

impl TokenMenu {
    pub fn index(self) -> usize {
        match self {
            TokenMenu::CopyName => 0,
            TokenMenu::Revoke => 2,
        }
    }

    pub fn from_index(index: usize) -> Option<Self> {
        [TokenMenu::CopyName, TokenMenu::Revoke]
            .into_iter()
            .find(|entry| entry.index() == index)
    }
}

pub(in super::super) fn token_menu() -> Vec<MenuEntry> {
    vec![
        menu_item(MENU_COPY_NAME),
        menu_separator(),
        menu_item_danger(MENU_REVOKE),
    ]
}

pub type CopySink = Rc<dyn Fn(&str)>;

thread_local! {
    static COPY_NAME_SINK: RefCell<Option<CopySink>> = const { RefCell::new(None) };
}

/// Diverts Copy name from the system clipboard; `None` restores it.
pub fn set_copy_name_sink_for_tests(sink: Option<CopySink>) {
    COPY_NAME_SINK.with(|cell| *cell.borrow_mut() = sink);
}

/// A name is not a secret, so it takes the plain clipboard write.
pub(in super::super) fn copy_token_name(name: &str) {
    let sink = COPY_NAME_SINK.with(|cell| cell.borrow().clone());
    let copied = match sink {
        Some(sink) => {
            sink(name);
            true
        }
        None => frust_clipboard::Clipboard::set_text(name).is_ok(),
    };
    app_toasts().show(if copied {
        COPIED_TOAST
    } else {
        COPY_FAILED_TOAST
    });
}

pub fn token_actions_label(name: &str) -> String {
    format!("{name} actions")
}

fn meta_line(token: &AuthToken, this_device: bool, colors: &Palette) -> impl View<SettingsState> {
    let theme = active_theme();
    let (label, ink) = if this_device {
        (
            format!("{THIS_DEVICE_PREFIX}{}", token.created_label()),
            colors.amber,
        )
    } else {
        (token.created_label(), colors.fg_dim)
    };
    text(label).style(inked(&theme.type_scale.label_small, ink))
}

fn token_item(row: &TokenRow, colors: &Palette) -> AnyView<SettingsState> // erasure: keep accumulator into Vec<AnyView>
{
    let theme = active_theme();
    let name = row.token.name.clone();
    let anchor = Rc::new(Cell::new(Rect::ZERO));
    let at = Rc::clone(&anchor);
    let more = icon_button(
        icons::MORE_HORIZ,
        token_actions_label(&name),
        move |state: &mut SettingsState| pages::open_token_menu(state, &name, at.get()),
    )
    .ink(colors.fg_dim)
    .icon_size(MORE_MARK)
    .box_size(Size::new(MORE_BOX, MORE_BOX))
    .radius(theme.shape.small);
    let name_line =
        text(row.token.name.clone()).style(inked(&theme.type_scale.body_small, colors.fg));
    any(token_row(
        name_line,
        meta_line(&row.token, row.this_device, colors),
        row.token.scope,
        AnchorProbe {
            child: more,
            anchor,
        },
        None,
        colors,
    ))
}

/// `token` is the scramble's own snapshot. The ⋯ stays as a handlerless stand-in, so the row
/// neither jumps when the animation starts nor offers a second revoke.
fn scrambling_token_item(
    token: &AuthToken,
    this_device: bool,
    pose: &ScramblePose,
    colors: &Palette,
) -> AnyView<SettingsState> // erasure: keep accumulator into Vec<AnyView>
{
    let theme = active_theme();
    let ink = if pose.glitching {
        colors.error
    } else {
        colors.fg
    };
    let name_line = text(pose.display.clone()).style(inked(&theme.type_scale.body_small, ink));
    let more = icon_button(
        icons::MORE_HORIZ,
        token_actions_label(&token.name),
        |_state: &mut SettingsState| {},
    )
    .ink(colors.fg_faintest)
    .icon_size(MORE_MARK)
    .box_size(Size::new(MORE_BOX, MORE_BOX))
    .radius(theme.shape.small);
    // The ticker rides inside the row it drives, so the scroller's paint
    // culling can never leave one running without the other.
    let ticker = pose.running.then(scramble::frame_ticker);
    let row = token_row(
        name_line,
        meta_line(token, this_device, colors),
        token.scope,
        more,
        ticker,
        colors,
    );
    any(scramble::collapsing(pose.ratio, row))
}

fn token_row(
    name_line: impl View<SettingsState>,
    meta: impl View<SettingsState>,
    scope: TokenScope,
    more: impl View<SettingsState>,
    tail: Option<AnyView<SettingsState>>,
    colors: &Palette,
) -> impl View<SettingsState> {
    let lines = column()
        .child(name_line)
        .child(meta)
        .gap(TOKEN_LINE_GAP)
        .cross_axis(CrossAxisAlignment::Start);
    let line = centered_row(vec![
        flexible(1, lines),
        inflexible(scope_chip(scope, colors)),
        inflexible(more),
    ])
    .gap(spacing::TIGHT);
    // erasure: keep accumulator into Vec<AnyView>
    let mut parts = vec![
        any(
            SizedBox::<SettingsState>(None, Some(ROW_HEIGHT - HAIRLINE)).child(Padding(
                EdgeInsets {
                    left: ROW_PAD_X,
                    top: 0.0,
                    right: spacing::TIGHT,
                    bottom: 0.0,
                },
                line,
            )),
        ),
        any(divider(colors.rule)),
    ];
    parts.extend(tail);
    stretch_column(parts)
}

/// Hand-composed: `glyph::badge` has no cyan/info variant for the read-only scope.
fn scope_chip(scope: TokenScope, colors: &Palette) -> impl View<SettingsState> {
    let theme = active_theme();
    let (fill, ink, border) = match scope {
        TokenScope::ReadWrite => (colors.raised, colors.fg_muted, Some(colors.outline)),
        TokenScope::ReadOnly => (colors.cyan_faint, colors.cyan, None),
    };
    let mut chip = container(Padding(
        EdgeInsets::symmetric(CHIP_PAD_X, CHIP_PAD_Y),
        text(scope.label()).style(inked(&theme.type_scale.label_small, ink)),
    ))
    .fill(fill)
    .radius(RADIUS_FULL);
    if let Some(border) = border {
        chip = chip.border(border, HAIRLINE);
    }
    chip
}

/// A token whose refresh already removed it keeps its scramble row at its old index (clamped),
/// orphans in ascending index, so nothing under it jumps while the collapse plays out.
fn token_rows(frame: &TokenFrame, colors: &Palette) -> Vec<AnyView<SettingsState>> {
    let animating = |name: &str| {
        frame
            .showing
            .iter()
            .find(|(entry, _)| entry.token.name == name)
    };
    let mut rows: Vec<AnyView<SettingsState>> = frame
        .view
        .rows
        .iter()
        .map(|row| match animating(&row.token.name) {
            Some((entry, pose)) => {
                scrambling_token_item(&entry.token, row.this_device, pose, colors)
            }
            None => token_item(row, colors),
        })
        .collect();

    let mut orphans: Vec<_> = frame
        .showing
        .iter()
        .filter(|(entry, _)| {
            !frame
                .view
                .rows
                .iter()
                .any(|row| row.token.name == entry.token.name)
        })
        .collect();
    orphans.sort_by_key(|(entry, _)| entry.index);
    for (entry, pose) in orphans {
        let at = entry.index.min(rows.len());
        let this_device = frame.device_token.as_deref() == Some(entry.token.name.as_str());
        rows.insert(
            at,
            scrambling_token_item(&entry.token, this_device, pose, colors),
        );
    }
    rows
}

/// The filter field and the scope segments, which stay above the rows.
fn token_list_header(frame: &TokenFrame) -> Vec<AnyView<SettingsState>> {
    let field = text_input(frame.query.clone(), pages::set_token_query)
        .placeholder(FILTER_PLACEHOLDER)
        .submit_on_enter(false);
    let labels = ScopeFilter::SEGMENTS
        .iter()
        .zip(frame.view.counts)
        .map(|(segment, count)| format!("{} {count}", segment.label()))
        .collect();
    let segments = segmented_control(
        labels,
        frame.segment.index(),
        |state: &mut SettingsState, index: usize| {
            if let Some(segment) = ScopeFilter::SEGMENTS.get(index) {
                pages::set_token_segment(state, *segment);
            }
        },
    );
    vec![any(Padding(
        EdgeInsets {
            left: ROW_PAD_X,
            top: FILTER_TOP,
            right: ROW_PAD_X,
            bottom: FILTER_GAP,
        },
        stretch_column((field, Align(Alignment::new(-1.0, 0.0), segments))).gap(FILTER_GAP),
    ))]
}

/// The list's load state, its empty filter result, or its `<shown> of <total>` count.
fn token_list_status(frame: &TokenFrame, colors: &Palette) -> Option<AnyView<SettingsState>> {
    let line = if frame.failed {
        TOKENS_FAILED.to_string()
    } else if !frame.loaded {
        TOKENS_LOADING.to_string()
    } else if frame.view.rows.is_empty() && frame.showing.is_empty() {
        if frame.view.total == 0 {
            return None;
        }
        NO_MATCH_LABEL.to_string()
    } else if frame.view.narrowed {
        format!("{} of {} tokens", frame.view.rows.len(), frame.view.total)
    } else {
        return None;
    };
    let theme = active_theme();
    Some(any(Padding(
        EdgeInsets::symmetric(ROW_PAD_X, STATUS_PAD),
        Align(
            Alignment::CENTER,
            text(line).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
        ),
    )))
}

fn token_list_body(frame: &TokenFrame, colors: &Palette) -> Vec<AnyView<SettingsState>> {
    let mut body = token_rows(frame, colors);
    body.extend(token_list_status(frame, colors));
    body
}

/// The page: the filter stays put while the rows scroll beneath it. The shell's bar consumes the
/// top inset, so the safe area skips its top edge.
pub(in super::super) fn tokens_phone_page(
    frame: &TokenFrame,
    wide_clamp: bool,
    colors: &Palette,
) -> impl View<SettingsState> + use<> {
    let rows = scroll_view(Padding(
        EdgeInsets {
            left: 0.0,
            top: 0.0,
            right: 0.0,
            bottom: SCROLL_PAD_BOTTOM,
        },
        FlexView::new(
            Axis::Vertical,
            token_list_body(frame, colors)
                .into_iter()
                .map(inflexible)
                .collect(),
        )
        .cross_axis(CrossAxisAlignment::Stretch),
    ));
    let mut children: Vec<_> = token_list_header(frame)
        .into_iter()
        .map(inflexible)
        .collect();
    children.push(flexible(1, rows));
    let page =
        safe_area(FlexView::new(Axis::Vertical, children).cross_axis(CrossAxisAlignment::Stretch))
            .top(false);
    if wide_clamp {
        Either::Left(Align(
            Alignment::CENTER,
            SizedBox::<SettingsState>(Some(SETTINGS_CLAMP_WIDTH), None).child(page),
        ))
    } else {
        Either::Right(page)
    }
}

/// The Full-band card: `auth tokens · <n>` with `+ new token`, then the whole list, scrolling
/// with the page.
pub(in super::super) fn tokens_card(
    frame: &TokenFrame,
    read_only: bool,
    colors: &Palette,
) -> AnyView<SettingsState> {
    let theme = active_theme();
    let title = if frame.loaded && !frame.failed {
        format!("auth tokens \u{b7} {}", frame.view.total)
    } else {
        "auth tokens".to_string()
    };
    let ink = if read_only {
        colors.fg_faintest
    } else {
        colors.amber
    };
    let new_token = GestureDetector(Padding(
        EdgeInsets {
            left: spacing::SM,
            top: spacing::SM,
            right: ROW_PAD_X,
            bottom: spacing::SM,
        },
        text(NEW_TOKEN_ACTION).style(inked(&theme.type_scale.label_small, ink)),
    ))
    .on_tap(pages::open_create_token_dialog);
    let header = centered_row(vec![
        flexible(1, group_label(&title, colors)),
        inflexible(new_token),
    ]);
    let mut list = token_list_header(frame);
    list.extend(token_list_body(frame, colors));
    any(glyph_card::<SettingsState>().title(header).desc(
        FlexView::new(Axis::Vertical, list.into_iter().map(inflexible).collect())
            .cross_axis(CrossAxisAlignment::Stretch),
    ))
}

/// A Glyph menu anchors in window space, and only paint learns where a widget landed.
struct AnchorProbe<V> {
    child: V,
    anchor: Rc<Cell<Rect>>,
}

struct AnchorProbeWidget {
    child: ChildPod,
    anchor: Rc<Cell<Rect>>,
}

impl<State: 'static, V: View<State>> View<State> for AnchorProbe<V> {
    type Element = AnchorProbeWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> AnchorProbeWidget {
        AnchorProbeWidget {
            child: build_child(&self.child, ctx),
            anchor: Rc::clone(&self.anchor),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut AnchorProbeWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // A press can land before the next paint: carry the last rect over.
        self.anchor.set(element.anchor.get());
        element.anchor = Rc::clone(&self.anchor);
        rebuild_child(&prev.child, &self.child, &mut element.child, ctx)
    }

    fn teardown(&self, element: &mut AnchorProbeWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for AnchorProbeWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::ZERO);
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.anchor
            .set(Rect::from_origin_size(ctx.origin(), self.child.size()));
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

/// `on_submit` gets the trimmed, non-empty name; every other dismissal closes without a callback.
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
                .content(
                    column()
                        .child(field)
                        .child(SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP)))
                        .child(toggle)
                        .cross_axis(CrossAxisAlignment::Stretch),
                )
                .action(
                    button(DIALOG_CANCEL_LABEL, move |_state: &mut AppState| {
                        cancel_nav.pop()
                    })
                    .style(ButtonStyle::Ghost)
                    .small(),
                )
                .action(
                    button(DIALOG_SUBMIT_LABEL, move |_state: &mut AppState| {
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
                )
        },
        move |state: &mut AppState, result: PopResult| {
            if let Some((typed, read_only)) = result.take::<(String, bool)>() {
                on_submit(state, typed, read_only);
            }
        },
    );
}

fn create_token_submittable(text: &str) -> bool {
    !text.trim().is_empty()
}

fn submit_create_token(nav: &NavigatorController<AppState>, typed: &str, read_only: bool) {
    let name = typed.trim();
    if name.is_empty() {
        return;
    }
    nav.pop_with_result(PopResult::of((name.to_string(), read_only)));
}

/// No outer `GestureDetector`: it and the toggle both capture the pointer on `Down`.
fn dialog_read_only_row(
    on: bool,
    colors: &Palette,
    on_toggle: impl Fn(&mut AppState) + 'static,
) -> impl View<AppState> {
    let theme = active_theme();
    let toggle_view = toggle::<AppState, _>(on, move |state: &mut AppState, _requested: bool| {
        on_toggle(state)
    })
    .label(READ_ONLY_TOGGLE_LABEL);
    centered_row(vec![
        flexible(
            1,
            text(READ_ONLY_TOGGLE_LABEL).style(inked(&theme.type_scale.label_large, colors.fg)),
        ),
        inflexible(toggle_view),
    ])
}

/// `minted` is `None` only when the pending secret was cleared before this body re-ran.
pub(in super::super) fn secret_reveal_sheet<D>(
    minted: Option<&MintedToken>,
    colors: &Palette,
    on_done: D,
) -> impl View<AppState> + use<D>
where
    D: Fn(&mut AppState) + 'static,
{
    let theme = active_theme();
    let Some(minted) = minted else {
        return Either::Left(
            text(SECRET_ALREADY_DISMISSED)
                .style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        );
    };
    // The rendered secret is a plain `String` outside `Zeroizing`'s scrub-on-drop for the rest
    // of this build call: the shaped glyph run and the clipboard write both need one.
    let secret = minted.token.to_string();
    let copy_secret = secret.clone();
    Either::Right(stretch_column((
        text(SECRET_REVEAL_TITLE).style(inked(&theme.type_scale.title_small_emphasized, colors.fg)),
        SizedBox::<AppState>(None, Some(PICK_TITLE_GAP)),
        text(minted.name.clone()).style(inked(&theme.type_scale.body_small, colors.fg_dim)),
        SizedBox::<AppState>(None, Some(ROW_LINE_GAP)),
        container(Padding(
            EdgeInsets::all(ROW_PAD_X),
            text(secret).style(inked(&theme.type_scale.title_small, colors.fg)),
        ))
        .fill(colors.raised)
        .radius(theme.shape.small),
        SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP)),
        button(COPY_SECRET_LABEL, move |_state: &mut AppState| {
            let copied = frust_clipboard::Clipboard::set_text_sensitive(&copy_secret).is_ok();
            app_toasts().show(if copied {
                COPIED_TOAST
            } else {
                COPY_FAILED_TOAST
            });
        })
        .style(ButtonStyle::Primary)
        .small(),
        SizedBox::<AppState>(None, Some(TOKEN_LINE_GAP)),
        text(SECRET_CAPTION).style(inked(&theme.type_scale.label_small, colors.fg_dim)),
        SizedBox::<AppState>(None, Some(DIALOG_FIELD_GAP)),
        button(SECRET_DONE_LABEL, move |state: &mut AppState| {
            on_done(state)
        })
        .style(ButtonStyle::Ghost)
        .small(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_menu_index_decodes_to_the_entry_it_names() {
        let entries = token_menu();
        for choice in [TokenMenu::CopyName, TokenMenu::Revoke] {
            assert!(matches!(entries[choice.index()], MenuEntry::Item(_)));
            assert_eq!(TokenMenu::from_index(choice.index()), Some(choice));
        }
        assert_eq!(
            TokenMenu::from_index(1),
            None,
            "the separator chooses nothing"
        );
    }
}
