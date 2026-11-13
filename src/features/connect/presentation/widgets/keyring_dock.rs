//! The unlock dock closing piece 8: one press unlocks the whole keyring behind a single
//! prompt. [`PressTarget`] exists because `frust::button` paints a text label only.

use std::rc::Rc;

use frust::authoring::{
    Action, BoxConstraints, BuildCtx, ChangeFlags, ChildPod, Color, ErasedCallback, EventCtx,
    EventResult, InputEvent, LayoutCtx, PRESSED_OPACITY, PaintCtx, PaintScene, Point, PointerPhase,
    Role, SemanticsCtx, Size, View, Widget, build_child, erase_callback, rebuild_child,
    teardown_child,
};
use frust::{
    Align, Alignment, ButtonStyle, EdgeInsets, IconSource, Padding, SizedBox, Stack, Theme, button,
    container, flexible, icon, inflexible, text,
};
use frust_glyph::progress;

use super::super::controller::DockState;
use super::{block_column, centered_row, h_gap, v_gap};
use crate::features::connect::domain::entities::SavedServer;
use crate::ui::paint::HAIRLINE;
use crate::ui::palette::{Palette, inked};
use crate::ui::spacing;
use crate::ui::splash::spec::{DOCK_GAP, DOCK_HEIGHT};

pub const UNLOCKING_LABEL: &str = "Unlocking…";
pub const TRY_AGAIN_LABEL: &str = "Try again";
pub const RESET_LABEL: &str = "Reset saved credentials";

/// A ring rather than a fingerprint or a face: the gate is either, per platform.
const BIOMETRIC_MARK: IconSource = IconSource {
    d: "M 12 2 A 10 10 0 1 1 12 22 A 10 10 0 1 1 12 2 Z M 12 4 A 8 8 0 1 0 12 20 A 8 8 0 1 0 12 4 Z M 12 8 A 4 4 0 1 1 12 16 A 4 4 0 1 1 12 8 Z",
    design: 24.0,
};
const MARK: f64 = 20.0;
const BAND_FILL: f64 = 0.4;
const BAND_HEIGHT: f64 = 3.0;
const TRAILING_ALPHA: f32 = 0.7;

type PressHandler<S> = Rc<dyn Fn(&mut S)>;

pub struct DockActions {
    pub on_unlock: Rc<dyn Fn()>,
    pub on_reset: Rc<dyn Fn()>,
}

pub fn dock_label(rows: usize) -> String {
    if rows == 1 {
        "Unlock".to_string()
    } else {
        format!("Unlock {rows} servers")
    }
}

/// `saved` is non-empty; the caller omits the dock otherwise.
pub fn keyring_dock<S: 'static>(
    theme: &Theme,
    colors: &Palette,
    saved: &[SavedServer],
    state: &DockState,
    actions: Option<&DockActions>,
) -> impl View<S> + use<S> {
    let on_amber = theme.scheme().on_primary;
    let trailing = saved.first().map(|row| format!("→ {}", row.host));
    let unlock = actions.map(|actions| Rc::clone(&actions.on_unlock));
    let (title, trailing, fill, ink, border, on_press) = match state {
        DockState::Idle => (
            dock_label(saved.len()),
            trailing,
            colors.amber,
            on_amber,
            None,
            unlock,
        ),
        DockState::Unlocking => (
            UNLOCKING_LABEL.to_string(),
            None,
            colors.amber_dim,
            on_amber,
            None,
            None,
        ),
        DockState::Failed(_) => (
            TRY_AGAIN_LABEL.to_string(),
            None,
            colors.raised,
            colors.fg,
            Some(colors.error),
            unlock,
        ),
        DockState::Unavailable => (
            dock_label(saved.len()),
            trailing,
            colors.raised,
            colors.fg_dim,
            Some(colors.outline),
            None,
        ),
    };
    let mut items = vec![
        inflexible(icon(BIOMETRIC_MARK).size(MARK).color(ink)),
        inflexible(h_gap::<S>(spacing::MD)),
        flexible(
            1,
            text(title.clone()).style(inked(&theme.type_scale.title_small_emphasized, ink)),
        ),
    ];
    if let Some(trailing) = trailing {
        items.push(inflexible(text(trailing).style(inked(
            &theme.type_scale.label_small,
            ink.with_alpha(TRAILING_ALPHA),
        ))));
    }
    let radius = theme.shape.small;
    let mut face = container(Align(
        Alignment::CENTER,
        Padding(
            EdgeInsets::symmetric(spacing::XL, 0.0),
            centered_row::<S>(items),
        ),
    ))
    .fill(fill)
    .radius(radius);
    if let Some(border) = border {
        face = face.border(border, HAIRLINE);
    }
    let band = matches!(state, DockState::Unlocking).then(|| {
        Align(
            Alignment::BOTTOM_LEFT,
            Padding(
                EdgeInsets::symmetric(radius, 0.0),
                SizedBox::<S>(None, Some(BAND_HEIGHT)).child(progress(BAND_FILL)),
            ),
        )
    });
    let target = SizedBox::<S>(None, Some(DOCK_HEIGHT)).child(PressTarget {
        child: Stack((face, band)),
        label: title,
        radius,
        wash: ink,
        on_press: on_press.map(|press| Rc::new(move |_state: &mut S| press()) as PressHandler<S>),
    });
    let failure = match state {
        DockState::Failed(failure) => Some(failure.clone()),
        _ => None,
    };
    let reset = failure
        .as_ref()
        .filter(|failure| failure.offers_reset)
        .and(actions)
        .map(|actions| {
            let on_reset = Rc::clone(&actions.on_reset);
            button(RESET_LABEL, move |_state: &mut S| on_reset())
                .style(ButtonStyle::Ghost)
                .small()
        });
    block_column::<S, _>((
        v_gap::<S>(DOCK_GAP),
        target,
        failure.map(|failure| {
            block_column::<S, _>((
                v_gap::<S>(spacing::SM),
                text(failure.message).style(inked(&theme.type_scale.label_small, colors.fg_muted)),
            ))
        }),
        reset.map(|reset| block_column::<S, _>((v_gap::<S>(spacing::TIGHT), reset))),
    ))
}

/// `None` is disabled: no input, disabled semantics.
struct PressTarget<S: 'static, V> {
    child: V,
    label: String,
    radius: f64,
    wash: Color,
    on_press: Option<PressHandler<S>>,
}

struct PressTargetWidget {
    child: ChildPod,
    label: String,
    radius: f64,
    wash: Color,
    pressed: bool,
    captured: bool,
    on_press: Option<ErasedCallback>,
}

impl<S: 'static, V: View<S>> View<S> for PressTarget<S, V> {
    type Element = PressTargetWidget;

    fn build(&self, ctx: &mut BuildCtx<'_>) -> PressTargetWidget {
        PressTargetWidget {
            child: build_child(&self.child, ctx),
            label: self.label.clone(),
            radius: self.radius,
            wash: self.wash,
            pressed: false,
            captured: false,
            on_press: self.on_press.as_ref().map(|press| erase_callback(press)),
        }
    }

    fn rebuild(
        &self,
        prev: &Self,
        element: &mut PressTargetWidget,
        ctx: &mut BuildCtx<'_>,
    ) -> ChangeFlags {
        // Closures aren't comparable; reinstall the adapter every time.
        element.on_press = self.on_press.as_ref().map(|press| erase_callback(press));
        if element.on_press.is_none() {
            element.pressed = false;
            element.captured = false;
        }
        let mut flags = rebuild_child(&prev.child, &self.child, &mut element.child, ctx);
        let enabled_changed = prev.on_press.is_some() != self.on_press.is_some();
        if prev.label != self.label || prev.wash != self.wash || enabled_changed {
            element.label = self.label.clone();
            element.wash = self.wash;
            element.radius = self.radius;
            // Semantics are recomputed off the paint pass.
            flags |= ChangeFlags::PAINT;
        }
        flags
    }

    fn teardown(&self, element: &mut PressTargetWidget, ctx: &mut BuildCtx<'_>) {
        teardown_child(&self.child, &mut element.child, ctx);
    }
}

fn inside(pos: Point, size: Size) -> bool {
    pos.x >= 0.0 && pos.y >= 0.0 && pos.x < size.width && pos.y < size.height
}

impl Widget for PressTargetWidget {
    fn layout(&mut self, ctx: &mut LayoutCtx, bc: &BoxConstraints) -> Size {
        let size = self.child.layout_child(ctx, bc);
        self.child.set_origin(Point::new(0.0, 0.0));
        size
    }

    fn paint(&mut self, ctx: &mut PaintCtx, scene: &mut dyn PaintScene) {
        self.child.paint_child(ctx, scene);
        if self.pressed {
            let wash = self.wash.with_alpha(PRESSED_OPACITY);
            scene.fill_rounded_rect(ctx.origin(), ctx.size(), self.radius, wash);
        }
    }

    fn event(&mut self, ctx: &mut EventCtx, event: &InputEvent) -> EventResult {
        let InputEvent::Pointer(pointer) = event else {
            return EventResult::Ignored;
        };
        if self.on_press.is_none() {
            return EventResult::Ignored;
        }
        match pointer.phase {
            PointerPhase::Down => {
                self.pressed = true;
                self.captured = true;
                ctx.capture_pointer();
            }
            PointerPhase::Move if self.captured => {
                self.pressed = inside(pointer.position, ctx.size());
            }
            PointerPhase::Up if self.captured => {
                if inside(pointer.position, ctx.size())
                    && let Some(on_press) = self.on_press.as_mut()
                {
                    on_press(ctx);
                }
                self.pressed = false;
                self.captured = false;
            }
            PointerPhase::Cancel if self.captured => {
                self.pressed = false;
                self.captured = false;
            }
            _ => return EventResult::Ignored,
        }
        ctx.request_redraw();
        EventResult::Handled
    }

    fn semantics(&self, ctx: &mut SemanticsCtx) {
        ctx.push_node(Role::Button, |node| {
            node.set_label(self.label.as_str());
            if self.on_press.is_some() {
                node.add_action(Action::Click);
            } else {
                node.set_disabled();
            }
        });
    }

    fn visit_children(&self, visitor: &mut dyn FnMut(&ChildPod)) {
        visitor(&self.child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_counts_rows_past_the_first() {
        assert_eq!(dock_label(1), "Unlock");
        assert_eq!(dock_label(3), "Unlock 3 servers");
    }

    #[test]
    fn the_dock_clears_the_minimum_tap_target() {
        const { assert!(DOCK_HEIGHT >= 48.0 && MARK < DOCK_HEIGHT) };
    }
}
