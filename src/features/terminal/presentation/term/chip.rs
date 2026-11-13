//! The Copy chip: a pill with `Copy` and dismiss where a selection drag released. It is a
//! full-bleed sibling above the gesture-wrapped grid, so a press on it never reaches the grid
//! and its clamp box is the terminal rect; it reads the [`SelectionSeam`] on every pass.

use std::rc::Rc;

use frust::authoring::text::{TextContext, TextLayout, TextStyle};
use frust::authoring::{
    Action, Affine, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, Color, EventCtx,
    EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, PointerPhase, Rect, Role,
    SemanticsCtx, Size, Vec2, View, Widget,
};
use frust::{IconSource, icons};

use super::SelectionSeam;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::toasts::app_toasts;

// Pill geometry, in logical px.

pub const CHIP_W: f64 = 132.0;
pub const CHIP_H: f64 = 40.0;
pub const CHIP_OFFSET_X: f64 = 48.0;
pub const CHIP_OFFSET_Y: f64 = 56.0;
pub const CHIP_MARGIN: f64 = 8.0;

const CHIP_RADIUS: f64 = CHIP_H / 2.0;
const CHIP_PAD_X: f64 = 14.0;
const COPY_ICON: f64 = 16.0;
const ICON_LABEL_GAP: f64 = 8.0;
const LABEL_SIZE: f32 = 13.0;
/// The split between the two press targets, from the pill's left edge.
const DIVIDER_X: f64 = 92.0;
const DIVIDER_W: f64 = 1.0;
const DIVIDER_H: f64 = 20.0;
const CLOSE_ICON: f64 = 14.0;
const BORDER_W: f64 = 1.0;
const SHADOW_BLUR: f64 = 8.0;
const SHADOW_ALPHA: f32 = 0.45;

pub const COPIED_TOAST: &str = "Copied to clipboard";
pub const COPY_FAILED_TOAST: &str = "Couldn't copy to clipboard";

/// Answers whether the clipboard write landed.
pub type CopySink = Rc<dyn Fn(&str) -> bool>;

/// `set_text_sensitive`, never `set_text`: terminal output is token-bearing, and the sensitive
/// write suppresses Android 13+'s copied-text preview and iOS Universal Clipboard.
pub fn system_clipboard_sink() -> CopySink {
    Rc::new(
        |text: &str| match frust_clipboard::Clipboard::set_text_sensitive(text) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("terminal copy failed: {error}");
                false
            }
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChipTarget {
    Copy,
    Dismiss,
}

/// `anchor` is widget-local px; `max` after `min` so a rect too small for the pill pins it
/// top-left rather than inverting the range.
pub fn chip_rect(anchor: Point, bounds: Size) -> Rect {
    let x = (anchor.x - CHIP_OFFSET_X)
        .min(bounds.width - CHIP_MARGIN - CHIP_W)
        .max(CHIP_MARGIN);
    let y = (anchor.y - CHIP_OFFSET_Y)
        .min(bounds.height - CHIP_MARGIN - CHIP_H)
        .max(CHIP_MARGIN);
    Rect::new(x, y, x + CHIP_W, y + CHIP_H)
}

pub struct CopyChipView {
    seam: SelectionSeam,
    sink: CopySink,
    fill: Color,
    border: Color,
    ink: Color,
    muted: Color,
    label: TextStyle,
}

/// Mount as the top child of [`terminal_surface`](super::terminal_surface).
pub fn copy_chip(seam: SelectionSeam) -> CopyChipView {
    let theme = active_theme();
    let colors = Palette::from_theme(&theme);
    let mut label = theme.type_scale.label_medium.clone();
    label.size = LABEL_SIZE;
    label.color = colors.fg;
    CopyChipView {
        seam,
        sink: system_clipboard_sink(),
        fill: colors.raised,
        border: colors.outline,
        ink: colors.fg,
        muted: colors.fg_muted,
        label,
    }
}

impl CopyChipView {
    #[must_use]
    pub fn copy_sink(mut self, sink: CopySink) -> Self {
        self.sink = sink;
        self
    }
}

pub struct CopyChipWidget {
    seam: SelectionSeam,
    sink: CopySink,
    fill: Color,
    border: Color,
    ink: Color,
    muted: Color,
    label_style: TextStyle,
    /// Cached across frames: `paint` cannot shape.
    label: Option<TextLayout>,
    copy_mark: BezPath,
    close_mark: BezPath,
    pressed: Option<ChipTarget>,
    copies: u32,
}

/// Panics on a malformed icon constant (a wiring bug, not a runtime condition).
fn mark(source: IconSource, size: f64) -> BezPath {
    let path = BezPath::from_svg(source.d)
        .unwrap_or_else(|e| panic!("chip icon path failed to parse (wiring bug): {e}"));
    Affine::scale(size / source.design) * path
}

impl<State: 'static> View<State> for CopyChipView {
    type Element = CopyChipWidget;

    fn build(&self, _ctx: &mut BuildCtx<'_>) -> CopyChipWidget {
        CopyChipWidget {
            seam: self.seam.clone(),
            sink: Rc::clone(&self.sink),
            fill: self.fill,
            border: self.border,
            ink: self.ink,
            muted: self.muted,
            label_style: self.label.clone(),
            label: None,
            copy_mark: mark(icons::CONTENT_COPY, COPY_ICON),
            close_mark: mark(icons::CLOSE, CLOSE_ICON),
            pressed: None,
            copies: 0,
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut CopyChipWidget,
        _ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        element.seam = self.seam.clone();
        element.sink = Rc::clone(&self.sink);
        let mut flags = ChangeFlags::NONE;
        if prev.fill != self.fill
            || prev.border != self.border
            || prev.ink != self.ink
            || prev.muted != self.muted
        {
            element.fill = self.fill;
            element.border = self.border;
            element.ink = self.ink;
            element.muted = self.muted;
            flags |= ChangeFlags::PAINT;
        }
        if prev.label != self.label {
            element.label_style = self.label.clone();
            // Re-shaping happens in `layout`; a bare PAINT would replay the stale run.
            element.label = None;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        flags
    }
}

impl Widget for CopyChipWidget {
    /// Takes the whole offered box (the terminal rect the pill clamps inside); an unbounded axis
    /// collapses to zero.
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        if self.label.is_none() {
            let text_ctx = ctx.text_context::<TextContext>();
            self.label = Some(text_ctx.layout("Copy", &self.label_style, None));
        }
        let max = bc.max();
        let finite = Size::new(
            if max.width.is_finite() {
                max.width
            } else {
                0.0
            },
            if max.height.is_finite() {
                max.height
            } else {
                0.0
            },
        );
        bc.constrain(finite)
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        let Some(anchor) = self.seam.chip_anchor() else {
            return;
        };
        let origin = ctx.origin();
        let pill = chip_rect(anchor, ctx.size());
        let at = Point::new(origin.x + pill.x0, origin.y + pill.y0);
        let size = Size::new(CHIP_W, CHIP_H);

        scene.draw_shadow(
            at,
            size,
            CHIP_RADIUS,
            SHADOW_BLUR,
            Color::BLACK.with_alpha(SHADOW_ALPHA),
        );
        // No stroked rounded rect in `PaintScene`: the border is the outer pill, the fill inset.
        scene.fill_rounded_rect(at, size, CHIP_RADIUS, self.border);
        scene.fill_rounded_rect(
            Point::new(at.x + BORDER_W, at.y + BORDER_W),
            Size::new(CHIP_W - 2.0 * BORDER_W, CHIP_H - 2.0 * BORDER_W),
            CHIP_RADIUS - BORDER_W,
            self.fill,
        );

        scene.fill_path(
            Point::new(at.x + CHIP_PAD_X, at.y + (CHIP_H - COPY_ICON) / 2.0),
            &self.copy_mark,
            &Brush::Solid(self.ink),
        );
        if let Some(label) = self.label.as_ref() {
            let x = at.x + CHIP_PAD_X + COPY_ICON + ICON_LABEL_GAP;
            let y = at.y + (CHIP_H - label.size().height) / 2.0;
            for glyphs in label.to_scene_runs(Point::new(x, y)) {
                scene.draw_glyph_run(glyphs);
            }
        }
        scene.fill_rect(
            Point::new(at.x + DIVIDER_X, at.y + (CHIP_H - DIVIDER_H) / 2.0),
            Size::new(DIVIDER_W, DIVIDER_H),
            self.border,
        );
        let close_x =
            at.x + DIVIDER_X + DIVIDER_W + (CHIP_W - DIVIDER_X - DIVIDER_W - CLOSE_ICON) / 2.0;
        scene.fill_path(
            Point::new(close_x, at.y + (CHIP_H - CLOSE_ICON) / 2.0),
            &self.close_mark,
            &Brush::Solid(self.muted),
        );

        // A wash, not a scale: half a pill cannot scale about its centre without detaching.
        if let Some(target) = self.pressed {
            let (x, width) = match target {
                ChipTarget::Copy => (at.x, DIVIDER_X),
                ChipTarget::Dismiss => (at.x + DIVIDER_X, CHIP_W - DIVIDER_X),
            };
            scene.fill_rect(
                Point::new(x, at.y),
                Size::new(width, CHIP_H),
                self.ink.with_alpha(0.08),
            );
        }
    }

    /// Hit-tests the pill only; everything else falls through to the grid on `Ignored`.
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(pointer) = event else {
            return EventResult::Ignored;
        };
        let Some(anchor) = self.seam.chip_anchor() else {
            return EventResult::Ignored;
        };
        let pill = chip_rect(anchor, ctx.size());
        let target = target_at(pointer.position, pill);
        match pointer.phase {
            PointerPhase::Down => {
                let Some(target) = target else {
                    return EventResult::Ignored;
                };
                self.pressed = Some(target);
                ctx.capture_pointer();
                ctx.request_redraw();
                EventResult::Handled
            }
            PointerPhase::Move => {
                if self.pressed.is_none() {
                    return EventResult::Ignored;
                }
                let still = self.pressed.filter(|armed| target == Some(*armed));
                if still != self.pressed {
                    self.pressed = still;
                    ctx.request_redraw();
                }
                EventResult::Handled
            }
            PointerPhase::Up => {
                let Some(armed) = self.pressed.take() else {
                    return EventResult::Ignored;
                };
                ctx.request_redraw();
                if target != Some(armed) {
                    return EventResult::Handled;
                }
                match armed {
                    ChipTarget::Copy => self.copy(),
                    ChipTarget::Dismiss => self.seam.clear(),
                }
                EventResult::Handled
            }
            // A `Cancel` never touches app state, so the selection survives a stolen gesture.
            PointerPhase::Cancel => {
                if self.pressed.take().is_some() {
                    ctx.request_redraw();
                }
                EventResult::Ignored
            }
        }
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        let Some(anchor) = self.seam.chip_anchor() else {
            return;
        };
        // Per-node bounds: the widget is full-bleed but its press targets are the pill's halves.
        let origin = ctx.origin();
        let pill = chip_rect(anchor, ctx.size()) + Vec2::new(origin.x, origin.y);
        for (role, label, rect) in [
            (
                Role::Button,
                "Copy",
                Rect::new(pill.x0, pill.y0, pill.x0 + DIVIDER_X, pill.y1),
            ),
            (
                Role::Button,
                "Dismiss selection",
                Rect::new(pill.x0 + DIVIDER_X, pill.y0, pill.x1, pill.y1),
            ),
        ] {
            ctx.push_node(role, |node| {
                node.set_label(label);
                node.add_action(Action::Click);
                node.set_bounds(frust::accesskit::Rect {
                    x0: rect.x0,
                    y0: rect.y0,
                    x1: rect.x1,
                    y1: rect.y1,
                });
            });
        }
    }
}

impl CopyChipWidget {
    /// Clears the selection whether or not the write landed; the toast tells the two apart.
    fn copy(&mut self) {
        let text = self.seam.text();
        let copied = (self.sink)(&text);
        self.copies += 1;
        self.seam.clear();
        app_toasts().show(if copied {
            COPIED_TOAST
        } else {
            COPY_FAILED_TOAST
        });
    }

    pub fn copies(&self) -> u32 {
        self.copies
    }
}

fn target_at(position: Point, pill: Rect) -> Option<ChipTarget> {
    if position.x < pill.x0 || position.x >= pill.x1 {
        return None;
    }
    if position.y < pill.y0 || position.y >= pill.y1 {
        return None;
    }
    Some(if position.x - pill.x0 < DIVIDER_X {
        ChipTarget::Copy
    } else {
        ChipTarget::Dismiss
    })
}

#[cfg(test)]
mod tests {
    use frust::authoring::Shape;

    use super::*;

    const BOX: Size = Size::new(400.0, 800.0);

    #[test]
    fn the_pill_sits_up_and_left_of_the_release_point() {
        let pill = chip_rect(Point::new(200.0, 300.0), BOX);
        assert_eq!(pill.x0, 200.0 - CHIP_OFFSET_X);
        assert_eq!(pill.y0, 300.0 - CHIP_OFFSET_Y);
        assert_eq!(pill.width(), CHIP_W);
        assert_eq!(pill.height(), CHIP_H);
    }

    #[test]
    fn the_pill_clamps_into_the_terminal_rect_at_every_corner() {
        let top_left = chip_rect(Point::new(4.0, 4.0), BOX);
        assert_eq!((top_left.x0, top_left.y0), (CHIP_MARGIN, CHIP_MARGIN));
        // The release is past the rect's corner, so both axes hit the clamp.
        let bottom_right = chip_rect(Point::new(BOX.width + 40.0, BOX.height + 40.0), BOX);
        assert_eq!(bottom_right.x0, BOX.width - 140.0);
        assert_eq!(bottom_right.y0, BOX.height - 48.0);
        assert_eq!(bottom_right.x1, BOX.width - CHIP_MARGIN);
        assert_eq!(bottom_right.y1, BOX.height - CHIP_MARGIN);
    }

    #[test]
    fn a_box_too_small_for_the_pill_pins_it_rather_than_inverting_the_clamp() {
        let pill = chip_rect(Point::new(10.0, 10.0), Size::new(40.0, 20.0));
        assert_eq!((pill.x0, pill.y0), (CHIP_MARGIN, CHIP_MARGIN));
    }

    #[test]
    fn the_divider_is_the_split_between_the_two_press_targets() {
        let pill = chip_rect(Point::new(200.0, 300.0), BOX);
        assert_eq!(
            target_at(Point::new(pill.x0 + 1.0, pill.y0 + 1.0), pill),
            Some(ChipTarget::Copy)
        );
        assert_eq!(
            target_at(Point::new(pill.x0 + DIVIDER_X - 0.5, pill.y0 + 20.0), pill),
            Some(ChipTarget::Copy)
        );
        assert_eq!(
            target_at(Point::new(pill.x0 + DIVIDER_X, pill.y0 + 20.0), pill),
            Some(ChipTarget::Dismiss)
        );
        assert_eq!(
            target_at(Point::new(pill.x1 - 0.5, pill.y1 - 0.5), pill),
            Some(ChipTarget::Dismiss)
        );
        assert_eq!(target_at(Point::new(pill.x0 - 1.0, pill.y0), pill), None);
        assert_eq!(target_at(Point::new(pill.x0, pill.y1), pill), None);
    }

    #[test]
    fn both_marks_are_vectors_that_parse_inside_their_design_box() {
        for (name, source) in [
            ("CONTENT_COPY", icons::CONTENT_COPY),
            ("CLOSE", icons::CLOSE),
        ] {
            let path = BezPath::from_svg(source.d).unwrap_or_else(|e| panic!("{name}: {e}"));
            let bounds = path.bounding_box();
            assert!(bounds.x0 >= 0.0 && bounds.y0 >= 0.0, "{name}: {bounds:?}");
            assert!(
                bounds.x1 <= source.design && bounds.y1 <= source.design,
                "{name}: {bounds:?}"
            );
        }
    }
}
