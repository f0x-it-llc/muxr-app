//! The picker sheet, over `AppState` because a sheet is a pushed page on the app's overlay host.

use std::rc::Rc;

use frust::authoring::{
    BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, EventCtx, EventResult, InputEvent,
    LayoutCtx, PaintCtx, PaintScene, Point, SemanticsCtx, Size, View, Widget, build_child,
    rebuild_child, route_event_single, teardown_child,
};
use frust::icons::CHECK;
use frust::{
    EdgeInsets, Either, GestureDetector, Padding, SizedBox, ViewSeq, flexible, icon, inflexible,
    scroll_view, text,
};

use crate::AppState;
use crate::ui::palette::{Palette, active_theme, inked};
use crate::ui::spacing;

use super::rows::{ROW_GAP, centered_row, h_gap, rounded_fill, stretch_column};

/// `.pick-row{padding:13px 4px}`.
pub(super) const PICK_PAD_X: f64 = spacing::TIGHT;
pub(super) const PICK_PAD_Y: f64 = 13.0;
/// `.pick-row.sel{margin:0 -20px}`; must equal `crate::ui::sheet`'s private `PANEL_PAD`.
pub(super) const PICK_BLEED: f64 = spacing::XXL;
/// `.sheet-toprow{margin-bottom:18px}` — the picker title to its first row.
pub(super) const PICK_TITLE_GAP: f64 = 18.0;
pub(super) const CHECK_SIZE: f64 = 13.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in super::super) enum PickerKind {
    FontFamily,
    FontSize,
    CursorStyle,
    LayoutRefresh,
}

impl PickerKind {
    pub(in super::super) fn title(self) -> &'static str {
        match self {
            PickerKind::FontFamily => "font family",
            PickerKind::FontSize => "font size",
            PickerKind::CursorStyle => "cursor style",
            PickerKind::LayoutRefresh => "layout refresh",
        }
    }
}

pub(in super::super) struct Picker {
    pub(in super::super) title: &'static str,
    pub(in super::super) options: Vec<String>,
    pub(in super::super) selected: Option<usize>,
    /// `None` leaves the list unbounded.
    pub(in super::super) max_height: Option<f64>,
}

type OnPick = Rc<dyn Fn(&mut AppState, usize)>;

pub(in super::super) fn picker_sheet<P>(
    picker: &Picker,
    colors: &Palette,
    on_pick: P,
) -> impl View<AppState> + use<P>
where
    P: Fn(&mut AppState, usize) + 'static,
{
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
        // A fixed height, not a flex share: the sheet body is unbounded in height.
        Some(height) => Either::Left(SizedBox::<AppState>(None, Some(height)).child(scroller)),
        None => Either::Right(scroller),
    };
    stretch_column((
        text(picker.title).style(inked(&theme.type_scale.title_small_emphasized, colors.fg)),
        SizedBox::<AppState>(None, Some(PICK_TITLE_GAP)),
        list,
    ))
}

/// The viewport is [`PICK_BLEED`] wider on each side and the content padded back in, so the
/// selected row's bleed lies inside the scroll clip.
pub(super) fn bleeding_scroll<M>(children: impl ViewSeq<AppState, M>) -> impl View<AppState> {
    WidenedView {
        child: scroll_view(Padding(
            EdgeInsets::symmetric(PICK_BLEED, 0.0),
            stretch_column(children),
        )),
        by: PICK_BLEED,
    }
}

/// Lays its child out `by` wider on each side, since frust's `Padding` clamps a negative
/// inset; reports its own unwidened width.
struct WidenedView<V> {
    child: V,
    by: f64,
}

struct WidenedWidget {
    child: ChildPod,
    by: f64,
}

impl<State: 'static, V: View<State>> View<State> for WidenedView<V> {
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
        // Must equal `ui::sheet`'s private `PANEL_PAD`; `tests/settings.rs` checks the geometry.
        assert_eq!(PICK_BLEED, 20.0);
    }
}
