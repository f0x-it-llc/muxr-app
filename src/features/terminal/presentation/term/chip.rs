//! The **Copy chip**: the small pill that appears where a selection drag
//! released, with a `Copy` action and a dismiss.
//!
//! ```text
//! ┌───────────────────────────────┐
//! │  ⧉  Copy            │    ✕     │
//! └───────────────────────────────┘
//! ```
//!
//! **A full-bleed overlay, not a child of the grid.** The chip is a **sibling
//! above** the gesture-wrapped grid (see
//! [`terminal_surface`](super::terminal_surface)), laid out over the whole
//! terminal rect and painting one pill inside it. Two things fall out, both
//! wanted: a press on the chip never reaches the grid or the gesture detector, so
//! tapping `Copy` cannot clear the selection it is about to copy; and the pill's
//! clamp box is the *terminal rect* the spec names, not the grid's own
//! sub-cell-rounded box. It reads its state straight off the shared
//! [`SelectionSeam`] on every pass — no signal, no rebuild: a paint pass walks the
//! whole tree, so the `request_redraw` the grid already issues is the mechanism.
//!
//! Both marks are filled vector paths (the "chrome icons are vectors" rule):
//! [`CLOSE`](frust::icons::CLOSE) from the vendored set, and [`COPY_MARK`]
//! authored here because the set carries no copy glyph.
//!
//! **The clipboard write is injected.** [`CopySink`] is the seam a test
//! substitutes: the default ([`system_clipboard_sink`]) is
//! `frust_clipboard::Clipboard::set_text_sensitive` — **never** plain `set_text`,
//! because terminal output is token-bearing — and a headless test injects a
//! recorder, since the desktop backend would otherwise clobber the developer's
//! real clipboard on every `cargo test`.

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

// Geometry — transcribed from the incumbent, exactly.

/// The pill's width, in logical px. Derived from the clamp the incumbent applies
/// rather than guessed: its x is clamped to `width - 140` and its y to
/// `height - 48`, i.e. "keep a [`CHIP_MARGIN`] gutter past a [`CHIP_W`] x
/// [`CHIP_H`] pill".
pub const CHIP_W: f64 = 132.0;
/// The pill's height — see [`CHIP_W`].
pub const CHIP_H: f64 = 40.0;
/// How far **left** of the release point the pill's left edge starts.
pub const CHIP_OFFSET_X: f64 = 48.0;
/// How far **above** the release point the pill's top edge starts — enough that
/// the finger that just released is not covering the thing it must now tap.
pub const CHIP_OFFSET_Y: f64 = 56.0;
/// The gutter the pill is kept inside the terminal rect by.
pub const CHIP_MARGIN: f64 = 8.0;

/// The pill's corner radius: a true pill (`--radius-full` in
/// `docs/terminal.html`).
const CHIP_RADIUS: f64 = CHIP_H / 2.0;
/// Padding at the pill's leading edge, before the copy mark.
const CHIP_PAD_X: f64 = 14.0;
/// The copy mark's drawn size.
const COPY_ICON: f64 = 16.0;
/// Gap between the copy mark and its label.
const ICON_LABEL_GAP: f64 = 8.0;
/// The label's size, in logical px.
const LABEL_SIZE: f32 = 13.0;
/// Where the divider sits, measured from the pill's left edge — and therefore
/// **the split between the two press targets**.
const DIVIDER_X: f64 = 92.0;
/// The divider's width and height.
const DIVIDER_W: f64 = 1.0;
const DIVIDER_H: f64 = 20.0;
/// The dismiss mark's drawn size.
const CLOSE_ICON: f64 = 14.0;
/// The pill's border width.
const BORDER_W: f64 = 1.0;
/// The pill's drop shadow: blur radius and ink alpha. A floating affordance
/// needs to read as floating over a busy grid.
const SHADOW_BLUR: f64 = 8.0;
const SHADOW_ALPHA: f32 = 0.45;

/// The side length of the square design box [`COPY_MARK`] is authored against —
/// Material Symbols' own 24, matching every other vector in the app.
const ICON_DESIGN_BOX: f64 = 24.0;

/// The copy mark: a filled sheet with a second sheet's L-shaped edge behind it.
/// Authored here — the vendored `frust::icons` set carries no copy glyph. The two
/// subpaths are **disjoint**, so the mark reads as two sheets in a single ink
/// rather than merging into one blob.
pub const COPY_MARK: IconSource = IconSource {
    d: "M 3 7 h 3 v 11 h 11 v 3 H 3 Z M 9 3 h 12 v 13 H 9 Z",
    design: ICON_DESIGN_BOX,
};

/// What a successful copy says.
pub const COPIED_TOAST: &str = "Copied to clipboard";
/// …and what a refused one says. A clipboard write can fail (an Android host that
/// never initialized its platform handles, a Wayland session with no data-control
/// protocol), and claiming a copy that did not happen is worse than saying so.
pub const COPY_FAILED_TOAST: &str = "Couldn't copy to clipboard";

// The copy sink.

/// Where `Copy` hands the selected text; answers whether the write landed. **The
/// injection point** — see the [module docs](self).
pub type CopySink = Rc<dyn Fn(&str) -> bool>;

/// The production sink: the system clipboard's **sensitive** write.
/// `set_text_sensitive`, never `set_text`: a terminal selection is exactly the
/// token-bearing content that plugin's security section names, and the sensitive
/// variant suppresses Android 13+'s copied-text preview and blocks iOS Universal
/// Clipboard from replicating it to the user's other devices.
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

/// Which half of the pill a press landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChipTarget {
    /// Everything left of the divider.
    Copy,
    /// Everything right of it.
    Dismiss,
}

/// The pill's rect inside a `bounds`-sized terminal rect, for a drag that released
/// at `anchor` (widget-local px): offset up and to the left of the finger, then
/// clamped to keep a [`CHIP_MARGIN`] gutter on every side. Written low-bound-last
/// (`max` after `min`) so a rect too small to hold the pill pins it to the
/// top-left corner instead of panicking on an inverted range.
pub fn chip_rect(anchor: Point, bounds: Size) -> Rect {
    let x = (anchor.x - CHIP_OFFSET_X)
        .min(bounds.width - CHIP_MARGIN - CHIP_W)
        .max(CHIP_MARGIN);
    let y = (anchor.y - CHIP_OFFSET_Y)
        .min(bounds.height - CHIP_MARGIN - CHIP_H)
        .max(CHIP_MARGIN);
    Rect::new(x, y, x + CHIP_W, y + CHIP_H)
}

// The view.

/// The declarative chip overlay — see [`copy_chip`].
pub struct CopyChipView {
    seam: SelectionSeam,
    sink: CopySink,
    fill: Color,
    border: Color,
    ink: Color,
    muted: Color,
    label: TextStyle,
}

/// The Copy chip over `seam`'s selection, inked from this screen's own token
/// palette and writing to the system clipboard. Mount it as the **top** child of
/// [`terminal_surface`](super::terminal_surface) — it is inert until a selection
/// drag releases.
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
    /// Replace the clipboard write with `sink` — **the test seam** (see the
    /// [module docs](self)). Production never calls this.
    #[must_use]
    pub fn copy_sink(mut self, sink: CopySink) -> Self {
        self.sink = sink;
        self
    }
}

/// The retained half of a [`CopyChipView`].
pub struct CopyChipWidget {
    seam: SelectionSeam,
    sink: CopySink,
    fill: Color,
    border: Color,
    ink: Color,
    muted: Color,
    label_style: TextStyle,
    /// The shaped `Copy` run, cached across frames (`paint` cannot shape).
    label: Option<TextLayout>,
    /// The two marks, already scaled out of their design box.
    copy_mark: BezPath,
    close_mark: BezPath,
    /// The pill a press is captured on, and which half it landed on.
    pressed: Option<ChipTarget>,
    /// How many copies this chip has performed — a test/diagnostic accessor.
    copies: u32,
}

/// Parse `source`'s SVG data and scale it out of its design box to `size`. A parse
/// failure is a wiring bug in a constant in this file, not a runtime condition —
/// the same call and reasoning as `frust_widgets::icon`'s own parse.
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
            copy_mark: mark(COPY_MARK, COPY_ICON),
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
        // The seam is `Rc`-backed and the sink is a closure: neither is usefully
        // comparable, so both are reinstalled every rebuild.
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
            // Re-shaping happens in `layout`, so a restyled label must relayout
            // — a bare PAINT would replay the stale run.
            element.label = None;
            flags |= ChangeFlags::LAYOUT | ChangeFlags::PAINT;
        }
        flags
    }
}

impl Widget for CopyChipWidget {
    /// Take the whole offered box: this is the terminal rect the pill clamps
    /// inside, not a box around the pill. An unbounded axis collapses to zero —
    /// there is no rect to clamp into, so nothing honest to place.
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
        // A hairline just inside the bounds: the outer pill in the border tone,
        // then the fill inset over it (`PaintScene` has no stroked-rounded-rect
        // primitive) — the key bar's own cap treatment.
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

        // The pressed half, drawn as a translucent wash rather than a scale
        // transform: half a pill cannot scale about its own centre without
        // detaching from the other half.
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

    /// Hit-test the pill only: everything else falls through to the grid
    /// underneath (`route_event` keeps walking down the stack on `Ignored`).
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
                // Follow the finger in and out, exactly as a key cap does.
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
                    // Released off-target: the app's fire-on-up-inside rule.
                    return EventResult::Handled;
                }
                match armed {
                    ChipTarget::Copy => self.copy(),
                    ChipTarget::Dismiss => self.seam.clear(),
                }
                EventResult::Handled
            }
            // Flags only — a `Cancel` arm never touches app state, so the
            // selection survives a stolen gesture and the chip stays up.
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
        // Bounds are overridden per node: this widget is full-bleed, but its two
        // press targets are the pill's halves, and a screen reader told "Copy"
        // covers the whole terminal would be told a lie.
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
    /// Copy the live selection, dismiss the chip, and say so. The clear happens
    /// **whether or not** the write landed: the chip is gone either way, and a
    /// toast is what tells the two apart.
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

    /// How many times `Copy` has been pressed — a test/diagnostic accessor.
    pub fn copies(&self) -> u32 {
        self.copies
    }
}

/// Which half of `pill` a widget-local `position` lands on, or `None` when it
/// misses the pill entirely.
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
        // Top-left: the offset would take it negative.
        let top_left = chip_rect(Point::new(4.0, 4.0), BOX);
        assert_eq!((top_left.x0, top_left.y0), (CHIP_MARGIN, CHIP_MARGIN));
        // Bottom-right: the incumbent's `[8, width-140] x [8, height-48]`. The
        // release is *past* the rect's own corner, so both axes really do hit
        // the clamp rather than the offset happening to land inside it.
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
        // Everything outside falls through to the grid.
        assert_eq!(target_at(Point::new(pill.x0 - 1.0, pill.y0), pill), None);
        assert_eq!(target_at(Point::new(pill.x0, pill.y1), pill), None);
    }

    #[test]
    fn both_marks_are_vectors_that_parse_inside_their_design_box() {
        for (name, source) in [("COPY_MARK", COPY_MARK), ("CLOSE", icons::CLOSE)] {
            let path = BezPath::from_svg(source.d).unwrap_or_else(|e| panic!("{name}: {e}"));
            let bounds = path.bounding_box();
            assert!(bounds.x0 >= 0.0 && bounds.y0 >= 0.0, "{name}: {bounds:?}");
            assert!(
                bounds.x1 <= source.design && bounds.y1 <= source.design,
                "{name}: {bounds:?}"
            );
        }
    }

    #[test]
    fn the_copy_marks_two_sheets_do_not_merge_into_one_blob() {
        // The back sheet's L-shaped arms must clear the front sheet's box, or the
        // mark reads as a single filled rectangle at 16px. Sampled by winding
        // rather than by bounding box — the boxes overlap; the fills must not.
        let path = BezPath::from_svg(COPY_MARK.d).expect("parses");
        let filled = |x: f64, y: f64| path.winding(Point::new(x, y)) != 0;
        assert!(filled(4.5, 12.0), "the back sheet's vertical arm is inked");
        assert!(filled(12.0, 19.5), "…and its horizontal one");
        assert!(filled(15.0, 9.0), "the front sheet is inked");
        assert!(!filled(7.5, 12.0), "a gap between the two sheets");
        assert!(!filled(15.0, 17.0), "…and below the front one");
    }
}
