//! The selection chip: a pill with `Copy` and/or `Paste` and dismiss where a long press released.
//! It is a full-bleed sibling above the gesture-wrapped grid, so a press on it never reaches the
//! grid and its clamp box is the terminal rect; it reads the [`SelectionSeam`] on every pass.

use std::rc::Rc;

use frust::authoring::text::{TextContext, TextLayout, TextStyle};
use frust::authoring::{
    Action, Affine, BezPath, BoxConstraints, Brush, BuildCtx, ChangeFlags, Color, EventCtx,
    EventResult, InputEvent, LayoutCtx, PaintCtx, PaintScene, Point, PointerPhase, Rect, Role,
    SemanticsCtx, Size, Vec2, View, Widget,
};
use frust::{IconSource, icons};
use zeroize::Zeroizing;

use super::SelectionSeam;
use crate::ui::palette::{Palette, active_theme};
use crate::ui::toasts::app_toasts;

// Pill geometry, in logical px.

/// One Copy or Paste press target.
pub const ACTION_W: f64 = 92.0;
pub const DISMISS_W: f64 = 40.0;
/// The widest pill: Copy, Paste and dismiss.
pub const CHIP_W: f64 = 2.0 * ACTION_W + DISMISS_W;
pub const CHIP_H: f64 = 40.0;
pub const CHIP_OFFSET_X: f64 = 48.0;
pub const CHIP_OFFSET_Y: f64 = 56.0;
pub const CHIP_MARGIN: f64 = 8.0;

const CHIP_RADIUS: f64 = CHIP_H / 2.0;
const CHIP_PAD_X: f64 = 14.0;
const ACTION_ICON: f64 = 16.0;
const ICON_LABEL_GAP: f64 = 8.0;
const LABEL_SIZE: f32 = 13.0;
const DIVIDER_W: f64 = 1.0;
const DIVIDER_H: f64 = 20.0;
const CLOSE_ICON: f64 = 14.0;
const BORDER_W: f64 = 1.0;
const SHADOW_BLUR: f64 = 8.0;
const SHADOW_ALPHA: f32 = 0.45;

/// A clipboard board with its clip, outline only; the hole winds opposite to the outer edge.
pub const CONTENT_PASTE: IconSource = IconSource {
    d: "M 5 3 H 8 V 2 Q 8 1 9 1 H 15 Q 16 1 16 2 V 3 H 19 Q 20 3 20 4 V 21 Q 20 22 19 22 H 5 \
        Q 4 22 4 21 V 4 Q 4 3 5 3 Z M 6 5 V 20 H 18 V 5 H 16 V 7 H 8 V 5 Z",
    design: 24.0,
};

pub const COPIED_TOAST: &str = "Copied to clipboard";
pub const COPY_FAILED_TOAST: &str = "Couldn't copy to clipboard";

/// Answers whether the clipboard write landed.
pub type CopySink = Rc<dyn Fn(&str) -> bool>;

/// `None` for an empty or unreadable clipboard.
pub type PasteSource = Rc<dyn Fn() -> Option<String>>;

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

pub fn system_clipboard_source() -> PasteSource {
    Rc::new(|| match frust_clipboard::Clipboard::get_text() {
        Ok(text) => text.filter(|text| !text.is_empty()),
        Err(error) => {
            log::warn!("terminal paste failed: {error}");
            None
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChipTarget {
    Copy,
    Paste,
    Dismiss,
}

pub fn chip_width(copy: bool, paste: bool) -> f64 {
    ACTION_W * f64::from(u8::from(copy) + u8::from(paste)) + DISMISS_W
}

/// `anchor` is widget-local px; `max` after `min` so a rect too small for the pill pins it
/// top-left rather than inverting the range.
pub fn chip_rect(anchor: Point, bounds: Size, width: f64) -> Rect {
    let x = (anchor.x - CHIP_OFFSET_X)
        .min(bounds.width - CHIP_MARGIN - width)
        .max(CHIP_MARGIN);
    let y = (anchor.y - CHIP_OFFSET_Y)
        .min(bounds.height - CHIP_MARGIN - CHIP_H)
        .max(CHIP_MARGIN);
    Rect::new(x, y, x + width, y + CHIP_H)
}

/// Left to right `(target, x offset, width)`: the offered actions, then dismiss.
fn segments(copy: bool, paste: bool) -> impl Iterator<Item = (ChipTarget, f64, f64)> {
    let mut x = 0.0;
    [(ChipTarget::Copy, copy), (ChipTarget::Paste, paste)]
        .into_iter()
        .filter_map(|(target, offered)| offered.then_some(target))
        .chain(std::iter::once(ChipTarget::Dismiss))
        .map(move |target| {
            let width = if target == ChipTarget::Dismiss {
                DISMISS_W
            } else {
                ACTION_W
            };
            let at = x;
            x += width;
            (target, at, width)
        })
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Offer {
    pill: Rect,
    copy: bool,
    paste: bool,
}

impl Offer {
    fn of(seam: &SelectionSeam, bounds: Size) -> Option<Self> {
        let anchor = seam.chip_anchor()?;
        let copy = seam.range().is_some();
        let paste = seam.can_paste();
        if !copy && !paste {
            return None;
        }
        let pill = chip_rect(anchor, bounds, chip_width(copy, paste));
        Some(Self { pill, copy, paste })
    }

    fn segments(self) -> impl Iterator<Item = (ChipTarget, Rect)> {
        let pill = self.pill;
        segments(self.copy, self.paste).map(move |(target, at, width)| {
            let x0 = pill.x0 + at;
            (target, Rect::new(x0, pill.y0, x0 + width, pill.y1))
        })
    }

    fn target_at(self, position: Point) -> Option<ChipTarget> {
        self.segments()
            .find(|(_, rect)| {
                position.x >= rect.x0
                    && position.x < rect.x1
                    && position.y >= rect.y0
                    && position.y < rect.y1
            })
            .map(|(target, _)| target)
    }
}

pub struct CopyChipView {
    seam: SelectionSeam,
    sink: CopySink,
    source: PasteSource,
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
        source: system_clipboard_source(),
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

    #[must_use]
    pub fn paste_source(mut self, source: PasteSource) -> Self {
        self.source = source;
        self
    }
}

pub struct CopyChipWidget {
    seam: SelectionSeam,
    sink: CopySink,
    source: PasteSource,
    fill: Color,
    border: Color,
    ink: Color,
    muted: Color,
    label_style: TextStyle,
    /// Cached across frames: `paint` cannot shape.
    labels: Option<(TextLayout, TextLayout)>,
    copy_mark: BezPath,
    paste_mark: BezPath,
    close_mark: BezPath,
    pressed: Option<ChipTarget>,
    copies: u32,
    pastes: u32,
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
            source: Rc::clone(&self.source),
            fill: self.fill,
            border: self.border,
            ink: self.ink,
            muted: self.muted,
            label_style: self.label.clone(),
            labels: None,
            copy_mark: mark(icons::CONTENT_COPY, ACTION_ICON),
            paste_mark: mark(CONTENT_PASTE, ACTION_ICON),
            close_mark: mark(icons::CLOSE, CLOSE_ICON),
            pressed: None,
            copies: 0,
            pastes: 0,
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
        element.source = Rc::clone(&self.source);
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
            element.labels = None;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        flags
    }
}

impl Widget for CopyChipWidget {
    /// Takes the whole offered box (the terminal rect the pill clamps inside); an unbounded axis
    /// collapses to zero.
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        if self.labels.is_none() {
            let text_ctx = ctx.text_context::<TextContext>();
            self.labels = Some((
                text_ctx.layout("Copy", &self.label_style, None),
                text_ctx.layout("Paste", &self.label_style, None),
            ));
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
        let Some(offer) = Offer::of(&self.seam, ctx.size()) else {
            return;
        };
        let origin = ctx.origin();
        let pill = offer.pill + Vec2::new(origin.x, origin.y);
        let at = Point::new(pill.x0, pill.y0);
        let size = Size::new(pill.width(), CHIP_H);

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
            Size::new(size.width - 2.0 * BORDER_W, CHIP_H - 2.0 * BORDER_W),
            CHIP_RADIUS - BORDER_W,
            self.fill,
        );

        for (target, rect) in offer.segments() {
            let rect = rect + Vec2::new(origin.x, origin.y);
            if rect.x0 > pill.x0 {
                scene.fill_rect(
                    Point::new(rect.x0, at.y + (CHIP_H - DIVIDER_H) / 2.0),
                    Size::new(DIVIDER_W, DIVIDER_H),
                    self.border,
                );
            }
            let (mark, label) = match target {
                ChipTarget::Copy => (&self.copy_mark, self.labels.as_ref().map(|l| &l.0)),
                ChipTarget::Paste => (&self.paste_mark, self.labels.as_ref().map(|l| &l.1)),
                ChipTarget::Dismiss => {
                    scene.fill_path(
                        Point::new(
                            rect.x0 + (rect.width() - CLOSE_ICON) / 2.0,
                            at.y + (CHIP_H - CLOSE_ICON) / 2.0,
                        ),
                        &self.close_mark,
                        &Brush::Solid(self.muted),
                    );
                    continue;
                }
            };
            scene.fill_path(
                Point::new(rect.x0 + CHIP_PAD_X, at.y + (CHIP_H - ACTION_ICON) / 2.0),
                mark,
                &Brush::Solid(self.ink),
            );
            if let Some(label) = label {
                let x = rect.x0 + CHIP_PAD_X + ACTION_ICON + ICON_LABEL_GAP;
                let y = at.y + (CHIP_H - label.size().height) / 2.0;
                for glyphs in label.to_scene_runs(Point::new(x, y)) {
                    scene.draw_glyph_run(glyphs);
                }
            }
        }

        // A wash, not a scale: part of a pill cannot scale about its centre without detaching.
        if let Some((_, rect)) = offer
            .segments()
            .find(|(target, _)| Some(*target) == self.pressed)
        {
            scene.fill_rect(
                Point::new(origin.x + rect.x0, at.y),
                Size::new(rect.width(), CHIP_H),
                self.ink.with_alpha(0.08),
            );
        }
    }

    /// Hit-tests the pill only; everything else falls through to the grid on `Ignored`.
    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(pointer) = event else {
            return EventResult::Ignored;
        };
        let Some(offer) = Offer::of(&self.seam, ctx.size()) else {
            return EventResult::Ignored;
        };
        let target = offer.target_at(pointer.position);
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
                    ChipTarget::Paste => self.paste(),
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
        let Some(offer) = Offer::of(&self.seam, ctx.size()) else {
            return;
        };
        // Per-node bounds: the widget is full-bleed but its press targets are the pill's parts.
        let origin = ctx.origin();
        for (target, rect) in offer.segments() {
            let rect = rect + Vec2::new(origin.x, origin.y);
            let label = match target {
                ChipTarget::Copy => "Copy",
                ChipTarget::Paste => "Paste",
                ChipTarget::Dismiss => "Dismiss selection",
            };
            ctx.push_node(Role::Button, |node| {
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

    /// The clipboard's one read. The chip dismisses either way; an empty clipboard sends nothing.
    fn paste(&mut self) {
        let allowed = self.seam.can_paste();
        self.seam.clear();
        if !allowed {
            return;
        }
        if let Some(text) = (self.source)().map(Zeroizing::new) {
            self.pastes += 1;
            let _ = self.seam.paste(&text);
        }
    }

    pub fn copies(&self) -> u32 {
        self.copies
    }

    pub fn pastes(&self) -> u32 {
        self.pastes
    }
}

#[cfg(test)]
mod tests {
    use frust::authoring::Shape;

    use super::super::{InputSeam, SharedFeed, TerminalEmulator};
    use super::*;
    use crate::features::terminal::data::feeds::demo_feed::DemoFeed;

    const BOX: Size = Size::new(400.0, 800.0);

    fn offer(copy: bool, paste: bool) -> Offer {
        Offer {
            pill: chip_rect(Point::new(200.0, 300.0), BOX, chip_width(copy, paste)),
            copy,
            paste,
        }
    }

    #[test]
    fn the_pill_sits_up_and_left_of_the_release_point() {
        let pill = chip_rect(Point::new(200.0, 300.0), BOX, CHIP_W);
        assert_eq!(pill.x0, 200.0 - CHIP_OFFSET_X);
        assert_eq!(pill.y0, 300.0 - CHIP_OFFSET_Y);
        assert_eq!(pill.width(), CHIP_W);
        assert_eq!(pill.height(), CHIP_H);
    }

    #[test]
    fn the_pill_is_one_action_wider_per_offered_action() {
        assert_eq!(chip_width(true, false), ACTION_W + DISMISS_W);
        assert_eq!(chip_width(false, true), ACTION_W + DISMISS_W);
        assert_eq!(chip_width(true, true), CHIP_W);
    }

    #[test]
    fn the_pill_clamps_into_the_terminal_rect_at_every_corner() {
        let top_left = chip_rect(Point::new(4.0, 4.0), BOX, CHIP_W);
        assert_eq!((top_left.x0, top_left.y0), (CHIP_MARGIN, CHIP_MARGIN));
        // The release is past the rect's corner, so both axes hit the clamp.
        let far = Point::new(BOX.width + 40.0, BOX.height + 40.0);
        let bottom_right = chip_rect(far, BOX, CHIP_W);
        assert_eq!(bottom_right.x0, BOX.width - CHIP_MARGIN - CHIP_W);
        assert_eq!(bottom_right.y0, BOX.height - 48.0);
        assert_eq!(bottom_right.x1, BOX.width - CHIP_MARGIN);
        assert_eq!(bottom_right.y1, BOX.height - CHIP_MARGIN);
        let narrow = chip_rect(far, BOX, chip_width(false, true));
        assert_eq!(narrow.x1, BOX.width - CHIP_MARGIN);
    }

    #[test]
    fn a_box_too_small_for_the_pill_pins_it_rather_than_inverting_the_clamp() {
        let pill = chip_rect(Point::new(10.0, 10.0), Size::new(40.0, 20.0), CHIP_W);
        assert_eq!((pill.x0, pill.y0), (CHIP_MARGIN, CHIP_MARGIN));
    }

    #[test]
    fn the_dividers_split_copy_paste_and_dismiss_in_that_order() {
        let both = offer(true, true);
        let pill = both.pill;
        let mid = pill.y0 + 20.0;
        assert_eq!(
            both.target_at(Point::new(pill.x0 + 1.0, pill.y0 + 1.0)),
            Some(ChipTarget::Copy)
        );
        assert_eq!(
            both.target_at(Point::new(pill.x0 + ACTION_W - 0.5, mid)),
            Some(ChipTarget::Copy)
        );
        assert_eq!(
            both.target_at(Point::new(pill.x0 + ACTION_W, mid)),
            Some(ChipTarget::Paste)
        );
        assert_eq!(
            both.target_at(Point::new(pill.x0 + 2.0 * ACTION_W, mid)),
            Some(ChipTarget::Dismiss)
        );
        assert_eq!(
            both.target_at(Point::new(pill.x1 - 0.5, pill.y1 - 0.5)),
            Some(ChipTarget::Dismiss)
        );
        assert_eq!(both.target_at(Point::new(pill.x0 - 1.0, pill.y0)), None);
        assert_eq!(both.target_at(Point::new(pill.x0, pill.y1)), None);
    }

    #[test]
    fn a_single_action_pill_keeps_the_action_left_of_dismiss() {
        for (copy, paste, action) in [
            (true, false, ChipTarget::Copy),
            (false, true, ChipTarget::Paste),
        ] {
            let single = offer(copy, paste);
            let pill = single.pill;
            let mid = pill.y0 + 20.0;
            assert_eq!(
                single.target_at(Point::new(pill.x0 + 1.0, mid)),
                Some(action)
            );
            assert_eq!(
                single.target_at(Point::new(pill.x0 + ACTION_W, mid)),
                Some(ChipTarget::Dismiss)
            );
            assert_eq!(single.segments().count(), 2);
        }
    }

    fn chip(source: PasteSource) -> CopyChipWidget {
        let feed: SharedFeed = Rc::new(std::cell::RefCell::new(DemoFeed::new()));
        let emulator = Rc::new(std::cell::RefCell::new(TerminalEmulator::with_defaults()));
        let seam = SelectionSeam::new(emulator);
        seam.set_paste_target(Some(InputSeam::new(feed)));
        let ink = Color::from_rgb8(0, 0, 0);
        CopyChipWidget {
            seam,
            sink: Rc::new(|_: &str| true),
            source,
            fill: ink,
            border: ink,
            ink,
            muted: ink,
            label_style: TextStyle::default(),
            labels: None,
            copy_mark: mark(icons::CONTENT_COPY, ACTION_ICON),
            paste_mark: mark(CONTENT_PASTE, ACTION_ICON),
            close_mark: mark(icons::CLOSE, CLOSE_ICON),
            pressed: None,
            copies: 0,
            pastes: 0,
        }
    }

    #[test]
    fn only_a_paste_that_hands_text_to_the_seam_is_counted() {
        let mut empty = chip(Rc::new(|| None));
        empty.paste();
        assert_eq!(empty.pastes(), 0, "an empty clipboard read is no paste");

        let mut full = chip(Rc::new(|| Some("ls".to_string())));
        full.paste();
        assert_eq!(full.pastes(), 1);
    }

    #[test]
    fn every_mark_is_a_vector_that_parses_inside_its_design_box() {
        for (name, source) in [
            ("CONTENT_COPY", icons::CONTENT_COPY),
            ("CONTENT_PASTE", CONTENT_PASTE),
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
