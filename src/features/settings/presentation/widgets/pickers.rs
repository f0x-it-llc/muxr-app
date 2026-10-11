//! Screen 6's picker sheet: one title over one `.pick-row` per option, the
//! selected one filled amber full-bleed with a `✓`. Reused by four rows
//! (`layout refresh`, `font size`, `cursor style`, and — through
//! `widgets::font_picker`'s own two-section variant — `font family`), which
//! is the whole point of the redesign's own note that this pattern is shared.
//!
//! Built over `AppState`, not `SettingsState`: a sheet is a pushed navigator
//! page on the *app's* overlay host, the same component boundary the dialogs
//! cross.

use std::rc::Rc;

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult, InputEvent,
    LayoutCtx, PaintCtx, PaintScene, Point, SemanticsCtx, Size, View, Widget, build_child,
    rebuild_child, route_event_single, teardown_child,
};
use frust::icons::CHECK;
use frust::{
    AnyView, EdgeInsets, Either, GestureDetector, Padding, SizedBox, ViewSeq, any, flexible, icon,
    inflexible, scroll_view, text,
};

use crate::AppState;
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::rows::{ROW_GAP, centered_row, h_gap, rounded_fill, stretch_column};

/// `.pick-row{padding:13px 4px}`.
pub(super) const PICK_PAD_X: f64 = spacing::TIGHT;
pub(super) const PICK_PAD_Y: f64 = 13.0;
/// `.pick-row.sel{margin:0 -20px}` — how far the selected row's fill escapes
/// the sheet's own content gutter (`crate::ui::sheet`'s private `PANEL_PAD`,
/// which this must equal for the fill to reach the panel's edges).
pub(super) const PICK_BLEED: f64 = spacing::XXL;
/// `.sheet-toprow{margin-bottom:18px}` — the picker title to its first row.
pub(super) const PICK_TITLE_GAP: f64 = 18.0;
/// The `✓` on the selected picker row.
pub(super) const CHECK_SIZE: f64 = 13.0;

/// Which picker sheet a row opens. One enum, four sheets, **one** builder.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in super::super) enum PickerKind {
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
    /// The sheet's own title — the row's label, verbatim.
    pub(in super::super) fn title(self) -> &'static str {
        match self {
            PickerKind::FontFamily => "font family",
            PickerKind::FontSize => "font size",
            PickerKind::CursorStyle => "cursor style",
            PickerKind::LayoutRefresh => "layout refresh",
        }
    }
}

/// One picker sheet's content: a title, and the options it offers with at
/// most one already chosen. Built by `pages` from the controller's signals;
/// [`picker_sheet`] is the pure render.
pub(in super::super) struct Picker {
    pub(in super::super) title: &'static str,
    /// Every option, in the order the domain declares them.
    pub(in super::super) options: Vec<String>,
    /// Which one is current, if the stored value is still an offered choice.
    pub(in super::super) selected: Option<usize>,
    /// The option list's scroll-band height cap, from the window's own
    /// metrics. `None` (every headless suite) leaves the list unbounded.
    pub(in super::super) max_height: Option<f64>,
}

/// The pick handler, shared by every row of one sheet.
type OnPick = Rc<dyn Fn(&mut AppState, usize)>;

pub(in super::super) fn picker_sheet(
    picker: &Picker,
    colors: &Palette,
    on_pick: impl Fn(&mut AppState, usize) + 'static,
) -> impl View<AppState> {
    let theme = active_theme();
    let on_pick: OnPick = Rc::new(on_pick);
    let last = picker.options.len().saturating_sub(1);
    let mut children = Vec::new();
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
    let scroller = bleeding_scroll(children);
    let list = match picker.max_height {
        // A fixed height, not a flex share: the sheet body is unbounded in
        // height, so a `flexible` child would divide zero free space.
        Some(height) => Either::Left(SizedBox::<AppState>(None, Some(height)).child(scroller)),
        None => Either::Right(scroller),
    };
    stretch_column((
        text(picker.title).style(inked(&theme.type_scale.title_small_emphasized, colors.fg)),
        SizedBox::<AppState>(None, Some(PICK_TITLE_GAP)),
        list,
    ))
}

/// `children` in a vertical scroll region whose *viewport* is [`PICK_BLEED`]
/// wider than the sheet's content gutter on each side, with the content padded
/// back in by the same amount: rows lay out exactly as before, but the
/// selected row's painted bleed now lies inside the viewport's clip.
pub(super) fn bleeding_scroll<M>(children: impl ViewSeq<AppState, M>) -> impl View<AppState> {
    let scroller = scroll_view(Padding(
        EdgeInsets::symmetric(PICK_BLEED, 0.0),
        stretch_column(children),
    ));
    WidenedView {
        child: any(scroller), // erasure: keep ChildPod-fed
        by: PICK_BLEED,
    }
}

/// Lays its child out `by` wider than itself on each side — frust's `Padding`
/// clamps a negative inset, so this is the one way to give a clipping child
/// the sheet gutter back. Reports its own (unwidened) width to the parent.
struct WidenedView<State: 'static> {
    child: AnyView<State>,
    by: f64,
}

struct WidenedWidget {
    child: ChildPod,
    by: f64,
}

impl<State: 'static> View<State> for WidenedView<State> {
    type Element = WidenedWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> WidenedWidget {
        WidenedWidget {
            child: build_child(&self.child, ctx),
            by: self.by,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut WidenedWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.by = self.by;
        rebuild_child(&prev.child, &self.child, &mut element.child, ctx)
    }

    fn teardown(&self, element: &mut WidenedWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

impl Widget for WidenedWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let width = bc.max().width;
        // An unbounded width has no edge to widen past.
        let by = if width.is_finite() { self.by } else { 0.0 };
        let wide = width + 2.0 * by;
        let child_bc = BoxConstraints::new(
            Size::new(wide, bc.min().height),
            Size::new(wide, bc.max().height),
        );
        let size = self.child.layout_child(ctx, &child_bc);
        self.child.set_origin(Point::new(-by, 0.0));
        bc.constrain(Size::new(width, size.height))
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.child.paint_child(ctx, scene);
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        route_event_single(&mut self.child, ctx, event)
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        self.child.semantics_child(ctx);
    }
}

/// One `.pick-row`. The selected one fills `--amber-faint` past the sheet's
/// own gutter (reproduced via [`super::rows::rounded_fill`]'s bleed, since
/// frust has no negative padding), inks its label amber, and carries the
/// `CHECK` vector; every row but the last closes with a hairline.
fn pick_row(
    option: &str,
    selected: bool,
    ruled: bool,
    colors: &Palette,
    on_tap: impl Fn(&mut AppState) + 'static,
) -> impl View<AppState> {
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
    let mut box_ = rounded_fill(
        Padding(
            EdgeInsets::symmetric(PICK_PAD_X, PICK_PAD_Y),
            centered_row(children),
        ),
        fill,
        theme.shape.small,
    );
    if selected {
        box_ = box_.bleed(PICK_BLEED);
    }
    if ruled {
        box_ = box_.bottom_rule(colors.rule);
    }
    GestureDetector(box_).on_tap(on_tap)
}

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
    fn the_selected_rows_bleed_is_the_sheets_own_gutter() {
        // Transcribed rather than asserted equal to `ui::sheet`'s private
        // `PANEL_PAD`: if the two ever diverge the amber fill stops short of
        // (or runs past) the sheet's edges — `tests/settings.rs` asserts the
        // measured geometry end to end, which is what actually catches it.
        assert_eq!(PICK_BLEED, 20.0);
    }
}
